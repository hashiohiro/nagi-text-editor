//! nagi の設定のうち、裏の処理（Maven・git・JDK・ターミナル・ブラウザ）が読むもの。UI（nagi の設定の画面）が
//! 適用するたびに `set` で入れ替える。入れていなければ既定（PATH の mvn・git、Wrapper を使う、UTF-8 で始める）。
//!
//! JDK・Maven などはプロジェクトの `.nagi\settings.conf` で上書きできるので、プロジェクトの根ごとに持つ
//! （1 つのプロセスに窓が何枚あっても、違うプロジェクトの窓の設定は混ざらない）。読む側はフォルダを渡し
//! （`for_path`）、そのフォルダを含むいちばん深い根の設定を受け取る。どの根にも入らなければ全体の設定（`global`）。

use nagi_core::Context;
use std::path::{Path, PathBuf};
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
    /// Windows で動かすもの（コマンド・ターミナル）に渡す環境変数（PATH・JAVA_HOME。プロジェクトの JDK・言語のツールを頭に置いたもの）。
    /// 空なら nagi の環境のまま
    pub env: Vec<(String, String)>,
    /// 検索・ツリー・索引から外すフォルダの名前
    pub exclude: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config { maven: None, use_wrapper: true, mvn_options: String::new(), jdk: None, git: None, git_wsl: true, browser: None, terminal_utf8: true, exclude: Vec::new(), env: Vec::new() }
    }
}

/// 全体の設定（どのプロジェクトにも入らないもの）と、プロジェクトの根ごとの設定
static GLOBAL: RwLock<Option<Config>> = RwLock::new(None);
static ROOTS: RwLock<Vec<(PathBuf, Config)>> = RwLock::new(Vec::new());

/// 設定を入れる。`root` が None なら全体の設定（プロジェクトの上書きを除いたもの）
pub fn set(root: Option<&Path>, c: Config) {
    match root {
        None => {
            if let Ok(mut g) = GLOBAL.write() {
                *g = Some(c);
            }
        }
        Some(r) => {
            let r = std::path::absolute(r).unwrap_or_else(|_| r.to_path_buf());
            if let Ok(mut v) = ROOTS.write() {
                match v.iter_mut().find(|(p, _)| same(p, &r)) {
                    Some(e) => e.1 = c,
                    None => v.push((r, c)),
                }
            }
        }
    }
}

/// 全体の設定（ブラウザなど、プロジェクトで変わらないものを読むとき）
pub fn global() -> Config {
    GLOBAL.read().ok().and_then(|c| c.clone()).unwrap_or_default()
}

/// `path`（フォルダ・ファイル）を含むプロジェクトの設定（いちばん深い根。無ければ全体の設定）
pub fn for_path(path: &Path) -> Config {
    let p = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let found = ROOTS.read().ok().and_then(|v| v.iter().filter(|(r, _)| under(&p, r)).max_by_key(|(r, _)| r.components().count()).map(|(_, c)| c.clone()));
    found.unwrap_or_else(global)
}

/// `p` が `root` の中（同じも含む）か。Windows のパスなので大文字・小文字は区別しない
fn under(p: &Path, root: &Path) -> bool {
    let mut a = p.components();
    for b in root.components() {
        match a.next() {
            Some(x) if x.as_os_str().eq_ignore_ascii_case(b.as_os_str()) => {}
            _ => return false,
        }
    }
    true
}

fn same(a: &Path, b: &Path) -> bool {
    under(a, b) && under(b, a)
}

impl Config {
    /// 渡す環境変数の値（無ければ nagi の環境変数）
    pub fn var(&self, key: &str) -> Option<std::ffi::OsString> {
        self.env.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.into()).or_else(|| std::env::var_os(key))
    }
}

/// URL を開く（設定のブラウザ、無ければ既定のブラウザ）
pub fn open_url(url: &str) -> nagi_core::Result<()> {
    let mut cmd = match global().browser {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 根ごとの設定: 違うプロジェクトの設定は混ざらず、中のフォルダはいちばん深い根の設定。どの根にも入らなければ全体の設定
    #[test]
    fn config_per_project_root() {
        let base = std::env::temp_dir().join(format!("nagi-config-{}", std::process::id()));
        let (a, b, inner) = (base.join("a"), base.join("b"), base.join("a").join("sub"));
        let jdk = |s: &str| Config { jdk: Some(PathBuf::from(s)), env: vec![("JAVA_HOME".into(), s.into())], ..Config::default() };
        set(Some(&a), jdk("C:\\jdk8"));
        set(Some(&b), jdk("C:\\jdk17"));
        set(Some(&inner), jdk("C:\\jdk21"));
        assert_eq!(for_path(&a.join("src").join("A.java")).jdk, Some(PathBuf::from("C:\\jdk8")));
        assert_eq!(for_path(&b).jdk, Some(PathBuf::from("C:\\jdk17")), "別のプロジェクトの設定は混ざらない");
        assert_eq!(for_path(&inner.join("x")).jdk, Some(PathBuf::from("C:\\jdk21")), "いちばん深い根");
        // 大文字・小文字は区別しない（Windows のパス）
        let upper = PathBuf::from(b.display().to_string().to_uppercase());
        assert_eq!(for_path(&upper).jdk, Some(PathBuf::from("C:\\jdk17")));
        // 同じ根に入れ直すと置き換わる
        set(Some(&b), jdk("C:\\jdk11"));
        assert_eq!(for_path(&b).var("JAVA_HOME"), Some("C:\\jdk11".into()));
        // どの根にも入らない
        assert_eq!(for_path(&base.join("c")), global());
        assert!(!under(Path::new("C:\\ab"), Path::new("C:\\a")), "名前の頭が同じだけのフォルダは中ではない");
    }
}
