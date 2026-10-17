//! 外部コマンドの実行（診断を取り込むため。architecture.md §10）。
//!
//! `cmd /C` でコマンドを動かし、標準出力と標準エラーを 1 行ずつ UI へ送る。コンソールの窓は出さない。
//! 落とすと（新しいコマンドを始めたときなど）子プロセスを止める。

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use crate::Waker;

pub enum JobMsg {
    Line(String),
    /// 終わった（終了コード。止めたときなどは None）
    Done(Option<i32>),
}

pub struct Job {
    pub rx: Receiver<JobMsg>,
    child: Arc<Mutex<Option<Child>>>,
}

impl Drop for Job {
    fn drop(&mut self) {
        if let Ok(mut c) = self.child.lock() {
            if let Some(ch) = c.as_mut() {
                let _ = ch.kill();
            }
        }
    }
}

/// 出力を読んで 1 行ずつ送る（文字コードは UTF-8 として読み、だめなら置き換え）
fn pump(r: impl Read + Send + 'static, tx: Sender<JobMsg>, waker: Waker) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut r = BufReader::new(r);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match r.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']).to_string();
                    if tx.send(JobMsg::Line(line)).is_err() {
                        break;
                    }
                    waker();
                }
            }
        }
    })
}

/// JAVA_HOME が使えない（指す先に java.exe が無い）とき、代わりに使う JDK（~/.jdks のいちばん新しいもの、無ければ PATH の java）。
/// 使えるなら None（そのまま）
pub fn java_home_fix() -> Option<std::path::PathBuf> {
    let exe = |d: &Path| d.join("bin").join("java.exe").is_file() || d.join("bin").join("java").is_file();
    if std::env::var_os("JAVA_HOME").is_some_and(|h| exe(Path::new(&h))) {
        return None;
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(std::path::PathBuf::from).unwrap_or_default();
    let mut jdks: Vec<std::path::PathBuf> = std::fs::read_dir(home.join(".jdks")).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.join("bin").join("javac.exe").is_file()).collect();
    jdks.sort();
    if let Some(j) = jdks.pop() {
        return Some(j);
    }
    // PATH の java の 2 つ上（…\bin\java.exe → …）。javac があるものだけ（JRE では Maven がコンパイルできない）
    std::env::var_os("PATH").and_then(|p| {
        std::env::split_paths(&p).find(|d| d.join("javac.exe").is_file() && d.join("java.exe").is_file()).and_then(|d| d.parent().map(Path::to_path_buf))
    })
}

/// `command` を `cwd` で動かす
pub fn run(command: &str, cwd: &Path, waker: Waker) -> std::io::Result<Job> {
    run_env(command, cwd, &[], waker)
}

/// 環境変数を足して動かす
pub fn run_env(command: &str, cwd: &Path, env: &[(String, String)], waker: Waker) -> std::io::Result<Job> {
    let mut cmd = Command::new("cmd");
    cmd.arg("/C");
    // cmd は引数の " をそのまま読むので、Rust の引用（" → \"）をかけずに渡す（pom.xml のパスの引用符が壊れた）
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.raw_arg(command);
    }
    #[cfg(not(windows))]
    cmd.arg(command);
    cmd.current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: コンソールの窓を出さない
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn()?;
    let (tx, rx) = channel();
    let out = child.stdout.take();
    let err = child.stderr.take();
    let child = Arc::new(Mutex::new(Some(child)));
    let c2 = child.clone();
    let _ = std::thread::Builder::new().name("job".into()).spawn(move || {
        let mut pumps = Vec::new();
        if let Some(o) = out {
            pumps.push(pump(o, tx.clone(), waker.clone()));
        }
        if let Some(e) = err {
            pumps.push(pump(e, tx.clone(), waker.clone()));
        }
        for p in pumps {
            let _ = p.join();
        }
        let code = c2.lock().ok().and_then(|mut c| c.as_mut().and_then(|ch| ch.wait().ok())).and_then(|s| s.code());
        let _ = tx.send(JobMsg::Done(code));
        waker();
    });
    Ok(Job { rx, child })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_and_streams() {
        let job = run("echo one&& echo two 1>&2&& exit /b 3", &std::env::temp_dir(), Arc::new(|| {})).unwrap();
        let mut lines = Vec::new();
        let code = loop {
            match job.rx.recv().unwrap() {
                JobMsg::Line(l) => lines.push(l),
                JobMsg::Done(c) => break c,
            }
        };
        lines.sort();
        assert_eq!(lines, vec!["one", "two "]);
        assert_eq!(code, Some(3));
    }
}
