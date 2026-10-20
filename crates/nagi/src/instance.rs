//! 1 つのプロセスで複数の窓（2 回目の起動は、動いている nagi に窓を足してもらう）。
//!
//! 窓ごとに別のプロセスを立てると、GPU の装置・窓と日本語入力の土台・フォントなど約 20 MB が窓の数だけ要る。
//! 動いている nagi は名前付きパイプで待ち、2 回目の nagi は開くファイルを書き込んで、自分は窓を作らずに終わる。
//!
//! パイプの名前は設定の置き場所（LOCALAPPDATA）ごとに分ける。撮影・計測は別の置き場所で動かすので、普段の nagi には渡らない。
//! 書くのは 2 行: `cwd\t<2 回目の nagi を起動したフォルダ>\n` と `open\t<絶対パス（空ならファイル無し）>\n`。
//! 足した窓は起動したフォルダを作業フォルダにする（プロセスの作業フォルダは窓をまたいで 1 つなので、窓ごとに持つ）。
//! 知らない行は読み飛ばすので、古い nagi に渡しても open の行は届く。

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
    let cwd = std::env::current_dir().map_or(String::new(), |d| format!("cwd\t{}\n", d.display()));
    let line = format!("{cwd}open\t{}\n", abs.as_deref().map_or(String::new(), |p| p.display().to_string()));
    pipe.write_all(line.as_bytes()).is_ok()
}

/// 届いた 1 行
#[derive(Debug, PartialEq, Eq)]
pub enum Msg {
    /// 2 回目の nagi を起動したフォルダ（続く open の窓の作業フォルダ）
    Cwd(PathBuf),
    /// 窓を足して開くファイル（無ければ None）
    Open(Option<PathBuf>),
}

/// 1 行を読む（`cwd\t...`・`open\t...`）。読めない行は Err
pub fn parse(line: &str) -> Result<Msg, ()> {
    let line = line.trim_end_matches(['\r', '\n']);
    if let Some(d) = line.strip_prefix("cwd\t") {
        return if d.is_empty() { Err(()) } else { Ok(Msg::Cwd(PathBuf::from(d))) };
    }
    let rest = line.strip_prefix("open\t").ok_or(())?;
    Ok(Msg::Open((!rest.is_empty()).then(|| PathBuf::from(rest))))
}

/// 2 回目の起動を待つ（裏のスレッド）。届くたびに `on_open(開くファイル, 起動したフォルダ)` を呼ぶ。パイプを作れなければ（ほかの nagi が先に作った）何もしない
pub fn serve(on_open: impl Fn(Option<PathBuf>, Option<PathBuf>) + Send + 'static) {
    serve_on(&pipe_name(), on_open)
}

fn serve_on(name: &str, on_open: impl Fn(Option<PathBuf>, Option<PathBuf>) + Send + 'static) {
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
                let mut cwd = None;
                for line in text.lines() {
                    match parse(line) {
                        Ok(Msg::Cwd(d)) => cwd = Some(d),
                        Ok(Msg::Open(file)) => on_open(file, cwd.take()),
                        Err(()) => {}
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
        assert_eq!(parse("open\tC:\\a\\b.rs\n"), Ok(Msg::Open(Some(PathBuf::from("C:\\a\\b.rs")))));
        assert_eq!(parse("open\t\r\n"), Ok(Msg::Open(None)));
        assert_eq!(parse("cwd\tC:\\proj\r\n"), Ok(Msg::Cwd(PathBuf::from("C:\\proj"))));
        assert_eq!(parse("close\tx"), Err(()));
    }

    /// 待っている nagi へ渡すと、届いたファイルで on_open が呼ばれる（同じ名前のパイプは 2 つ目を作れない）
    #[test]
    fn hands_off_to_the_running_instance() {
        // 試験だけの名前（普段の nagi のパイプに繋がらないように）
        let name = format!(r"\\.\pipe\nagi-test-{}", std::process::id());
        assert!(!hand_off_to(&name, None), "まだ誰も待っていない");
        let (tx, rx) = std::sync::mpsc::channel();
        serve_on(&name, move |f, cwd| tx.send((f, cwd)).unwrap());
        assert!(hand_off_to(&name, Some(Path::new("C:\\x\\y.txt"))));
        // 起動したフォルダも届く（足した窓の作業フォルダ）
        let here = Some(std::env::current_dir().unwrap());
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(), (Some(PathBuf::from("C:\\x\\y.txt")), here.clone()));
        // 2 回目も届く
        assert!(hand_off_to(&name, None));
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(), (None, here));
    }
}
