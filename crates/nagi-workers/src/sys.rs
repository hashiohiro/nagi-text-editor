//! 外のプログラムを動かすときの小さな共通の処理（窓を出さない・Windows に付いているツールの場所・版の比べ方）

use std::path::PathBuf;
use std::process::Command;

/// コンソールの窓を出さずに動かす（CREATE_NO_WINDOW）。標準入出力はそのまま
pub fn no_window(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

/// Windows に付いているツール（curl.exe・tar.exe）。System32 に無ければ名前だけ（PATH から）
pub fn system_tool(name: &str) -> PathBuf {
    let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let p = root.join("System32").join(name);
    if p.is_file() { p } else { PathBuf::from(name) }
}

/// 版の数の並び（並べ替え用。"3.14.8" → [3, 14, 8]、"go1.27.2" → [1, 27, 2]）
pub fn version_key(v: &str) -> Vec<u32> {
    v.split(|c: char| !c.is_ascii_digit()).filter_map(|p| p.parse().ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(version_key("3.15.0") > version_key("3.9.20"));
        assert_eq!(version_key("go1.27.2"), [1, 27, 2]);
    }
}
