//! ソースマップ（v3）。ブラウザで動いている JS の位置と、元の TypeScript の位置を行き来する（cdp.rs）。

/// 1 つの対応（動いている JS の 行, 列 → 元のソースの番号, 行）。行・列は 0 始まり
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Seg {
    line: u32,
    col: u32,
    src: u32,
    src_line: u32,
}

#[derive(Debug, Default)]
pub struct SourceMap {
    /// 元のソース（sourceRoot をつけたもの）
    pub sources: Vec<String>,
    /// 動いている JS の位置の順
    segs: Vec<Seg>,
}

fn vlq(s: &[u8], i: &mut usize) -> Option<i64> {
    let (mut v, mut shift) = (0i64, 0);
    loop {
        let c = *s.get(*i)?;
        *i += 1;
        let d = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as i64;
        v |= (d & 31) << shift;
        shift += 5;
        if d & 32 == 0 {
            break;
        }
    }
    Some(if v & 1 == 1 { -(v >> 1) } else { v >> 1 })
}

impl SourceMap {
    /// JSON を読む（sections の形は読まない）
    pub fn parse(json: &str) -> Option<SourceMap> {
        let v: serde_json::Value = serde_json::from_str(json).ok()?;
        let root = v.get("sourceRoot").and_then(|r| r.as_str()).unwrap_or("");
        let sources = v.get("sources")?.as_array()?.iter().map(|s| {
            let s = s.as_str().unwrap_or("");
            if root.is_empty() || s.contains("://") || s.starts_with('/') { s.to_string() } else { format!("{}/{s}", root.trim_end_matches('/')) }
        }).collect();
        let m = v.get("mappings")?.as_str()?.as_bytes();
        let mut segs = Vec::new();
        let (mut src, mut sline) = (0i64, 0i64);
        let (mut line, mut i) = (0u32, 0usize);
        while i <= m.len() {
            let mut col = 0i64;
            // 1 行ぶん
            while i < m.len() && m[i] != b';' {
                if m[i] == b',' {
                    i += 1;
                    continue;
                }
                col += vlq(m, &mut i)?;
                if i < m.len() && !matches!(m[i], b',' | b';') {
                    src += vlq(m, &mut i)?;
                    sline += vlq(m, &mut i)?;
                    vlq(m, &mut i)?;
                    if i < m.len() && !matches!(m[i], b',' | b';') {
                        vlq(m, &mut i)?;
                    }
                    segs.push(Seg { line, col: col as u32, src: src as u32, src_line: sline as u32 });
                }
            }
            line += 1;
            i += 1;
        }
        Some(SourceMap { sources, segs })
    }

    /// 動いている JS の (行, 列) → (元のソースの番号, 行)
    pub fn original(&self, line: u32, col: u32) -> Option<(usize, u32)> {
        let k = self.segs.partition_point(|s| (s.line, s.col) <= (line, col));
        let s = self.segs.get(k.checked_sub(1)?)?;
        // 同じ行のものが無ければ、その行の最初のもの
        let s = if s.line == line { s } else { self.segs.get(k).filter(|n| n.line == line)? };
        Some((s.src as usize, s.src_line))
    }

    /// 元のソース `src` の `line` 行 → 動いている JS の (行, 列)（いちばん前）
    pub fn generated(&self, src: usize, line: u32) -> Option<(u32, u32)> {
        self.segs.iter().filter(|s| s.src as usize == src && s.src_line == line).map(|s| (s.line, s.col)).min()
    }
}

/// 元のソースの名前をパスの形に（webpack://app/./src/a.ts → /src/a.ts、http://localhost:5173/src/a.ts → /src/a.ts）。
/// 相対のものはスクリプトの URL のフォルダからに
pub fn normalize(source: &str, script_url: &str) -> String {
    let strip_host = |u: &str| -> String {
        match u.split_once("://") {
            Some((scheme, rest)) if scheme == "webpack" => {
                // webpack://名前/./src/a.ts
                let p = rest.split_once('/').map_or("", |x| x.1);
                format!("/{}", p.trim_start_matches("./"))
            }
            Some(("file", rest)) => rest.trim_start_matches('/').to_string(),
            Some((_, rest)) => rest.find('/').map_or("/".into(), |i| rest[i..].to_string()),
            None => u.to_string(),
        }
    };
    let s = strip_host(source);
    let joined = if s.starts_with('/') || s.contains(":/") || s.contains(":\\") {
        s
    } else {
        let base = strip_host(script_url);
        let dir = base.rsplit_once('/').map_or("", |x| x.0);
        format!("{dir}/{s}")
    };
    // . と .. を畳む（? と # の後は捨てる）
    let joined = joined.split(['?', '#']).next().unwrap_or("").replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for p in joined.split('/') {
        match p {
            "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_both_ways() {
        // a.ts の 1 行目 → JS の 1 行目の 0 列、a.ts の 3 行目 → JS の 2 行目の 4 列
        let m = SourceMap::parse(r#"{"version":3,"sources":["a.ts"],"mappings":"AAAA;IAEA"}"#).unwrap();
        assert_eq!(m.original(0, 0), Some((0, 0)));
        assert_eq!(m.original(1, 10), Some((0, 2)));
        assert_eq!(m.generated(0, 2), Some((1, 4)));
        assert_eq!(m.generated(0, 1), None);
    }

    #[test]
    fn source_names() {
        assert_eq!(normalize("webpack://app/./src/App.tsx", ""), "/src/App.tsx");
        assert_eq!(normalize("App.tsx", "http://localhost:5173/src/App.tsx"), "/src/App.tsx");
        assert_eq!(normalize("../lib/x.ts", "http://localhost:5173/src/main.ts"), "/lib/x.ts");
        assert_eq!(normalize("http://localhost:5173/src/a.ts?t=1", ""), "/src/a.ts");
    }
}
