//! 実行の構成（IntelliJ の Run Configurations と同じ考え方）。
//!
//! IntelliJ が保存したものをそのまま読む:
//! - `.run/*.run.xml`（プロジェクトに入れて共有するもの）
//! - `.idea/runConfigurations/*.xml`（古い共有の形）
//! - 作業状態（`.idea/workspace.xml` か `%APPDATA%\JetBrains\<製品>\workspace\<ID>.xml`）の `RunManager`（自分だけのもの。選んでいるものも）
//!
//! nagi 独自の構成は `.nagi/run.conf`（`#` で始まる行は注釈）。GUI（実行の構成の画面）はこの形で書き出す:
//!
//! ```text
//! [DemoApplication]
//! type = spring-boot
//! module = rest.demo
//! main = com.example.DemoApplication
//! env = TZ=Asia/Tokyo; LANG=ja_JP.UTF-8
//! ```
//!
//! 見出しの前の `名前 = コマンド` の 1 行はシェルの構成（手で書く短い形）。nagi の構成は、同じ名前の IntelliJ の構成より先に出る
//! （GUI で IntelliJ の構成を直すと、nagi の構成として保存し、IntelliJ のファイルは書き換えない）。
//! 種類ごとのコマンドへの直し方は `launch`（Maven・Spring Boot・アプリケーション・npm・Gradle・シェル・URL と、
//! 言語のツール: Cargo・Go・.NET・Python・Node.js・PHP・Ruby・Make・CMake）。WSL のプロジェクトでは WSL の中のツールで動かす。

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
    /// 言語のツールで動かす（`tool` は TOOLS の 1 つ目。対象はツールごとに、Cargo ならサブコマンド、Python ならスクリプト）
    Tool { tool: String, target: String, args: String, env: Vec<(String, String)>, workdir: Option<String> },
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
            Kind::Tool { tool, .. } => tool_def(tool).map_or(tool.clone(), |t| t.1.to_string()),
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

/// nagi の構成のファイル
pub fn conf_path(root: &Path) -> PathBuf {
    root.join(".nagi").join("run.conf")
}

/// プロジェクトの構成を全部読む（nagi → IntelliJ の共有 → 作業状態の順。同じ名前は先のもの）。
/// 2 つ目は IntelliJ で選んでいた構成の名前
pub fn load(root: &Path, workspace_xmls: &[String]) -> (Vec<RunConfig>, Option<String>) {
    let mut out: Vec<RunConfig> = Vec::new();
    let mut push = |c: RunConfig| {
        if !out.iter().any(|x| x.name == c.name) {
            out.push(c);
        }
    };
    if let Ok(s) = std::fs::read_to_string(conf_path(root)) {
        parse_conf(&s).into_iter().for_each(&mut push);
    }
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
    (out, selected)
}

/// 種類（.nagi/run.conf の type の値, 画面に出す名前）
pub const KINDS: [(&str, &str); 16] = [
    ("python", "Python"),
    ("cargo", "Cargo"),
    ("go", "Go"),
    ("dotnet", ".NET"),
    ("node", "Node.js"),
    ("php", "PHP"),
    ("ruby", "Ruby"),
    ("make", "Make"),
    ("cmake", "CMake"),
    ("shell", "シェル"),
    ("maven", "Maven"),
    ("spring-boot", "Spring Boot"),
    ("application", "アプリケーション"),
    ("npm", "npm"),
    ("gradle", "Gradle"),
    ("url", "ブラウザ"),
];

/// 言語のツール（type の値, 画面の名前, 対象の欄の名前, 対象の既定値, 対象が無いと動かせないか）
pub const TOOLS: [(&str, &str, &str, &str, bool); 9] = [
    ("python", "Python", "スクリプト（-m 名前 でモジュール）", "", true),
    ("cargo", "Cargo", "コマンド（run・test・build など）", "run", false),
    ("go", "Go", "パッケージかファイル", ".", false),
    ("dotnet", ".NET", "プロジェクト（.csproj。空ならフォルダのもの）", "", false),
    ("node", "Node.js", "スクリプト", "", true),
    ("php", "PHP", "スクリプト", "", true),
    ("ruby", "Ruby", "スクリプト", "", true),
    ("make", "Make", "ターゲット", "", false),
    ("cmake", "CMake", "ターゲット（空なら全部）", "", false),
];

fn tool_def(id: &str) -> Option<&'static (&'static str, &'static str, &'static str, &'static str, bool)> {
    TOOLS.iter().find(|t| t.0 == id)
}

impl Kind {
    /// 種類の値（KINDS の 1 つ目。未対応は None）
    pub fn id(&self) -> Option<&'static str> {
        Some(match self {
            Kind::Shell { .. } => "shell",
            Kind::Maven { .. } => "maven",
            Kind::SpringBoot { .. } => "spring-boot",
            Kind::Application { .. } => "application",
            Kind::Npm { .. } => "npm",
            Kind::Gradle { .. } => "gradle",
            Kind::Url { .. } => "url",
            Kind::Tool { tool, .. } => return tool_def(tool).map(|t| t.0),
            Kind::Unsupported { .. } => return None,
        })
    }

    /// 種類の空の構成
    pub fn new(id: &str) -> Kind {
        let mut k = match id {
            "maven" => Kind::Maven { goals: Vec::new(), profiles: Vec::new(), options: String::new(), workdir: None, pom: None },
            "spring-boot" => Kind::SpringBoot { module: None, main_class: String::new(), vm: String::new(), args: String::new(), profiles: String::new(), env: Vec::new() },
            "application" => Kind::Application { module: None, main_class: String::new(), args: String::new(), env: Vec::new() },
            "npm" => Kind::Npm { package_json: None, command: String::new(), scripts: Vec::new(), args: String::new(), env: Vec::new() },
            "gradle" => Kind::Gradle { tasks: Vec::new(), args: String::new() },
            "url" => Kind::Url { url: String::new() },
            t if tool_def(t).is_some() => Kind::Tool { tool: t.to_string(), target: tool_def(t).unwrap().3.to_string(), args: String::new(), env: Vec::new(), workdir: None },
            _ => Kind::Shell { command: String::new(), workdir: None },
        };
        if let Kind::Npm { command, .. } = &mut k {
            *command = "run".into();
        }
        k
    }

    /// 画面の欄（キー, 名前, 値）。値はどれも 1 行の文字
    pub fn fields(&self) -> Vec<(&'static str, &'static str, String)> {
        let opt = |o: &Option<String>| o.clone().unwrap_or_default();
        match self {
            Kind::Shell { command, workdir } => vec![("command", "コマンド", command.clone()), ("dir", "作業フォルダ", opt(workdir))],
            Kind::Maven { goals, profiles, options, workdir, pom } => vec![
                ("goals", "ゴール", goals.join(" ")),
                ("profiles", "プロファイル", profiles.iter().map(|(n, on)| if *on { n.clone() } else { format!("!{n}") }).collect::<Vec<_>>().join(", ")),
                ("options", "オプション", options.clone()),
                ("dir", "作業フォルダ", opt(workdir)),
                ("pom", "pom のファイル", opt(pom)),
            ],
            Kind::SpringBoot { module, main_class, vm, args, profiles, env } => vec![
                ("module", "モジュール", opt(module)),
                ("main", "メインクラス", main_class.clone()),
                ("vm", "VM オプション", vm.clone()),
                ("args", "引数", args.clone()),
                ("profiles", "プロファイル", profiles.clone()),
                ("env", "環境変数", env_text(env)),
            ],
            Kind::Application { module, main_class, args, env } => {
                vec![("module", "モジュール", opt(module)), ("main", "メインクラス", main_class.clone()), ("args", "引数", args.clone()), ("env", "環境変数", env_text(env))]
            }
            Kind::Npm { package_json, command, scripts, args, env } => vec![
                ("package", "package.json", opt(package_json)),
                ("command", "コマンド", command.clone()),
                ("scripts", "スクリプト", scripts.join(" ")),
                ("args", "引数", args.clone()),
                ("env", "環境変数", env_text(env)),
            ],
            Kind::Gradle { tasks, args } => vec![("tasks", "タスク", tasks.join(" ")), ("args", "引数", args.clone())],
            Kind::Url { url } => vec![("url", "URL", url.clone())],
            Kind::Tool { tool, target, args, env, workdir } => vec![
                ("target", tool_def(tool).map_or("対象", |t| t.2), target.clone()),
                ("args", "引数", args.clone()),
                ("env", "環境変数", env_text(env)),
                ("dir", "作業フォルダ", opt(workdir)),
            ],
            Kind::Unsupported { .. } => Vec::new(),
        }
    }

    /// 欄の値を入れる（知らないキーは無視）
    pub fn set(&mut self, key: &str, value: &str) {
        let v = value.trim().to_string();
        let opt = || (!v.is_empty()).then(|| v.clone());
        let words = || v.split_whitespace().map(String::from).collect::<Vec<_>>();
        match (self, key) {
            (Kind::Shell { command, .. }, "command") => *command = v,
            (Kind::Shell { workdir, .. } | Kind::Maven { workdir, .. } | Kind::Tool { workdir, .. }, "dir") => *workdir = opt(),
            (Kind::Tool { target, .. }, "target") => *target = v,
            (Kind::Tool { args, .. }, "args") => *args = v,
            (Kind::Tool { env, .. }, "env") => *env = parse_env(&v),
            (Kind::Maven { goals, .. }, "goals") => *goals = words(),
            (Kind::Maven { profiles, .. }, "profiles") => {
                *profiles = v.split([',', ' ']).map(str::trim).filter(|p| !p.is_empty()).map(|p| match p.strip_prefix('!') {
                    Some(n) => (n.to_string(), false),
                    None => (p.to_string(), true),
                }).collect()
            }
            (Kind::Maven { options, .. }, "options") => *options = v,
            (Kind::Maven { pom, .. }, "pom") => *pom = opt(),
            (Kind::SpringBoot { module, .. } | Kind::Application { module, .. }, "module") => *module = opt(),
            (Kind::SpringBoot { main_class, .. } | Kind::Application { main_class, .. }, "main") => *main_class = v,
            (Kind::SpringBoot { vm, .. }, "vm") => *vm = v,
            (Kind::SpringBoot { args, .. } | Kind::Application { args, .. } | Kind::Npm { args, .. } | Kind::Gradle { args, .. }, "args") => *args = v,
            (Kind::SpringBoot { profiles, .. }, "profiles") => *profiles = v,
            (Kind::SpringBoot { env, .. } | Kind::Application { env, .. } | Kind::Npm { env, .. }, "env") => *env = parse_env(&v),
            (Kind::Npm { package_json, .. }, "package") => *package_json = opt(),
            (Kind::Npm { command, .. }, "command") => *command = v,
            (Kind::Npm { scripts, .. }, "scripts") => *scripts = words(),
            (Kind::Gradle { tasks, .. }, "tasks") => *tasks = words(),
            (Kind::Url { url }, "url") => *url = v,
            _ => {}
        }
    }
}

/// 環境変数の欄の文字（`A=1; B=2`）
fn env_text(env: &[(String, String)]) -> String {
    env.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ")
}

fn parse_env(s: &str) -> Vec<(String, String)> {
    s.split(';').filter_map(|p| p.split_once('=')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).filter(|(k, _)| !k.is_empty()).collect()
}

/// `.nagi/run.conf` を読む（`[名前]` の節と、節の前の `名前 = コマンド` の 1 行）
pub fn parse_conf(s: &str) -> Vec<RunConfig> {
    let mut out: Vec<RunConfig> = Vec::new();
    let mut cur: Option<(String, Vec<(String, String)>)> = None;
    let finish = |out: &mut Vec<RunConfig>, cur: Option<(String, Vec<(String, String)>)>| {
        if let Some((name, kv)) = cur {
            let ty = kv.iter().find(|(k, _)| k == "type").map_or("shell", |(_, v)| v.as_str());
            let mut kind = Kind::new(ty);
            for (k, v) in &kv {
                kind.set(k, v);
            }
            out.push(RunConfig { name, kind, from: "nagi" });
        }
    };
    for raw in s.lines() {
        let l = raw.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if let Some(name) = l.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
            finish(&mut out, cur.take());
            cur = Some((name.trim().to_string(), Vec::new()));
            continue;
        }
        let Some((k, v)) = l.split_once('=') else { continue };
        match &mut cur {
            Some((_, kv)) => kv.push((k.trim().to_string(), v.trim().to_string())),
            None => {
                // 手で書く短い形: 名前 = コマンド（# から行末は注釈）
                let c = v.split_once(" #").map_or(v, |x| x.0).trim();
                if !k.trim().is_empty() && !c.is_empty() {
                    out.push(RunConfig { name: k.trim().to_string(), kind: Kind::Shell { command: c.to_string(), workdir: None }, from: "nagi" });
                }
            }
        }
    }
    finish(&mut out, cur);
    out
}

/// nagi の構成を `.nagi/run.conf` の文字に（どれも節の形。空の欄は書かない）
pub fn to_conf(configs: &[RunConfig]) -> String {
    let mut s = String::from("# nagi の実行の構成（実行の構成の画面で直せる。手で書いてもよい）\n# [名前] の後に type = shell・maven・spring-boot・application・npm・gradle・url・python・cargo・go・dotnet・node・php・ruby・make・cmake と、その種類の項目\n");
    for c in configs {
        let Some(id) = c.kind.id() else { continue };
        s.push_str(&format!("\n[{}]\ntype = {id}\n", c.name));
        for (k, _, v) in c.kind.fields() {
            if !v.is_empty() {
                s.push_str(&format!("{k} = {v}\n"));
            }
        }
    }
    s
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
        // Python: スクリプトかモジュール
        "PythonConfigurationType" => {
            let module = option(body, "MODULE_MODE").as_deref() == Some("true");
            let script = s("SCRIPT_NAME");
            let script = script.strip_prefix("$PROJECT_DIR$/").unwrap_or(&script).to_string();
            tool("python", if module { format!("-m {script}") } else { script }, s("PARAMETERS"), envs(body), option(body, "WORKING_DIRECTORY").filter(|w| !w.is_empty()))
        }
        // RustRover・IntelliJ Rust: command に「run --package x」
        "CargoCommandRunConfiguration" => tool("cargo", s("command"), String::new(), envs(body), None),
        // GoLand: パッケージかファイル
        "GoApplicationRunConfiguration" => {
            let t = if option(body, "kind").as_deref() == Some("FILE") || attr(tags(body, "kind").next().unwrap_or(""), "value").as_deref() == Some("FILE") {
                tags(body, "filePath").next().and_then(|t| attr(t, "value")).unwrap_or_default()
            } else {
                tags(body, "package").next().and_then(|t| attr(t, "value")).unwrap_or_else(|| ".".into())
            };
            let t = t.strip_prefix("$PROJECT_DIR$/").unwrap_or(&t).to_string();
            tool("go", t, tags(body, "parameters").next().and_then(|t| attr(t, "value")).unwrap_or_default(), envs(body), None)
        }
        "RubyRunConfigurationType" => {
            let sp = s("SCRIPT_PATH");
            tool("ruby", sp.strip_prefix("$PROJECT_DIR$/").unwrap_or(&sp).to_string(), s("SCRIPT_ARGS"), envs(body), option(body, "WORK DIR").filter(|w| !w.is_empty()))
        }
        "JavascriptDebugType" => match attr(head, "uri") {
            Some(url) => Kind::Url { url },
            None => Kind::Unsupported { type_name: ty.to_string() },
        },
        _ => Kind::Unsupported { type_name: ty.to_string() },
    }
}

/// 開いているファイルをそのまま動かす構成（IntelliJ の「現在のファイル」）。動かし方の分からない拡張子は None
pub fn for_file(file: &Path, root: &Path) -> Option<RunConfig> {
    let ext = file.extension()?.to_str()?.to_ascii_lowercase();
    let t = match ext.as_str() {
        "py" | "pyw" => "python",
        "js" | "mjs" | "cjs" => "node",
        "php" => "php",
        "rb" => "ruby",
        "go" => "go",
        _ => return None,
    };
    let rel = file.strip_prefix(root).map_or_else(|_| file.display().to_string(), |r| r.to_string_lossy().replace('\\', "/"));
    let name = format!("今のファイル: {}", file.file_name()?.to_string_lossy());
    Some(RunConfig { name, kind: tool(t, rel, String::new(), Vec::new(), None), from: "file" })
}

fn tool(t: &str, target: String, args: String, env: Vec<(String, String)>, workdir: Option<String>) -> Kind {
    Kind::Tool { tool: t.into(), target, args, env, workdir }
}

/// 空白を含む引数は "" で囲む（もう囲んであればそのまま）
fn quote(s: &str) -> String {
    let s = s.trim();
    if s.contains(' ') && !s.starts_with('"') { format!("\"{s}\"") } else { s.to_string() }
}

/// 言語のツールのコマンド（WSL のプロジェクトでは Linux のコマンド名・パス）
fn tool_command(tool: &str, target: &str, args: &str, cwd: &Path, wsl: bool) -> nagi_core::Result<String> {
    let (t, a) = (target.trim(), args.trim());
    let def = tool_def(tool).ok_or_else(|| format!("知らないツールです: {tool}"))?;
    if def.4 && t.is_empty() {
        return Err(format!("{}が空です", def.2.split('（').next().unwrap_or(def.2)).into());
    }
    let tail = |c: String| if a.is_empty() { c } else { format!("{c} {a}") };
    Ok(match tool {
        "python" => {
            // 仮想環境（.venv・venv）があればその Python。-u で出力を溜めずに出す
            let venv = [".venv", "venv"].iter().find_map(|v| {
                let p = if wsl { cwd.join(v).join("bin").join("python") } else { cwd.join(v).join("Scripts").join("python.exe") };
                p.is_file().then(|| if wsl { format!("{v}/bin/python") } else { format!("{v}\\Scripts\\python.exe") })
            });
            let py = venv.unwrap_or_else(|| if wsl { "python3".into() } else { "python".into() });
            let t = match t.strip_prefix("-m ") {
                Some(m) => format!("-m {}", m.trim()),
                None => quote(t),
            };
            tail(format!("{py} -u {t}"))
        }
        "cargo" => {
            let sub = if t.is_empty() { "run" } else { t };
            // run・test・bench の引数はプログラムへ（-- の後）
            let first = sub.split_whitespace().next().unwrap_or("");
            if !a.is_empty() && matches!(first, "run" | "r" | "test" | "t" | "bench") && !sub.contains(" -- ") { format!("cargo {sub} -- {a}") } else { tail(format!("cargo {sub}")) }
        }
        "go" => tail(format!("go run {}", if t.is_empty() { "." } else { t })),
        "dotnet" => {
            let p = if t.is_empty() { String::new() } else { format!(" --project {}", quote(t)) };
            if a.is_empty() { format!("dotnet run{p}") } else { format!("dotnet run{p} -- {a}") }
        }
        "node" => tail(format!("node {}", quote(t))),
        "php" => tail(format!("php {}", quote(t))),
        "ruby" => {
            let bundle = if cwd.join("Gemfile").is_file() { "bundle exec " } else { "" };
            tail(format!("{bundle}ruby {}", quote(t)))
        }
        "make" => tail(format!("make {t}").trim().to_string()),
        "cmake" => {
            let target = if t.is_empty() { String::new() } else { format!(" --target {t}") };
            tail(format!("cmake -S . -B build && cmake --build build{target}"))
        }
        _ => unreachable!(),
    })
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
pub fn launch(cfg: &RunConfig, root: &Path) -> nagi_core::Result<Launch> {
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
            let wsl = crate::exec::Exec::in_dir(root).is_wsl();
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
        Kind::Tool { tool, target, args, env, workdir } => {
            let cwd = workdir.as_deref().map_or(root.to_path_buf(), |w| expand(w, root));
            let wsl = crate::exec::Exec::in_dir(root).is_wsl();
            cmd(tool_command(tool, target, args, &cwd, wsl)?, cwd, env.clone())
        }
        Kind::Unsupported { type_name } => Err(format!("この種類の構成はまだ動かせません（{type_name}）").into()),
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
        assert_eq!(v.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["サーバー", "core.lib インストール", "DemoApplication", "Angular CLI Server", "Angular Application"], "nagi の構成が先");
        let v: Vec<RunConfig> = v[1..].iter().cloned().chain(std::iter::once(v[0].clone())).collect();
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

    /// 節の形で書き出して読み直すと同じになる（IntelliJ の構成を nagi の構成として保存するときも）
    #[test]
    fn conf_round_trip() {
        let mut v = parse_xml(WS);
        v.extend(parse_xml(MAVEN));
        v.push(RunConfig { name: "掃除".into(), kind: Kind::Shell { command: "rmdir /s /q target".into(), workdir: Some("app".into()) }, from: "nagi" });
        let text = to_conf(&v);
        assert!(text.contains("[DemoApplication]\ntype = spring-boot\nmodule = rest.demo\nmain = com.example.DemoApplication\nvm = -Xmx512m\nenv = TZ=Asia/Tokyo\n"), "{text}");
        let back = parse_conf(&text);
        assert_eq!(back.len(), v.len());
        for (a, b) in v.iter().zip(&back) {
            assert_eq!((&a.name, &a.kind), (&b.name, &b.kind));
            assert_eq!(b.from, "nagi");
        }
        // 短い形と節を混ぜてもよい。URL の # は注釈ではない
        let mixed = parse_conf("dev = npm run dev  # 開発\n[docs]\ntype = url\nurl = http://localhost:4200/#/top\n");
        assert_eq!(mixed[0].kind, Kind::Shell { command: "npm run dev".into(), workdir: None });
        assert_eq!(mixed[1].kind, Kind::Url { url: "http://localhost:4200/#/top".into() });
    }

    /// 言語のツールの構成（コマンド・IntelliJ の Python・Go・Cargo・往復）
    #[test]
    fn tools() {
        let dir = std::env::temp_dir().join(format!("nagi-runtool-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".venv/Scripts")).unwrap();
        let run = |k: Kind| match launch(&RunConfig { name: "x".into(), kind: k, from: "nagi" }, &dir) {
            Ok(Launch::Command { cmd, .. }) => cmd,
            Ok(o) => panic!("{o:?}"),
            Err(e) => format!("Err: {e}"),
        };
        let t = |name: &str, target: &str, args: &str| tool(name, target.into(), args.into(), Vec::new(), None);
        assert_eq!(run(t("python", "app/main.py", "--port 80")), "python -u app/main.py --port 80");
        assert_eq!(run(t("python", "-m  http.server", "")), "python -u -m http.server");
        assert!(run(t("python", "", "")).starts_with("Err: スクリプト"));
        std::fs::write(dir.join(".venv/Scripts/python.exe"), "").unwrap();
        assert_eq!(run(t("python", "my app.py", "")), r#".venv\Scripts\python.exe -u "my app.py""#);
        assert_eq!(run(t("cargo", "run", "a b")), "cargo run -- a b");
        assert_eq!(run(t("cargo", "build --release", "")), "cargo build --release");
        assert_eq!(run(t("go", ".", "")), "go run .");
        assert_eq!(run(t("dotnet", "src/App/App.csproj", "x")), "dotnet run --project src/App/App.csproj -- x");
        assert_eq!(run(t("cmake", "app", "")), "cmake -S . -B build && cmake --build build --target app");
        assert_eq!(run(t("make", "", "")), "make");
        std::fs::write(dir.join("Gemfile"), "").unwrap();
        assert_eq!(run(t("ruby", "bin/app.rb", "")), "bundle exec ruby bin/app.rb");
        // IntelliJ（PyCharm・GoLand・RustRover）の構成
        let xml = r#"<component name="RunManager">
  <configuration name="main" type="PythonConfigurationType" factoryName="Python">
    <option name="SCRIPT_NAME" value="$PROJECT_DIR$/main.py" />
    <option name="PARAMETERS" value="-v" />
    <envs><env name="PYTHONUNBUFFERED" value="1" /></envs>
  </configuration>
  <configuration name="srv" type="GoApplicationRunConfiguration" factoryName="Go Application">
    <kind value="PACKAGE" />
    <package value="example.com/srv/cmd/srv" />
  </configuration>
  <configuration name="test" type="CargoCommandRunConfiguration" factoryName="Cargo Command">
    <option name="command" value="test --package nagi" />
  </configuration>
</component>"#;
        let v = parse_xml(xml);
        assert_eq!(v[0].kind, Kind::Tool { tool: "python".into(), target: "main.py".into(), args: "-v".into(), env: vec![("PYTHONUNBUFFERED".into(), "1".into())], workdir: None });
        assert_eq!(v[1].kind, t("go", "example.com/srv/cmd/srv", ""));
        assert_eq!(v[2].kind, t("cargo", "test --package nagi", ""));
        assert_eq!(v[2].kind_label(), "Cargo");
        let back = parse_conf(&to_conf(&v));
        assert_eq!(back.iter().map(|c| c.kind.clone()).collect::<Vec<_>>(), v.iter().map(|c| c.kind.clone()).collect::<Vec<_>>());
        assert_eq!(Kind::new("go"), t("go", ".", ""));
    }
}
