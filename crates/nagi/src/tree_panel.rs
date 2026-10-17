//! ファイルツリー（左にドッキングするパネル）。アイコンは使わず、字下げと `▸` `▾` だけで木を表す。
//!
//! フォルダの中身は裏のスレッドで読み（UI スレッドではファイルを読まない）、届いたら展開する。
//! 開いたときは、今のファイルまでのフォルダを順に展開してそのファイルを選ぶ。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use nagi_vim::Key;
use nagi_workers::files::DirList;

use crate::editor::{Editor, Effect, Mode};
use crate::panes::Rect;

/// 画面に出す 1 行
#[derive(Clone, Debug)]
pub struct Row {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub dir: bool,
}

pub struct TreePanel {
    pub root: PathBuf,
    /// 読み終えたフォルダの中身
    children: HashMap<PathBuf, Vec<(String, bool)>>,
    /// 展開しているフォルダ
    expanded: std::collections::HashSet<PathBuf>,
    /// 画面に出す行（展開の状態から作り直す）
    pub rows: Vec<Row>,
    pub selected: usize,
    pub scroll: usize,
    tx: Sender<DirList>,
    rx: Receiver<DirList>,
    /// 読んでいる途中のフォルダ
    loading: std::collections::HashSet<PathBuf>,
    /// 開いたときに選びたいファイル（そこまでのフォルダが届くたびに展開を進める）
    reveal: Option<PathBuf>,
    pending_g: bool,
    pending_w: bool,
}

/// 左の欄（プロジェクト・コミット）の幅（36 列。狭い画面では 1/3 まで）
pub fn tree_width(cols: usize) -> usize {
    36.min(cols / 3).max(10)
}

/// 左の欄のタブ（IntelliJ の左のツールウィンドウと同じく、どちらか 1 つを出す）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LeftTab {
    #[default]
    Project,
    Commit,
}

/// タブの見出し（1 行目に並べる）
pub const TABS: [(&str, LeftTab); 2] = [(" プロジェクト ", LeftTab::Project), (" コミット ", LeftTab::Commit)];

/// 左のストライプのラベル（縦書き。見出し, タブ, 上端の行）
pub const STRIPE: [(&str, LeftTab, usize); 2] = [("プロジェクト", LeftTab::Project, 0), ("コミット", LeftTab::Commit, 7)];

/// 左のストライプの `row` にあるラベル
pub fn stripe_at(row: usize) -> Option<LeftTab> {
    STRIPE.iter().find(|(l, _, top)| row >= *top && row < top + l.chars().count()).map(|s| s.1)
}

/// 1 行目の `col` にあるタブ
pub fn tab_at(col: usize) -> Option<LeftTab> {
    let mut x = 0;
    for (label, tab) in TABS {
        let w = nagi_core::display_col(label, label.len(), false);
        if col >= x && col < x + w {
            return Some(tab);
        }
        x += w;
    }
    None
}

impl TreePanel {
    fn rebuild(&mut self) {
        let sel = self.rows.get(self.selected).map(|r| r.path.clone());
        let mut rows = Vec::new();
        self.push_children(&self.root.clone(), 0, &mut rows);
        self.rows = rows;
        if let Some(p) = sel {
            if let Some(i) = self.rows.iter().position(|r| r.path == p) {
                self.selected = i;
            }
        }
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }

    fn push_children(&self, dir: &Path, depth: usize, out: &mut Vec<Row>) {
        let Some(list) = self.children.get(dir) else { return };
        for (name, is_dir) in list {
            let path = dir.join(name);
            let open = *is_dir && self.expanded.contains(&path);
            out.push(Row { path: path.clone(), name: name.clone(), depth, dir: *is_dir });
            if open {
                self.push_children(&path, depth + 1, out);
            }
        }
    }

    pub fn is_expanded(&self, p: &Path) -> bool {
        self.expanded.contains(p)
    }

    pub fn loading(&self) -> bool {
        !self.loading.is_empty()
    }

    fn load(&mut self, dir: &Path, waker: &nagi_workers::Waker) {
        if self.children.contains_key(dir) || !self.loading.insert(dir.to_path_buf()) {
            return;
        }
        nagi_workers::files::list_dir(dir.to_path_buf(), self.tx.clone(), waker.clone());
    }

    fn expand(&mut self, dir: &Path, waker: &nagi_workers::Waker) {
        self.expanded.insert(dir.to_path_buf());
        self.load(dir, waker);
        self.rebuild();
    }

    /// 選びたいファイルまで、届いているフォルダを展開して進める
    fn advance_reveal(&mut self, waker: &nagi_workers::Waker) {
        let Some(target) = self.reveal.clone() else { return };
        let Ok(rel) = target.strip_prefix(&self.root) else {
            self.reveal = None;
            return;
        };
        let mut dir = self.root.clone();
        let parts: Vec<_> = rel.components().collect();
        for (i, c) in parts.iter().enumerate() {
            if !self.children.contains_key(&dir) {
                self.load(&dir, waker);
                return;
            }
            let next = dir.join(c);
            if i + 1 == parts.len() {
                self.rebuild();
                if let Some(k) = self.rows.iter().position(|r| r.path == next) {
                    self.selected = k;
                }
                self.reveal = None;
                return;
            }
            self.expanded.insert(next.clone());
            dir = next;
        }
        self.reveal = None;
    }
}

impl Editor {
    fn waker_or_noop(&self) -> nagi_workers::Waker {
        self.waker.clone().unwrap_or_else(|| std::sync::Arc::new(|| {}))
    }

    /// 左の欄に出しているタブ（両方開いていれば、最後に選んだ方）
    pub fn left_visible(&self) -> Option<LeftTab> {
        match (self.tree.is_some(), self.commit.is_some()) {
            (true, true) => Some(self.left_tab),
            (true, false) => Some(LeftTab::Project),
            (false, true) => Some(LeftTab::Commit),
            (false, false) => None,
        }
    }

    /// 左の欄の矩形（1 行目はタブ）
    pub fn left_rect(&self) -> Option<Rect> {
        self.left_visible()?;
        let (cols, _) = self.screen;
        Some(Rect { x: 0, y: 0, w: tree_width(cols), h: self.windows_rows() })
    }

    pub fn tree_rect(&self) -> Option<Rect> {
        (self.left_visible()? == LeftTab::Project).then(|| self.left_rect()).flatten()
    }

    /// 窓に使える範囲の左端と幅（左の欄の分と区切りの 1 列を除く）
    pub(crate) fn windows_x(&self) -> (usize, usize) {
        let cols = self.screen.0;
        match self.left_visible() {
            Some(_) => {
                let w = tree_width(cols) + 1;
                (w, cols.saturating_sub(w))
            }
            None => (0, cols),
        }
    }

    /// ツリーを開く（開いていれば移る）。今のファイルを選ぶ
    pub(crate) fn open_tree(&mut self) {
        let waker = self.waker_or_noop();
        let current = self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok());
        if self.tree.is_none() {
            let root = current
                .as_deref()
                .and_then(nagi_workers::files::project_root)
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_default();
            let root = std::path::absolute(&root).unwrap_or(root);
            let (tx, rx) = channel();
            let mut t = TreePanel {
                root: root.clone(),
                children: HashMap::new(),
                expanded: Default::default(),
                rows: Vec::new(),
                selected: 0,
                scroll: 0,
                tx,
                rx,
                loading: Default::default(),
                reveal: None,
                pending_g: false,
                pending_w: false,
            };
            t.load(&root, &waker);
            self.tree = Some(t);
        }
        self.left_tab = LeftTab::Project;
        let t = self.tree.as_mut().unwrap();
        t.reveal = current;
        t.advance_reveal(&waker);
        self.list_side = crate::editor::Side::Left;
        self.mode = Mode::List;
        self.pending_keys.clear();
        self.rows = self.text_rows(self.active_rect());
    }

    /// `<leader>.`: 閉じていれば開いて移り、開いていて移っていれば閉じる
    pub(crate) fn toggle_tree(&mut self) {
        if self.left_visible() == Some(LeftTab::Project) && self.mode == Mode::List && self.list_side == crate::editor::Side::Left {
            self.close_tree();
        } else {
            self.open_tree();
        }
    }

    /// 左のストライプの「コミット」: 閉じていれば開いて移り、出していて移っていれば閉じる
    pub(crate) fn toggle_commit(&mut self) {
        if self.left_visible() == Some(LeftTab::Commit) && matches!(self.mode, Mode::List | Mode::CommitMsg) && (self.mode == Mode::CommitMsg || self.list_side == crate::editor::Side::Commit) {
            self.close_commit_panel();
        } else {
            self.open_commit_panel();
        }
    }

    /// タブを選ぶ（開いていなければ開く）
    pub(crate) fn select_left_tab(&mut self, tab: LeftTab) {
        match tab {
            LeftTab::Project => self.open_tree(),
            LeftTab::Commit => self.open_commit_panel(),
        }
    }

    pub(crate) fn close_tree(&mut self) {
        self.tree = None;
        if self.mode == Mode::List && self.list_side == crate::editor::Side::Left {
            self.mode = Mode::Normal;
        }
        self.rows = self.text_rows(self.active_rect());
    }

    /// 届いたフォルダの中身を受け取る。変わったら true
    pub fn poll_tree(&mut self) -> bool {
        let waker = self.waker_or_noop();
        let Some(t) = &mut self.tree else { return false };
        let mut changed = false;
        let mut loaded = Vec::new();
        while let Ok((dir, list)) = t.rx.try_recv() {
            t.loading.remove(&dir);
            t.children.insert(dir.clone(), list);
            loaded.push(dir);
            changed = true;
        }
        if changed {
            t.rebuild();
            t.advance_reveal(&waker);
        }
        // 読み込んだフォルダの中身の増減を見張る（変わったら読み直す）
        for d in loaded {
            self.watch_listing(&d);
        }
        changed
    }

    /// 見張っているフォルダの中身が変わった: ツリーで読み込んでいれば読み直す
    pub(crate) fn tree_reload_dir(&mut self, dir: &Path) -> bool {
        let waker = self.waker_or_noop();
        let Some(t) = &mut self.tree else { return false };
        let low = dir.to_string_lossy().to_lowercase();
        let Some(k) = t.children.keys().find(|k| k.to_string_lossy().to_lowercase() == low).cloned() else { return false };
        // 読み直している間も今の中身を出しておき、届いたら差し替える
        if t.loading.insert(k.clone()) {
            nagi_workers::files::list_dir(k, t.tx.clone(), waker);
        }
        false
    }

    pub(crate) fn tree_key(&mut self, key: Key) -> Effect {
        let waker = self.waker_or_noop();
        let half = (self.tree_rect().map_or(10, |r| r.h) / 2).max(1);
        let Some(t) = &mut self.tree else {
            self.mode = Mode::Normal;
            return Effect::None;
        };
        let g_pending = std::mem::take(&mut t.pending_g);
        if std::mem::take(&mut t.pending_w) {
            match key {
                Key::Char('c' | 'q') | Key::Ctrl('c' | 'q') => self.close_tree(),
                Key::Char('h') | Key::Ctrl('h') => {}
                Key::Char('j') | Key::Ctrl('j') if self.grep.is_some() => self.focus_panel(),
                _ => self.mode = Mode::Normal,
            }
            return Effect::None;
        }
        let last = t.rows.len().saturating_sub(1);
        match key {
            Key::Ctrl('w') => t.pending_w = true,
            Key::Char('j') | Key::Down => t.selected = (t.selected + 1).min(last),
            Key::Char('k') | Key::Up => t.selected = t.selected.saturating_sub(1),
            Key::Char('g') if g_pending => t.selected = 0,
            Key::Char('g') => t.pending_g = true,
            Key::Char('G') => t.selected = last,
            Key::Ctrl('d') => t.selected = (t.selected + half).min(last),
            Key::Ctrl('u') => t.selected = t.selected.saturating_sub(half),
            Key::Enter | Key::Char('l') | Key::Char('o') => {
                let stay = key == Key::Char('o');
                let Some(row) = t.rows.get(t.selected).cloned() else { return Effect::None };
                if row.dir {
                    if t.expanded.contains(&row.path) {
                        // l は開いているフォルダでは何もしない（Enter と o は畳む）
                        if key != Key::Char('l') {
                            t.expanded.remove(&row.path);
                            t.rebuild();
                        }
                    } else {
                        t.expand(&row.path, &waker);
                    }
                } else {
                    self.edit_file(row.path);
                    self.mode = if stay { Mode::List } else { Mode::Normal };
                }
            }
            Key::Char('h') => {
                let Some(row) = t.rows.get(t.selected).cloned() else { return Effect::None };
                if row.dir && t.expanded.contains(&row.path) {
                    t.expanded.remove(&row.path);
                    t.rebuild();
                } else if let Some(parent) = row.path.parent() {
                    // 親のフォルダへ移って畳む
                    if let Some(i) = t.rows.iter().position(|r| r.path == parent) {
                        t.selected = i;
                        t.expanded.remove(parent);
                        t.rebuild();
                    }
                }
            }
            Key::Char('R') => {
                t.children.clear();
                let root = t.root.clone();
                t.load(&root, &waker);
                let open: Vec<PathBuf> = t.expanded.iter().cloned().collect();
                for d in open {
                    t.load(&d, &waker);
                }
            }
            Key::Char('q') => self.close_tree(),
            Key::Esc => self.mode = Mode::Normal,
            Key::Char(':') => {
                self.mode = Mode::Command;
                self.cmdline = Default::default();
            }
            _ => {}
        }
        Effect::None
    }

    /// ツリーの中のクリック。`row` はツリーの中の行（0 は見出し）
    pub fn tree_click(&mut self, row: usize, clicks: u32) {
        self.list_side = crate::editor::Side::Left;
        self.mode = Mode::List;
        let waker = self.waker_or_noop();
        let Some(t) = &mut self.tree else { return };
        if row == 0 {
            return;
        }
        let i = t.scroll + row - 1;
        let Some(r) = t.rows.get(i).cloned() else { return };
        t.selected = i;
        if r.dir {
            // フォルダは 1 回のクリックで開閉する
            if t.expanded.contains(&r.path) {
                t.expanded.remove(&r.path);
                t.rebuild();
            } else {
                t.expand(&r.path, &waker);
            }
        } else if clicks >= 2 {
            self.edit_file(r.path);
            self.mode = Mode::Normal;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    fn keys(e: &mut Editor, k: &str) {
        for key in nagi_vim::parse_keys(k, " ") {
            e.key(key);
        }
    }

    fn settle(e: &mut Editor) {
        for _ in 0..500 {
            e.poll_tree();
            if e.tree.as_ref().is_some_and(|t| !t.loading() && t.reveal.is_none()) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("ツリーが読み終わらない");
    }

    #[test]
    fn reveal_navigate_and_open() {
        let dir = std::env::temp_dir().join(format!("nagi-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("src/view")).unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(dir.join("src/view/panes.rs"), "panes\n").unwrap();
        std::fs::write(dir.join("src/main.rs"), "main\n").unwrap();
        std::fs::write(dir.join("README.md"), "readme\n").unwrap();
        let mut e = Editor::new(Buffer::open(&dir.join("src/view/panes.rs")).unwrap(), None);
        e.buf.path = Some(dir.join("src/view/panes.rs"));
        e.set_screen(120, 40);
        e.toggle_tree();
        settle(&mut e);
        assert_eq!(e.mode, Mode::List);
        assert_eq!(e.windows_x(), (37, 83), "ツリーの分だけ窓が右へ寄る");
        let t = e.tree.as_ref().unwrap();
        let names: Vec<String> = t.rows.iter().map(|r| format!("{}{}", "  ".repeat(r.depth), r.name)).collect();
        // フォルダが先、.gitignore の target は出さない、.git も出さない。今のファイルまで展開して選ぶ
        assert_eq!(names, vec!["src", "  view", "    panes.rs", "  main.rs", ".gitignore", "README.md"]);
        assert_eq!(t.rows[t.selected].name, "panes.rs");
        // h で親へ移って畳む、もう一度 h でさらに親へ
        keys(&mut e, "h");
        let t = e.tree.as_ref().unwrap();
        assert_eq!(t.rows[t.selected].name, "view");
        assert!(!t.is_expanded(&dir.join("src/view")));
        // j で main.rs、Enter で開いて編集画面へ
        keys(&mut e, "j<CR>");
        assert_eq!(e.mode, Mode::Normal);
        assert_eq!(e.buf.line(0), "main");
        // Ctrl-w h でツリーへ戻り、q で閉じる
        keys(&mut e, "<C-w>h");
        assert_eq!(e.mode, Mode::List);
        keys(&mut e, "q");
        assert!(e.tree.is_none());
        assert_eq!(e.windows_x(), (0, 120));
    }
}
