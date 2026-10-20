//! Maven のプロジェクト（pom.xml）を読む。右の欄の「Maven」タブ用。
//!
//! XML は要る所（project 直下の artifactId・name・packaging・modules・dependencies）だけを、要素の入れ子を
//! 数えながら拾う。`${...}` の置き換えはしない（表示するだけなので）。

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dep {
    pub group: String,
    pub artifact: String,
    pub version: String,
    pub scope: String,
}

#[derive(Clone, Debug, Default)]
pub struct Pom {
    pub dir: PathBuf,
    pub artifact: String,
    pub name: String,
    pub packaging: String,
    pub modules: Vec<String>,
    pub deps: Vec<Dep>,
    /// 読んだモジュール（modules の順）
    pub children: Vec<Pom>,
}

impl Pom {
    pub fn label(&self) -> &str {
        if self.name.is_empty() || self.name.contains("${") { &self.artifact } else { &self.name }
    }
}

/// pom.xml の本文を読む
pub fn parse_pom(text: &str) -> Pom {
    let mut pom = Pom::default();
    let mut stack: Vec<String> = Vec::new();
    let mut dep = Dep::default();
    let mut rest = text;
    while let Some(lt) = rest.find('<') {
        let content = &rest[..lt];
        rest = &rest[lt..];
        if let Some(r) = rest.strip_prefix("<!--") {
            rest = r.find("-->").map_or("", |i| &r[i + 3..]);
            continue;
        }
        if rest.starts_with("<?") || rest.starts_with("<!") {
            rest = rest.find('>').map_or("", |i| &rest[i + 1..]);
            continue;
        }
        let Some(gt) = rest.find('>') else { break };
        let tag = &rest[1..gt];
        rest = &rest[gt + 1..];
        if let Some(name) = tag.strip_prefix('/') {
            // 閉じたときに、その要素の中身の文字を使う
            let path: Vec<&str> = stack.iter().map(String::as_str).collect();
            let v = content.trim().to_string();
            match path.as_slice() {
                ["project", "artifactId"] => pom.artifact = v,
                ["project", "name"] => pom.name = v,
                ["project", "packaging"] => pom.packaging = v,
                ["project", "modules", "module"] => pom.modules.push(v),
                ["project", "dependencies", "dependency", f] => match *f {
                    "groupId" => dep.group = v,
                    "artifactId" => dep.artifact = v,
                    "version" => dep.version = v,
                    "scope" => dep.scope = v,
                    _ => {}
                },
                ["project", "dependencies", "dependency"] => pom.deps.push(std::mem::take(&mut dep)),
                _ => {}
            }
            if stack.last().map(String::as_str) == Some(name.trim()) {
                stack.pop();
            }
            continue;
        }
        if tag.ends_with('/') {
            continue;
        }
        let name = tag.split(|c: char| c.is_whitespace()).next().unwrap_or("");
        stack.push(name.to_string());
    }
    if pom.packaging.is_empty() {
        pom.packaging = "jar".into();
    }
    pom
}

/// `dir/pom.xml` とそのモジュールを読む（深さ 6 まで）
pub fn load(dir: &Path) -> Option<Pom> {
    fn go(dir: &Path, depth: usize) -> Option<Pom> {
        let text = std::fs::read_to_string(dir.join("pom.xml")).ok()?;
        let mut pom = parse_pom(&text);
        pom.dir = dir.to_path_buf();
        if depth < 6 {
            pom.children = pom.modules.iter().filter_map(|m| go(&dir.join(m), depth + 1)).collect();
        }
        Some(pom)
    }
    go(dir, 0)
}

/// `from` を含む Maven のプロジェクトの根（pom.xml のある、いちばん上の続いたフォルダ）
pub fn find_root(from: &Path) -> Option<PathBuf> {
    let start = if from.is_dir() { from } else { from.parent()? };
    let mut found = None;
    let mut dir = Some(start);
    while let Some(d) = dir {
        if d.join("pom.xml").is_file() {
            found = Some(d.to_path_buf());
        } else if found.is_some() {
            break;
        }
        dir = d.parent();
    }
    found
}

/// Maven Wrapper が使えるか（mvnw だけあって `.mvn/wrapper/maven-wrapper.properties` が無いと、wrapper は起動できない）
fn wrapper_ready(root: &Path, script: &str) -> bool {
    crate::config::for_path(root).use_wrapper && root.join(script).is_file() && root.join(".mvn").join("wrapper").join("maven-wrapper.properties").is_file()
}

/// ゴールを動かすコマンド（根に使える mvnw があればそれを、無ければ mvn。mvn の場所は bin が決め、設定の適用が PATH に置く）
pub fn command(root: &Path, pom_dir: &Path, goals: &str) -> String {
    let pom = pom_dir.join("pom.xml");
    // WSL のプロジェクトは WSL の中で動かすので、Linux のパスと ./mvnw
    let ex = crate::exec::Exec::in_dir(root);
    if ex.is_wsl() {
        let mvn = if wrapper_ready(root, "mvnw") { "./mvnw" } else { "mvn" };
        return format!("{mvn} -B -f \"{}\" {goals}", ex.path(&pom));
    }
    // 設定の「追加のオプション」
    let cfg = crate::config::for_path(root);
    let mvn = if wrapper_ready(root, "mvnw.cmd") { "mvnw.cmd" } else { "mvn" };
    let goals = if cfg.mvn_options.trim().is_empty() { goals.to_string() } else { format!("{} {goals}", cfg.mvn_options.trim()) };
    format!("{mvn} -B -f \"{}\" {goals}", pom.display())
}

/// mvn のある bin。Maven の場所はここだけで決める（設定の適用がこれを PATH の頭に置き、動かす行は `mvn` で呼ぶ。
/// :Make・Maven の欄・実行の構成・ターミナルのどれも同じ mvn になる）。
/// 設定の Maven → `path`（PATH）→ MAVEN_HOME・M2_HOME → nagi が入れたもの（tools\maven-<版>）→ ~\.m2\wrapper\dists → scoop。
/// IDE（IntelliJ・Eclipse）に同梱のものは使わない（IDE が無くても同じに動くように）。
/// どれも無ければ None（mvn を動かすときに nagi がダウンロードして入れる。missing）
pub fn bin(cfg: &crate::config::Config, path: Option<std::ffi::OsString>) -> Option<PathBuf> {
    // 設定の Maven（bin を選ぶが、Maven のフォルダそのものでも）
    if let Some(m) = cfg.maven.clone() {
        if let Some(b) = [m.clone(), m.join("bin")].into_iter().find(|b| b.join("mvn.cmd").is_file()) {
            return Some(b);
        }
    }
    let var = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let homes: Vec<PathBuf> = ["MAVEN_HOME", "M2_HOME"].iter().filter_map(|k| var(k)).collect();
    let user = var("USERPROFILE").or_else(|| var("HOME")).unwrap_or_default();
    find_bin(path, &homes, &crate::toolchains::tools_dir(), &user)
}

/// この行が mvn を動かすのに、mvn がどこにも無いか（bin が見つけられない）。WSL の中では見ない
pub fn missing(line: &str, ex: &crate::exec::Exec) -> bool {
    let first = line.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
    !ex.is_wsl() && matches!(first.as_str(), "mvn" | "mvn.cmd") && bin(ex.config(), ex.config().var("PATH")).is_none()
}

/// `dir` から `depth` 段下のフォルダ
fn below(dir: &Path, depth: usize) -> Vec<PathBuf> {
    let mut v = vec![dir.to_path_buf()];
    for _ in 0..depth {
        v = v.iter().flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten().map(|e| e.path())).filter(|p| p.is_dir()).collect();
    }
    v
}

fn find_bin(path: Option<std::ffi::OsString>, homes: &[PathBuf], tools: &Path, user: &Path) -> Option<PathBuf> {
    let has = |bin: &Path| bin.join("mvn.cmd").is_file();
    if let Some(b) = path.and_then(|p| std::env::split_paths(&p).find(|d| has(d))) {
        return Some(b);
    }
    if let Some(b) = homes.iter().map(|h| h.join("bin")).find(|b| has(b)) {
        return Some(b);
    }
    // いちばん新しいもの（版の順で最後）
    let newest = |mut v: Vec<PathBuf>| {
        v.sort_by_key(|p| crate::sys::version_key(&p.to_string_lossy()));
        v.pop()
    };
    // nagi が入れたもの: tools\maven-<版>\apache-maven-<版>\bin
    let mine = below(tools, 2).into_iter().filter(|d| d.parent().and_then(|p| p.file_name()).is_some_and(|n| n.to_string_lossy().starts_with("maven-"))).map(|d| d.join("bin")).filter(|b| has(b)).collect();
    if let Some(b) = newest(mine) {
        return Some(b);
    }
    // Wrapper が取ってきたもの: dists\apache-maven-<版>[-bin]\<ハッシュ>[\apache-maven-<版>]\bin
    let dists = user.join(".m2").join("wrapper").join("dists");
    let wrapped = [2, 3].iter().flat_map(|&n| below(&dists, n)).map(|d| d.join("bin")).filter(|b| has(b)).collect();
    if let Some(b) = newest(wrapped) {
        return Some(b);
    }
    Some(user.join("scoop").join("apps").join("maven").join("current").join("bin")).filter(|b| has(b))
}

/// ライフサイクルのゴール（IntelliJ の Maven の画面と同じ並び）
pub const LIFECYCLE: [&str; 9] = ["clean", "validate", "compile", "test", "package", "verify", "install", "site", "deploy"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse() {
        let pom = parse_pom(
            r#"<?xml version="1.0"?>
<!-- comment <artifactId>no</artifactId> -->
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <parent><artifactId>spring-boot-starter-parent</artifactId></parent>
  <artifactId>myapp</artifactId>
  <name>My App</name>
  <packaging>pom</packaging>
  <modules><module>app.core</module><module>rest.demo</module></modules>
  <dependencyManagement><dependencies><dependency><artifactId>skip</artifactId></dependency></dependencies></dependencyManagement>
  <dependencies>
    <dependency><groupId>com.oracle.database.jdbc</groupId><artifactId>ojdbc11</artifactId><scope>runtime</scope></dependency>
    <dependency><groupId>org.x</groupId><artifactId>y</artifactId><version>${y.version}</version><exclusions><exclusion><artifactId>z</artifactId></exclusion></exclusions></dependency>
  </dependencies>
  <build><plugins><plugin><artifactId>plug</artifactId></plugin></plugins></build>
  <empty/>
</project>"#,
        );
        assert_eq!((pom.artifact.as_str(), pom.label(), pom.packaging.as_str()), ("myapp", "My App", "pom"));
        assert_eq!(pom.modules, vec!["app.core", "rest.demo"]);
        assert_eq!(pom.deps.len(), 2);
        assert_eq!((pom.deps[0].artifact.as_str(), pom.deps[0].scope.as_str()), ("ojdbc11", "runtime"));
        assert_eq!((pom.deps[1].artifact.as_str(), pom.deps[1].version.as_str()), ("y", "${y.version}"));
    }

    /// mvnw.cmd だけあって .mvn/wrapper の設定が無ければ、PATH の mvn を使う
    #[test]
    fn wrapper_needs_properties() {
        let dir = std::env::temp_dir().join(format!("nagi-mvnw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mvnw.cmd"), "").unwrap();
        assert!(command(&dir, &dir, "install").starts_with("mvn -B"));
        std::fs::create_dir_all(dir.join(".mvn/wrapper")).unwrap();
        std::fs::write(dir.join(".mvn/wrapper/maven-wrapper.properties"), "distributionUrl=x").unwrap();
        assert!(command(&dir, &dir, "install").starts_with("mvnw.cmd -B"));
    }

    /// PATH → MAVEN_HOME → nagi が入れたもの（新しい版）→ Wrapper のもの
    #[test]
    fn finds_maven_without_path() {
        let dir = std::env::temp_dir().join(format!("nagi-mvnhome-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mvn = |bin: &Path| {
            std::fs::create_dir_all(bin).unwrap();
            std::fs::write(bin.join("mvn.cmd"), "").unwrap();
            bin.to_path_buf()
        };
        let (user, tools) = (dir.join("user"), dir.join("tools"));
        assert_eq!(find_bin(None, &[], &tools, &user), None);
        let wrapped = mvn(&user.join(".m2/wrapper/dists/apache-maven-3.9.6-bin/abc/apache-maven-3.9.6/bin"));
        assert_eq!(find_bin(None, &[], &tools, &user), Some(wrapped));
        mvn(&tools.join("maven-3.9.9/apache-maven-3.9.9/bin"));
        let mine = mvn(&tools.join("maven-3.9.11/apache-maven-3.9.11/bin"));
        assert_eq!(find_bin(None, &[], &tools, &user), Some(mine), "3.9.11 は 3.9.9 より新しい");
        let home = dir.join("maven");
        let home_bin = mvn(&home.join("bin"));
        assert_eq!(find_bin(None, &[home.clone()], &tools, &user), Some(home_bin.clone()));
        let path_bin = mvn(&dir.join("onpath"));
        let path = std::env::join_paths([dir.join("nothing"), path_bin.clone()]).unwrap();
        assert_eq!(find_bin(Some(path), &[home], &tools, &user), Some(path_bin), "PATH にあればそれ");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
