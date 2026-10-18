//! WSL 上のプロジェクト（`\\wsl.localhost\<ディストリビューション>\home\...`）を扱う。
//!
//! ファイルは Windows から UNC のパスで読み書きし、git とコマンド（:Make・Maven）とシェルは WSL の中で動かす
//! （Windows の git で UNC を扱うと遅く、所有者が違うので safe.directory で断られる）。

use std::path::{Path, PathBuf};

/// WSL のディストリビューション（既定のものが先頭）。データ用（〜-data）は除く
pub fn distros() -> Vec<String> {
    let Ok(out) = crate::exec::system("wsl.exe").args(["-l", "-q"]).output() else { return Vec::new() };
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
                let home = crate::exec::Exec::in_distro(d).shell("printf %s \"$HOME\"").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|h| h.starts_with('/')).unwrap_or_else(|| "/root".into());
                format!("{home}/{rest}")
            } else {
                p.to_string()
            };
            return to_unc(d, &linux);
        }
    }
    PathBuf::from(p)
}

/// ログインのシェル（~/.profile と ~/.bashrc。nvm・pyenv・sdkman などもここで PATH に入る）の環境のうち、
/// コマンドに要るもの（PATH と主な変数）を `export K='V'; ` の並びにしたもの。ディストリビューションごとに 1 度だけ調べる。
/// コマンドはログインシェルにせず（-l だとログインの挨拶が毎回出る）、これを頭に付けた `sh -c` で動かす（exec.rs）。
/// PATH から Windows の場所（/mnt/…）は外す（WSL のプロジェクトでは WSL のツールだけを使う）
pub fn login_env(distro: &str) -> String {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(v) = cache.lock().ok().and_then(|m| m.get(distro).cloned()) {
        return v;
    }
    let script = format!("printf '\\n{MARK}\\n'; for k in {KEYS}; do v=$(printenv \"$k\") && printf '%s=%s\\n' \"$k\" \"$v\"; done");
    let out = crate::exec::system("wsl.exe").args(["-d", distro, "--exec", "bash", "-lic", &script]).output();
    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    let v = env_from(&text);
    if let Ok(mut m) = cache.lock() {
        m.insert(distro.to_string(), v.clone());
    }
    v
}

/// 調べた出力の始まりの印（ログインの挨拶と分けるため）
const MARK: &str = "NAGI_ENV";
/// 引き継ぐ変数
const KEYS: &str = "PATH JAVA_HOME GOPATH GOROOT CARGO_HOME RUSTUP_HOME NVM_DIR PYENV_ROOT SDKMAN_DIR DOTNET_ROOT LANG";

/// 出力（印の後ろの K=V の行）から export の並びに
fn env_from(out: &str) -> String {
    let Some(i) = out.find(MARK) else { return String::new() };
    let q = |v: &str| format!("'{}'", v.replace('\'', "'\\''"));
    let mut s = String::new();
    for l in out[i + MARK.len()..].lines() {
        let Some((k, v)) = l.split_once('=') else { continue };
        if !KEYS.split(' ').any(|x| x == k) {
            continue;
        }
        let v = if k == "PATH" { v.split(':').filter(|d| !d.starts_with("/mnt/") && !d.is_empty()).collect::<Vec<_>>().join(":") } else { v.to_string() };
        s.push_str(&format!("export {k}={}; ", q(&v)));
    }
    s
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
    fn login_env_lines() {
        let out = "┏━(Message from Kali developers)\nNAGI_ENV\nPATH=/home/me/.nvm/bin:/usr/bin:/mnt/c/WINDOWS:/mnt/c/tools/mvn/bin\nNVM_DIR=/home/me/.nvm\nLANG=en_US.UTF-8\nOTHER=x\n";
        assert_eq!(env_from(out), "export PATH='/home/me/.nvm/bin:/usr/bin'; export NVM_DIR='/home/me/.nvm'; export LANG='en_US.UTF-8'; ");
        assert_eq!(env_from("挨拶だけ"), "");
    }
}
