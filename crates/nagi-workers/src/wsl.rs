//! WSL 上のプロジェクト（`\\wsl.localhost\<ディストリビューション>\home\...`）を扱う。
//!
//! ファイルは Windows から UNC のパスで読み書きし、git とコマンド（:Make・Maven）とシェルは WSL の中で動かす
//! （Windows の git で UNC を扱うと遅く、所有者が違うので safe.directory で断られる）。

use std::path::{Path, PathBuf};
use std::process::Command;

/// WSL のディストリビューション（既定のものが先頭）。データ用（〜-data）は除く
pub fn distros() -> Vec<String> {
    let mut cmd = Command::new("wsl.exe");
    cmd.args(["-l", "-q"]);
    hide(&mut cmd);
    let Ok(out) = cmd.output() else { return Vec::new() };
    // UTF-16LE で返る
    let u16s: Vec<u16> = out.stdout.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    String::from_utf16_lossy(&u16s)
        .lines()
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '\0').to_string())
        .filter(|l| !l.is_empty() && !l.ends_with("-data") && l != "docker-desktop")
        .collect()
}

/// `\\wsl.localhost\D\a\b`・`\\wsl$\D\a\b`（`\\?\UNC\` 付きも）→ (D, /a/b)
pub fn split_unc(path: &Path) -> Option<(String, String)> {
    let s = path.to_string_lossy().replace('/', "\\");
    let s = s.strip_prefix("\\\\?\\UNC\\").map(|r| format!("\\\\{r}")).unwrap_or(s);
    let low = s.to_ascii_lowercase();
    let rest = if low.starts_with("\\\\wsl.localhost\\") {
        &s["\\\\wsl.localhost\\".len()..]
    } else if low.starts_with("\\\\wsl$\\") {
        &s["\\\\wsl$\\".len()..]
    } else {
        return None;
    };
    let (distro, tail) = rest.split_once('\\').unwrap_or((rest, ""));
    if distro.is_empty() {
        return None;
    }
    Some((distro.to_string(), format!("/{}", tail.trim_end_matches('\\').replace('\\', "/"))))
}

/// (D, /a/b) → `\\wsl.localhost\D\a\b`
pub fn to_unc(distro: &str, linux: &str) -> PathBuf {
    PathBuf::from(format!("\\\\wsl.localhost\\{distro}{}", linux.replace('/', "\\")))
}

/// 打たれたパスを開ける形に（`/home/x` は既定のディストリビューションの UNC、`~/x` も同じ）
pub fn resolve_typed(path: &str) -> PathBuf {
    let p = path.trim().trim_matches('"');
    if p.starts_with('/') || p.starts_with("~/") {
        if let Some(d) = distros().first() {
            let linux = if let Some(rest) = p.strip_prefix("~/") {
                // ~ はそのディストリビューションの $HOME
                let mut cmd = Command::new("wsl.exe");
                cmd.args(["-d", d, "--", "sh", "-c", "printf %s \"$HOME\""]);
                hide(&mut cmd);
                let home = cmd.output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|h| h.starts_with('/')).unwrap_or_else(|| "/root".into());
                format!("{home}/{rest}")
            } else {
                p.to_string()
            };
            return to_unc(d, &linux);
        }
    }
    PathBuf::from(p)
}

/// WSL のパスなら、そのディストリビューションの中で `program args` を `cwd` で動かすコマンド
pub fn command_in(cwd: &Path, program: &str, args: &[&str]) -> Option<Command> {
    let (distro, linux) = split_unc(cwd)?;
    let mut cmd = Command::new("wsl.exe");
    cmd.args(["-d", &distro, "--cd", &linux, "--", program]).args(args);
    hide(&mut cmd);
    Some(cmd)
}

/// WSL のパスなら Linux のパス（シェルに渡す用）、それ以外はそのまま
pub fn shell_path(p: &Path) -> String {
    match split_unc(p) {
        Some((_, linux)) => linux,
        None => p.display().to_string(),
    }
}

fn hide(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let _ = cmd;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unc_paths() {
        assert_eq!(split_unc(Path::new(r"\\wsl.localhost\Ubuntu\home\me\proj")), Some(("Ubuntu".into(), "/home/me/proj".into())));
        assert_eq!(split_unc(Path::new(r"\\wsl$\Ubuntu\home\me")), Some(("Ubuntu".into(), "/home/me".into())));
        assert_eq!(split_unc(Path::new(r"\\?\UNC\wsl.localhost\Ubuntu\srv")), Some(("Ubuntu".into(), "/srv".into())));
        assert_eq!(split_unc(Path::new(r"\\wsl.localhost\Ubuntu")), Some(("Ubuntu".into(), "/".into())));
        assert_eq!(split_unc(Path::new(r"C:\work")), None);
        assert_eq!(to_unc("Ubuntu", "/home/me/proj"), PathBuf::from(r"\\wsl.localhost\Ubuntu\home\me\proj"));
        assert_eq!(shell_path(Path::new(r"\\wsl.localhost\Ubuntu\home\me\pom.xml")), "/home/me/pom.xml");
    }
}
