//! 1 つのプロセスで複数の窓（2 回目の起動は、動いている nagi に窓を足してもらう）。
//!
//! 窓ごとに別のプロセスを立てると、GPU の装置・窓と日本語入力の土台・フォントなど約 20 MB が窓の数だけ要る。
//! 動いている nagi は名前付きパイプで待ち、2 回目の nagi は開くファイルを書き込んで、自分は窓を作らずに終わる。
//!
//! パイプの名前は設定の置き場所（LOCALAPPDATA）ごとに分ける。撮影・計測は別の置き場所で動かすので、普段の nagi には渡らない。
//! 書くのは 1 行: `open\t<絶対パス（空ならファイル無し）>\n`。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// パイプの名前（利用者と設定の置き場所ごと）
pub fn pipe_name() -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::env::var_os("LOCALAPPDATA").hash(&mut h);
    std::env::var_os("USERNAME").hash(&mut h);
    format!(r"\\.\pipe\nagi-{:016x}", h.finish())
}

/// 動いている nagi へ `file` を渡す。渡せたら true（呼んだ側は終わる）。動いている nagi が無ければ false
pub fn hand_off(file: Option<&Path>) -> bool {
    hand_off_to(&pipe_name(), file)
}

fn hand_off_to(name: &str, file: Option<&Path>) -> bool {
    let Ok(mut pipe) = std::fs::OpenOptions::new().write(true).open(name) else { return false };
    // 足した窓を前に出せるように（前に出す権利は、利用者が起動したこちらのプロセスにある）
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(windows::Win32::UI::WindowsAndMessaging::ASFW_ANY);
    }
    let abs = file.map(|p| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()));
    let line = format!("open\t{}\n", abs.as_deref().map_or(String::new(), |p| p.display().to_string()));
    pipe.write_all(line.as_bytes()).is_ok()
}

/// 1 行を読む（`open\t...`）。開くファイル（無ければ None）。読めない行は Err
pub fn parse(line: &str) -> Result<Option<PathBuf>, ()> {
    let rest = line.trim_end_matches(['\r', '\n']).strip_prefix("open\t").ok_or(())?;
    Ok((!rest.is_empty()).then(|| PathBuf::from(rest)))
}

/// 2 回目の起動を待つ（裏のスレッド）。届くたびに `on_open` を呼ぶ。パイプを作れなければ（ほかの nagi が先に作った）何もしない
pub fn serve(on_open: impl Fn(Option<PathBuf>) + Send + 'static) {
    serve_on(&pipe_name(), on_open)
}

fn serve_on(name: &str, on_open: impl Fn(Option<PathBuf>) + Send + 'static) {
    use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND};
    use windows::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT};
    let name: Vec<u16> = name.encode_utf16().chain([0]).collect();
    let create = move |first: bool| {
        let mode = if first { PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE } else { PIPE_ACCESS_INBOUND };
        let h = unsafe { CreateNamedPipeW(windows::core::PCWSTR(name.as_ptr()), mode, PIPE_TYPE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS, PIPE_UNLIMITED_INSTANCES, 0, 4096, 0, None) };
        // 窓のスレッドへは番号（isize）で渡す（HANDLE はスレッドをまたげない型）
        (!h.is_invalid()).then_some(h.0 as isize)
    };
    let Some(first) = create(true) else { return };
    let _ = std::thread::Builder::new().name("instance".into()).spawn(move || {
        let mut h = first;
        loop {
            // 繋がるのを待つ（先に繋がっていれば ERROR_PIPE_CONNECTED で、そのまま読む）
            let _ = unsafe { ConnectNamedPipe(windows::Win32::Foundation::HANDLE(h as *mut _), None) };
            // 次の 2 回目のために、読む前に次のパイプを作っておく
            let next = create(false);
            let mut f = unsafe { <std::fs::File as std::os::windows::io::FromRawHandle>::from_raw_handle(h as *mut _) };
            let mut text = String::new();
            if f.read_to_string(&mut text).is_ok() {
                for line in text.lines() {
                    if let Ok(file) = parse(line) {
                        on_open(file);
                    }
                }
            }
            drop(f);
            match next {
                Some(n) => h = n,
                None => return,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_open_lines() {
        assert_eq!(parse("open\tC:\\a\\b.rs\n"), Ok(Some(PathBuf::from("C:\\a\\b.rs"))));
        assert_eq!(parse("open\t\r\n"), Ok(None));
        assert_eq!(parse("close\tx"), Err(()));
    }

    /// 待っている nagi へ渡すと、届いたファイルで on_open が呼ばれる（同じ名前のパイプは 2 つ目を作れない）
    #[test]
    fn hands_off_to_the_running_instance() {
        // 試験だけの名前（普段の nagi のパイプに繋がらないように）
        let name = format!(r"\\.\pipe\nagi-test-{}", std::process::id());
        assert!(!hand_off_to(&name, None), "まだ誰も待っていない");
        let (tx, rx) = std::sync::mpsc::channel();
        serve_on(&name, move |f| tx.send(f).unwrap());
        assert!(hand_off_to(&name, Some(Path::new("C:\\x\\y.txt"))));
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(), Some(PathBuf::from("C:\\x\\y.txt")));
        // 2 回目も届く
        assert!(hand_off_to(&name, None));
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(), None);
    }
}
