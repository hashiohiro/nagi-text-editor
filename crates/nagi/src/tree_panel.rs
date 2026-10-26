//! ファイルツリー（左にドッキングするパネル）。アイコンは使わず、字下げと `▸` `▾` だけで木を表す。
//!
//! フォルダの中身は裏のスレッドで読み（UI スレッドではファイルを読まない）、届いたら展開する。
//! 開いたときは、今のファイルまでのフォルダを順に展開してそのファイルを選ぶ。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use nagi_vim::Key;
use nagi_workers::files::DirList;

use crate::editor::{Editor, Effect, State};
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
    /// 左端の列（深いフォルダ・長い名前を横に送って見る）
    pub left: usize,
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
    /// 実行の構成（run_panel.rs）
    Run,
}

/// タブ（1 行目にアイコンで並べる。名前は出しているタブの横に出す）
pub const TABS: [(&str, LeftTab); 3] = [("プロジェクト", LeftTab::Project), ("コミット", LeftTab::Commit), ("実行", LeftTab::Run)];

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
        if !self.tree_sync || self.left_hidden || self.left_tab != LeftTab::Project || (self.state() == State::List && self.list_side == crate::editor::Side::Left) {
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

    /// 左の欄に出しているタブ（いくつか開いていれば、最後に選んだもの）
    pub fn left_visible(&self) -> Option<LeftTab> {
        if self.left_hidden {
            return None;
        }
        self.left_open()
    }

    /// 開いている左の欄のタブ（隠していても）。最後に選んだものが開いていればそれ、無ければ開いている最初のもの
    pub fn left_open(&self) -> Option<LeftTab> {
        let open = |t: LeftTab| match t {
            LeftTab::Project => self.tree.is_some(),
            LeftTab::Commit => self.commit.is_some(),
            LeftTab::Run => self.run_panel.is_some(),
        };
        if open(self.left_tab) { Some(self.left_tab) } else { TABS.iter().map(|t| t.1).find(|&t| open(t)) }
    }

    /// 左の欄の矩形（1 行目はタブ）
    pub fn left_rect(&self) -> Option<Rect> {
        self.left_visible()?;
        let a = self.area();
        Some(Rect { x: a.x, y: a.y, w: self.left_width(), h: self.windows_rows().saturating_sub(a.y) })
    }

    pub fn tree_rect(&self) -> Option<Rect> {
        (self.left_visible()? == LeftTab::Project).then(|| self.left_rect()).flatten()
    }

    /// 窓に使える範囲の左端と幅（左の欄の分と区切りの 1 列を除く）
    pub(crate) fn windows_x(&self) -> (usize, usize) {
        let a = self.area();
        // 右の欄の分と区切りの 1 列も除く
        let right = self.right_rect().map_or(0, |r| r.w + 1);
        match self.left_visible() {
            Some(_) => {
                let w = self.left_width() + 1;
                (a.x + w, a.w.saturating_sub(w + right))
            }
            None => (a.x, a.w.saturating_sub(right)),
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
                .or_else(|| Some(self.cwd.clone()))
                .unwrap_or_default();
            let root = std::path::absolute(&root).unwrap_or(root);
            let (tx, rx) = channel();
            let mut t = TreePanel {
                root: root.clone(),
                children: HashMap::new(),
                expanded: Default::default(),
                rows: Vec::new(),
                left: 0,
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
        self.set_state(State::List);
        self.pending_keys.clear();
        self.rows = self.text_rows(self.active_rect());
    }

    /// `<leader>.`: 閉じていれば開いて移り、開いていて移っていれば閉じる
    pub(crate) fn toggle_tree(&mut self) {
        if self.left_visible() == Some(LeftTab::Project) && self.state() == State::List && self.list_side == crate::editor::Side::Left {
            self.close_tree();
        } else {
            self.open_tree();
        }
    }

    /// 左のストライプの「コミット」: 閉じていれば開いて移り、出していて移っていれば閉じる
    pub(crate) fn toggle_commit(&mut self) {
        if self.left_visible() == Some(LeftTab::Commit) && matches!(self.state(), State::List | State::CommitMsg) && (self.state() == State::CommitMsg || self.list_side == crate::editor::Side::Commit) {
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
            LeftTab::Run => self.open_run_panel(),
        }
    }

    pub(crate) fn close_tree(&mut self) {
        self.tree = None;
        if self.state() == State::List && self.list_side == crate::editor::Side::Left {
            self.set_state(State::Normal);
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
            self.set_state(State::Normal);
            return Effect::None;
        };
        let g_pending = std::mem::take(&mut t.pending_g);
        if std::mem::take(&mut t.pending_w) {
            match key {
                Key::Char('c' | 'q') | Key::Ctrl('c' | 'q') => self.close_tree(),
                Key::Char('h') | Key::Ctrl('h') => {}
                Key::Char('j') | Key::Ctrl('j') if self.grep.is_some() => self.focus_panel(),
                _ => self.set_state(State::Normal),
            }
            return Effect::None;
        }
        let moved = crate::list::step(key, t.selected, t.rows.len(), half * 2);
        match key {
            _ if moved.is_some() => t.selected = moved.unwrap_or(t.selected),
            Key::Ctrl('w') => t.pending_w = true,
            Key::Char('g') if g_pending => t.selected = 0,
            Key::Char('g') => t.pending_g = true,
            Key::Enter | Key::Char('l') | Key::Right | Key::Char('o') => {
                let stay = key == Key::Char('o');
                let Some(row) = t.rows.get(t.selected).cloned() else { return Effect::None };
                if row.dir {
                    if t.expanded.contains(&row.path) {
                        // l・→ は開いているフォルダでは何もしない（Enter と o は畳む）
                        if !matches!(key, Key::Char('l') | Key::Right) {
                            t.expanded.remove(&row.path);
                            t.rebuild();
                        }
                    } else {
                        t.expand(&row.path, &waker);
                    }
                } else if key != Key::Right {
                    // → はフォルダを開くだけ（ファイルは Enter・l・o・ダブルクリックで開く）
                    self.edit_file(row.path);
                    self.set_state(if stay { State::List } else { State::Normal });
                }
            }
            Key::Char('h') | Key::Left => {
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
            Key::Esc => self.set_state(State::Normal),
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
        self.set_state(State::List);
        let waker = self.waker_or_noop();
        let Some(t) = &mut self.tree else { return };
        let len = t.rows.len();
        let Some(pick) = crate::list::pick_row(&mut t.selected, t.scroll, row, 1, len, clicks) else { return };
        let r = t.rows[t.selected].clone();
        if r.dir {
            // フォルダは 1 回のクリックで開閉する
            if t.expanded.contains(&r.path) {
                t.expanded.remove(&r.path);
                t.rebuild();
            } else {
                t.expand(&r.path, &waker);
            }
        } else if matches!(pick, crate::list::Pick::Accept(_)) {
            self.edit_file(r.path);
            self.set_state(State::Normal);
        }
    }
}



/// この部品の入口（component.rs）
pub struct TreeComponent;

impl crate::component::Component for TreeComponent {
    fn status(&self, ed: &crate::editor::Editor) -> Option<crate::component::Status> {
        let _ = ed;
        crate::component::Status::keys("-- プロジェクト --", "", "Enter 開く  ←→ 開閉  右クリック 操作  Esc 窓へ ")
    }

    fn key(&self, ed: &mut crate::editor::Editor, key: nagi_vim::Key) -> crate::editor::Effect {
        ed.tree_key(key)
    }
}


/// ファイルの木（layout.rs の View）
pub struct TreeView;

impl crate::layout::View for TreeView {
    fn rect(&self, ed: &crate::editor::Editor, s: crate::layout::Screen) -> Option<crate::panes::Rect> {
        let _ = s;
        ed.tree.as_ref()?;
        ed.tree_rect()
    }

    fn draw(&self, ed: &crate::editor::Editor, p: &mut crate::view::Painter, r: crate::panes::Rect, s: crate::layout::Screen) {
        let _ = (r, s);
        if let Some(t) = &ed.tree {
            let git = ed.tree_git_states();
            draw::draw_tree(p, t, r, ed.state() == crate::editor::State::List && ed.list_side == crate::editor::Side::Left, &git, ed.nav.index.as_ref());
        }
    }

    fn click(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, row: usize, col: usize, clicks: u32) -> crate::layout::Click {
        let _ = (r, row, col, clicks);
        ed.tree_click(row, clicks);
        crate::layout::Click::Done
    }

    fn wheel(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, row: usize, col: usize, dy: isize, dx: isize) -> bool {
        let _ = (row, col);
        if let Some(t) = &mut ed.tree {
            // 1 行目は見出し（view.rs の build と同じ高さ）
            let h = r.h.saturating_sub(1);
            crate::list::wheel(&mut t.selected, &mut t.scroll, t.rows.len(), h, dy);
            if dx != 0 {
                let widest = t.rows.iter().skip(t.scroll).take(h).map(|x| x.depth * 2 + 4 + nagi_core::display_col(&x.name, x.name.len(), false)).max().unwrap_or(0);
                crate::list::hwheel(&mut t.left, widest + 1, r.w, dx);
            }
        }
        true
    }

    fn context(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, row: usize, col: usize) -> bool {
        use nagi_vim::Key::*;
        let _ = (r, row, col);
        // 押した行を選ぶだけ（左クリックと違ってフォルダを開け閉めしない）
        let Some(t) = &mut ed.tree else { return false };
        let len = t.rows.len();
        if crate::list::pick_row(&mut t.selected, t.scroll, row - r.y, 1, len, 1).is_none() {
            return false;
        }
        let dir = t.rows[t.selected].dir;
        let path = t.rows[t.selected].path.clone();
        let folder = if dir { path } else { path.parent().map(|p| p.to_path_buf()).unwrap_or(path) };
        ed.list_side = crate::editor::Side::Left;
        ed.set_state(crate::editor::State::List);
        let m = crate::menu::Menu::new(row, col).keys(if dir { "開く・畳む" } else { "開く" }, &[Enter]);
        let m = if dir { m } else { m.keys("開いて木に残る", &[Char('o')]) };
        let m = m.sep().item("このフォルダを検索…（Find in Path）", move |e| e.open_grep_in(folder));
        let m = m.sep().keys("読み直す", &[Char('R')]).keys("木を閉じる", &[Char('q')]);
        ed.open_menu(m);
        true
    }
}


/// 左の欄の 1 行目のタブ（layout.rs の View）
pub struct LeftTabsView;

impl crate::layout::View for LeftTabsView {
    fn rect(&self, ed: &crate::editor::Editor, s: crate::layout::Screen) -> Option<crate::panes::Rect> {
        let _ = s;
        ed.left_rect().map(|r| crate::panes::Rect { h: 1, ..r })
    }

    fn click(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, row: usize, col: usize, clicks: u32) -> crate::layout::Click {
        let _ = (r, row, col, clicks);
        // 見出しを押したら、その欄へ移る（タブを替えるのは左のアイコンの列）
        if let Some(tab) = ed.left_visible() {
            ed.select_left_tab(tab);
        }
        crate::layout::Click::Done
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
        p.out.push(Instance::fill(x, y0, w, r.h as f32 * cell.h, PANEL_BG));
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
                selection(p, x, y, w, cell.h, LIST_SEL_BG);
            } else if selected {
                p.out.push(Instance::fill(x, y, w, cell.h, MATCH_BG));
            }
            let mark = if row.dir { if t.is_expanded(&row.path) { "▾ " } else { "▸ " } } else { "  " };
            let icon_col = r.x + row.depth * 2 + 2;
            let state = git.get(&row.path).copied();
            let fg = match state {
                _ if reversed => FG_BRIGHT,
                Some(FileState::Added) => GIT_ADD,
                Some(FileState::Modified | FileState::Renamed) => GIT_MOD,
                Some(FileState::Untracked) => GIT_DEL,
                Some(FileState::Conflicted) => DIAG_WARN,
                Some(FileState::Deleted) => STATUS_FG,
                None => FG,
            };
            // 横に送っていれば、字下げ・印・アイコン・名前を左端の分だけ左へ（左端より左は描かない）
            let left = t.left;
            let label: String = format!("{}{mark}", "  ".repeat(row.depth)).chars().skip(left).collect();
            // 字下げと ▸ ▾ はセルの位置で（アイコンと名前をそろえる）
            let ui = std::mem::replace(&mut p.ui, false);
            p.text(&label, r.x, y, if reversed { FG_BRIGHT } else { STATUS_FG });
            p.ui = ui;
            let icon_at = row.depth * 2 + 2;
            if icon_at >= left && icon_col - left + 3 < r.x + r.w {
                draw_tree_icon(p, &tree_icon(row, index, &t.root), icon_col - left, y);
            }
            let name_at = icon_at + 2;
            let mut items = p.items(&row.name, fg);
            crate::view::skip_cols(&mut items, left.saturating_sub(name_at), &mut None);
            p.draw(&items, r.x + name_at.saturating_sub(left), y);
        }
        p.cols = saved;
    }

    /// 木のアイコン
    pub(crate) enum TreeIcon {
        /// IntelliJ のアイコン（icons.rs の名前）
        Svg(&'static str),
        /// IntelliJ にアイコンの無い種類のファイル（文字・地の色・文字の色）
        Badge(&'static str, [f32; 4], [f32; 4]),
    }

    pub(crate) fn tree_icon(row: &crate::tree_panel::Row, index: Option<&nagi_workers::index::Index>, root: &std::path::Path) -> TreeIcon {
        let name = row.name.to_ascii_lowercase();
        if row.dir {
            let rel = row.path.strip_prefix(root).map(|r| r.to_string_lossy().replace('\\', "/").to_ascii_lowercase()).unwrap_or_default();
            let icon = if matches!(name.as_str(), "target" | "out" | "dist" | "build" | "node_modules" | "bin" | ".angular" | ".next") {
                "n_excludeRoot"
            } else if rel.ends_with("src/test/java") || rel.ends_with("src/test/kotlin") || matches!(name.as_str(), "test" | "tests" | "__tests__" | "e2e" | "cypress" | "spec") {
                "n_testRoot"
            } else if rel.ends_with("src/main/java") || rel.ends_with("src/main/kotlin") || (name == "src" && !row.path.join("main").is_dir()) {
                "n_sourceRoot"
            } else if rel.ends_with("src/main/resources") || rel.ends_with("src/test/resources") {
                "n_resourcesRoot"
            } else {
                "n_folder"
            };
            return TreeIcon::Svg(icon);
        }
        let dark = BG;
        let white = rgb(0xffffff);
        let ext = name.rsplit_once('.').map_or("", |x| x.1);
        let b = |l: &'static str, c: u32, light: bool| TreeIcon::Badge(l, rgb(c), if light { dark } else { white });
        match (name.as_str(), ext) {
            ("pom.xml", _) => b("m", 0xc71a36, false),
            ("package.json", _) => b("n", 0xcb3837, false),
            ("dockerfile", _) => TreeIcon::Svg("f_docker"),
            (".gitignore" | ".gitattributes", _) => TreeIcon::Svg("f_gitignore"),
            (_, "java") => {
                let rel = row.path.strip_prefix(root).map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                match index.and_then(|i| i.file_type_kind(&rel)) {
                    Some(nagi_syntax::defs::DefKind::Interface) => TreeIcon::Svg("n_interface"),
                    Some(nagi_syntax::defs::DefKind::Enum) => TreeIcon::Svg("n_enum"),
                    Some(nagi_syntax::defs::DefKind::Class) => TreeIcon::Svg("n_class"),
                    _ => TreeIcon::Svg("f_java"),
                }
            }
            (_, "kt" | "kts") => b("K", 0xa97bff, false),
            (_, "ts" | "mts" | "cts") => TreeIcon::Svg("f_typeScript"),
            (_, "tsx") => b("TS", 0x61dafb, true),
            (_, "js" | "mjs" | "cjs") => TreeIcon::Svg("f_javaScript"),
            (_, "jsx") => b("JS", 0x61dafb, true),
            (_, "html" | "htm") => TreeIcon::Svg("f_html"),
            (_, "css") => TreeIcon::Svg("f_css"),
            (_, "scss" | "sass") => b("S", 0xcd6799, false),
            (_, "json" | "jsonc") => TreeIcon::Svg("f_json"),
            (_, "md" | "markdown") => TreeIcon::Svg("f_markdown"),
            (_, "xml" | "xsd" | "wsdl") => TreeIcon::Svg("f_xml"),
            (_, "yml" | "yaml") => TreeIcon::Svg("f_yaml"),
            (_, "properties" | "ini" | "toml" | "conf" | "cfg" | "env") => TreeIcon::Svg("f_properties"),
            (_, "sql") => TreeIcon::Svg("f_sql"),
            (_, "rs") => b("R", 0xdea584, true),
            (_, "py") => b("Py", 0x3572a5, false),
            (_, "sh" | "bash" | "bat" | "cmd" | "ps1") => TreeIcon::Svg("f_shell"),
            (_, "png" | "jpg" | "jpeg" | "gif" | "svg" | "ico" | "webp") => TreeIcon::Svg("f_image"),
            (_, "vim") => b("V", 0x199f4b, false),
            (_, "txt" | "log" | "csv") => TreeIcon::Svg("f_text"),
            _ => TreeIcon::Svg("f_unknown"),
        }
    }

    /// アイコンを列 `col`・行の y に描く（2 列ぶん。中は 16px ほど）
    pub(crate) fn draw_tree_icon(p: &mut Painter, icon: &TreeIcon, col: usize, y: f32) {
        let cell = p.fonts.cell();
        match icon {
            TreeIcon::Svg(name) => {
                let px = crate::icons::small(cell.h);
                let x = col as f32 * cell.w + ((2.0 * cell.w - px) / 2.0).max(0.0) - 2.0;
                crate::icons::draw(p, name, x, crate::icons::centered(y, cell.h, px), px, None);
            }
            TreeIcon::Badge(label, bg, fg) => {
                let s = (cell.h * 0.62).round().clamp(10.0, 26.0);
                let x = col as f32 * cell.w + ((2.0 * cell.w - s) / 2.0).max(0.0) - 2.0;
                let y = y + ((cell.h - s) / 2.0).round();
                p.out.push(Instance::fill(x, y, s, s, *bg));
                let n = label.chars().count() as f32;
                let scale = if n >= 2.0 { (s * 0.95 / (2.0 * cell.w)).min(0.5) } else { 0.62 };
                let tw = cell.w * scale * n;
                p.small_text(label, x + (s - tw) / 2.0, y + (s - cell.h * scale) / 2.0, scale, *fg);
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
        assert_eq!(e.state(), State::List);
        // 本文の 2 行目をクリック
        let r = e.active_rect();
        e.click(1, r.x + e.gutter() + 1);
        assert_eq!(e.state(), State::Normal, "本文に戻る");
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
        e.set_state(State::Normal);
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
        assert_eq!(e.state(), State::List);
        let a = e.area();
        assert_eq!(e.windows_x(), (a.x + e.left_width() + 1, a.w - e.left_width() - 1), "ツリーの分だけ窓が右へ寄る");
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
        assert_eq!(e.state(), State::Normal);
        assert_eq!(e.buf.line(0), "main");
        // Ctrl-w h でツリーへ戻り、q で閉じる
        keys(&mut e, "<C-w>h");
        assert_eq!(e.state(), State::List);
        keys(&mut e, "q");
        assert!(e.tree.is_none());
        assert_eq!(e.windows_x(), (a.x, a.w));
    }
}
