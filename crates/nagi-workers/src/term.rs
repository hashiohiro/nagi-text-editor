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
fn utf8_args(prog: &str, args: Vec<String>, utf8: bool) -> Vec<String> {
    let base = Path::new(prog).file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    // 設定の「UTF-8 で始める」がオフなら、Windows の既定のまま（PowerShell のロゴだけ消す）
    if !utf8 {
        return match base.as_str() {
            "powershell.exe" | "powershell" | "pwsh.exe" | "pwsh" if args.is_empty() => s(&["-NoLogo"]),
            _ => args,
        };
    }
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
    pub fn spawn(shell: &str, cwd: &Path, rows: u16, cols: u16, waker: Waker) -> nagi_core::Result<Term> {
        let (rows, cols) = (rows.max(2), cols.max(10));
        let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }).map_err(|e| e.to_string())?;
        // 空白で区切る。"C:\Program Files\..." のように引用符で囲めば 1 つ（中の \" は "）
        let shell = shell.trim();
        let mut words = split_args(shell);
        let prog = if words.is_empty() { "powershell.exe".to_string() } else { words.remove(0) };
        let mut cmd = CommandBuilder::new(prog.as_str());
        // このフォルダのプロジェクトの設定（UTF-8 で始めるか・JDK や言語のツールを頭に置いた PATH）
        let cfg = crate::config::for_path(cwd);
        for a in utf8_args(&prog, words, cfg.terminal_utf8) {
            cmd.arg(a);
        }
        cmd.cwd(cwd);
        // nagi の環境をそのまま渡す（portable-pty は Windows ではレジストリの PATH などで上書きするので、
        // 設定の適用が PATH に足した Maven・言語のツールや、JAVA_HOME が届かなかった）
        cmd.env_clear();
        for (k, v) in std::env::vars_os() {
            cmd.env(k, v);
        }
        for (k, v) in &cfg.env {
            cmd.env(k, v);
        }
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

    /// 画面を変えながら読む（さかのぼった行を読むなど。読む間に出力は混ざらない）
    pub fn with_screen_mut<R>(&self, f: impl FnOnce(&mut vt100::Screen) -> R) -> R {
        let mut p = self.parser.lock().unwrap_or_else(|e| e.into_inner());
        f(p.screen_mut())
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

    /// 幅を縮めて全角文字が右端で半分に切れても、そこへ書いて落ちない（vt100 は patches/vt100 で直したものを使う）
    #[test]
    fn shrink_cuts_wide_char() {
        let mut p = vt100::Parser::new(5, 10, 100);
        p.process("abcdefghあ".as_bytes()); // あ は 9・10 列目
        p.screen_mut().set_size(5, 9); // 9 列に縮める（resize と同じ）→ あ の後半が切れる
        p.process(b"\x1b[1;9Hx");
        assert_eq!(p.screen().contents_between(0, 0, 0, 9), "abcdefghx");
    }

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

    /// nagi の環境変数がそのまま届く（レジストリにもある変数でも、nagi の値が勝つ。PATH に足した Maven が届かなかった）
    #[test]
    fn passes_nagi_env() {
        // DriverData はレジストリ（HKLM の Environment）にある。nagi のほかの試験は使わない
        unsafe { std::env::set_var("DriverData", "nagi-env-ok") };
        let mut t = Term::spawn("cmd.exe /Q /K prompt $G", &std::env::temp_dir(), 10, 60, Arc::new(|| {})).expect("起動");
        t.write(b"echo [%DriverData%]\r\n");
        let start = std::time::Instant::now();
        while !t.with_screen(|s| s.contents()).contains("[nagi-env-ok]") {
            assert!(start.elapsed().as_secs() < 15, "nagi の値が届かない: {:?}", t.with_screen(|s| s.contents()));
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
            // ほかの試験と並べて流すと、シェルの起動（Git Bash の --login）に 10 秒を超えることがある。時間で待つ
            let start = std::time::Instant::now();
            let mut sent = false;
            let mut ok = false;
            while start.elapsed() < std::time::Duration::from_secs(60) {
                std::thread::sleep(std::time::Duration::from_millis(25));
                let screen = t.with_screen(|s| s.contents());
                if !sent && (screen.contains('>') || screen.contains('$') || start.elapsed() > std::time::Duration::from_secs(3)) {
                    t.write(input.as_bytes());
                    sent = true;
                }
                if sent && screen.matches("UTF8の日本語ファイル").count() >= 1 {
                    ok = true;
                    break;
                }
            }
            if std::env::var_os("NAGI_TERM_TIMING").is_some() {
                eprintln!("{shell}: {:?}", start.elapsed());
            }
            assert!(ok, "{shell}: {:?} {}", start.elapsed(), t.with_screen(|s| s.contents()));
        }
    }
}
