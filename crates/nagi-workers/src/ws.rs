//! 手元（localhost）の HTTP と WebSocket の小さなクライアント（ブラウザのデバッグ・cdp.rs 用）。TLS は使わない。
//!
//! HTTP は GET だけ（Content-Length と chunked）。WebSocket は RFC 6455 のテキストのフレーム
//! （送るものはマスクする、分かれたフレームをつなぐ、ping に pong を返す）。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// "http://host:port/path" → (host:port, /path)
fn split_url(url: &str) -> Result<(String, String), String> {
    let rest = url.strip_prefix("http://").or_else(|| url.strip_prefix("ws://")).ok_or_else(|| format!("http・ws だけ: {url}"))?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let host = if host.contains(':') { host.to_string() } else { format!("{host}:80") };
    Ok((host, path.to_string()))
}

fn connect(host: &str) -> Result<TcpStream, String> {
    // localhost は IPv6 にもなるので、つながる方へ
    let addrs: Vec<std::net::SocketAddr> = std::net::ToSocketAddrs::to_socket_addrs(host).map_err(|e| format!("{host}: {e}"))?.collect();
    let mut last = format!("{host} が見つかりません");
    for a in addrs {
        match TcpStream::connect_timeout(&a, Duration::from_secs(5)) {
            Ok(s) => return Ok(s),
            Err(e) => last = format!("{host}: {e}"),
        }
    }
    Err(last)
}

/// HTTP の GET（本文）
pub fn get(url: &str) -> Result<Vec<u8>, String> {
    let (host, path) = split_url(url)?;
    let mut s = connect(&host)?;
    s.set_read_timeout(Some(Duration::from_secs(10))).ok();
    write!(s, "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nAccept: */*\r\n\r\n").map_err(|e| e.to_string())?;
    let mut r = BufReader::new(s);
    let mut status = String::new();
    r.read_line(&mut status).map_err(|e| e.to_string())?;
    if !status.split_whitespace().nth(1).is_some_and(|c| c.starts_with('2')) {
        return Err(format!("{url}: {}", status.trim()));
    }
    let (mut len, mut chunked) = (None, false);
    loop {
        let mut h = String::new();
        r.read_line(&mut h).map_err(|e| e.to_string())?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        let lower = h.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            len = v.trim().parse::<usize>().ok();
        }
        if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
            chunked = true;
        }
    }
    let mut body = Vec::new();
    if chunked {
        loop {
            let mut size = String::new();
            r.read_line(&mut size).map_err(|e| e.to_string())?;
            let n = usize::from_str_radix(size.trim().split(';').next().unwrap_or("0"), 16).map_err(|_| "chunked が読めません")?;
            if n == 0 {
                break;
            }
            let mut buf = vec![0u8; n];
            r.read_exact(&mut buf).map_err(|e| e.to_string())?;
            body.extend_from_slice(&buf);
            let mut crlf = String::new();
            r.read_line(&mut crlf).map_err(|e| e.to_string())?;
        }
    } else if let Some(n) = len {
        body.resize(n, 0);
        r.read_exact(&mut body).map_err(|e| e.to_string())?;
    } else {
        r.read_to_end(&mut body).map_err(|e| e.to_string())?;
    }
    Ok(body)
}

const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(d: &[u8]) -> String {
    let mut s = String::new();
    for c in d.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            if k <= c.len() {
                s.push(B64[(n >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=') {
        let v = match c {
            b'-' => 62,
            b'_' => 63,
            c => B64.iter().position(|x| *x == c)? as u32,
        };
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// WebSocket（テキストを送る・受け取る）
pub struct Ws {
    s: TcpStream,
}

impl Ws {
    pub fn connect(url: &str) -> Result<Ws, String> {
        let (host, path) = split_url(url)?;
        let mut s = connect(&host)?;
        let key = base64_encode(&std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos().to_le_bytes()).unwrap_or_default());
        write!(s, "GET {path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n").map_err(|e| e.to_string())?;
        // 返事の見出しを読む（\r\n\r\n まで）
        let mut head = Vec::new();
        let mut b = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            s.read_exact(&mut b).map_err(|e| format!("WebSocket: {e}"))?;
            head.push(b[0]);
            if head.len() > 16 << 10 {
                return Err("WebSocket の返事が長すぎます".into());
            }
        }
        let head = String::from_utf8_lossy(&head);
        if !head.starts_with("HTTP/1.1 101") {
            return Err(format!("WebSocket につなげません: {}", head.lines().next().unwrap_or("")));
        }
        s.set_nodelay(true).ok();
        Ok(Ws { s })
    }

    pub fn try_clone(&self) -> Result<Ws, String> {
        Ok(Ws { s: self.s.try_clone().map_err(|e| e.to_string())? })
    }

    fn frame(&mut self, op: u8, data: &[u8]) -> Result<(), String> {
        let mut f = vec![0x80 | op];
        let n = data.len();
        if n < 126 {
            f.push(0x80 | n as u8);
        } else if n < 65536 {
            f.push(0x80 | 126);
            f.extend_from_slice(&(n as u16).to_be_bytes());
        } else {
            f.push(0x80 | 127);
            f.extend_from_slice(&(n as u64).to_be_bytes());
        }
        let mask: [u8; 4] = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(7)).to_le_bytes();
        f.extend_from_slice(&mask);
        f.extend(data.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        self.s.write_all(&f).map_err(|e| format!("WebSocket に書けません: {e}"))
    }

    pub fn send(&mut self, text: &str) -> Result<(), String> {
        self.frame(1, text.as_bytes())
    }

    /// 次のテキストのメッセージ（切れたら Err）
    pub fn recv(&mut self) -> Result<String, String> {
        let mut msg = Vec::new();
        loop {
            let mut h = [0u8; 2];
            self.s.read_exact(&mut h).map_err(|e| format!("WebSocket が切れました: {e}"))?;
            let fin = h[0] & 0x80 != 0;
            let op = h[0] & 0x0f;
            let masked = h[1] & 0x80 != 0;
            let mut n = (h[1] & 0x7f) as u64;
            if n == 126 {
                let mut b = [0u8; 2];
                self.s.read_exact(&mut b).map_err(|e| e.to_string())?;
                n = u16::from_be_bytes(b) as u64;
            } else if n == 127 {
                let mut b = [0u8; 8];
                self.s.read_exact(&mut b).map_err(|e| e.to_string())?;
                n = u64::from_be_bytes(b);
            }
            let mut mask = [0u8; 4];
            if masked {
                self.s.read_exact(&mut mask).map_err(|e| e.to_string())?;
            }
            let mut data = vec![0u8; n as usize];
            self.s.read_exact(&mut data).map_err(|e| e.to_string())?;
            if masked {
                data.iter_mut().enumerate().for_each(|(i, b)| *b ^= mask[i % 4]);
            }
            match op {
                8 => return Err("WebSocket が閉じられました".into()),
                9 => {
                    self.frame(10, &data)?;
                    continue;
                }
                10 => continue,
                _ => msg.extend_from_slice(&data),
            }
            if fin {
                return Ok(String::from_utf8_lossy(&msg).into_owned());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64() {
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("eyJ2IjozfQ").unwrap(), br#"{"v":3}"#);
    }
}
