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
    crate::sys::no_window(&mut cmd);
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
                crate::sys::no_window(&mut cmd);
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

/// 対話のシェル（~/.bashrc）でだけ PATH に入るツール（nvm の node・pyenv・sdkman など）のフォルダを、
/// コマンドの頭で PATH に足す文（`export PATH='…':"$PATH"; `。足すものが無ければ空）。ディストリビューションごとに 1 度だけ調べる
pub fn path_prefix(distro: &str) -> String {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(v) = cache.lock().ok().and_then(|m| m.get(distro).cloned()) {
        return v;
    }
    const CMDS: &str = "python3 python pip node npm npx deno bun go cargo rustc dotnet gcc g++ clang cmake make zig dart php ruby bundle lua luac ghc stack cabal mix elixir swift sbt scala java mvn gradle kotlinc";
    let script = format!("for c in {CMDS}; do command -v \"$c\"; done");
    let mut cmd = Command::new("wsl.exe");
    cmd.args(["-d", distro, "--exec", "bash", "-ic", &script]).stdin(std::process::Stdio::null());
    crate::sys::no_window(&mut cmd);
    let out = cmd.output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    let v = prefix_from(&out);
    if let Ok(mut m) = cache.lock() {
        m.insert(distro.to_string(), v.clone());
    }
    v
}

/// `command -v` の出力から、いつもの場所でないフォルダを PATH に足す文に
fn prefix_from(out: &str) -> String {
    const STD: &[&str] = &["/usr/bin", "/bin", "/usr/sbin", "/sbin", "/usr/local/bin", "/usr/local/sbin"];
    let mut dirs: Vec<&str> = Vec::new();
    for l in out.lines().map(str::trim).filter(|l| l.starts_with('/')) {
        let Some((d, _)) = l.rsplit_once('/') else { continue };
        if !STD.contains(&d) && !d.starts_with("/mnt/") && !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    if dirs.is_empty() { String::new() } else { format!("export PATH='{}':\"$PATH\"; ", dirs.join(":").replace('\'', "")) }
}

/// WSL のパスなら Linux のパス（シェルに渡す用）、それ以外はそのまま
pub fn shell_path(p: &Path) -> String {
    match split_unc(p) {
        Some((_, linux)) => linux,
        None => p.display().to_string(),
    }
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

    #[test]
    fn prefix() {
        assert_eq!(prefix_from("/usr/bin/python3\n/home/me/.nvm/versions/node/v22/bin/node\n/home/me/.nvm/versions/node/v22/bin/npm\n/mnt/c/x/go.exe\n"), "export PATH='/home/me/.nvm/versions/node/v22/bin':\"$PATH\"; ");
        assert_eq!(prefix_from("/usr/bin/gcc\n"), "");
    }
}
