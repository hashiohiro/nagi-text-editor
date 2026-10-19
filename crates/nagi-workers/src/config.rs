//! nagi の設定のうち、裏の処理（Maven・git・JDK・ターミナル・ブラウザ）が読むもの。UI（nagi の設定の画面）が
//! 適用するたびに `set` で入れ替える。入れていなければ既定（PATH の mvn・git、Wrapper を使う、UTF-8 で始める）。

use nagi_core::Context;
use std::path::PathBuf;
use std::sync::RwLock;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// 設定の Maven（mvn のある bin。None なら maven::bin が探す）
    pub maven: Option<PathBuf>,
    /// Maven Wrapper（mvnw）を使うか
    pub use_wrapper: bool,
    /// Maven にいつも付けるオプション
    pub mvn_options: String,
    /// Maven・Gradle の JDK（JAVA_HOME にする。None なら JAVA_HOME か ~\.jdks から探す）
    pub jdk: Option<PathBuf>,
    /// git の場所（None なら PATH の git）
    pub git: Option<String>,
    /// WSL のプロジェクトは WSL の中の git で動かすか
    pub git_wsl: bool,
    /// URL を開くブラウザ（None なら既定のブラウザ）
    pub browser: Option<String>,
    /// ターミナルのシェルを UTF-8 で始めるか
    pub terminal_utf8: bool,
    /// 検索・ツリー・索引から外すフォルダの名前
    pub exclude: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config { maven: None, use_wrapper: true, mvn_options: String::new(), jdk: None, git: None, git_wsl: true, browser: None, terminal_utf8: true, exclude: Vec::new() }
    }
}

static CONFIG: RwLock<Option<Config>> = RwLock::new(None);

pub fn get() -> Config {
    CONFIG.read().ok().and_then(|c| c.clone()).unwrap_or_default()
}

pub fn set(c: Config) {
    if let Ok(mut g) = CONFIG.write() {
        *g = Some(c);
    }
}

/// URL を開く（設定のブラウザ、無ければ既定のブラウザ）
pub fn open_url(url: &str) -> nagi_core::Result<()> {
    let mut cmd = match get().browser {
        Some(b) => {
            let mut c = std::process::Command::new(b);
            c.arg(url);
            c
        }
        None => {
            let mut c = std::process::Command::new("rundll32.exe");
            c.args(["url.dll,FileProtocolHandler", url]);
            c
        }
    };
    crate::sys::no_window(&mut cmd);
    cmd.spawn().map(|_| ()).context("ブラウザを開けません")
}
