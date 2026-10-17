//! IntelliJ・Eclipse のプロジェクトをそのまま開く（`:Project`、`:e フォルダ`、起動の引数のフォルダ、最近のプロジェクト）。
//!
//! 開くと、作業フォルダをプロジェクトの根にし、木・索引・Maven・git のパネルを作り直す。IntelliJ のプロジェクトなら、
//! 前に IntelliJ で開いていたファイルとカーソルの位置を復元する。Eclipse のワークスペースで、外に置いたプロジェクトが
//! あれば、どれを開くかを選ばせる。プロジェクトの設定は読むだけで書き換えない。

use std::path::{Path, PathBuf};

use nagi_workers::project::{self, Kind};

use crate::editor::{Editor, Mode};
use crate::finder::Source;

/// Nagi で開いたプロジェクトの記録（`%LOCALAPPDATA%\nagi\projects.txt`、新しい順）
fn projects_file() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("nagi").join("projects.txt"))
}

fn remember_project(root: &Path) {
    // 試験で開いた一時フォルダを、使っている人の記録に残さない
    if cfg!(test) {
        return;
    }
    let Some(file) = projects_file() else { return };
    let me = root.to_string_lossy().into_owned();
    let old = std::fs::read_to_string(&file).unwrap_or_default();
    let mut lines = vec![me.clone()];
    lines.extend(old.lines().filter(|l| !l.eq_ignore_ascii_case(&me) && !l.is_empty() && Path::new(l).is_dir()).take(99).map(String::from));
    if let Some(d) = file.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(&file, lines.join("\n") + "\n");
}

/// 最近のプロジェクト（Nagi で開いたもの → IntelliJ・WebStorm など → Eclipse）: (表示, 場所)
pub(crate) fn recent_list() -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    let mut push = |p: PathBuf, from: &str| {
        if p.is_dir() && !out.iter().any(|(_, q)| q.to_string_lossy().eq_ignore_ascii_case(&p.to_string_lossy())) {
            let name = p.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
            out.push((format!("{name}  {from}  {}", p.display()), p));
        }
    };
    if let Some(s) = projects_file().and_then(|f| std::fs::read_to_string(f).ok()) {
        for l in s.lines().filter(|l| !l.is_empty()) {
            push(PathBuf::from(l), "Nagi");
        }
    }
    for r in project::recent_projects() {
        push(r.path, &r.from);
    }
    out
}

impl Editor {
    /// `:Project`（引数なし）・ManageRecentProjects: 最近のプロジェクトから選ぶ
    pub(crate) fn pick_project(&mut self) {
        self.open_finder(Source::Projects);
    }

    /// フォルダをプロジェクトとして開く
    pub(crate) fn open_project(&mut self, dir: PathBuf) {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

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
