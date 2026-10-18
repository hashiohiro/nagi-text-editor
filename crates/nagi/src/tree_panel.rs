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
    /// 最後に合わせた開いているファイル（変わったら木をそこまで展開して選ぶ）
    synced: Option<PathBuf>,
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

/// タブ（1 行目にアイコンで並べる。名前は出しているタブの横に出す）
pub const TABS: [(&str, LeftTab); 2] = [("プロジェクト", LeftTab::Project), ("コミット", LeftTab::Commit)];

/// アイコンのタブ 1 つの幅（列）
pub const TAB_W: usize = 4;

/// 左のストライプのラベル（縦書き。見出し, タブ, 上端の行）
pub const STRIPE: [(&str, LeftTab, usize); 2] = [("プロジェクト", LeftTab::Project, 0), ("コミット", LeftTab::Commit, 7)];

/// 左のストライプの `row` にあるラベル
pub fn stripe_at(row: usize) -> Option<LeftTab> {
    STRIPE.iter().find(|(l, _, top)| row >= *top && row < top + l.chars().count()).map(|s| s.1)
}

/// 1 行目の `col` にあるタブ
pub fn tab_at(col: usize) -> Option<LeftTab> {
    TABS.get(col / TAB_W).map(|t| t.1)
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
    /// 開いているファイルに木を合わせる（IntelliJ の Always Select Opened File）。木が出ていて、木にいないとき、
    /// 開いているファイルが変わっていたら、そこまで展開して選ぶ（描く前に呼ぶ）
    pub(crate) fn sync_tree(&mut self) {
        if !self.tree_sync || self.left_hidden || self.left_tab != LeftTab::Project || (self.mode == Mode::List && self.list_side == crate::editor::Side::Left) {
            return;
        }
        let current = self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok());
        let waker = self.waker_or_noop();
        let Some(t) = &mut self.tree else { return };
        if current.is_none() || t.synced == current {
            return;
        }
        t.synced = current.clone();
        if current.as_ref().is_some_and(|c| c.starts_with(&t.root)) {
            t.reveal = current;
            t.advance_reveal(&waker);
        }
    }

    /// SelectInProjectView（Alt+F1）: 今のファイルを木で選ぶ（木を出して移る）
    pub(crate) fn select_in_tree(&mut self) {
        if let Some(t) = &mut self.tree {
            t.synced = None;
        }
        self.open_tree();
    }

    fn waker_or_noop(&self) -> nagi_workers::Waker {
        self.waker()
    }

    /// 左の欄に出しているタブ（両方開いていれば、最後に選んだ方）
    pub fn left_visible(&self) -> Option<LeftTab> {
        if self.left_hidden {
            return None;
        }
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
        let _ = cols;
        // 下の欄を隠しているときは、左下の隅を ▴ のボタンに空ける
        let h = self.windows_rows() - usize::from(self.show_bottom_button() && self.windows_rows() > 2);
        Some(Rect { x: 0, y: 0, w: self.left_width(), h })
    }

    pub fn tree_rect(&self) -> Option<Rect> {
        (self.left_visible()? == LeftTab::Project).then(|| self.left_rect()).flatten()
    }

    /// 窓に使える範囲の左端と幅（左の欄の分と区切りの 1 列を除く）
    pub(crate) fn windows_x(&self) -> (usize, usize) {
        let cols = self.screen.0;
        // 右の欄の分と区切りの 1 列も除く
        let right = self.right_rect().map_or(0, |r| r.w + 1);
        match self.left_visible() {
            Some(_) => {
                let w = self.left_width() + 1;
                (w, cols.saturating_sub(w + right))
            }
            None => (0, cols.saturating_sub(right)),
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
                synced: None,
                pending_g: false,
                pending_w: false,
            };
            t.load(&root, &waker);
            self.tree = Some(t);
        }
        self.left_tab = LeftTab::Project;
        self.left_hidden = false;
        let t = self.tree.as_mut().unwrap();
        t.synced = current.clone();
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
        let moved = crate::list::step(key, t.selected, t.rows.len(), half * 2);
        match key {
            _ if moved.is_some() => t.selected = moved.unwrap_or(t.selected),
            Key::Ctrl('w') => t.pending_w = true,
            Key::Char('g') if g_pending => t.selected = 0,
            Key::Char('g') => t.pending_g = true,
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
                self.enter_command("");
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


/// 描き方（view.rs の Painter で描く）
pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    /// ファイルツリー。1 行目はタブと根の名前、2 行目から字下げした木（フォルダは青で `▸` `▾`）。右端に 1px の区切り線
    /// プロジェクトの木。字は白（フォルダもファイルも同じ）、git の状態で色分け: 追加は緑、変更は青、管理対象外は赤、
    /// 競合は黄。フォルダは管理されていなければ赤、管理されている変更を含めば青（IntelliJ と同じ）
    pub(crate) fn draw_tree(
        p: &mut Painter,
        t: &crate::tree_panel::TreePanel,
        r: Rect,
        focused: bool,
        git: &std::collections::HashMap<std::path::PathBuf, nagi_workers::git::FileState>,
        index: Option<&nagi_workers::index::Index>,
    ) {
        use nagi_workers::git::FileState;
        let cell = p.fonts.cell();
        let (x, y0, w) = (r.x as f32 * cell.w, r.y as f32 * cell.h, r.w as f32 * cell.w);
        p.out.push(Instance::fill(x, y0, w, r.h as f32 * cell.h, BG));
        p.out.push(Instance::fill(((r.x + r.w) as f32 + 0.5) * cell.w, y0, 1.0, r.h as f32 * cell.h, LINE_NO));
        let saved = p.cols;
        p.cols = r.x + r.w;
        let name = t.root.file_name().map_or(t.root.display().to_string(), |n| n.to_string_lossy().into_owned());
        // プロジェクトの名前は窓の題名に出す（タブの横には出さない）
        let _ = name;
        draw_left_tabs(p, r, crate::tree_panel::LeftTab::Project, focused, if t.loading() { "…" } else { "" });
        for k in 0..r.h.saturating_sub(1) {
            let Some(row) = t.rows.get(t.scroll + k) else { break };
            let y = (r.y + 1 + k) as f32 * cell.h;
            let selected = t.scroll + k == t.selected;
            let reversed = selected && focused;
            if reversed {
                p.out.push(Instance::fill(x, y, w, cell.h, FG));
            } else if selected {
                p.out.push(Instance::fill(x, y, w, cell.h, MATCH_BG));
            }
            let mark = if row.dir { if t.is_expanded(&row.path) { "▾ " } else { "▸ " } } else { "  " };
            let icon_col = r.x + row.depth * 2 + 2;
            let state = git.get(&row.path).copied();
            let fg = match state {
                _ if reversed => BG,
                Some(FileState::Added) => GIT_ADD,
                Some(FileState::Modified | FileState::Renamed) => GIT_MOD,
                Some(FileState::Untracked) => GIT_DEL,
                Some(FileState::Conflicted) => DIAG_WARN,
                Some(FileState::Deleted) => STATUS_FG,
                None => FG,
            };
            let label = format!("{}{mark}", "  ".repeat(row.depth));
            p.text(&label, r.x, y, if reversed { BG } else { STATUS_FG });
            if icon_col + 3 < r.x + r.w {
                draw_tree_icon(p, &tree_icon(row, index, &t.root), icon_col, y);
            }
            p.text(&row.name, icon_col + 2, y, fg);
        }
        p.cols = saved;
    }

    /// 木のアイコン
    pub(crate) enum TreeIcon {
        /// フォルダ（色）
        Folder([f32; 4]),
        /// 種類の分かるファイル（文字・地の色・文字の色）
        Badge(&'static str, [f32; 4], [f32; 4]),
        /// ふつうのファイル（紙の形）
        Page,
    }

    /// 行のアイコン（IntelliJ と同じ考え方: フォルダはソース・テスト・除外で色を変え、ファイルは種類の文字と色）
    pub(crate) fn tree_icon(row: &crate::tree_panel::Row, index: Option<&nagi_workers::index::Index>, root: &std::path::Path) -> TreeIcon {
        let name = row.name.to_ascii_lowercase();
        if row.dir {
            let rel = row.path.strip_prefix(root).map(|r| r.to_string_lossy().replace('\\', "/").to_ascii_lowercase()).unwrap_or_default();
            let color = if matches!(name.as_str(), "target" | "out" | "dist" | "build" | "node_modules" | "bin" | ".angular" | ".next") {
                rgb(0xc98a4b)
            } else if rel.ends_with("src/test/java") || rel.ends_with("src/test/kotlin") || matches!(name.as_str(), "test" | "tests" | "__tests__" | "e2e" | "cypress" | "spec") {
                rgb(0x62b543)
            } else if rel.ends_with("src/main/java") || rel.ends_with("src/main/kotlin") || (name == "src" && !row.path.join("main").is_dir()) {
                rgb(0x548af7)
            } else if rel.ends_with("src/main/resources") || rel.ends_with("src/test/resources") {
                rgb(0xd9a343)
            } else {
                rgb(0x8c9aa6)
            };
            return TreeIcon::Folder(color);
        }
        let dark = BG;
        let white = rgb(0xffffff);
        let ext = name.rsplit_once('.').map_or("", |x| x.1);
        let b = |l: &'static str, c: u32, light: bool| TreeIcon::Badge(l, rgb(c), if light { dark } else { white });
        match (name.as_str(), ext) {
            ("pom.xml", _) => b("m", 0xc71a36, false),
            ("package.json", _) => b("n", 0xcb3837, false),
            ("dockerfile", _) => b("D", 0x2496ed, false),
            (".gitignore" | ".gitattributes", _) => b("G", 0xf05033, false),
            (_, "java") => {
                let rel = row.path.strip_prefix(root).map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                match index.and_then(|i| i.file_type_kind(&rel)) {
                    Some(nagi_syntax::defs::DefKind::Interface) => b("I", 0x62b543, true),
                    Some(nagi_syntax::defs::DefKind::Enum) => b("E", 0xd9a343, true),
                    Some(nagi_syntax::defs::DefKind::Class) => b("C", 0x3592c4, false),
                    _ => b("J", 0xe76f00, false),
                }
            }
            (_, "kt" | "kts") => b("K", 0xa97bff, false),
            (_, "ts" | "mts" | "cts") => b("TS", 0x3178c6, false),
            (_, "tsx") => b("TS", 0x61dafb, true),
            (_, "js" | "mjs" | "cjs") => b("JS", 0xf1e05a, true),
            (_, "jsx") => b("JS", 0x61dafb, true),
            (_, "html" | "htm") => b("<>", 0xe44d26, false),
            (_, "css") => b("#", 0x3d8fd1, false),
            (_, "scss" | "sass") => b("S", 0xcd6799, false),
            (_, "json" | "jsonc") => b("{}", 0xcbcb41, true),
            (_, "md" | "markdown") => b("M", 0x519aba, false),
            (_, "xml" | "xsd" | "wsdl") => b("<>", 0xe37933, false),
            (_, "yml" | "yaml") => b("Y", 0xcb171e, false),
            (_, "properties" | "ini" | "toml" | "conf" | "cfg" | "env") => b("P", 0x8c9aa6, true),
            (_, "sql") => b("Q", 0xe38c00, true),
            (_, "rs") => b("R", 0xdea584, true),
            (_, "py") => b("Py", 0x3572a5, false),
            (_, "sh" | "bash" | "bat" | "cmd" | "ps1") => b(">", 0x89e051, true),
            (_, "png" | "jpg" | "jpeg" | "gif" | "svg" | "ico" | "webp") => b("◩", 0x62b543, true),
            (_, "vim") => b("V", 0x199f4b, false),
            _ => TreeIcon::Page,
        }
    }

    /// アイコンを列 `col`・行の y に描く（2 列ぶん。中は 16px ほど）
    pub(crate) fn draw_tree_icon(p: &mut Painter, icon: &TreeIcon, col: usize, y: f32) {
        let cell = p.fonts.cell();
        let s = (cell.h * 0.62).round().clamp(10.0, 26.0);
        let x = col as f32 * cell.w + ((2.0 * cell.w - s) / 2.0).max(0.0) - 2.0;
        let y = y + ((cell.h - s) / 2.0).round();
        let px = (s / 16.0).max(1.0).round();
        match icon {
            TreeIcon::Folder(c) => {
                // つまみ（左上）と本体
                p.out.push(Instance::fill(x, y + px, s * 0.45, px * 2.0, *c));
                p.out.push(Instance::fill(x, y + px * 3.0, s, s - px * 4.0, *c));
            }
            TreeIcon::Badge(label, bg, fg) => {
                p.out.push(Instance::fill(x, y, s, s, *bg));
                let n = label.chars().count() as f32;
                let scale = if n >= 2.0 { (s * 0.95 / (2.0 * cell.w)).min(0.5) } else { 0.62 };
                let tw = cell.w * scale * n;
                p.small_text(label, x + (s - tw) / 2.0, y + (s - cell.h * scale) / 2.0, scale, *fg);
            }
            TreeIcon::Page => {
                // 紙の形（枠と、右上の折り返し）
                let c = rgb(0x8c9aa6);
                let (w, h) = (s * 0.75, s);
                let x = x + (s - w) / 2.0;
                p.out.push(Instance::fill(x, y, w - px * 3.0, px, c));
                p.out.push(Instance::fill(x, y, px, h, c));
                p.out.push(Instance::fill(x + w - px, y + px * 3.0, px, h - px * 3.0, c));
                p.out.push(Instance::fill(x, y + h - px, w, px, c));
                p.out.push(Instance::fill(x + w - px * 3.0, y, px, px * 3.0, c));
                p.out.push(Instance::fill(x + w - px * 3.0, y + px * 3.0 - px, px * 3.0, px, c));
            }
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

    /// 木を操作した後に本文をクリックすると、本文の操作に戻る（カーソルが出て、キーが本文へ行く）
    #[test]
    fn click_text_after_tree_returns_to_editor() {
        let dir = std::env::temp_dir().join(format!("nagi-tree-click-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let mut buf = Buffer::open(&dir.join("a.txt")).unwrap();
        buf.path = Some(dir.join("a.txt"));
        let mut e = Editor::new(buf, None);
        e.set_screen(120, 40);
        e.open_tree();
        e.tree_click(1, 1);
        assert_eq!(e.mode, Mode::List);
        // 本文の 2 行目をクリック
        let r = e.active_rect();
        e.click(1, r.x + e.gutter() + 1);
        assert_eq!(e.mode, Mode::Normal, "本文に戻る");
        assert_eq!(e.cursor.line, 1);
        e.key(nagi_vim::Key::Char('j'));
        assert_eq!(e.cursor.line, 2, "キーは本文へ");
    }

    /// 開いているファイルが変わると、木をそこまで展開して選ぶ（木にいる間は動かさない）
    #[test]
    fn follows_open_file() {
        let dir = std::env::temp_dir().join(format!("nagi-tree-sync-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        std::fs::create_dir_all(dir.join("c")).unwrap();
        std::fs::write(dir.join("a/b/deep.txt"), "x\n").unwrap();
        std::fs::write(dir.join("c/other.txt"), "y\n").unwrap();
        let mut buf = Buffer::open(&dir.join("c/other.txt")).unwrap();
        buf.path = Some(dir.join("c/other.txt"));
        let mut e = Editor::new(buf, None);
        e.set_screen(120, 40);
        e.open_tree();
        let settle = |e: &mut Editor| {
            for _ in 0..500 {
                e.poll_tree();
                e.sync_tree();
                if e.tree.as_ref().is_some_and(|t| !t.loading() && t.reveal.is_none()) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        };
        settle(&mut e);
        let sel = |e: &Editor| e.tree.as_ref().map(|t| t.rows[t.selected].path.clone()).unwrap();
        assert!(sel(&e).ends_with("other.txt"));
        // 本文へ戻って別のファイルを開くと、木もそこへ
        e.mode = Mode::Normal;
        e.edit_file(dir.join("a/b/deep.txt"));
        e.sync_tree();
        settle(&mut e);
        assert!(sel(&e).ends_with("deep.txt"), "{:?}", sel(&e));
        // :TreeSync で止める
        for k in nagi_vim::parse_keys(":TreeSync<CR>", " ") {
            e.key(k);
        }
        e.edit_file(dir.join("c/other.txt"));
        e.sync_tree();
        settle(&mut e);
        assert!(sel(&e).ends_with("deep.txt"), "止めたら動かさない");
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
