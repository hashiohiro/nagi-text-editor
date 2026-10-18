//! IntelliJ・Eclipse のプロジェクトをそのまま開くための読み取り（書き換えはしない）。
//!
//! - プロジェクトの根: `.git` か `.idea` のあるところ。無ければ `.project`・`pom.xml` などのあるところ（Maven の親へ上がる）
//! - 除外するフォルダ: IntelliJ の `.iml` の excludeFolder と出力先（`misc.xml`）、Eclipse の `.classpath` の出力先
//! - 前に開いていたファイル: IntelliJ の作業状態（`%APPDATA%\JetBrains\<製品>\workspace\<ID>.xml`、古いものは `.idea/workspace.xml`）
//! - 最近のプロジェクト: JetBrains の各製品の `recentProjects.xml`、Eclipse の最近のワークスペース
//! - Eclipse のワークスペース（`.metadata` のあるフォルダ）: 中のプロジェクト（外に置いたものも `.location` から）

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    IntelliJ,
    Eclipse,
    EclipseWorkspace,
    /// 設定の無いフォルダ（git・Maven・npm など）
    Plain,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::IntelliJ => "IntelliJ",
            Kind::Eclipse => "Eclipse",
            Kind::EclipseWorkspace => "Eclipse ワークスペース",
            Kind::Plain => "フォルダ",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Project {
    pub root: PathBuf,
    pub name: String,
    pub kind: Kind,
    /// 除外するフォルダ（絶対パス）
    pub excludes: Vec<PathBuf>,
    /// Eclipse のワークスペースの中のプロジェクト（名前, 場所）
    pub members: Vec<(String, PathBuf)>,
}

/// 前に開いていたファイル
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenFile {
    pub path: PathBuf,
    /// 0 始まり
    pub line: usize,
    pub col: usize,
    /// 前面にあったもの
    pub current: bool,
}

/// 最近のプロジェクト
#[derive(Clone, Debug)]
pub struct Recent {
    pub path: PathBuf,
    /// どこで開いたか（IntelliJIdea、WebStorm、Eclipse など）
    pub from: String,
    /// 最後に使った時刻（ミリ秒。分からなければ 0）
    pub when: u64,
}

/// `path`（ファイルかフォルダ）を含むプロジェクトの根
pub fn root_of(path: &Path) -> Option<PathBuf> {
    let start = if path.is_dir() { path } else { path.parent()? };
    if let Some(r) = start.ancestors().find(|d| d.join(".git").exists() || d.join(".idea").is_dir()) {
        return Some(r.to_path_buf());
    }
    let build = |d: &Path| ["pom.xml", "build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts"].iter().any(|f| d.join(f).is_file());
    let marker = |d: &Path| d.join(".project").is_file() || build(d) || d.join("package.json").is_file() || d.join("Cargo.toml").is_file();
    let mut r = start.ancestors().find(|d| marker(d))?.to_path_buf();
    // Maven・Gradle の子モジュールなら親へ
    while let Some(p) = r.parent().filter(|p| build(p)) {
        r = p.to_path_buf();
    }
    Some(r)
}

/// フォルダのプロジェクトの情報
pub fn detect(dir: &Path) -> Project {
    let root = dir.to_path_buf();
    let dir_name = root.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
    let mut excludes = Vec::new();
    let mut members = Vec::new();
    let kind = if root.join(".idea").is_dir() {
        Kind::IntelliJ
    } else if root.join(".metadata").is_dir() {
        Kind::EclipseWorkspace
    } else if root.join(".project").is_file() {
        Kind::Eclipse
    } else {
        Kind::Plain
    };
    let mut name = dir_name.clone();
    if kind == Kind::IntelliJ {
        if let Ok(n) = std::fs::read_to_string(root.join(".idea").join(".name")) {
            if !n.trim().is_empty() {
                name = n.trim().to_string();
            }
        }
        excludes.extend(intellij_excludes(&root));
    }
    if kind == Kind::Eclipse {
        if let Some(n) = eclipse_name(&root) {
            name = n;
        }
    }
    if kind == Kind::EclipseWorkspace {
        members = workspace_members(&root);
        excludes.push(root.join(".metadata"));
    }
    // Eclipse の出力先（根と、その下のモジュール・ワークスペースのプロジェクト）
    let mut eclipse_dirs: Vec<PathBuf> = vec![root.clone()];
    if let Ok(rd) = std::fs::read_dir(&root) {
        eclipse_dirs.extend(rd.flatten().map(|e| e.path()).filter(|p| p.join(".classpath").is_file()));
    }
    eclipse_dirs.extend(members.iter().map(|(_, p)| p.clone()));
    for d in eclipse_dirs {
        excludes.extend(eclipse_outputs(&d));
    }
    excludes.sort();
    excludes.dedup();
    Project { root, name, kind, excludes, members }
}

/// IntelliJ の除外フォルダ: 各モジュールの .iml の excludeFolder と、プロジェクトの出力先
fn intellij_excludes(root: &Path) -> Vec<PathBuf> {
    let idea = root.join(".idea");
    let mut out = Vec::new();
    let mut imls: Vec<PathBuf> = Vec::new();
    if let Ok(m) = std::fs::read_to_string(idea.join("modules.xml")) {
        for fp in attrs(&m, "module", "filepath") {
            imls.push(PathBuf::from(fp.replace("$PROJECT_DIR$", &root.to_string_lossy())));
        }
    }
    for iml in imls {
        let Ok(s) = std::fs::read_to_string(&iml) else { continue };
        // .idea の中の .iml の $MODULE_DIR$ はプロジェクトの根として扱う
        let mdir = match iml.parent() {
            Some(p) if p == idea => root.to_path_buf(),
            Some(p) => p.to_path_buf(),
            None => continue,
        };
        for url in attrs(&s, "excludeFolder", "url") {
            if let Some(p) = file_url(&url, root, &mdir) {
                out.push(p);
            }
        }
    }
    if let Ok(m) = std::fs::read_to_string(idea.join("misc.xml")) {
        for url in attrs(&m, "output", "url") {
            if let Some(p) = file_url(&url, root, root) {
                out.push(p);
            }
        }
    }
    out
}

/// Eclipse の .project の名前
fn eclipse_name(dir: &Path) -> Option<String> {
    let s = std::fs::read_to_string(dir.join(".project")).ok()?;
    let i = s.find("<name>")? + 6;
    let j = s[i..].find("</name>")? + i;
    Some(s[i..j].trim().to_string()).filter(|n| !n.is_empty())
}

/// Eclipse の .classpath の出力先（target/classes → target、bin → bin）
fn eclipse_outputs(dir: &Path) -> Vec<PathBuf> {
    let Ok(s) = std::fs::read_to_string(dir.join(".classpath")) else { return Vec::new() };
    let mut out: Vec<PathBuf> = Vec::new();
    for tag in s.split("<classpathentry").skip(1) {
        let tag = &tag[..tag.find('>').unwrap_or(tag.len())];
        let mut outs: Vec<String> = Vec::new();
        if attr(tag, "kind").as_deref() == Some("output") {
            outs.extend(attr(tag, "path"));
        }
        outs.extend(attr(tag, "output"));
        for o in outs {
            if let Some(first) = o.split(['/', '\\']).find(|s| !s.is_empty()) {
                out.push(dir.join(first));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Eclipse のワークスペースの中のプロジェクト（直下の .project と、.metadata に記録された外の場所）
fn workspace_members(ws: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(ws) {
        for e in rd.flatten() {
            let p = e.path();
            if p.join(".project").is_file() {
                out.push((eclipse_name(&p).unwrap_or_else(|| e.file_name().to_string_lossy().into_owned()), p));
            }
        }
    }
    let projects = ws.join(".metadata").join(".plugins").join("org.eclipse.core.resources").join(".projects");
    if let Ok(rd) = std::fs::read_dir(projects) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if out.iter().any(|(n, _)| *n == name) {
                continue;
            }
            // .location は独自の形式。中の "URI//file:/C:/..." を読む
            let Ok(b) = std::fs::read(e.path().join(".location")) else { continue };
            let s = String::from_utf8_lossy(&b);
            if let Some(i) = s.find("URI//file:") {
                let rest: String = s[i + 10..].chars().take_while(|c| !c.is_control()).collect();
                let path = percent_decode(rest.trim_start_matches('/'));
                let p = PathBuf::from(path.replace('/', std::path::MAIN_SEPARATOR_STR));
                if p.join(".project").is_file() {
                    out.push((name, p));
                }
            }
        }
    }
    out.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    out
}

/// IntelliJ の作業状態のファイルの中身（設定フォルダの workspace\<ID>.xml と .idea/workspace.xml の、あるもの全部。新しい形が先）
pub fn workspace_xmls(root: &Path) -> Vec<String> {
    let mut best: Option<(u64, PathBuf)> = None;
    for cfg in jetbrains_dirs() {
        let Ok(s) = std::fs::read_to_string(cfg.join("options").join("recentProjects.xml")) else { continue };
        for e in recent_entries(&s) {
            if same_path(&e.0, root) {
                if let Some(id) = e.2 {
                    let f = cfg.join("workspace").join(format!("{id}.xml"));
                    if f.is_file() && best.as_ref().is_none_or(|b| e.1 > b.0) {
                        best = Some((e.1, f));
                    }
                }
            }
        }
    }
    [best.map(|b| b.1), Some(root.join(".idea").join("workspace.xml"))].into_iter().flatten().filter_map(|f| std::fs::read_to_string(f).ok()).collect()
}

/// 前に開いていたファイル（IntelliJ の作業状態から）
pub fn open_files(root: &Path) -> Vec<OpenFile> {
    // 新しい IntelliJ: 設定フォルダの workspace\<ID>.xml（ID は recentProjects.xml に）
    let mut best: Option<(u64, PathBuf)> = None;
    for cfg in jetbrains_dirs() {
        let Ok(s) = std::fs::read_to_string(cfg.join("options").join("recentProjects.xml")) else { continue };
        for e in recent_entries(&s) {
            if same_path(&e.0, root) {
                if let Some(id) = e.2 {
                    let f = cfg.join("workspace").join(format!("{id}.xml"));
                    if f.is_file() && best.as_ref().is_none_or(|b| e.1 > b.0) {
                        best = Some((e.1, f));
                    }
                }
            }
        }
    }
    let mut files = best.and_then(|(_, f)| std::fs::read_to_string(f).ok()).map(|s| editor_files(&s, root)).unwrap_or_default();
    if files.is_empty() {
        if let Ok(s) = std::fs::read_to_string(root.join(".idea").join("workspace.xml")) {
            files = editor_files(&s, root);
        }
    }
    files.retain(|f| f.path.is_file());
    files
}

/// FileEditorManager の中の、開いていたファイルとカーソル
fn editor_files(xml: &str, root: &Path) -> Vec<OpenFile> {
    let Some(i) = xml.find("name=\"FileEditorManager\"") else { return Vec::new() };
    let end = xml[i..].find("</component>").map_or(xml.len(), |j| i + j);
    let comp = &xml[i..end];
    let mut out = Vec::new();
    for part in comp.split("<file").skip(1) {
        let current = part.starts_with(" current-in-tab=\"true\"") || part[..part.find('>').unwrap_or(0)].contains("current-in-tab=\"true\"");
        let Some(url) = attr_in(part, "entry", "file") else { continue };
        let Some(path) = file_url(&url, root, root) else { continue };
        let line = attr_in(part, "caret", "line").and_then(|v| v.parse().ok()).unwrap_or(0);
        let col = attr_in(part, "caret", "column").and_then(|v| v.parse().ok()).unwrap_or(0);
        out.push(OpenFile { path, line, col, current });
    }
    out
}

/// 最近のプロジェクト（JetBrains の各製品と Eclipse）。新しい順、同じ場所は 1 つ
pub fn recent_projects() -> Vec<Recent> {
    let mut out: Vec<Recent> = Vec::new();
    for cfg in jetbrains_dirs() {
        let Ok(s) = std::fs::read_to_string(cfg.join("options").join("recentProjects.xml")) else { continue };
        let product: String = cfg.file_name().map_or(String::new(), |n| n.to_string_lossy().trim_end_matches(|c: char| c.is_ascii_digit() || c == '.').to_string());
        for (path, when, _) in recent_entries(&s) {
            out.push(Recent { path, from: product.clone(), when });
        }
    }
    for ws in eclipse_recent_workspaces() {
        out.push(Recent { path: ws, from: "Eclipse".into(), when: 0 });
    }
    out.retain(|r| r.path.is_dir());
    out.sort_by(|a, b| b.when.cmp(&a.when));
    let mut seen: Vec<String> = Vec::new();
    out.retain(|r| {
        let k = r.path.to_string_lossy().to_lowercase();
        !seen.contains(&k) && {
            seen.push(k);
            true
        }
    });
    out
}

/// JetBrains の製品ごとの設定フォルダ（`%APPDATA%\JetBrains\<製品><版>`）
fn jetbrains_dirs() -> Vec<PathBuf> {
    let base = match std::env::var_os("NAGI_JETBRAINS_DIR") {
        Some(d) => PathBuf::from(d),
        None => match std::env::var_os("APPDATA") {
            Some(a) => PathBuf::from(a).join("JetBrains"),
            None => return Vec::new(),
        },
    };
    let mut v: Vec<PathBuf> = std::fs::read_dir(base).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.join("options").join("recentProjects.xml").is_file()).collect();
    v.sort();
    v
}

/// recentProjects.xml の (場所, 最後に使った時刻, 作業状態の ID)
fn recent_entries(xml: &str) -> Vec<(PathBuf, u64, Option<String>)> {
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let mut out = Vec::new();
    for part in xml.split("<entry key=\"").skip(1) {
        let Some(q) = part.find('"') else { continue };
        let key = part[..q].replace("$USER_HOME$", &home);
        let body = &part[..part.find("</entry>").unwrap_or(part.len())];
        let when = body.find("\"activationTimestamp\" value=\"").and_then(|i| body[i + 29..].split('"').next()?.parse().ok()).unwrap_or(0);
        let id = attr_in(body, "RecentProjectMetaInfo", "projectWorkspaceId");
        out.push((PathBuf::from(key.replace('/', std::path::MAIN_SEPARATOR_STR)), when, id));
    }
    out
}

/// Eclipse の最近のワークスペース（`configuration\.settings\org.eclipse.ui.ide.prefs` の RECENT_WORKSPACES）
fn eclipse_recent_workspaces() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(h) = std::env::var_os("USERPROFILE") {
        let h = PathBuf::from(h);
        roots.extend([h.join(".eclipse"), h.join("eclipse"), h.join("pleiades")]);
    }
    roots.extend([PathBuf::from("C:\\pleiades"), PathBuf::from("C:\\eclipse")]);
    let mut prefs = Vec::new();
    for r in roots {
        find_named(&r, "org.eclipse.ui.ide.prefs", 6, &mut prefs);
    }
    let mut out = Vec::new();
    for p in prefs {
        let Ok(s) = std::fs::read_to_string(&p) else { continue };
        for line in s.lines() {
            if let Some(v) = line.strip_prefix("RECENT_WORKSPACES=") {
                // \n で区切り、: と \ はエスケープされている
                for w in v.split("\\n") {
                    let w = w.replace("\\:", ":").replace("\\\\", "\\");
                    if !w.is_empty() {
                        out.push(PathBuf::from(w));
                    }
                }
            }
        }
    }
    out
}

fn find_named(dir: &Path, name: &str, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if e.file_name().to_string_lossy() == name {
            out.push(p);
        } else if depth > 0 && e.file_type().is_ok_and(|t| t.is_dir()) {
            find_named(&p, name, depth - 1, out);
        }
    }
}

/// `file://$PROJECT_DIR$/a/b` などを絶対パスに
fn file_url(url: &str, root: &Path, module: &Path) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let s = rest.replace("$PROJECT_DIR$", &root.to_string_lossy()).replace("$MODULE_DIR$", &module.to_string_lossy()).replace("$USER_HOME$", &std::env::var("USERPROFILE").unwrap_or_default());
    if s.contains('$') {
        return None;
    }
    let mut p = PathBuf::new();
    for seg in s.replace('\\', "/").split('/') {
        match seg {
            "" | "." if p.as_os_str().is_empty() && seg.is_empty() => {}
            "." => {}
            ".." => {
                p.pop();
            }
            s if p.as_os_str().is_empty() && s.ends_with(':') => p.push(format!("{s}\\")),
            s => p.push(s),
        }
    }
    // Windows 以外（試験）で先頭の / が消えないように
    if s.starts_with('/') && !cfg!(windows) {
        p = Path::new("/").join(p);
    }
    Some(p)
}

/// タグ `tag` の属性 `name` の値をすべて
fn attrs(xml: &str, tag: &str, name: &str) -> Vec<String> {
    xml.split(&format!("<{tag} ")).skip(1).filter_map(|t| attr(&t[..t.find('>').unwrap_or(t.len())], name)).collect()
}

/// 最初のタグ `tag` の属性 `name`
fn attr_in(xml: &str, tag: &str, name: &str) -> Option<String> {
    attrs(xml, tag, name).into_iter().next()
}

/// タグの中身（`a="x" b="y"`）から属性の値
fn attr(tag: &str, name: &str) -> Option<String> {
    let mut from = 0;
    while let Some(i) = tag[from..].find(&format!("{name}=\"")).map(|i| i + from) {
        from = i + name.len() + 2;
        if i > 0 && !tag.as_bytes()[i - 1].is_ascii_whitespace() {
            continue;
        }
        let v = &tag[from..];
        return Some(unescape(&v[..v.find('"')?]));
    }
    None
}

fn unescape(s: &str) -> String {
    s.replace("&quot;", "\"").replace("&lt;", "<").replace("&gt;", ">").replace("&apos;", "'").replace("&amp;", "&")
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn same_path(a: &Path, b: &Path) -> bool {
    let n = |p: &Path| p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase();
    n(a) == n(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nagi-proj-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn intellij_project() {
        let d = tmp("idea");
        std::fs::create_dir_all(d.join(".idea")).unwrap();
        std::fs::create_dir_all(d.join("core/src")).unwrap();
        std::fs::write(d.join(".idea/.name"), "MyApp\n").unwrap();
        std::fs::write(d.join(".idea/modules.xml"), "<project><component name=\"ProjectModuleManager\"><modules>\n<module fileurl=\"file://$PROJECT_DIR$/core/core.iml\" filepath=\"$PROJECT_DIR$/core/core.iml\" />\n</modules></component></project>").unwrap();
        std::fs::write(d.join("core/core.iml"), "<module><component><content url=\"file://$MODULE_DIR$\"><excludeFolder url=\"file://$MODULE_DIR$/target\" /></content></component></module>").unwrap();
        std::fs::write(d.join(".idea/misc.xml"), "<project><component name=\"ProjectRootManager\"><output url=\"file://$PROJECT_DIR$/out\" /></component></project>").unwrap();
        std::fs::write(d.join("core/src/A.java"), "class A {}\n").unwrap();
        std::fs::write(
            d.join(".idea/workspace.xml"),
            "<project><component name=\"FileEditorManager\"><leaf>\n<file>\n<entry file=\"file://$PROJECT_DIR$/core/src/A.java\"><provider selected=\"true\"><state><caret line=\"0\" column=\"6\" /></state></provider></entry>\n</file>\n<file current-in-tab=\"true\"><entry file=\"file://$PROJECT_DIR$/core/src/A.java\"><provider><state><caret line=\"0\" /></state></provider></entry></file>\n</leaf></component></project>",
        )
        .unwrap();
        // 作業状態は .idea/workspace.xml から（試験では JetBrains の設定フォルダを見ない）
        unsafe { std::env::set_var("NAGI_JETBRAINS_DIR", d.join("no-jetbrains")) };
        let p = detect(&d);
        assert_eq!((p.kind, p.name.as_str()), (Kind::IntelliJ, "MyApp"));
        assert!(p.excludes.contains(&d.join("core").join("target")), "{:?}", p.excludes);
        assert!(p.excludes.contains(&d.join("out")), "{:?}", p.excludes);
        let f = open_files(&d);
        assert_eq!(f.len(), 2);
        assert_eq!((f[0].line, f[0].col, f[0].current), (0, 6, false));
        assert!(f[1].current);
        assert_eq!(f[0].path, d.join("core").join("src").join("A.java"));
        // ファイルからの根: .idea のあるところ
        assert_eq!(root_of(&d.join("core/src/A.java")), Some(d.clone()));
    }

    #[test]
    fn eclipse_project_and_workspace() {
        let ws = tmp("ws");
        let p = ws.join("shop");
        std::fs::create_dir_all(p.join("src")).unwrap();
        std::fs::write(p.join(".project"), "<projectDescription><name>shop-core</name><comment></comment></projectDescription>").unwrap();
        std::fs::write(p.join(".classpath"), "<classpath>\n<classpathentry kind=\"src\" output=\"target/classes\" path=\"src\"/>\n<classpathentry kind=\"output\" path=\"bin\"/>\n</classpath>").unwrap();
        let d = detect(&p);
        assert_eq!((d.kind, d.name.as_str()), (Kind::Eclipse, "shop-core"));
        assert_eq!(d.excludes, vec![p.join("bin"), p.join("target")]);
        // ワークスペース: 直下のプロジェクトと、.metadata に記録された外のプロジェクト
        let outside = tmp("outside");
        std::fs::write(outside.join(".project"), "<projectDescription><name>ext</name></projectDescription>").unwrap();
        let loc = ws.join(".metadata/.plugins/org.eclipse.core.resources/.projects/ext");
        std::fs::create_dir_all(&loc).unwrap();
        let uri = format!("URI//file:/{}", outside.to_string_lossy().replace('\\', "/").replace(' ', "%20"));
        let mut bytes = vec![0x40u8, 0xb1, 0x8b, 0x81, 0x23, 0xbc, 0x00, 0x14, 0x1a, 0x25, 0x96, 0xe7, 0xa3, 0x93, 0xbe, 0x1e, 0x00, uri.len() as u8];
        bytes.extend(uri.as_bytes());
        bytes.extend([0u8, 0, 0, 0]);
        std::fs::write(loc.join(".location"), bytes).unwrap();
        let w = detect(&ws);
        assert_eq!(w.kind, Kind::EclipseWorkspace);
        let names: Vec<&str> = w.members.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["ext", "shop-core"]);
        assert!(w.members[0].1.ends_with(outside.file_name().unwrap()), "{:?}", w.members);
        assert!(w.excludes.contains(&ws.join(".metadata")));
        // Maven の子モジュールのファイル → 親の pom のあるところ（.git も .idea も無いとき）
        let m = tmp("maven");
        std::fs::create_dir_all(m.join("child/src")).unwrap();
        std::fs::write(m.join("pom.xml"), "<project/>").unwrap();
        std::fs::write(m.join("child/pom.xml"), "<project/>").unwrap();
        std::fs::write(m.join("child/.project"), "<projectDescription><name>child</name></projectDescription>").unwrap();
        assert_eq!(root_of(&m.join("child/src")), Some(m.clone()));
    }

    #[test]
    fn recent_xml() {
        let xml = "<application><component name=\"RecentProjectsManager\"><option name=\"additionalInfo\"><map>\n<entry key=\"$USER_HOME$/IdeaProjects/env\">\n<value><RecentProjectMetaInfo frameTitle=\"env\" projectWorkspaceId=\"3AR2\"><option name=\"activationTimestamp\" value=\"1772868151143\" /></RecentProjectMetaInfo></value>\n</entry>\n<entry key=\"C:/work/old\"><value><RecentProjectMetaInfo></RecentProjectMetaInfo></value></entry>\n</map></option></component></application>";
        let e = recent_entries(xml);
        assert_eq!(e.len(), 2);
        assert!(e[0].0.to_string_lossy().ends_with(&format!("IdeaProjects{}env", std::path::MAIN_SEPARATOR)));
        assert_eq!((e[0].1, e[0].2.as_deref()), (1772868151143, Some("3AR2")));
        assert_eq!((e[1].1, e[1].2.as_deref()), (0, None));
    }
}
