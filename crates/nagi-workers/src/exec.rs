//! 外のコマンドを動かす骨組み（ADR 0016 の 4）。「どこで・どの環境で動かすか」はここだけで決める。
//!
//! ```ignore
//! let job = Exec::in_dir(&root).env("TZ", "Asia/Tokyo").stream("mvn -B compile", waker)?;
//! let out = Exec::in_dir(&root).program("git", &["status"]).output()?;
//! let line = Exec::in_dir(&cwd).terminal_line("サーバー", "npm run dev")?;   // ターミナルの窓に渡す
//! ```
//!
//! WSL の決まり（全体共通。作業フォルダが `\\wsl.localhost\<名前>\…` のとき）:
//! - WSL の中で動かす（`wsl.exe -d <名前> --cd <フォルダ> --exec sh -c "<行>"`。`--exec` で既定のシェルを通さない。
//!   `--` だと外側のシェルが `$` や `&&` を先に読んでしまう）。
//! - ログインのシェルの環境（.profile・.bashrc の PATH。nvm の node など）を行の頭に付ける（wsl::login_env。1 度だけ調べる）。
//!   ログインシェル（-l）にはしない（ログインの挨拶が毎回出るため）。PATH から Windows の場所（/mnt/…）は外す。
//! - Windows 側のツールの設定と JDK（JAVA_HOME）は渡さない。
//!
//! Windows では窓を出さずに cmd で動かす（設定のツールと JDK は nagi の環境変数で PATH に入っている）。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::Waker;
use crate::jobs::Job;

/// 動かす場所と環境
#[derive(Clone, Debug)]
pub struct Exec {
    cwd: PathBuf,
    /// WSL なら (ディストリビューション, Linux のフォルダ)
    wsl: Option<(String, String)>,
    env: Vec<(String, String)>,
}

/// WSL に渡さない環境変数（Windows のパスを指すもの）
const WINDOWS_ONLY: &[&str] = &["JAVA_HOME"];

impl Exec {
    /// `cwd` で動かす（WSL のフォルダなら WSL の中で）
    pub fn in_dir(cwd: &Path) -> Self {
        Self { cwd: cwd.to_path_buf(), wsl: crate::wsl::split_unc(cwd), env: Vec::new() }
    }

    /// WSL のディストリビューション `distro` のホームで動かす（ツールの調べもの・~ の解決）
    pub fn in_distro(distro: &str) -> Self {
        Self { cwd: crate::wsl::to_unc(distro, "/"), wsl: Some((distro.to_string(), "~".to_string())), env: Vec::new() }
    }

    /// WSL のフォルダでも Windows のコマンドで動かす（設定で WSL の git を使わないときなど）
    pub fn windows(cwd: &Path) -> Self {
        Self { cwd: cwd.to_path_buf(), wsl: None, env: Vec::new() }
    }

    pub fn is_wsl(&self) -> bool {
        self.wsl.is_some()
    }

    /// 対話のシェル（ターミナルの窓）: WSL ならそのディストリビューションのシェルをこのフォルダから（名前, コマンド）
    pub fn login_shell(&self) -> Option<(String, String)> {
        let (d, linux) = self.wsl.as_ref()?;
        Some((format!("WSL: {d}"), format!("wsl.exe -d {d} --cd {linux}")))
    }

    /// WSL のディストリビューションの名前
    pub fn distro(&self) -> Option<&str> {
        self.wsl.as_ref().map(|w| w.0.as_str())
    }

    pub fn env(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.env.push((k.into(), v.into()));
        self
    }

    pub fn envs(mut self, env: &[(String, String)]) -> Self {
        self.env.extend(env.iter().cloned());
        self
    }

    /// その場所でのコマンドの名前（Windows と Linux で違うもの: python と python3 など）
    pub fn pick<'a>(&self, windows: &'a str, linux: &'a str) -> &'a str {
        if self.is_wsl() { linux } else { windows }
    }

    /// その場所のコマンドから見たパス（WSL なら Linux のパス）
    pub fn path(&self, p: &Path) -> String {
        if self.is_wsl() { crate::wsl::shell_path(p) } else { p.display().to_string() }
    }

    /// Maven・Gradle などで、JAVA_HOME が使えなければ（古い JRE を指したままなど）見つけた JDK を渡す。
    /// WSL では渡さない（WSL の中の JDK を使う）。渡したらその場所
    pub fn fix_jdk(&mut self) -> Option<PathBuf> {
        if self.is_wsl() || self.env.iter().any(|(k, _)| k == "JAVA_HOME") {
            return None;
        }
        let j = crate::jobs::java_home_fix()?;
        self.env.push(("JAVA_HOME".into(), j.display().to_string()));
        Some(j)
    }

    /// この行を動かすのに足りないもの（Maven, JDK）。どちらも nagi がダウンロードして入れられる。WSL では見ない（WSL の中のものを使う）
    pub fn missing_tools(&self, line: &str) -> (bool, bool) {
        let jdk = !self.is_wsl() && needs_jdk(line) && !self.env.iter().any(|(k, _)| k == "JAVA_HOME") && crate::jobs::jdk_missing();
        (crate::maven::missing(line, self), jdk)
    }

    /// WSL の行の頭に付ける環境変数（Windows のものは外す）
    fn exports(&self) -> String {
        let q = |v: &str| format!("'{}'", v.replace('\'', "'\\''"));
        self.env.iter().filter(|(k, _)| !WINDOWS_ONLY.contains(&k.as_str())).map(|(k, v)| format!("export {k}={}; ", q(v))).collect()
    }

    /// WSL の中で動かす sh の行（ログインのシェルの環境・環境変数を頭に）
    fn wsl_line(&self, distro: &str, line: &str) -> String {
        // 試験では WSL を呼ばない
        let login = if cfg!(test) { String::new() } else { crate::wsl::login_env(distro) };
        format!("{login}{}{line}", self.exports())
    }

    /// シェルの行を動かす Command（標準入力なし・標準出力と標準エラーは受け取る・窓を出さない）
    pub fn shell(&self, line: &str) -> Command {
        let mut cmd = match &self.wsl {
            Some((distro, linux)) => {
                let mut c = Command::new("wsl.exe");
                c.args(["-d", distro, "--cd", linux, "--exec", "sh", "-c", &self.wsl_line(distro, line)]);
                c
            }
            None => {
                let mut c = Command::new("cmd");
                c.arg("/C");
                // cmd は引数の " をそのまま読むので、Rust の引用（" → \"）をかけずに渡す（pom.xml のパスの引用符が壊れた）
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    c.raw_arg(line);
                }
                #[cfg(not(windows))]
                c.arg(line);
                c.current_dir(&self.cwd).envs(self.env.iter().map(|(k, v)| (k, v)));
                c
            }
        };
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        crate::sys::no_window(&mut cmd);
        cmd
    }

    /// プログラムを引数つきで動かす Command（シェルを通さない。WSL では `env K=V プログラム 引数`）
    pub fn program(&self, prog: &str, args: &[&str]) -> Command {
        let mut cmd = match &self.wsl {
            Some((distro, linux)) => {
                let mut c = Command::new("wsl.exe");
                c.args(["-d", distro, "--cd", linux, "--exec", "env"]);
                c.args(self.env.iter().filter(|(k, _)| !WINDOWS_ONLY.contains(&k.as_str())).map(|(k, v)| format!("{k}={v}")));
                c.arg(prog).args(args);
                c
            }
            None => {
                let mut c = Command::new(prog);
                c.args(args).current_dir(&self.cwd).envs(self.env.iter().map(|(k, v)| (k, v)));
                c
            }
        };
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        crate::sys::no_window(&mut cmd);
        cmd
    }

    /// シェルの行を動かし、出力を 1 行ずつ流す（裏の処理。文字コードは jobs::decode_line）
    pub fn stream(&self, line: &str, waker: Waker) -> std::io::Result<Job> {
        crate::jobs::spawn(self.shell(line), waker)
    }

    /// ターミナルの窓（ConPTY）に渡すコマンドの行。Windows は作業フォルダと環境変数も書いた一時の .cmd を
    /// `cmd.exe /C` で（UTF-8 のコードページで）、WSL は `wsl.exe … --exec sh -c "…"` で。`name` は一時ファイルの名前に使う
    pub fn terminal_line(&self, name: &str, line: &str) -> nagi_core::Result<String> {
        if let Some((distro, linux)) = &self.wsl {
            let l = self.wsl_line(distro, line);
            return Ok(format!("wsl.exe -d {distro} --cd {linux} --exec sh -c \"{}\"", l.replace('"', "\\\"")));
        }
        let dir = std::env::temp_dir().join("nagi-run");
        std::fs::create_dir_all(&dir).map_err(|e| format!("一時フォルダを作れません: {e}"))?;
        let file = dir.join(format!("run-{:016x}.cmd", hash(name)));
        // ファイルは UTF-8 で書き、コードページも UTF-8 に（ターミナルの窓と同じ。UTF-8 で書き出すプログラムが化けない）
        let mut text = String::from("@echo off\r\nchcp 65001 >nul\r\n");
        text.push_str(&format!("cd /d \"{}\"\r\n", self.cwd.display()));
        for (k, v) in &self.env {
            text.push_str(&format!("set \"{k}={v}\"\r\n"));
        }
        text.push_str(line);
        text.push_str("\r\n");
        std::fs::write(&file, text).map_err(|e| format!("{}: {e}", file.display()))?;
        Ok(format!("cmd.exe /C \"{}\"", file.display()))
    }
}

/// Windows に付いているツール（curl.exe・tar.exe・wsl.exe）を窓を出さずに動かす Command（標準入力なし・出力は受け取る）
pub fn system(name: &str) -> Command {
    let mut c = Command::new(crate::sys::system_tool(name));
    c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    crate::sys::no_window(&mut c);
    c
}

/// Maven・Gradle など JDK の要るコマンドか（行の最初の語で見る）
pub fn needs_jdk(line: &str) -> bool {
    let first = line.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
    let first = first.trim_start_matches("./");
    ["mvn", "gradle", "java"].iter().any(|t| first.starts_with(t))
}

fn hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_and_lines() {
        let w = Exec::in_dir(Path::new(r"\\wsl.localhost\kali-linux\home\me\os")).env("A", "it's").env("JAVA_HOME", r"C:\jdk");
        assert!(w.is_wsl());
        assert_eq!(w.pick("python", "python3"), "python3");
        assert_eq!(w.path(Path::new(r"\\wsl.localhost\kali-linux\home\me\os\a.py")), "/home/me/os/a.py");
        let line = w.terminal_line("x", "echo \"a\"").unwrap();
        assert_eq!(crate::term::split_args(&line), ["wsl.exe", "-d", "kali-linux", "--cd", "/home/me/os", "--exec", "sh", "-c", "export A='it'\\''s'; echo \"a\""], "JAVA_HOME は WSL に渡さない");
        let c = w.program("git", &["status"]);
        let args: Vec<String> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args, ["-d", "kali-linux", "--cd", "/home/me/os", "--exec", "env", "A=it's", "git", "status"]);
        let mut w = w;
        assert_eq!(w.fix_jdk(), None, "WSL では JDK を渡さない");
        let win = Exec::in_dir(Path::new(r"C:\work\shop")).env("TZ", "Asia/Tokyo");
        assert!(!win.is_wsl());
        let line = win.terminal_line("サーバー", "npm run dev").unwrap();
        assert!(line.starts_with("cmd.exe /C \"") && line.ends_with(".cmd\""), "{line}");
        let text = std::fs::read_to_string(&crate::term::split_args(&line)[2]).unwrap();
        assert_eq!(text, "@echo off\r\nchcp 65001 >nul\r\ncd /d \"C:\\work\\shop\"\r\nset \"TZ=Asia/Tokyo\"\r\nnpm run dev\r\n");
        assert!(needs_jdk("./mvnw -B compile") && needs_jdk("gradlew.bat build") && !needs_jdk("npm run dev"));
    }

    #[test]
    fn streams_on_windows() {
        let job = Exec::in_dir(&std::env::temp_dir()).env("NAGI_X", "x1").stream("echo %NAGI_X%&& exit /b 2", nagi_core::task::no_waker()).unwrap();
        let mut lines = Vec::new();
        let code = loop {
            match job.rx.recv().unwrap() {
                crate::jobs::JobMsg::Line(l) => lines.push(l),
                crate::jobs::JobMsg::Done(c) => break c,
            }
        };
        assert_eq!((lines, code), (vec!["x1".to_string()], Some(2)));
    }
}
