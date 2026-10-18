//! データベースのドライバーの準備（DataGrip の「足りないドライバーのファイルをダウンロード」と同じ）。
//!
//! Oracle は Instant Client（Basic Light）の DLL が要る。`%LOCALAPPDATA%\nagi\drivers\oracle\instantclient_*` に
//! あればそれを、無ければ PATH などにあるもの（ODPI-C の既定の探し方）を使う。どちらにも無ければ、
//! 初めてつなぐときに UI が確かめてからダウンロードする（Windows に入っている curl.exe と tar.exe を使う）。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::sync::Mutex;

use crate::Waker;

/// Instant Client の Basic Light（Windows 64 ビット。いちばん新しい版を指す固定のリンク）
pub const ORACLE_CLIENT_URL: &str = "https://download.oracle.com/otn_software/nt/instantclient/instantclient-basiclite-windows.zip";
pub const ORACLE_CLIENT_SIZE_MB: u64 = 63;
pub const ORACLE_LICENSE: &str = "Oracle Free Use Terms and Conditions";

pub fn drivers_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("NAGI_DRIVERS_DIR") {
        return PathBuf::from(d);
    }
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("nagi").join("drivers")
}

/// Nagi が置いた Instant Client（いちばん新しいもの）
pub fn oracle_client_dir() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(drivers_dir().join("oracle")).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.join("oci.dll").is_file()).collect();
    dirs.sort();
    dirs.pop()
}

/// PATH にある oci.dll
fn oci_on_path() -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("oci.dll").is_file()))
}

/// Oracle につなぐ準備ができているか
pub fn oracle_ready() -> bool {
    oracle_client_dir().is_some() || oci_on_path()
}

static INIT: Mutex<bool> = Mutex::new(false);

/// Oracle のクライアントを読み込む（1 度だけ。失敗したら次にまた試す）
pub fn init_oracle() -> Result<(), String> {
    let mut done = INIT.lock().map_err(|e| e.to_string())?;
    if *done {
        return Ok(());
    }
    let mut p = oracle::InitParams::new();
    p.default_driver_name("nagi").map_err(|e| e.to_string())?;
    if let Some(dir) = oracle_client_dir() {
        p.oracle_client_lib_dir(dir.as_os_str()).map_err(|e| e.to_string())?;
    }
    match p.init() {
        Ok(_) => {
            *done = true;
            Ok(())
        }
        Err(e) if e.to_string().contains("DPI-1047") => Err("Oracle のドライバー（Instant Client）がありません。接続先の上で Enter を押すとダウンロードできます".into()),
        Err(e) => Err(e.to_string().lines().next().unwrap_or("").to_string()),
    }
}

/// ダウンロードの進み具合
#[derive(Clone, Debug)]
pub enum DlMsg {
    /// (受け取ったバイト, 全体のバイト。分からなければ 0)
    Progress(u64, u64),
    Extracting,
    Done(Result<PathBuf, String>),
}

pub struct Download {
    pub rx: Receiver<DlMsg>,
}

pub(crate) fn system_tool(name: &str) -> PathBuf {
    let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let p = root.join("System32").join(name);
    if p.is_file() { p } else { PathBuf::from(name) }
}

fn hidden(c: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped())
}

/// Instant Client をダウンロードして展開する（裏のスレッド）
pub fn download_oracle(waker: Waker) -> Download {
    download_zip(ORACLE_CLIENT_URL, ORACLE_CLIENT_SIZE_MB * 1024 * 1024, &drivers_dir().join("oracle"), waker)
}

pub fn download_zip(url: &str, expect: u64, dest: &Path, waker: Waker) -> Download {
    let find = |dest: &Path| -> Option<PathBuf> {
        oracle_client_dir().filter(|d| d.starts_with(dest)).or_else(|| std::fs::read_dir(dest).ok()?.flatten().map(|e| e.path()).find(|p| p.join("oci.dll").is_file()))
    };
    download_and_extract(url, expect, dest, Box::new(find), "oci.dll", waker)
}

/// zip をダウンロードして `dest` に展開し、`find` で目当てのフォルダを探す（裏のスレッド）。`what` は見つからないときの知らせ用
pub fn download_and_extract(url: &str, expect: u64, dest: &Path, find: Box<dyn Fn(&Path) -> Option<PathBuf> + Send>, what: &str, waker: Waker) -> Download {
    let (tx, rx) = channel();
    let (url, dest, what) = (url.to_string(), dest.to_path_buf(), what.to_string());
    let _ = std::thread::Builder::new().name("download".into()).spawn(move || {
        let send = |m: DlMsg| {
            let _ = tx.send(m);
            waker();
        };
        let run = || -> Result<PathBuf, String> {
            std::fs::create_dir_all(&dest).map_err(|e| format!("{} を作れません: {e}", dest.display()))?;
            let zip = dest.join("download.zip");
            let _ = std::fs::remove_file(&zip);
            let mut child = hidden(Command::new(system_tool("curl.exe")).args(["-sSfL", "--retry", "2", "-o"]).arg(&zip).arg(&url)).spawn().map_err(|e| format!("curl.exe を起動できません: {e}"))?;
            let status = loop {
                if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
                    break s;
                }
                let got = std::fs::metadata(&zip).map_or(0, |m| m.len());
                send(DlMsg::Progress(got, expect));
                std::thread::sleep(std::time::Duration::from_millis(250));
            };
            if !status.success() {
                let mut err = String::new();
                if let Some(mut e) = child.stderr.take() {
                    let _ = std::io::Read::read_to_string(&mut e, &mut err);
                }
                return Err(format!("ダウンロードできません: {}", err.trim()));
            }
            send(DlMsg::Extracting);
            let out = hidden(Command::new(system_tool("tar.exe")).arg("-xf").arg(&zip).arg("-C").arg(&dest)).output().map_err(|e| format!("tar.exe を起動できません: {e}"))?;
            let _ = std::fs::remove_file(&zip);
            if !out.status.success() {
                return Err(format!("展開できません: {}", String::from_utf8_lossy(&out.stderr).trim()));
            }
            find(&dest).ok_or_else(|| format!("展開したものの中に {what} がありません"))
        };
        send(DlMsg::Done(run()));
    });
    Download { rx }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 手元に Instant Client があれば（NAGI_TEST_ORACLE_CLIENT のフォルダを drivers\oracle に見立てる）、
    /// 読み込んで、どこにも無い宛先につなぎ、Oracle のエラー（ORA-12541）が返ることを確かめる
    #[test]
    fn oracle_client_loads() {
        let Some(ic) = std::env::var_os("NAGI_TEST_ORACLE_CLIENT") else {
            eprintln!("NAGI_TEST_ORACLE_CLIENT が無いので飛ばす");
            return;
        };
        // SAFETY: テストの中だけで使う
        unsafe { std::env::set_var("NAGI_DRIVERS_DIR", &ic) };
        assert!(oracle_ready(), "{:?} の oracle\\instantclient_* に oci.dll が無い", ic);
        init_oracle().unwrap();
        let c = crate::db::Client::connect("jdbc:oracle:thin:@//127.0.0.1:1/NOWHERE", "scott", "tiger", std::sync::Arc::new(|| {})).unwrap();
        match c.rx.recv_timeout(std::time::Duration::from_secs(60)).unwrap() {
            crate::db::Reply::Error(e) => assert!(e.contains("ORA-12541"), "{e}"),
            r => panic!("{r:?}"),
        }
    }
}
