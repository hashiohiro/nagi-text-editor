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

/// ゴールを動かすコマンド（根に mvnw があればそれを使う）
pub fn command(root: &Path, pom_dir: &Path, goals: &str) -> String {
    let mvn = if root.join("mvnw.cmd").is_file() { "mvnw.cmd" } else { "mvn" };
    let pom = pom_dir.join("pom.xml");
    format!("{mvn} -B -f \"{}\" {goals}", pom.display())
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
}
