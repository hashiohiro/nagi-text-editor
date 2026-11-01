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

impl nagi_ui::TreeRow for Row {
    type Key = PathBuf;
    fn depth(&self) -> usize {
        self.depth
    }
    fn folder(&self) -> Option<&PathBuf> {
        self.dir.then_some(&self.path)
    }
}

pub struct TreePanel {
    pub root: PathBuf,
    /// 読み終えたフォルダの中身
    children: HashMap<PathBuf, Vec<(String, bool)>>,
    /// 展開しているフォルダ
    folds: nagi_ui::Folds<PathBuf>,
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
}

/// 木の上の見出しの行数（描き方・押した行・見える範囲で使う）
pub const HEAD: usize = 1;

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

/// 木の字下げ 1 段の列の数（▸ ▾ のアイコンの幅。metrics.rs）
pub fn indent_cols() -> usize {
    crate::metrics::icon_cols(crate::metrics::Icon::Small).max(2)
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
            let open = *is_dir && self.folds.is_open(&path);
            out.push(Row { path: path.clone(), name: name.clone(), depth, dir: *is_dir });
            if open {
                self.push_children(&path, depth + 1, out);
            }
        }
    }

    pub fn is_expanded(&self, p: &Path) -> bool {
        self.folds.is_open(&p.to_path_buf())
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
        self.folds.set_open(&dir.to_path_buf(), true);
        self.load(dir, waker);
        self.rebuild();
    }

    /// 選びたいファイルまで、届いているフォルダを展開して進める。開いているファイルのパスは大文字・小文字が木と違うことがある
    /// （Windows。打った名前・別の道具から来たパス）ので、根と各段の名前は大文字・小文字を区別せずに合わせ、木の名前で進む
    fn advance_reveal(&mut self, waker: &nagi_workers::Waker) {
        let Some(target) = self.reveal.clone() else { return };
        let Some(parts) = rel_parts(&target, &self.root) else {
            self.reveal = None;
            return;
        };
        let mut dir = self.root.clone();
        for (i, c) in parts.iter().enumerate() {
            let Some(list) = self.children.get(&dir) else {
                self.load(&dir, waker);
                return;
            };
            let Some(name) = list.iter().find(|(n, _)| n == c).or_else(|| list.iter().find(|(n, _)| n.to_lowercase() == c.to_lowercase())).map(|(n, _)| n.clone()) else { break };
            let next = dir.join(name);
            if i + 1 == parts.len() {
                self.rebuild();
                if let Some(k) = self.rows.iter().position(|r| r.path == next) {
                    self.selected = k;
                }
                break;
            }
            self.folds.set_open(&next, true);
            dir = next;
        }
        self.reveal = None;
    }
}

/// `p` の `root` からの名前の並び（大文字・小文字を区別せずに根を合わせる）。根の外なら None
fn rel_parts(p: &Path, root: &Path) -> Option<Vec<String>> {
    let low = |c: std::path::Component| c.as_os_str().to_string_lossy().to_lowercase();
    let mut it = p.components();
    for r in root.components() {
        if low(it.next()?) != low(r) {
            return None;
        }
    }
    Some(it.map(|c| c.as_os_str().to_string_lossy().into_owned()).collect())
}

/// 木のキーで、部品の外に及ぶこと（Editor が行う。ADR 0018 の §5）
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeAction {
    None,
    /// 本文へ戻る
    ToText,
    /// 木を閉じる
    Close,
    /// ファイルを開く（true なら木に残る）
    Open(PathBuf, bool),
    /// 選んでいる行を開く・畳む（ファイルなら開いて、true なら木に残る）
    Activate(bool),
    /// 読み直す
    Reload,
}

/// プロジェクトの木のアクション（ui_action.rs。ADR 0019）
pub static ACTIONS: [crate::ui_action::UiAction; 4] = {
    use crate::ui_action::{Scope::ProjectTree, UiAction};
    fn open(e: &Editor) -> bool {
        e.panels.tree.is_some()
    }
    fn file(e: &Editor) -> bool {
        e.panels.tree.as_ref().and_then(|t| t.rows.get(t.selected)).is_some_and(|r| !r.dir)
    }
    fn selected(e: &Editor) -> bool {
        e.panels.tree.as_ref().is_some_and(|t| t.selected < t.rows.len())
    }
    [
        UiAction { id: "tree.open", label: "開く・畳む", scope: ProjectTree, enabled: selected, run: |e| e.tree_do(TreeAction::Activate(false)) },
        UiAction { id: "tree.openKeep", label: "開いて木に残る", scope: ProjectTree, enabled: file, run: |e| e.tree_do(TreeAction::Activate(true)) },
        UiAction { id: "tree.reload", label: "読み直す", scope: ProjectTree, enabled: open, run: |e| e.tree_do(TreeAction::Reload) },
        UiAction { id: "tree.close", label: "木を閉じる", scope: ProjectTree, enabled: open, run: |e| e.tree_do(TreeAction::Close) },
    ]
};

impl TreePanel {
    /// 木のキー（`half` は半画面の行数）。部品の中で済むこと（移動・開閉・読み直し）はここで行い、外に及ぶことを返す
    /// 木の上は標準のキーだけ（ADR 0019。読み直す・閉じるなどはアクション: 右クリックのメニュー・パレット・付けたショートカット）
    pub fn key(&mut self, key: Key, half: usize, waker: &nagi_workers::Waker) -> TreeAction {
        let moved = nagi_ui::list::nav(key, self.selected, self.rows.len(), half * 2);
        match key {
            _ if moved.is_some() => self.selected = moved.unwrap_or(self.selected),
            Key::Enter => return self.activate(false, waker),
            Key::Right => {
                // → はフォルダを開くだけ（開いているフォルダ・ファイルでは何もしない）
                if let Some(row) = self.rows.get(self.selected).filter(|r| r.dir && !self.folds.is_open(&r.path)).cloned() {
                    self.expand(&row.path, waker);
                }
            }
            // 開いたフォルダなら畳む、それ以外は親のフォルダへ（木の共通の動き。nagi-ui）
            Key::Left => match nagi_ui::tree::left(&self.rows, self.selected, &self.folds) {
                nagi_ui::Step::Fold(k, open) => {
                    self.folds.set_open(&k, open);
                    self.rebuild();
                }
                nagi_ui::Step::Select(i) => self.selected = i,
                nagi_ui::Step::None => {}
            },
            Key::Esc => return TreeAction::ToText,
            _ => {}
        }
        TreeAction::None
    }

    /// 選んでいる行: フォルダなら開く・畳む、ファイルなら開く（`stay` なら木に残る）
    pub fn activate(&mut self, stay: bool, waker: &nagi_workers::Waker) -> TreeAction {
        let Some(row) = self.rows.get(self.selected).cloned() else { return TreeAction::None };
        if !row.dir {
            return TreeAction::Open(row.path, stay);
        }
        if self.folds.is_open(&row.path) {
            self.folds.set_open(&row.path, false);
            self.rebuild();
        } else {
            self.expand(&row.path, waker);
        }
        TreeAction::None
    }

    /// 読み直す（開いているフォルダも）
    pub fn reload(&mut self, waker: &nagi_workers::Waker) {
        self.children.clear();
        let root = self.root.clone();
        self.load(&root, waker);
        let open: Vec<PathBuf> = self.folds.flipped().cloned().collect();
        for d in open {
            self.load(&d, waker);
        }
    }
}

impl Editor {
    /// 開いているファイルに木を合わせる（IntelliJ の Always Select Opened File）。木が出ていて、木にいないとき、
    /// 開いているファイルが変わっていたら、そこまで展開して選ぶ（描く前に呼ぶ）
    pub(crate) fn sync_tree(&mut self) {
        if !self.tree_sync || self.left_hidden || self.left_tab != LeftTab::Project || (self.state() == State::List && self.list_side == crate::editor::Side::Left) {
            return;
        }
        let current = self.core.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok());
        let waker = self.waker_or_noop();
        let Some(t) = &mut self.panels.tree else { return };
        if current.is_none() || t.synced == current {
            return;
        }
        t.synced = current.clone();
        t.reveal = current;
        t.advance_reveal(&waker);
    }

    /// SelectInProjectView（Alt+F1）: 今のファイルを木で選ぶ（木を出して移る）
    pub(crate) fn select_in_tree(&mut self) {
        if let Some(t) = &mut self.panels.tree {
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
            LeftTab::Project => self.panels.tree.is_some(),
            LeftTab::Commit => self.panels.commit.is_some(),
            LeftTab::Run => self.panels.run_panel.is_some(),
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
        let current = self.core.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok());
        if self.panels.tree.is_none() {
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
                folds: nagi_ui::Folds::closed(),
                rows: Vec::new(),
                left: 0,
                selected: 0,
                scroll: 0,
                tx,
                rx,
                loading: Default::default(),
                reveal: None,
                synced: None,
            };
            t.load(&root, &waker);
            self.panels.tree = Some(t);
        }
        self.left_tab = LeftTab::Project;
        self.left_hidden = false;
        let t = self.panels.tree.as_mut().unwrap();
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
        self.panels.tree = None;
        if self.state() == State::List && self.list_side == crate::editor::Side::Left {
            self.set_state(State::Normal);
        }
        self.rows = self.text_rows(self.active_rect());
    }

    /// 届いたフォルダの中身を受け取る。変わったら true
    pub fn poll_tree(&mut self) -> bool {
        let waker = self.waker_or_noop();
        let Some(t) = &mut self.panels.tree else { return false };
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
        let Some(t) = &mut self.panels.tree else { return false };
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
        let Some(t) = &mut self.panels.tree else {
            self.set_state(State::Normal);
            return Effect::None;
        };
        // 部品の中で済むことは TreePanel::key が行い、外に及ぶことを返す（ADR 0018 の §5）
        let a = t.key(key, half, &waker);
        self.tree_do(a);
        Effect::None
    }

    /// 木の操作を行う（キー・アクションから）
    pub(crate) fn tree_do(&mut self, a: TreeAction) {
        let waker = self.waker_or_noop();
        match a {
            TreeAction::None => {}
            TreeAction::ToText => self.set_state(State::Normal),
            TreeAction::Close => self.close_tree(),
            TreeAction::Open(path, stay) => {
                self.edit_file(path);
                if stay {
                    self.list_side = crate::editor::Side::Left;
                }
                self.set_state(if stay { State::List } else { State::Normal });
            }
            TreeAction::Activate(stay) => {
                let Some(t) = &mut self.panels.tree else { return };
                let a = t.activate(stay, &waker);
                self.tree_do(a);
            }
            TreeAction::Reload => {
                if let Some(t) = &mut self.panels.tree {
                    t.reload(&waker);
                }
            }
        }
    }

    /// ツリーの中のクリック。`row` はツリーの中の行（0 は見出し）
    pub fn tree_click(&mut self, row: usize, clicks: u32) {
        self.list_side = crate::editor::Side::Left;
        self.set_state(State::List);
        let waker = self.waker_or_noop();
        let Some(t) = &mut self.panels.tree else { return };
        let len = t.rows.len();
        let Some(pick) = nagi_ui::list::pick_row(&mut t.selected, t.scroll, row, HEAD, len, clicks) else { return };
        let r = t.rows[t.selected].clone();
        if r.dir {
            // フォルダは 1 回のクリックで開閉する
            if t.folds.is_open(&r.path) {
                t.folds.set_open(&r.path, false);
                t.rebuild();
            } else {
                t.expand(&r.path, &waker);
            }
        } else if matches!(pick, nagi_ui::list::Pick::Accept(_)) {
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
        crate::component::Status::new("-- プロジェクト --", "Enter 開く  ←→ 開閉  右クリック 操作  Esc 窓へ ")
    }

    fn scope(&self, ed: &crate::editor::Editor) -> Option<crate::ui_action::Scope> {
        let _ = ed;
        Some(crate::ui_action::Scope::ProjectTree)
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
        ed.panels.tree.as_ref()?;
        ed.tree_rect()
    }

    fn draw(&self, ed: &crate::editor::Editor, p: &mut crate::view::Painter, r: crate::panes::Rect, s: crate::layout::Screen) {
        let _ = (r, s);
        if let Some(t) = &ed.panels.tree {
            let git = ed.tree_git_states();
            draw::draw_tree(p, t, r, ed.state() == crate::editor::State::List && ed.list_side == crate::editor::Side::Left, &git, ed.nav.index.as_ref());
        }
    }

    fn click(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, at: crate::layout::At, clicks: u32) -> crate::layout::Click {
        let _ = r;
        // at.row は木の中の行（0 は見出し）
        ed.tree_click(at.row, clicks);
        crate::layout::Click::Done
    }

    fn wheel(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, at: crate::layout::At, dy: isize, dx: isize) -> bool {
        let _ = at;
        if let Some(t) = &mut ed.panels.tree {
            let h = r.h.saturating_sub(HEAD);
            nagi_ui::list::wheel(&mut t.selected, &mut t.scroll, t.rows.len(), h, dy);
            if dx != 0 {
                let ind = crate::tree_panel::indent_cols();
                let widest = t.rows.iter().skip(t.scroll).take(h).map(|x| (x.depth + 1) * ind + crate::metrics::icon_cols(crate::metrics::Icon::Small) + nagi_core::display_col(&x.name, x.name.len(), false)).max().unwrap_or(0);
                nagi_ui::list::hwheel(&mut t.left, widest + 1, r.w, dx);
            }
        }
        true
    }

    fn context(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, at: crate::layout::At) -> bool {
        let (row, col) = at.screen;
        let _ = r;
        // 押した行を選ぶだけ（左クリックと違ってフォルダを開け閉めしない）
        let Some(t) = &mut ed.panels.tree else { return false };
        let len = t.rows.len();
        if nagi_ui::list::pick_row(&mut t.selected, t.scroll, at.row, crate::tree_panel::HEAD, len, 1).is_none() {
            return false;
        }
        let dir = t.rows[t.selected].dir;
        let path = t.rows[t.selected].path.clone();
        let folder = if dir { path } else { path.parent().map(|p| p.to_path_buf()).unwrap_or(path) };
        ed.list_side = crate::editor::Side::Left;
        ed.set_state(crate::editor::State::List);
        let m = crate::menu::Menu::new(row, col).action(ed, "tree.open").action(ed, "tree.openKeep");
        let m = m.sep().item("このフォルダを検索…（Find in Path）", move |e| e.open_grep_in(folder));
        let m = m.sep().action(ed, "tree.reload").action(ed, "tree.close");
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

    fn click(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, at: crate::layout::At, clicks: u32) -> crate::layout::Click {
        let _ = (at, r, clicks);
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
        for k in 0..r.h.saturating_sub(crate::tree_panel::HEAD) {
            let Some(row) = t.rows.get(t.scroll + k) else { break };
            let y = (r.y + crate::tree_panel::HEAD + k) as f32 * cell.h;
            let selected = t.scroll + k == t.selected;
            let reversed = selected && focused;
            if reversed {
                selection(p, x, y, w, cell.h, LIST_SEL_BG);
            } else if selected {
                p.out.push(Instance::fill(x, y, w, cell.h, MATCH_BG));
            }
            // 字下げ 1 段と ▸ ▾ の幅は、アイコンの幅（metrics.rs）
            let ind = crate::tree_panel::indent_cols();
            let icon_col = r.x + (row.depth + 1) * ind;
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
            // ▸ ▾ は IntelliJ の山形のアイコン
            let arrow_at = row.depth * ind;
            if row.dir && arrow_at >= left {
                let name = if t.is_expanded(&row.path) { "g_chevronDown" } else { "g_chevronRight" };
                let px = crate::metrics::icon(crate::metrics::Icon::Small);
                crate::icons::draw(p, name, (r.x + arrow_at - left) as f32 * cell.w, crate::metrics::centered(y, px), px, None);
            }
            let icon_at = (row.depth + 1) * ind;
            if icon_at >= left && icon_col - left + 3 < r.x + r.w {
                draw_tree_icon(p, &tree_icon(row, index, &t.root), icon_col - left, y);
            }
            let name_at = icon_at + crate::metrics::icon_cols(crate::metrics::Icon::Small);
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
                // アイコンの列（metrics::icon_cols）の左に寄せる。後ろの文字はその列の後から
                let px = crate::metrics::icon(crate::metrics::Icon::Small);
                let x = col as f32 * cell.w;
                crate::icons::draw(p, name, x, crate::metrics::centered(y, px), px, None);
            }
            TreeIcon::Badge(label, bg, fg) => {
                // 名前の無いアイコンの代わりの札も、ふつうのアイコンと同じ大きさの割合で
                let s = (crate::metrics::icon(crate::metrics::Icon::Small) * 0.8).round();
                let x = col as f32 * cell.w + ((crate::metrics::icon(crate::metrics::Icon::Small) - s) / 2.0).round();
                let y = y + ((cell.h - s) / 2.0).round();
                p.out.push(Instance::fill(x, y, s, s, *bg));
                let n = label.chars().count() as f32;
                let scale = if n >= 2.0 { (s * 0.95 / (n * cell.w)).min(s * 0.5 / cell.h) } else { s * 0.62 / cell.h };
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
            if e.panels.tree.as_ref().is_some_and(|t| !t.loading() && t.reveal.is_none()) {
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
        assert_eq!(e.core.cursor.line, 1);
        e.key(nagi_vim::Key::Char('j'));
        assert_eq!(e.core.cursor.line, 2, "キーは本文へ");
    }

    /// 画面の位置で押した行を選ぶ（木の上に帯があってもずれない）。タブを替えても木が合う
    #[test]
    fn click_by_screen_row_and_follow_tabs() {
        let dir = std::env::temp_dir().join(format!("nagi-tree-press-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        for f in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(dir.join(f), "x\n").unwrap();
        }
        let mut buf = Buffer::open(&dir.join("a.txt")).unwrap();
        buf.path = Some(dir.join("a.txt"));
        let mut e = Editor::new(buf, None);
        e.set_screen(120, 40);
        e.open_tree();
        settle(&mut e);
        let r = e.tree_rect().unwrap();
        assert!(r.y > 0, "木の上に帯がある前提: {r:?}");
        let s = crate::layout::Screen { rows: 40, cols: 120 };
        let name = |e: &Editor| e.panels.tree.as_ref().map(|t| t.rows[t.selected].name.clone()).unwrap();
        // b.txt の行を押して選び、もう一度押して（ダブルクリック）開く
        let at = e.panels.tree.as_ref().unwrap().rows.iter().position(|x| x.name == "b.txt").unwrap();
        crate::layout::press(&mut e, s, r.y + 1 + at, r.x + 3, |_| 1);
        assert_eq!(name(&e), "b.txt", "押した行を選ぶ");
        crate::layout::press(&mut e, s, r.y + 1 + at, r.x + 3, |_| 2);
        assert!(e.core.buf.path.as_ref().unwrap().ends_with("b.txt"));
        e.edit_file(dir.join("c.txt"));
        e.sync_tree();
        settle(&mut e);
        assert_eq!(name(&e), "c.txt");
        // タブで a.txt へ戻ると、木も a.txt へ
        let a = e.buffer_list().into_iter().find(|t| t.1.as_ref().is_some_and(|p| p.ends_with("a.txt"))).unwrap().0;
        e.tab_select(a);
        e.sync_tree();
        settle(&mut e);
        assert_eq!(name(&e), "a.txt");
        // 大文字・小文字の違うパスで開いても、木の行に合わせる
        e.edit_file(std::path::PathBuf::from(dir.to_string_lossy().to_uppercase()).join("B.TXT"));
        e.sync_tree();
        settle(&mut e);
        assert_eq!(name(&e), "b.txt");
        // 木の行を押して木にいるまま、タブで c.txt へ移っても、木は c.txt へ
        crate::layout::press(&mut e, s, r.y + 1 + at, r.x + 3, |_| 1);
        assert_eq!(e.state(), State::List);
        let c = e.buffer_list().into_iter().find(|t| t.1.as_ref().is_some_and(|p| p.ends_with("c.txt"))).unwrap().0;
        e.tab_select(c);
        e.sync_tree();
        settle(&mut e);
        assert_eq!(name(&e), "c.txt");
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
                if e.panels.tree.as_ref().is_some_and(|t| !t.loading() && t.reveal.is_none()) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        };
        settle(&mut e);
        let sel = |e: &Editor| e.panels.tree.as_ref().map(|t| t.rows[t.selected].path.clone()).unwrap();
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
        e.core.buf.path = Some(dir.join("src/view/panes.rs"));
        e.set_screen(120, 40);
        e.toggle_tree();
        settle(&mut e);
        assert_eq!(e.state(), State::List);
        let a = e.area();
        assert_eq!(e.windows_x(), (a.x + e.left_width() + 1, a.w - e.left_width() - 1), "ツリーの分だけ窓が右へ寄る");
        let t = e.panels.tree.as_ref().unwrap();
        let names: Vec<String> = t.rows.iter().map(|r| format!("{}{}", "  ".repeat(r.depth), r.name)).collect();
        // フォルダが先、.gitignore の target は出さない、.git も出さない。今のファイルまで展開して選ぶ
        assert_eq!(names, vec!["src", "  view", "    panes.rs", "  main.rs", ".gitignore", "README.md"]);
        assert_eq!(t.rows[t.selected].name, "panes.rs");
        // ← で親へ移る（畳まない。IntelliJ と同じ）、もう一度 ← で畳む
        keys(&mut e, "<Left>");
        let t = e.panels.tree.as_ref().unwrap();
        assert_eq!(t.rows[t.selected].name, "view");
        assert!(t.is_expanded(&dir.join("src/view")));
        keys(&mut e, "<Left>");
        assert!(!e.panels.tree.as_ref().unwrap().is_expanded(&dir.join("src/view")));
        // ↓ で main.rs、Enter で開いて編集画面へ
        keys(&mut e, "<Down><CR>");
        assert_eq!(e.state(), State::Normal);
        assert_eq!(e.core.buf.line(0), "main");
        // 本文（Vim）の Ctrl-w h でツリーへ戻り、「木を閉じる」で閉じる。木の上の q は何もしない
        keys(&mut e, "<C-w>h");
        assert_eq!(e.state(), State::List);
        keys(&mut e, "q");
        assert!(e.panels.tree.is_some(), "木の上の 1 文字キーは何もしない（ADR 0019）");
        assert!(e.run_ui_action("tree.close"));
        assert!(e.panels.tree.is_none());
        assert_eq!(e.windows_x(), (a.x, a.w));
    }
}
