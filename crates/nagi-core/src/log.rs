//! ログ（ADR 0016 の 2）。`%LOCALAPPDATA%\nagi\nagi.log` に 1 行ずつ書く。
//!
//! - `init` を呼ぶまで（試験・道具）は何も書かない。
//! - 開くときに上限（設定の「ログの上限」）を超えていれば `nagi.old.log` に回す（前の分は 1 つだけ残す）。
//! - 標準エラーをつないでいない起動（エクスプローラーから）では、標準エラーも同じファイルへ向ける（main.rs）。
//! - 書く形: `2026-10-18 19:30:01.123 WARN  [スレッド] 本文`

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Level::Info => "INFO ",
            Level::Warn => "WARN ",
            Level::Error => "ERROR",
        }
    }
}

static FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

/// ログのファイルを開く（起動時に 1 回）。`max` バイトを超えていれば古いものに回す
pub fn init(path: PathBuf, max: u64) {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > max) {
        let _ = std::fs::rename(&path, path.with_extension("old.log"));
    }
    if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        if let Ok(mut g) = FILE.lock() {
            *g = Some(f);
        }
    }
}

/// 開いているログのファイルの写し（標準エラーを向けるため）
pub fn file_clone() -> Option<std::fs::File> {
    FILE.lock().ok()?.as_ref()?.try_clone().ok()
}

/// 既定の場所（%LOCALAPPDATA%\nagi\nagi.log）
pub fn default_path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("nagi").join("nagi.log"))
}

pub fn write(level: Level, msg: &str) {
    let Ok(mut g) = FILE.lock() else { return };
    let Some(f) = g.as_mut() else { return };
    let th = std::thread::current();
    let name = th.name().unwrap_or("?");
    // 複数行の本文は 2 行目から字下げ（1 件が 1 つのまとまりに見えるように）
    let body = msg.trim_end().replace('\n', "\n    ");
    let _ = writeln!(f, "{} {} [{name}] {body}", now(), level.label());
}

pub fn info(msg: &str) {
    write(Level::Info, msg);
}

pub fn warn(msg: &str) {
    write(Level::Warn, msg);
}

pub fn error(msg: &str) {
    write(Level::Error, msg);
}

/// 今の時刻（その PC の時間帯）
#[cfg(windows)]
pub fn now() -> String {
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    // SAFETY: 書き込み先を渡すだけ
    let t = unsafe {
        let mut t = std::mem::zeroed();
        GetLocalTime(&mut t);
        t
    };
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds)
}

#[cfg(not(windows))]
pub fn now() -> String {
    let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    format!("{s}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_rotates() {
        let dir = std::env::temp_dir().join(format!("nagi-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("nagi.log");
        std::fs::write(&p, vec![b'x'; 2000]).unwrap();
        init(p.clone(), 1000);
        warn("一行目\n二行目");
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("WARN  ["), "{text}");
        assert!(text.contains("一行目\n    二行目"), "{text}");
        assert!(dir.join("nagi.old.log").is_file(), "大きければ回す");
        assert_eq!(now().len(), 23);
        *FILE.lock().unwrap() = None;
    }
}
