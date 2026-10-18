//! ターミナル: ConPTY でシェルを動かし（portable-pty）、出力を画面の文字の並びにする（vt100）。
//!
//! 読むのは裏のスレッド。届くたびに解析して UI を起こす。書く（キー）・大きさを変えるは UI のスレッドから。

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::Waker;

/// さかのぼって見られる行の数
const SCROLLBACK: usize = 5000;

/// コマンドの文字を引数に分ける（空白で区切り、"…" の中は 1 つ。"…" の中の \" は "）
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut any) = (false, false);
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// シェルを UTF-8 で始める引数（日本語の Windows の既定 CP932 のままだと、UTF-8 で書き出すプログラムの出力が化ける）。
/// cmd は chcp 65001、PowerShell は入出力の文字コードと Get-Content の既定を UTF-8 に、Git Bash は起動の初めに chcp.com 65001。
/// 引数を付けて呼んだもの（cmd /C … など）はそのまま
fn utf8_args(prog: &str, args: Vec<String>) -> Vec<String> {
    let base = Path::new(prog).file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    match base.as_str() {
        "cmd.exe" | "cmd" if args.is_empty() => s(&["/K", "chcp", "65001", ">nul"]),
        "powershell.exe" | "powershell" | "pwsh.exe" | "pwsh" if args.is_empty() => s(&[
            "-NoLogo",
            "-NoExit",
            "-Command",
            "[Console]::OutputEncoding=[Text.Encoding]::UTF8;[Console]::InputEncoding=[Text.Encoding]::UTF8;$OutputEncoding=[Text.Encoding]::UTF8;$PSDefaultParameterValues['Get-Content:Encoding']='UTF8'",
        ]),
        "bash.exe" | "bash" if args.is_empty() || args == ["--login", "-i"] => s(&["-c", "chcp.com 65001 >/dev/null 2>&1; exec bash --login -i"]),
        _ => args,
    }
}

pub struct Term {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    exited: Arc<AtomicBool>,
    size: (u16, u16),
}

/// 使うシェル（PowerShell 7 があればそれ、無ければ Windows PowerShell）
pub fn default_shell() -> String {
    if let Some(s) = std::env::var_os("NAGI_SHELL") {
        return s.to_string_lossy().into_owned();
    }
    let has = |exe: &str| std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(exe).is_file()));
    if has("pwsh.exe") { "pwsh.exe".into() } else { "powershell.exe".into() }
}

impl Term {
    /// `cwd` でシェルを動かす（rows × cols）
    pub fn spawn(shell: &str, cwd: &Path, rows: u16, cols: u16, waker: Waker) -> Result<Term, String> {
        let (rows, cols) = (rows.max(2), cols.max(10));
        let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }).map_err(|e| e.to_string())?;
        // 空白で区切る。"C:\Program Files\..." のように引用符で囲めば 1 つ（中の \" は "）
        let shell = shell.trim();
        let mut words = split_args(shell);
        let prog = if words.is_empty() { "powershell.exe".to_string() } else { words.remove(0) };
        let mut cmd = CommandBuilder::new(prog.as_str());
        for a in utf8_args(&prog, words) {
            cmd.arg(a);
        }
        cmd.cwd(cwd);
        let child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = Arc::new(Mutex::new(pair.master.take_writer().map_err(|e| e.to_string())?));
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, SCROLLBACK)));
        let exited = Arc::new(AtomicBool::new(false));
        let (p2, e2, w2) = (parser.clone(), exited.clone(), writer.clone());
        let _ = std::thread::Builder::new().name("term-read".into()).spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut reply = None;
                        if let Ok(mut p) = p2.lock() {
                            p.process(&buf[..n]);
                            // ConPTY は最初にカーソルの位置を尋ねて（ESC[6n）、答えるまで出力を止める
                            if buf[..n].windows(4).any(|w| w == b"\x1b[6n") {
                                let (r, c) = p.screen().cursor_position();
                                reply = Some(format!("\x1b[{};{}R", r + 1, c + 1));
                            }
                        }
                        if let (Some(r), Ok(mut w)) = (reply, w2.lock()) {
                            let _ = w.write_all(r.as_bytes());
                            let _ = w.flush();
                        }
                        waker();
                    }
                }
            }
            e2.store(true, Ordering::Relaxed);
            waker();
        });
        Ok(Term { parser, writer, master: pair.master, child, exited, size: (rows, cols) })
    }

    pub fn write(&mut self, bytes: &[u8]) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(bytes);
            let _ = w.flush();
        }
    }

    /// 大きさを合わせる（変わったときだけ）
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(2), cols.max(10));
        if self.size == (rows, cols) {
            return;
        }
        self.size = (rows, cols);
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        if let Ok(mut p) = self.parser.lock() {
            p.screen_mut().set_size(rows, cols);
        }
    }

    /// 画面を読む
    pub fn with_screen<R>(&self, f: impl FnOnce(&vt100::Screen) -> R) -> R {
        let p = self.parser.lock().unwrap_or_else(|e| e.into_inner());
        f(p.screen())
    }

    /// さかのぼって見る行数（0 で一番下）
    pub fn set_scrollback(&self, rows: usize) {
        if let Ok(mut p) = self.parser.lock() {
            p.screen_mut().set_scrollback(rows);
        }
    }

    pub fn scrollback(&self) -> usize {
        self.with_screen(|s| s.scrollback())
    }

    /// シェルが終わったか（ConPTY は子が終わっても読み取りが EOF にならないので、子の終了も確かめる）
    pub fn exited(&mut self) -> bool {
        self.exited.load(Ordering::Relaxed) || self.child.try_wait().ok().flatten().is_some()
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// cmd を動かして echo の出力が画面に出る。大きさを変えられる。終われば exited
    #[test]
    fn runs_shell_and_reads_screen() {
        let dir = std::env::temp_dir();
        let mut t = Term::spawn("cmd.exe /Q /K prompt $G", &dir, 10, 60, Arc::new(|| {})).expect("起動");
        t.write(b"echo nagi-term-ok\r\n");
        let start = std::time::Instant::now();
        loop {
            if t.with_screen(|s| s.contents()).contains("nagi-term-ok\n") || t.with_screen(|s| s.contents().matches("nagi-term-ok").count()) >= 2 {
                break;
            }
            assert!(start.elapsed().as_secs() < 15, "出力が来ない: {:?}", t.with_screen(|s| s.contents()));
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        t.resize(20, 80);
        assert_eq!(t.with_screen(|s| s.size()), (20, 80));
        t.write(b"exit\r\n");
        let start = std::time::Instant::now();
        while !t.exited() {
            assert!(start.elapsed().as_secs() < 15, "終わらない");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn split_quoted() {
        assert_eq!(split_args(r#""C:\Program Files\Git\bin\bash.exe" --login -i"#), ["C:\\Program Files\\Git\\bin\\bash.exe", "--login", "-i"]);
        assert_eq!(split_args(r#"wsl.exe -d kali sh -lc "echo \"a b\" && ls""#), ["wsl.exe", "-d", "kali", "sh", "-lc", "echo \"a b\" && ls"]);
        assert_eq!(split_args(r#"cmd.exe /C """#), ["cmd.exe", "/C", ""]);
    }
}

#[cfg(test)]
mod utf8_tests {
    use super::*;

    /// UTF-8 のファイルを cmd の type と PowerShell の Get-Content で出しても化けない
    #[test]
    fn shells_show_utf8() {
        let dir = std::env::temp_dir().join(format!("nagi-utf8-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("u.txt"), "UTF8の日本語ファイル\r\n").unwrap();
        let mut shells = vec![("cmd.exe", "type u.txt\r"), ("powershell.exe", "Get-Content u.txt\r")];
        // Git Bash（入っていれば）
        if Path::new(r"C:\Program Files\Git\bin\bash.exe").is_file() {
            shells.push((r#""C:\Program Files\Git\bin\bash.exe" --login -i"#, "cat u.txt\r"));
        }
        for (shell, input) in shells {
            let mut t = Term::spawn(shell, &dir, 30, 120, std::sync::Arc::new(|| {})).unwrap();
            let mut sent = false;
            let mut ok = false;
            for k in 0..400 {
                std::thread::sleep(std::time::Duration::from_millis(25));
                let screen = t.with_screen(|s| s.contents());
                if !sent && (screen.contains('>') || screen.contains('$') || k > 120) {
                    t.write(input.as_bytes());
                    sent = true;
                }
                if sent && screen.matches("UTF8の日本語ファイル").count() >= 1 {
                    ok = true;
                    break;
                }
            }
            assert!(ok, "{shell}: {}", t.with_screen(|s| s.contents()));
        }
    }
}
