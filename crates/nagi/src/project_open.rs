//! IntelliJ・Eclipse のプロジェクトをそのまま開く（`:Project`、`:e フォルダ`、起動の引数のフォルダ、最近のプロジェクト）。
//!
//! 開くと、作業フォルダをプロジェクトの根にし、木・索引・Maven・git のパネルを作り直す。IntelliJ のプロジェクトなら、
//! 前に IntelliJ で開いていたファイルとカーソルの位置を復元する。Eclipse のワークスペースで、外に置いたプロジェクトが
//! あれば、どれを開くかを選ばせる。プロジェクトの設定は読むだけで書き換えない。

use std::path::{Path, PathBuf};

use nagi_workers::project::{self, Kind};

use crate::editor::{Editor, Mode};
use crate::finder::Source;

/// Nagi の設定フォルダのファイル（`%LOCALAPPDATA%\nagi\<name>`）
fn data_file(name: &str) -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("nagi").join(name))
}

/// 1 行 1 つのパスの一覧を読む（タブの後ろは付け足しの情報）
fn read_list(name: &str) -> Vec<(PathBuf, String)> {
    data_file(name)
        .and_then(|f| std::fs::read_to_string(f).ok())
        .map(|s| s.lines().filter(|l| !l.is_empty()).map(|l| l.split_once('\t').map_or((PathBuf::from(l), String::new()), |(a, b)| (PathBuf::from(a), b.to_string()))).collect())
        .unwrap_or_default()
}

fn write_list(name: &str, items: &[(PathBuf, String)]) {
    // 試験で開いた一時フォルダを、使っている人の記録に残さない
    if cfg!(test) {
        return;
    }
    let Some(file) = data_file(name) else { return };
    if let Some(d) = file.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let text: String = items.iter().map(|(p, x)| if x.is_empty() { format!("{}\n", p.display()) } else { format!("{}\t{x}\n", p.display()) }).collect();
    let _ = std::fs::write(&file, text);
}

fn same(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy())
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn remember_project(root: &Path) {
    let mut v = read_list("projects.txt");
    v.retain(|(p, _)| !same(p, root) && p.is_dir());
    v.insert(0, (root.to_path_buf(), now_ms().to_string()));
    v.truncate(100);
    write_list("projects.txt", &v);
    crate::jumplist::refresh();
    // 外していたものを開き直したら、一覧に戻す
    let mut h = read_list("hidden-projects.txt");
    if h.iter().any(|(p, _)| same(p, root)) {
        h.retain(|(p, _)| !same(p, root));
        write_list("hidden-projects.txt", &h);
    }
}

/// ピン留めを切り替える（付けたら true）
pub(crate) fn toggle_pin(path: &Path) -> bool {
    let mut v = read_list("pinned-projects.txt");
    let on = !v.iter().any(|(p, _)| same(p, path));
    if on {
        v.push((path.to_path_buf(), String::new()));
    } else {
        v.retain(|(p, _)| !same(p, path));
    }
    write_list("pinned-projects.txt", &v);
    on
}

/// 一覧から外す（IntelliJ などの履歴は書き換えず、Nagi の側で隠す）
pub(crate) fn hide_project(path: &Path) {
    let mut v = read_list("projects.txt");
    v.retain(|(p, _)| !same(p, path));
    write_list("projects.txt", &v);
    let mut p = read_list("pinned-projects.txt");
    p.retain(|(x, _)| !same(x, path));
    write_list("pinned-projects.txt", &p);
    let mut h = read_list("hidden-projects.txt");
    if !h.iter().any(|(x, _)| same(x, path)) {
        h.push((path.to_path_buf(), String::new()));
    }
    write_list("hidden-projects.txt", &h);
}

/// ランチャーの一覧の 1 行
#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub name: String,
    pub path: PathBuf,
    /// どこで開いたか（Nagi・IntelliJIdea・WebStorm・Eclipse）
    pub from: String,
    /// 最後に開いた時刻（ミリ秒、分からなければ 0）
    pub when: u64,
    pub pinned: bool,
    /// フォルダがもう無い
    pub missing: bool,
    /// 種類（Maven 3 モジュール・npm・Angular など）
    pub kind: String,
    /// git のブランチ（git でなければ None）
    pub branch: Option<String>,
}

/// 最近のプロジェクト（ピン留め → Nagi・IntelliJ・WebStorm・Eclipse を新しい順に。同じ場所は 1 つ、外したものは出さない）
pub(crate) fn recent_entries() -> Vec<Entry> {
    let pinned = read_list("pinned-projects.txt");
    let hidden = read_list("hidden-projects.txt");
    let mut raw: Vec<(PathBuf, String, u64)> = Vec::new();
    for (p, t) in read_list("projects.txt") {
        raw.push((p, "Nagi".into(), t.parse().unwrap_or(0)));
    }
    for r in project::recent_projects() {
        raw.push((r.path, r.from, r.when));
    }
    let mut out: Vec<Entry> = Vec::new();
    for (path, from, when) in raw {
        if hidden.iter().any(|(h, _)| same(h, &path)) {
            continue;
        }
        if let Some(e) = out.iter_mut().find(|e| same(&e.path, &path)) {
            // 同じ場所: 新しい方の時刻（どこで開いたかは Nagi より IDE の名前を残す）
            if when > e.when {
                e.when = when;
            }
            if e.from == "Nagi" {
                e.from = from;
            }
            continue;
        }
        out.push(entry(path, from, when, false));
    }
    for (p, _) in &pinned {
        match out.iter_mut().find(|e| same(&e.path, p)) {
            Some(e) => e.pinned = true,
            None => out.push(entry(p.clone(), "Nagi".into(), 0, true)),
        }
    }
    out.sort_by(|a, b| b.pinned.cmp(&a.pinned).then(b.when.cmp(&a.when)));
    out
}

fn entry(path: PathBuf, from: String, when: u64, pinned: bool) -> Entry {
    let name = path.file_name().map_or(path.display().to_string(), |n| n.to_string_lossy().into_owned());
    let missing = !path.is_dir();
    let kind = if missing { String::new() } else { kind_of(&path) };
    let branch = if missing { None } else { branch_of(&path) };
    Entry { name, path, from, when, pinned, missing, kind, branch }
}

/// プロジェクトの種類（ビルドの仕組みと、npm なら枠組み）
pub(crate) fn kind_of(dir: &Path) -> String {
    if let Ok(pom) = std::fs::read_to_string(dir.join("pom.xml")) {
        let n = pom.matches("<module>").count();
        return if n > 0 { format!("Maven {n} モジュール") } else { "Maven".into() };
    }
    if dir.join("build.gradle").is_file() || dir.join("build.gradle.kts").is_file() {
        return "Gradle".into();
    }
    if let Ok(pkg) = std::fs::read_to_string(dir.join("package.json")) {
        let fw = [("\"@angular/core\"", "Angular"), ("\"next\"", "Next.js"), ("\"svelte\"", "Svelte"), ("\"vue\"", "Vue"), ("\"react\"", "React")].iter().find(|(k, _)| pkg.contains(k)).map(|(_, v)| *v);
        return fw.map_or("npm".into(), |f| format!("npm・{f}"));
    }
    if dir.join("Cargo.toml").is_file() {
        return "Cargo".into();
    }
    if dir.join(".classpath").is_file() {
        return ".classpath".into();
    }
    if dir.join("pyproject.toml").is_file() || dir.join("requirements.txt").is_file() {
        return "Python".into();
    }
    if dir.join(".metadata").is_dir() {
        return "ワークスペース".into();
    }
    String::new()
}

/// git のブランチ（.git/HEAD を読むだけ）
pub(crate) fn branch_of(dir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(dir.join(".git").join("HEAD")).ok()?;
    let h = head.trim();
    Some(match h.strip_prefix("ref: refs/heads/") {
        Some(b) => b.to_string(),
        None => h.chars().take(7).collect(),
    })
}

/// 最近のプロジェクト（選択肢の表示, 場所）
pub(crate) fn recent_list() -> Vec<(String, PathBuf)> {
    recent_entries().into_iter().filter(|e| !e.missing).map(|e| (format!("{}  {}  {}", e.name, e.from, e.path.display()), e.path)).collect()
}

impl Editor {
    /// `:Project`（引数なし）・ManageRecentProjects: 最近のプロジェクトから選ぶ
    pub(crate) fn pick_project(&mut self) {
        self.open_launcher();
    }

    /// フォルダをプロジェクトとして開く
    pub(crate) fn open_project(&mut self, dir: PathBuf) {
        // /home/... ~/... は WSL のパスとして開く
        // （UNC の \\wsl.localhost\... や //server はそのまま）
        let s = dir.to_string_lossy().into_owned();
        let linux = (s.starts_with('/') && !s.starts_with("//")) || s.starts_with("~/");
        let dir = if linux { nagi_workers::wsl::resolve_typed(&s) } else { dir };
        let dir = std::path::absolute(&dir).unwrap_or(dir);
        if !dir.is_dir() {
            self.message = format!("フォルダではありません: {}", dir.display());
            return;
        }
        let p = project::detect(&dir);
        // Eclipse のワークスペースで外に置いたプロジェクトがあれば、どれを開くか選ばせる
        if p.kind == Kind::EclipseWorkspace && p.members.iter().any(|(_, m)| !m.starts_with(&p.root)) {
            let mut items = vec![format!("（ワークスペース全体）  {}", p.root.display())];
            let mut locs = vec![(p.root.clone(), 0, 0)];
            for (n, m) in &p.members {
                items.push(format!("{n}  Eclipse  {}", m.display()));
                locs.push((m.clone(), 0, 0));
            }
            self.open_finder(Source::Projects);
            if let Some(f) = &mut self.finder {
                f.set_projects(items, locs, true);
            }
            self.message = format!("Eclipse のワークスペース {}: 開くプロジェクトを選んでください", p.name);
            return;
        }
        self.switch_project(p);
    }

    /// 選んだワークスペースの中のプロジェクト（ワークスペース全体なら、外のものは選ばずにそのまま開く）
    pub(crate) fn open_project_direct(&mut self, dir: PathBuf) {
        let p = project::detect(&dir);
        self.switch_project(p);
    }

    fn switch_project(&mut self, p: project::Project) {
        if let Err(e) = std::env::set_current_dir(&p.root) {
            self.message = format!("{}: {e}", p.root.display());
            return;
        }
        remember_project(&p.root);
        // プロジェクトごとのパネルと索引を作り直す
        self.tree = None;
        self.nav = Default::default();
        self.maven = None;
        self.grep = None;
        self.commit = None;
        self.log = None;
        self.branch_popup = None;
        self.diags.clear();
        self.finder = None;
        self.mode = Mode::Normal;
        // 実行の構成は、選んでいたものを忘れて読み直す（ステータスバーに出す）
        self.run_selected = None;
        self.run_last = None;
        // 前に開いていたファイル（IntelliJ）。前面にあったものを最後に開く
        let mut files = project::open_files(&p.root);
        files.sort_by_key(|f| f.current);
        let restored = files.len();
        if files.is_empty() {
            // 別のプロジェクトのファイルを開いたままだと、木や索引がそちらを向くので、空のバッファにする
            let here = self.buf.path.as_deref().and_then(|b| std::path::absolute(b).ok());
            if here.is_none_or(|h| !h.starts_with(&p.root)) {
                self.new_empty_buffer();
            }
        }
        for f in files {
            self.edit_file(f.path.clone());
            if self.buf.path.as_deref().is_some_and(|b| b == f.path) {
                self.cursor.line = f.line.min(self.buf.line_count().saturating_sub(1));
                let len = self.buf.line(self.cursor.line).len();
                self.cursor.byte = f.col.min(len);
                self.clamp_normal();
                self.scroll_into_view();
            }
        }
        self.start_index();
        self.start_git();
        self.open_tree();
        // ファイルを復元したら本文へ（木は出したまま）
        if restored > 0 {
            self.mode = Mode::Normal;
        }
        let what = match p.kind {
            Kind::Plain => "プロジェクト".to_string(),
            k => format!("{} のプロジェクト", k.label()),
        };
        let ex = if p.excludes.is_empty() { String::new() } else { format!("、除外 {} か所", p.excludes.len()) };
        let files = if restored > 0 { format!("、前に開いていたファイル {restored} 件") } else { String::new() };
        self.message = format!("{what}「{}」を開きました{files}{ex}", p.name);
        if !cfg!(test) {
            self.load_run_configs();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    #[test]
    fn unc_is_not_linux_path() {
        let mut e = Editor::new(Buffer::from_str(""), None);
        e.open_project(PathBuf::from(r"\\wsl.localhost\no-such-distro\home\x"));
        assert!(e.message.contains(r"\\wsl.localhost\no-such-distro\home\x"), "UNC は変換しない: {}", e.message);
    }

    #[test]
    fn opens_intellij_project_and_restores_files() {
        let d = std::env::temp_dir().join(format!("nagi-open-proj-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(".idea")).unwrap();
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::create_dir_all(d.join("out")).unwrap();
        unsafe {
            std::env::set_var("NAGI_JETBRAINS_DIR", d.join("no-jetbrains"));
            std::env::set_var("NAGI_INDEX_DIR", d.join("cache"));
        }
        std::fs::write(d.join(".idea/misc.xml"), "<project><component><output url=\"file://$PROJECT_DIR$/out\" /></component></project>").unwrap();
        std::fs::write(d.join("src/A.java"), "class A {\n  void run() {}\n}\n").unwrap();
        std::fs::write(d.join("out/A.java"), "class A {}\n").unwrap();
        std::fs::write(
            d.join(".idea/workspace.xml"),
            "<project><component name=\"FileEditorManager\"><leaf><file current-in-tab=\"true\"><entry file=\"file://$PROJECT_DIR$/src/A.java\"><provider><state><caret line=\"1\" column=\"7\" /></state></provider></entry></file></leaf></component></project>",
        )
        .unwrap();
        let cwd = std::env::current_dir().unwrap();
        let mut e = Editor::new(Buffer::from_str(""), None);
        e.set_screen(120, 40);
        e.open_project(d.clone());
        let opened = std::env::current_dir().unwrap();
        std::env::set_current_dir(&cwd).unwrap();
        assert_eq!(std::path::absolute(&opened).unwrap(), std::path::absolute(&d).unwrap());
        assert!(e.buf.path.as_ref().unwrap().ends_with("A.java"), "{:?} {}", e.buf.path, e.message);
        assert_eq!((e.cursor.line, e.cursor.byte), (1, 7));
        assert!(e.message.contains("IntelliJ"), "{}", e.message);
        assert!(e.tree.is_some(), "木を出す");
        // 索引は出力先（out）を見ない
        for _ in 0..1000 {
            e.poll_index();
            if e.nav.index.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let idx = e.nav.index.as_ref().expect("索引");
        assert_eq!(idx.lookup("A").len(), 1, "out/A.java は除外");
    }
}
