//! 実行の構成（IntelliJ の Run Configurations と同じ考え方）。
//!
//! IntelliJ が保存したものをそのまま読む:
//! - `.run/*.run.xml`（プロジェクトに入れて共有するもの）
//! - `.idea/runConfigurations/*.xml`（古い共有の形）
//! - 作業状態（`.idea/workspace.xml` か `%APPDATA%\JetBrains\<製品>\workspace\<ID>.xml`）の `RunManager`（自分だけのもの。選んでいるものも）
//!
//! nagi 独自の構成は `.nagi/run.conf` に `名前 = コマンド` の形で 1 行に 1 つ（`#` から行末は注釈）。
//! 種類ごとのコマンドへの直し方は `launch`（Maven・Spring Boot・アプリケーション・npm・Gradle・シェル・URL）。

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Maven のゴール（`-N validate` など）、プロファイル（名前, 有効か）、追加のオプション、作業フォルダ、pom のファイル名
    Maven { goals: Vec<String>, profiles: Vec<(String, bool)>, options: String, workdir: Option<String>, pom: Option<String> },
    /// Spring Boot（Maven の spring-boot:run で動かす）
    SpringBoot { module: Option<String>, main_class: String, vm: String, args: String, profiles: String, env: Vec<(String, String)> },
    /// Java のアプリケーション（Maven の exec:java で動かす）
    Application { module: Option<String>, main_class: String, args: String, env: Vec<(String, String)> },
    /// npm（`run` と スクリプト名、引数）
    Npm { package_json: Option<String>, command: String, scripts: Vec<String>, args: String, env: Vec<(String, String)> },
    /// Gradle のタスク
    Gradle { tasks: Vec<String>, args: String },
    /// シェルのコマンド（nagi 独自の構成と IntelliJ のシェルスクリプト）
    Shell { command: String, workdir: Option<String> },
    /// ブラウザで開く（JavaScript のデバッグの構成）
    Url { url: String },
    /// 動かし方が分からない種類（IntelliJ の型の名前）
    Unsupported { type_name: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunConfig {
    pub name: String,
    pub kind: Kind,
    /// どこから読んだか（"IntelliJ" か "nagi"）
    pub from: &'static str,
}

impl RunConfig {
    /// 一覧に出す種類の名前
    pub fn kind_label(&self) -> String {
        match &self.kind {
            Kind::Maven { .. } => "Maven".into(),
            Kind::SpringBoot { .. } => "Spring Boot".into(),
            Kind::Application { .. } => "アプリケーション".into(),
            Kind::Npm { .. } => "npm".into(),
            Kind::Gradle { .. } => "Gradle".into(),
            Kind::Shell { .. } => "シェル".into(),
            Kind::Url { .. } => "ブラウザ".into(),
            Kind::Unsupported { type_name } => format!("未対応: {type_name}"),
        }
    }
}

/// 動かし方（コマンド・作業フォルダ・環境変数）。URL はブラウザで開く
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Launch {
    Command { cmd: String, cwd: PathBuf, env: Vec<(String, String)> },
    Open(String),
}

/// プロジェクトの構成を全部読む（IntelliJ の共有 → 作業状態 → nagi の順。同じ名前は先のもの）。
/// 2 つ目は IntelliJ で選んでいた構成の名前
pub fn load(root: &Path, workspace_xmls: &[String]) -> (Vec<RunConfig>, Option<String>) {
    let mut out: Vec<RunConfig> = Vec::new();
    let mut push = |c: RunConfig| {
        if !out.iter().any(|x| x.name == c.name) {
            out.push(c);
        }
    };
    for dir in [root.join(".run"), root.join(".idea").join("runConfigurations")] {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "xml")).collect();
        files.sort();
        for f in files {
            if let Ok(s) = std::fs::read_to_string(&f) {
                parse_xml(&s).into_iter().for_each(&mut push);
            }
        }
    }
    let mut selected = None;
    for ws in workspace_xmls {
        if let Some(rm) = component(ws, "RunManager") {
            // selected="Spring Boot.DemoApplication"（種類の名前.構成の名前）
            if selected.is_none() {
                selected = attr(rm.split('>').next().unwrap_or(""), "selected").map(|s| s.split_once('.').map_or(s.clone(), |x| x.1.to_string()));
            }
            parse_xml(rm).into_iter().for_each(&mut push);
        }
    }
    if let Ok(s) = std::fs::read_to_string(root.join(".nagi").join("run.conf")) {
        parse_conf(&s).into_iter().for_each(&mut push);
    }
    (out, selected)
}

/// `.nagi/run.conf`: `名前 = コマンド`
pub fn parse_conf(s: &str) -> Vec<RunConfig> {
    s.lines()
        .map(|l| l.split_once('#').map_or(l, |x| x.0).trim())
        .filter_map(|l| l.split_once('='))
        .map(|(n, c)| (n.trim(), c.trim()))
        .filter(|(n, c)| !n.is_empty() && !c.is_empty())
        .map(|(n, c)| RunConfig { name: n.to_string(), kind: Kind::Shell { command: c.to_string(), workdir: None }, from: "nagi" })
        .collect()
}

/// `<component name="...">…</component>` の中身
fn component<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let i = xml.find(&format!("<component name=\"{name}\""))?;
    let rest = &xml[i..];
    let end = rest.find("</component>").map_or(rest.len(), |e| e + "</component>".len());
    Some(&rest[..end])
}

/// 実体参照を戻す
fn unescape(s: &str) -> String {
    s.replace("&quot;", "\"").replace("&apos;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&#10;", "\n").replace("&#13;", "\r").replace("&#9;", "\t").replace("&amp;", "&")
}

/// タグの文字（`<x a="1" b="2">`）から属性
fn attr(tag: &str, name: &str) -> Option<String> {
    let key = format!(" {name}=\"");
    let i = tag.find(&key)? + key.len();
    let j = tag[i..].find('"')?;
    Some(unescape(&tag[i..i + j]))
}

/// 中身の `<タグ …>` を前から順に（開くタグだけ。閉じるタグと注釈は飛ばす）
fn tags<'a>(body: &'a str, tag: &str) -> impl Iterator<Item = &'a str> + 'a {
    let open = format!("<{tag}");
    let found: Vec<&'a str> = body
        .match_indices(open.as_str())
        .filter_map(|(i, _)| {
            let rest = &body[i..];
            // <option と <options を区別する
            let next = rest[open.len()..].chars().next()?;
            if !(next.is_whitespace() || next == '>' || next == '/') {
                return None;
            }
            rest.find('>').map(|e| &rest[..=e])
        })
        .collect();
    found.into_iter()
}

/// `<option name="X" value="Y" />` の値
fn option(body: &str, name: &str) -> Option<String> {
    tags(body, "option").find(|t| attr(t, "name").as_deref() == Some(name)).and_then(|t| attr(t, "value"))
}

/// `<option name="X"> <list> <option value=".."/> … </list> </option>` の値の並び
fn option_list(body: &str, name: &str) -> Vec<String> {
    let Some(i) = body.find(&format!("<option name=\"{name}\">")) else { return Vec::new() };
    let rest = &body[i + 1..];
    let end = rest.find("</option>").unwrap_or(rest.len());
    // 入れ子の <option value=> は </option> を持たない（自分で閉じる）ので、最初の </option> までが中身
    tags(&rest[..end], "option").filter_map(|t| attr(t, "value")).collect()
}

/// `<envs><env name="A" value="1" /></envs>`
fn envs(body: &str) -> Vec<(String, String)> {
    tags(body, "env").filter_map(|t| Some((attr(t, "name")?, attr(t, "value").unwrap_or_default()))).collect()
}

/// XML の中の `<configuration>` を全部（テンプレート・既定は飛ばす）
pub fn parse_xml(xml: &str) -> Vec<RunConfig> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(i) = xml[at..].find("<configuration ") {
        let start = at + i;
        let rest = &xml[start..];
        let head_end = rest.find('>').map_or(rest.len(), |e| e + 1);
        let head = &rest[..head_end];
        let (body, len) = if head.ends_with("/>") {
            ("", head_end)
        } else {
            let e = rest.find("</configuration>").unwrap_or(rest.len());
            (&rest[head_end..e], e + "</configuration>".len().min(rest.len() - e))
        };
        at = start + len.max(1);
        if attr(head, "default").as_deref() == Some("true") || head.contains("isTemplate") {
            continue;
        }
        let (Some(name), Some(ty)) = (attr(head, "name"), attr(head, "type")) else { continue };
        let kind = parse_kind(&ty, head, body);
        out.push(RunConfig { name, kind, from: "IntelliJ" });
    }
    out
}

fn parse_kind(ty: &str, head: &str, body: &str) -> Kind {
    let module = tags(body, "module").next().and_then(|t| attr(t, "name"));
    let s = |k: &str| option(body, k).unwrap_or_default();
    match ty {
        "MavenRunConfiguration" => {
            let profiles = match body.find("<option name=\"profilesMap\">") {
                Some(i) => {
                    let rest = &body[i..];
                    let end = rest.find("</map>").unwrap_or(rest.len());
                    tags(&rest[..end], "entry").filter_map(|t| Some((attr(t, "key")?, attr(t, "value").as_deref() == Some("true")))).collect()
                }
                None => Vec::new(),
            };
            Kind::Maven {
                goals: option_list(body, "goals"),
                profiles,
                options: option(body, "cmdOptions").unwrap_or_default(),
                workdir: option(body, "workingDirPath").filter(|w| !w.is_empty()),
                pom: option(body, "pomFileName").filter(|p| !p.is_empty()),
            }
        }
        "SpringBootApplicationConfigurationType" => Kind::SpringBoot {
            module,
            main_class: option(body, "SPRING_BOOT_MAIN_CLASS").or_else(|| option(body, "MAIN_CLASS_NAME")).unwrap_or_default(),
            vm: s("VM_PARAMETERS"),
            args: s("PROGRAM_PARAMETERS"),
            profiles: s("ACTIVE_PROFILES"),
            env: envs(body),
        },
        "Application" => Kind::Application { module, main_class: s("MAIN_CLASS_NAME"), args: s("PROGRAM_PARAMETERS"), env: envs(body) },
        "js.build_tools.npm" => Kind::Npm {
            package_json: tags(body, "package-json").next().and_then(|t| attr(t, "value")),
            command: tags(body, "command").next().and_then(|t| attr(t, "value")).unwrap_or_else(|| "run".into()),
            scripts: tags(body, "script").filter_map(|t| attr(t, "value")).collect(),
            args: tags(body, "arguments").next().and_then(|t| attr(t, "value")).unwrap_or_default(),
            env: envs(body),
        },
        "GradleRunConfiguration" => Kind::Gradle { tasks: option_list(body, "taskNames"), args: s("scriptParameters") },
        "ShConfigurationType" => {
            let file = option(body, "EXECUTE_SCRIPT_FILE").as_deref() == Some("true");
            let command = if file {
                let interp = s("INTERPRETER_PATH");
                let path = s("SCRIPT_PATH");
                let opts = s("SCRIPT_OPTIONS");
                [interp, format!("\"{path}\""), opts].into_iter().filter(|x| !x.is_empty() && x != "\"\"").collect::<Vec<_>>().join(" ")
            } else {
                s("SCRIPT_TEXT")
            };
            Kind::Shell { command, workdir: option(body, "SCRIPT_WORKING_DIRECTORY").filter(|w| !w.is_empty()) }
        }
        "JavascriptDebugType" => match attr(head, "uri") {
            Some(url) => Kind::Url { url },
            None => Kind::Unsupported { type_name: ty.to_string() },
        },
        _ => Kind::Unsupported { type_name: ty.to_string() },
    }
}

/// `$PROJECT_DIR$` などを置き換えたパス
fn expand(s: &str, root: &Path) -> PathBuf {
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let t = s.replace("$PROJECT_DIR$", &root.to_string_lossy()).replace("$USER_HOME$", &home).replace("$MODULE_DIR$", &root.to_string_lossy());
    let p = PathBuf::from(t);
    if p.is_absolute() || crate::wsl::split_unc(&p).is_some() { p } else { root.join(p) }
}

/// Maven の -pl の値（モジュールのフォルダがあればそのパス、無ければ artifactId として `:名前`）
fn maven_module(root: &Path, module: &str) -> String {
    if root.join(module).join("pom.xml").is_file() { module.to_string() } else { format!(":{module}") }
}

/// 空でなければ `-D名前="値"`
fn prop(name: &str, value: &str) -> String {
    if value.trim().is_empty() { String::new() } else { format!(" \"-D{name}={}\"", value.trim().replace('"', "\\\"")) }
}

/// 構成を動かすコマンドに直す
pub fn launch(cfg: &RunConfig, root: &Path) -> Result<Launch, String> {
    let cmd = |cmd: String, cwd: PathBuf, env: Vec<(String, String)>| Ok(Launch::Command { cmd, cwd, env });
    match &cfg.kind {
        Kind::Maven { goals, profiles, options, workdir, pom } => {
            let cwd = workdir.as_deref().map_or(root.to_path_buf(), |w| expand(w, root));
            let mut g = goals.join(" ");
            if !profiles.is_empty() {
                let p: Vec<String> = profiles.iter().map(|(n, on)| if *on { n.clone() } else { format!("!{n}") }).collect();
                g = format!("-P {} {g}", p.join(","));
            }
            if !options.trim().is_empty() {
                g = format!("{} {g}", options.trim());
            }
            let mut c = crate::maven::command(root, &cwd, &g);
            if let Some(p) = pom.as_deref().filter(|p| *p != "pom.xml") {
                c = c.replace(&format!("{}", cwd.join("pom.xml").display()), &format!("{}", cwd.join(p).display()));
            }
            cmd(c, cwd, Vec::new())
        }
        Kind::SpringBoot { module, main_class, vm, args, profiles, env } => {
            let pl = module.as_deref().map_or(String::new(), |m| format!("-pl {} ", maven_module(root, m)));
            let goals = format!(
                "{pl}spring-boot:run{}{}{}{}",
                prop("spring-boot.run.main-class", main_class),
                prop("spring-boot.run.jvmArguments", vm),
                prop("spring-boot.run.arguments", args),
                prop("spring-boot.run.profiles", profiles)
            );
            cmd(crate::maven::command(root, root, &goals), root.to_path_buf(), env.clone())
        }
        Kind::Application { module, main_class, args, env } => {
            if !root.join("pom.xml").is_file() {
                return Err("Maven のプロジェクトでないアプリケーションの構成は、まだ動かせません".into());
            }
            let pl = module.as_deref().map_or(String::new(), |m| format!("-pl {} ", maven_module(root, m)));
            let goals = format!("{pl}compile exec:java{}{}", prop("exec.mainClass", main_class), prop("exec.args", args));
            cmd(crate::maven::command(root, root, &goals), root.to_path_buf(), env.clone())
        }
        Kind::Npm { package_json, command, scripts, args, env } => {
            let cwd = package_json.as_deref().map(|p| expand(p, root)).and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(|| root.to_path_buf());
            let mut c = format!("npm {command}");
            for s in scripts {
                c.push(' ');
                c.push_str(s);
            }
            if !args.trim().is_empty() {
                c.push_str(" -- ");
                c.push_str(args.trim());
            }
            cmd(c, cwd, env.clone())
        }
        Kind::Gradle { tasks, args } => {
            let wsl = crate::wsl::split_unc(root).is_some();
            let gradle = if wsl && root.join("gradlew").is_file() {
                "./gradlew"
            } else if !wsl && root.join("gradlew.bat").is_file() {
                "gradlew.bat"
            } else {
                "gradle"
            };
            cmd(format!("{gradle} {} {}", tasks.join(" "), args).trim().to_string(), root.to_path_buf(), Vec::new())
        }
        Kind::Shell { command, workdir } => {
            if command.trim().is_empty() {
                return Err("コマンドが空です".into());
            }
            cmd(command.clone(), workdir.as_deref().map_or(root.to_path_buf(), |w| expand(w, root)), Vec::new())
        }
        Kind::Url { url } => Ok(Launch::Open(url.clone())),
        Kind::Unsupported { type_name } => Err(format!("この種類の構成はまだ動かせません（{type_name}）")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WS: &str = r#"<project version="4">
  <component name="RunManager" selected="Spring Boot.DemoApplication">
    <configuration name="DemoApplication" type="SpringBootApplicationConfigurationType" factoryName="Spring Boot" nameIsGenerated="true">
      <module name="rest.demo" />
      <option name="SPRING_BOOT_MAIN_CLASS" value="com.example.DemoApplication" />
      <option name="VM_PARAMETERS" value="-Xmx512m" />
      <envs><env name="TZ" value="Asia/Tokyo" /></envs>
      <method v="2"><option name="Make" enabled="true" /></method>
    </configuration>
    <configuration name="Angular CLI Server" type="js.build_tools.npm">
      <package-json value="$PROJECT_DIR$/package.json" />
      <command value="run" />
      <scripts><script value="start" /></scripts>
      <node-interpreter value="project" />
      <envs />
      <method v="2" />
    </configuration>
    <configuration name="Angular Application" type="JavascriptDebugType" uri="http://localhost:4200">
      <method v="2" />
    </configuration>
    <configuration default="true" type="Application" factoryName="Application"><method v="2" /></configuration>
  </component>
</project>"#;

    const MAVEN: &str = r#"<component name="ProjectRunConfigurationManager">
  <configuration default="false" name="core.lib インストール" type="MavenRunConfiguration" factoryName="Maven">
    <MavenSettings>
      <option name="myGeneralSettings" />
      <option name="myRunnerParameters">
        <MavenRunnerParameters>
          <option name="cmdOptions" />
          <option name="goals">
            <list>
              <option value="-N" />
              <option value="validate" />
            </list>
          </option>
          <option name="pomFileName" value="pom.xml" />
          <option name="profilesMap">
            <map>
              <entry key="install-core-lib" value="true" />
              <entry key="slow" value="false" />
            </map>
          </option>
          <option name="workingDirPath" value="$PROJECT_DIR$" />
        </MavenRunnerParameters>
      </option>
    </MavenSettings>
    <method v="2" />
  </configuration>
</component>"#;

    #[test]
    fn parses_intellij() {
        let v = parse_xml(WS);
        assert_eq!(v.len(), 3, "既定（テンプレート）は飛ばす");
        assert_eq!(
            v[0].kind,
            Kind::SpringBoot { module: Some("rest.demo".into()), main_class: "com.example.DemoApplication".into(), vm: "-Xmx512m".into(), args: String::new(), profiles: String::new(), env: vec![("TZ".into(), "Asia/Tokyo".into())] }
        );
        assert_eq!(v[1].kind, Kind::Npm { package_json: Some("$PROJECT_DIR$/package.json".into()), command: "run".into(), scripts: vec!["start".into()], args: String::new(), env: vec![] });
        assert_eq!(v[2].kind, Kind::Url { url: "http://localhost:4200".into() });
        let m = parse_xml(MAVEN);
        assert_eq!(m[0].name, "core.lib インストール");
        assert_eq!(
            m[0].kind,
            Kind::Maven { goals: vec!["-N".into(), "validate".into()], profiles: vec![("install-core-lib".into(), true), ("slow".into(), false)], options: String::new(), workdir: Some("$PROJECT_DIR$".into()), pom: Some("pom.xml".into()) }
        );
    }

    #[test]
    fn loads_and_launches() {
        let dir = std::env::temp_dir().join(format!("nagi-runcfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".run")).unwrap();
        std::fs::create_dir_all(dir.join(".nagi")).unwrap();
        std::fs::create_dir_all(dir.join("rest.demo")).unwrap();
        std::fs::write(dir.join("rest.demo/pom.xml"), "<project/>").unwrap();
        std::fs::write(dir.join("pom.xml"), "<project/>").unwrap();
        std::fs::write(dir.join(".run/a.run.xml"), MAVEN).unwrap();
        std::fs::write(dir.join(".nagi/run.conf"), "# 自分の\nサーバー = npm run dev   # 開発用\n").unwrap();
        let (v, sel) = load(&dir, &[WS.to_string()]);
        assert_eq!(v.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["core.lib インストール", "DemoApplication", "Angular CLI Server", "Angular Application", "サーバー"]);
        assert_eq!(sel.as_deref(), Some("DemoApplication"));
        let Launch::Command { cmd, cwd, .. } = launch(&v[0], &dir).unwrap() else { panic!() };
        assert!(cmd.starts_with("mvn -B -f ") && cmd.ends_with("-P install-core-lib,!slow -N validate"), "{cmd}");
        assert_eq!(cwd, dir);
        let Launch::Command { cmd, env, .. } = launch(&v[1], &dir).unwrap() else { panic!() };
        assert!(cmd.ends_with("-pl rest.demo spring-boot:run \"-Dspring-boot.run.main-class=com.example.DemoApplication\" \"-Dspring-boot.run.jvmArguments=-Xmx512m\""), "{cmd}");
        assert_eq!(env, vec![("TZ".to_string(), "Asia/Tokyo".to_string())]);
        let Launch::Command { cmd, cwd, .. } = launch(&v[2], &dir).unwrap() else { panic!() };
        assert_eq!((cmd.as_str(), cwd), ("npm run start", dir.clone()));
        assert_eq!(launch(&v[3], &dir).unwrap(), Launch::Open("http://localhost:4200".into()));
        let Launch::Command { cmd, .. } = launch(&v[4], &dir).unwrap() else { panic!() };
        assert_eq!(cmd, "npm run dev");
    }
}
