//! git の画面の 3 段目: 左右並べの差分（IntelliJ の差分の画面と同じ形）。
//!
//! 窓の領域全体に出す。左が HEAD（履歴から開いたときは 1 つ前の版）、右が作業中のファイル（またはその版）。
//! 足した・消した・変えた行を色分けし、対になる行は変わった部分を濃く塗る。作業中のファイルとの差分では
//! `u` でその変更を戻し、`s` でステージする。

use std::path::{Path, PathBuf};

use nagi_core::linediff::{Hunk, HunkKind, diff_lines, split_lines};
use nagi_vim::Key;

use crate::editor::{Editor, State};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RowKind {
    Eq,
    Add,
    Del,
    Mod,
}

#[derive(Clone, Debug)]
pub struct Row {
    /// (行番号 0 始まり, 本文)
    pub left: Option<(usize, String)>,
    pub right: Option<(usize, String)>,
    pub kind: RowKind,
    /// 変更のまとまりの番号
    pub hunk: Option<usize>,
}

pub struct DiffView {
    pub left_title: String,
    pub right_title: String,
    pub rows: Vec<Row>,
    pub top: usize,
    pub cursor: usize,
    /// カーソルの列（本文の表示の列。行の長さより右なら行末に出す）
    pub col: usize,
    /// カーソルを左（古い版）に置くか。右に行が無ければ左に出す
    pub on_left: bool,
    /// 本文の左端の列（長い行の右を見るために横に送る。左右とも同じだけ）
    pub left: usize,
    /// 各まとまりの最初の行（rows の番号）
    pub hunk_rows: Vec<usize>,
    /// 作業中のファイルとの差分なら (根, 相対パス, 絶対パス)。戻す・ステージができる
    pub work: Option<(PathBuf, String, PathBuf)>,
    hunks: Vec<Hunk>,
    old_text: String,
}

/// 差分の画面のキーで、部品の外に及ぶこと（Editor が行う。ADR 0018 の §5）
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffAction {
    None,
    Close,
    /// 作業中のファイルのその行を開く（ファイル, 行）
    OpenAt(Option<PathBuf>, Option<usize>),
}

/// 行番号の欄の幅（"{:>5} "）
pub const NUM_W: usize = 6;

/// 差分の画面のアクション（ui_action.rs。ADR 0019）
pub static ACTIONS: [crate::ui_action::UiAction; 6] = {
    use crate::ui_action::{Scope::Diff, UiAction};
    fn open(e: &Editor) -> bool {
        e.panels.diff.is_some()
    }
    fn work(e: &Editor) -> bool {
        e.panels.diff.as_ref().is_some_and(|d| d.work.is_some())
    }
    [
        UiAction { id: "diff.next", label: "次の差分", scope: Diff, enabled: open, run: |e| e.diff_hunk(true) },
        UiAction { id: "diff.prev", label: "前の差分", scope: Diff, enabled: open, run: |e| e.diff_hunk(false) },
        UiAction { id: "diff.stage", label: "この変更をステージ", scope: Diff, enabled: work, run: |e| e.diff_apply(true) },
        UiAction { id: "diff.revert", label: "行のロールバック", scope: Diff, enabled: work, run: |e| e.diff_apply(false) },
        UiAction { id: "diff.open", label: "ソースに移動", scope: Diff, enabled: open, run: |e| e.diff_key(Key::Enter) },
        UiAction { id: "diff.close", label: "閉じる", scope: Diff, enabled: open, run: |e| e.close_diff() },
    ]
};

impl DiffView {
    /// 差分の画面のキー（`rows` は見える行数、`tw` は本文の幅）。移動・横の移動・変更へ跳ぶはここで行い、外に及ぶことを返す
    pub fn key(&mut self, key: Key, rows: usize, tw: usize) -> DiffAction {
        // 標準のキーだけ（ADR 0019。次・前の変更、ステージ・戻すはアクション）
        let last = self.rows.len().saturating_sub(1);
        let moved = nagi_ui::list::nav(key, self.cursor, last + 1, rows);
        let act = match key {
            // 横の移動（読むだけでも、どこを見ているか分かるように列のカーソルを持つ）
            Key::Left => {
                self.col = self.shown_col().saturating_sub(1);
                DiffAction::None
            }
            Key::Right => {
                let t = self.cursor_text();
                self.col = (self.shown_col() + 1).min(nagi_core::display_col(t, t.len(), false).saturating_sub(1));
                DiffAction::None
            }
            // Home・End は行の頭・行末（本文と同じ）
            Key::Home => {
                self.col = 0;
                DiffAction::None
            }
            Key::End => {
                self.col = usize::MAX;
                DiffAction::None
            }
            _ if moved.is_some() => {
                self.cursor = moved.unwrap_or(self.cursor);
                DiffAction::None
            }
            Key::Esc => return DiffAction::Close,
            Key::Enter => {
                // その行を作業中のファイルで開く
                let target = self.rows[self.cursor].right.as_ref().map(|r| r.0).or_else(|| self.rows[..self.cursor].iter().rev().find_map(|r| r.right.as_ref().map(|x| x.0 + 1)));
                return DiffAction::OpenAt(self.work.as_ref().map(|w| w.2.clone()), target);
            }
            _ => DiffAction::None,
        };
        nagi_ui::list::fit(self.cursor, &mut self.top, rows);
        self.follow(tw);
        act
    }

    /// 次（`fwd`）・前の変更へ
    pub fn jump_hunk(&mut self, fwd: bool, rows: usize, tw: usize) {
        let to = if fwd { self.hunk_rows.iter().find(|&&r| r > self.cursor).copied() } else { self.hunk_rows.iter().rev().find(|&&r| r < self.cursor).copied() };
        if let Some(r) = to {
            self.cursor = r;
        }
        nagi_ui::list::fit(self.cursor, &mut self.top, rows);
        self.follow(tw);
    }

    /// カーソルの行で、カーソルを出す側の本文（右に無ければ左）
    pub fn cursor_text(&self) -> &str {
        let Some(r) = self.rows.get(self.cursor) else { return "" };
        let side = if self.on_left { r.left.as_ref().or(r.right.as_ref()) } else { r.right.as_ref().or(r.left.as_ref()) };
        side.map_or("", |s| s.1.as_str())
    }

    /// カーソルを出す列（行末より右は行末の文字へ）
    pub fn shown_col(&self) -> usize {
        let t = self.cursor_text();
        self.col.min(nagi_core::display_col(t, t.len(), false).saturating_sub(1))
    }

    /// カーソルの列が見えるように左端を送る（`w` は本文の見える幅）
    pub fn follow(&mut self, w: usize) {
        let c = self.shown_col();
        let w = w.max(1);
        if c < self.left {
            self.left = c;
        } else if c >= self.left + w {
            self.left = c + 1 - w;
        }
    }
}

/// 2 つの中身から、左右に並べた行を作る
pub fn build_rows(old: &str, new: &str) -> (Vec<Row>, Vec<usize>, Vec<Hunk>) {
    let (a, b) = (split_lines(old), split_lines(new));
    let hunks = diff_lines(&a, &b);
    let mut rows = Vec::new();
    let mut starts = Vec::new();
    let (mut i, mut j) = (0, 0);
    let eq = |rows: &mut Vec<Row>, i: &mut usize, j: &mut usize, until_i: usize| {
        while *i < until_i {
            rows.push(Row { left: Some((*i, a[*i].to_string())), right: Some((*j, b[*j].to_string())), kind: RowKind::Eq, hunk: None });
            *i += 1;
            *j += 1;
        }
    };
    for (k, h) in hunks.iter().enumerate() {
        eq(&mut rows, &mut i, &mut j, h.old.start);
        starts.push(rows.len());
        let (lo, ln) = (h.old.len(), h.new.len());
        let kind = match h.kind {
            HunkKind::Add => RowKind::Add,
            HunkKind::Del => RowKind::Del,
            HunkKind::Mod => RowKind::Mod,
        };
        for r in 0..lo.max(ln) {
            let left = (r < lo).then(|| (h.old.start + r, a[h.old.start + r].to_string()));
            let right = (r < ln).then(|| (h.new.start + r, b[h.new.start + r].to_string()));
            // 変えたまとまりで片方しか無い行は、足した・消した行として塗る
            let kind = match (kind, &left, &right) {
                (RowKind::Mod, None, _) => RowKind::Add,
                (RowKind::Mod, _, None) => RowKind::Del,
                (k, _, _) => k,
            };
            rows.push(Row { left, right, kind, hunk: Some(k) });
        }
        i = h.old.end;
        j = h.new.end;
    }
    eq(&mut rows, &mut i, &mut j, a.len());
    (rows, starts, hunks)
}

impl Editor {
    /// 作業中のファイル `rel` と HEAD の差分を開く（コミットのパネルの d、:Gdiff）
    pub(crate) fn open_file_diff(&mut self, root: &Path, rel: &str) {
        let path = root.join(rel);
        // 開いているバッファがあればその中身（保存前の変更も見せる）、無ければファイル
        let new = match self.find_doc(&path) {
            Some(d) if d == self.core.cur_doc => self.core.buf.text(),
            Some(d) => self.parked_doc(d).map(|x| x.buf.text()).unwrap_or_default(),
            None => std::fs::read_to_string(&path).unwrap_or_default(),
        };
        // HEAD の版は裏で読む（ADR 0018 の §4）
        let (root, rel) = (root.to_path_buf(), rel.to_string());
        let (r2, rel2) = (root.clone(), rel.clone());
        self.job("git-blob", move || nagi_workers::git::head_blob(&r2, &rel2).unwrap_or_default(), move |e, old| {
            e.show_diff(format!("HEAD  {rel}"), format!("作業中のファイル  {rel}"), &old, &new, Some((root, rel.clone(), path)));
        });
    }

    /// 今のファイルの差分（`:Gdiff`）
    pub(crate) fn diff_current(&mut self) {
        let (Some(root), Some(p)) = (self.svc.git.root.clone(), self.core.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok())) else {
            self.svc.notify.info("git のリポジトリのファイルではありません");
            return;
        };
        let Ok(rel) = p.strip_prefix(&root) else { return };
        let rel = rel.to_string_lossy().replace('\\', "/");
        self.open_file_diff(&root, &rel);
    }

    /// 2 つの中身の差分を開く
    pub(crate) fn show_diff(&mut self, left_title: String, right_title: String, old: &str, new: &str, work: Option<(PathBuf, String, PathBuf)>) {
        let (rows, hunk_rows, hunks) = build_rows(old, new);
        if hunks.is_empty() {
            self.svc.notify.info("差分はありません");
        }
        let cursor = hunk_rows.first().copied().unwrap_or(0);
        self.panels.diff = Some(DiffView { left_title, right_title, rows, top: cursor.saturating_sub(5), cursor, col: 0, on_left: false, left: 0, hunk_rows, work, hunks, old_text: old.to_string() });
        self.diff_return = Some(self.state());
        self.set_state(State::Diff);
    }

    fn close_diff(&mut self) {
        self.panels.diff = None;
        let back = match self.diff_return.take() {
            Some(m @ (State::List | State::Normal | State::Log)) => m,
            _ => State::Normal,
        };
        self.set_state(back);
    }

    /// 差分の画面のキー
    pub(crate) fn diff_key(&mut self, key: Key) {
        let rows = self.screen.1.saturating_sub(2).max(1);
        let tw = self.diff_text_w();
        let Some(d) = &mut self.panels.diff else {
            self.set_state(State::Normal);
            return;
        };
        // 部品の中で済むことは DiffView::key が行い、外に及ぶことを返す（ADR 0018 の §5）
        match d.key(key, rows, tw) {
            DiffAction::None => {}
            DiffAction::Close => self.close_diff(),
            DiffAction::OpenAt(path, line) => {
                self.close_diff();
                self.set_state(State::Normal);
                if let (Some(p), Some(l)) = (path, line) {
                    self.jump_to(&p, l, 0);
                }
            }
        }
    }

    /// 次（`fwd`）・前の変更へ
    pub(crate) fn diff_hunk(&mut self, fwd: bool) {
        let rows = self.screen.1.saturating_sub(2).max(1);
        let tw = self.diff_text_w();
        if let Some(d) = &mut self.panels.diff {
            d.jump_hunk(fwd, rows, tw);
        }
    }

    /// 差分の本文の見える幅（左右の狭い方。行番号の欄を除く）
    fn diff_text_w(&self) -> usize {
        (self.windows_x().1.saturating_sub(1) / 2).saturating_sub(NUM_W).max(1)
    }

    /// カーソルの変更を戻す（`stage` ならステージ）。作業中のファイルとの差分だけ
    pub(crate) fn diff_apply(&mut self, stage: bool) {
        let Some(d) = &self.panels.diff else { return };
        let Some((root, rel, path)) = d.work.clone() else {
            self.svc.notify.warn("この差分では戻せません（作業中のファイルとの差分だけ）");
            return;
        };
        let Some(k) = d.rows[d.cursor].hunk.or_else(|| d.hunk_rows.iter().rposition(|&r| r <= d.cursor)) else {
            self.svc.notify.info("カーソルの行に変更はありません");
            return;
        };
        let h = d.hunks[k].clone();
        let old_lines: Vec<String> = split_lines(&d.old_text)[h.old.clone()].iter().map(|s| s.to_string()).collect();
        if stage {
            let new_lines: Vec<String> = d.rows.iter().filter(|r| r.hunk == Some(k)).filter_map(|r| r.right.as_ref().map(|x| x.1.clone())).collect();
            let o: Vec<&str> = old_lines.iter().map(String::as_str).collect();
            let n: Vec<&str> = new_lines.iter().map(String::as_str).collect();
            let new_file = d.old_text.is_empty();
            let patch = nagi_workers::git::hunk_patch(&rel, h.old.start, &o, h.new.start, &n, new_file);
            // index に当てるのは裏で（ADR 0018 の §4）
            self.job("git-stage", move || nagi_workers::git::apply_cached(&root, &patch), |e, r| {
                match r {
                    Ok(()) => e.svc.notify.info("変更をステージしました"),
                    Err(err) => e.svc.notify.error(format!("ステージできません: {}", err.first_line())),
                }
                e.refresh_status();
            });
            return;
        }
        // 戻す: ファイルをバッファで開いて、その範囲を HEAD の行に置き換える（u で取り消せる）
        let keep = self.panels.diff.take();
        let ret = self.diff_return.take();
        self.edit_file(path.clone());
        let nl = self.core.buf.newline().to_string();
        let total = self.core.buf.line_count();
        let start = if h.new.start < total { self.core.buf.line_start(h.new.start) } else { self.core.buf.len_bytes() };
        let end = if h.new.end < total { self.core.buf.line_start(h.new.end) } else { self.core.buf.len_bytes() };
        let text: String = old_lines.iter().map(|l| format!("{l}{nl}")).collect();
        self.core.hist.begin();
        self.edit(start, end, &text);
        self.core.hist.close();
        self.svc.notify.info("変更を戻しました（バッファで u で取り消し）");
        drop(keep);
        // 差分を作り直して開き直す
        let new = self.core.buf.text();
        let (r2, rel2) = (root.clone(), rel.clone());
        self.job("git-blob", move || nagi_workers::git::head_blob(&r2, &rel2).unwrap_or_default(), move |e, old| {
            e.show_diff(format!("HEAD  {rel}"), format!("作業中のファイル  {rel}"), &old, &new, Some((root, rel.clone(), path)));
            e.diff_return = ret;
        });
    }
}



/// この部品の入口（component.rs）
pub struct DiffComponent;

impl crate::component::Component for DiffComponent {
    fn status(&self, ed: &crate::editor::Editor) -> Option<crate::component::Status> {
        let d = ed.panels.diff.as_ref();
        let n = d.map_or(0, |d| d.hunk_rows.len());
        let i = d.map_or(0, |d| d.hunk_rows.iter().rposition(|&r| r <= d.cursor).map_or(0, |i| i + 1));
        crate::component::Status::new(format!("-- 差分 --   変更 {i}/{n}"), "↑↓←→ 動く  Enter ソースに移動  右クリック ステージ・行のロールバック・次の差分  Esc 閉じる ")
    }

    fn key(&self, ed: &mut crate::editor::Editor, key: nagi_vim::Key) -> crate::editor::Effect {
        ed.diff_key(key);
        crate::editor::Effect::None
    }

    fn scope(&self, ed: &crate::editor::Editor) -> Option<crate::ui_action::Scope> {
        let _ = ed;
        Some(crate::ui_action::Scope::Diff)
    }
}


/// 差分の画面（layout.rs の View）
pub struct DiffScreenView;

impl crate::layout::View for DiffScreenView {
    fn mono(&self) -> bool {
        true
    }

    fn rect(&self, ed: &crate::editor::Editor, s: crate::layout::Screen) -> Option<crate::panes::Rect> {
        let _ = s;
        ed.panels.diff.as_ref()?;
        let (x, w) = ed.windows_x();
        Some(crate::panes::Rect { x, y: ed.area().y, w, h: ed.windows_rows().saturating_sub(ed.area().y) })
    }

    fn draw(&self, ed: &crate::editor::Editor, p: &mut crate::view::Painter, r: crate::panes::Rect, s: crate::layout::Screen) {
        let _ = (r, s);
        if let Some(d) = &ed.panels.diff {
            draw::draw_diff_view(p, d, r);
        }
    }

    fn click(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, at: crate::layout::At, clicks: u32) -> crate::layout::Click {
        let _ = clicks;
        // 窓を覆って出ている差分: 押したら差分へ向き、押した行・列へカーソルを置く（下の本文へは通さない）
        ed.set_state(crate::editor::State::Diff);
        let tw = ed.diff_text_w();
        if let Some(d) = &mut ed.panels.diff {
            let half = r.w.saturating_sub(1) / 2;
            let k = at.row.checked_sub(1).map(|k| d.top + k).filter(|&k| k < d.rows.len());
            if let Some(k) = k {
                d.cursor = k;
                d.on_left = at.col < half;
                let x0 = if d.on_left { 0 } else { half + 1 } + NUM_W;
                d.col = at.col.saturating_sub(x0) + d.left;
                d.col = d.shown_col();
                d.follow(tw);
            }
        }
        crate::layout::Click::Done
    }

    fn wheel(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, at: crate::layout::At, dy: isize, dx: isize) -> bool {
        let _ = at;
        let tw = ed.diff_text_w();
        let Some(d) = &mut ed.panels.diff else { return false };
        // 表示だけを送る（カーソルは動かさない。キーで動かすと見える範囲へ戻る）
        let body = r.h.saturating_sub(1);
        d.top = (d.top as isize + dy).clamp(0, d.rows.len().saturating_sub(body) as isize) as usize;
        if dx != 0 {
            let width = |s: &Option<(usize, String)>| s.as_ref().map_or(0, |x| nagi_core::display_col(&x.1, x.1.len(), false));
            let widest = d.rows.iter().skip(d.top).take(body).map(|x| width(&x.left).max(width(&x.right))).max().unwrap_or(0);
            nagi_ui::list::hwheel(&mut d.left, widest + 1, tw, dx);
        }
        true
    }

    fn context(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, at: crate::layout::At) -> bool {
        let (row, col) = at.screen;
        let _ = r;
        ed.set_state(crate::editor::State::Diff);
        let m = crate::menu::Menu::new(row, col)
            .action(ed, "diff.prev")
            .action(ed, "diff.next")
            .sep()
            .action(ed, "diff.stage")
            .action(ed, "diff.revert")
            .sep()
            .action(ed, "diff.open")
            .action(ed, "diff.close");
        ed.open_menu(m);
        true
    }
}

/// 描き方（view.rs の Painter で描く）
pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    /// 左右並べの差分（窓の領域全体）。足した行は緑、消した行は赤、変えた行は青の地、対の行は変わった部分を濃く
    pub(crate) fn draw_diff_view(p: &mut Painter, d: &crate::diff_view::DiffView, r: Rect) {
        use crate::diff_view::RowKind;
        let cell = p.fonts.cell();
        let (x0, y0) = (r.x as f32 * cell.w, r.y as f32 * cell.h);
        p.out.push(Instance::fill(x0, y0, r.w as f32 * cell.w, r.h as f32 * cell.h, BG));
        let half = r.w.saturating_sub(1) / 2;
        let rx = r.x + half + 1;
        // 題名はそれぞれの半分で切る（長いパスで左が右へはみ出していた）
        let saved = p.cols;
        p.cols = r.x + half;
        p.text(&format!(" {}", d.left_title), r.x, y0, STATUS_FG);
        p.cols = r.x + r.w;
        p.text(&format!(" {}", d.right_title), rx, y0, FG);
        p.cols = saved;
        p.out.push(Instance::fill(x0, y0 + cell.h - 1.0, r.w as f32 * cell.w, 1.0, LINE_NO));
        let body = r.h.saturating_sub(1);
        let mod_bg = rgb(0x17202e);
        let fill_bg = POPUP_BG;
        for (k, row) in d.rows.iter().enumerate().skip(d.top).take(body) {
            let y = y0 + (1 + k - d.top) as f32 * cell.h;
            let (lbg, rbg) = match row.kind {
                RowKind::Eq => (BG, BG),
                RowKind::Add => (fill_bg, DIFF_ADD_BG),
                RowKind::Del => (DIFF_DEL_BG, fill_bg),
                RowKind::Mod => (mod_bg, mod_bg),
            };
            p.out.push(Instance::fill(x0, y, half as f32 * cell.w, cell.h, lbg));
            p.out.push(Instance::fill(rx as f32 * cell.w, y, (r.w - half - 1) as f32 * cell.w, cell.h, rbg));
            let spans = match (&row.left, &row.right, row.kind) {
                (Some(l), Some(n), RowKind::Mod) => {
                    let (a, b) = nagi_core::linediff::changed_span(&l.1, &n.1);
                    (Some(a), Some(b))
                }
                _ => (None, None),
            };
            let cursor = k == d.cursor;
            // カーソルを出す側（右に行が無ければ左）
            let caret_left = d.on_left && row.left.is_some() || row.right.is_none();
            for (is_left, side, col, w, span, word) in [(true, &row.left, r.x, half, spans.0, DIFF_WORD_DEL_BG), (false, &row.right, rx, r.w - half - 1, spans.1, DIFF_WORD_BG)] {
                let saved = p.cols;
                p.cols = col + w;
                if let Some((n, text)) = side {
                    let num = format!("{:>5} ", n + 1);
                    let c = if cursor {
                        p.out.push(Instance::fill(col as f32 * cell.w, y, 5.0 * cell.w, cell.h, FG));
                        p.text(&num, col, y, BG)
                    } else {
                        p.text(&num, col, y, LINE_NO)
                    };
                    let mut items = Vec::new();
                    // カーソルの文字（読むだけでも、今の位置が分かるように反転で出す）
                    let caret = (cursor && caret_left == is_left).then(|| d.shown_col());
                    let mut caret_item = None;
                    let mut at = 0;
                    for (b, g) in text.grapheme_indices(true) {
                        let width = p.cell_width(g);
                        let hit = span.as_ref().is_some_and(|s| b >= s.start && b < s.end);
                        if caret.is_some_and(|cc| cc >= at && cc < at + width) {
                            caret_item = Some(items.len());
                        }
                        at += width;
                        items.push(Item { g: Cow::Borrowed(g), width, fg: FG, bg: if hit { Some(word) } else { None }, underline: false });
                    }
                    let gone = d.left > 0 && (skip_cols(&mut items, d.left, &mut caret_item) || at < d.left);
                    if let Some(i) = caret_item {
                        items[i].bg = Some(CURSOR);
                        items[i].fg = BG;
                    }
                    let end = p.draw(&items, c, y);
                    // 空の行: 行の頭に 1 セルの塗り
                    if caret.is_some() && caret_item.is_none() && !gone && end < col + w {
                        p.out.push(Instance::fill(end as f32 * cell.w, y, cell.w, cell.h, CURSOR));
                    }
                } else if cursor {
                    p.out.push(Instance::fill(col as f32 * cell.w, y, 5.0 * cell.w, cell.h, MATCH_BG));
                }
                p.cols = saved;
            }
        }
        p.out.push(Instance::fill((r.x + half) as f32 * cell.w + cell.w * 0.5, y0 + cell.h, 1.0, body as f32 * cell.h, LINE_NO));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_align() {
        let (rows, starts, _) = build_rows("a\nb\nc\nd\n", "a\nB\nc\nx\nd\n");
        let k: Vec<RowKind> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(k, vec![RowKind::Eq, RowKind::Mod, RowKind::Eq, RowKind::Add, RowKind::Eq]);
        assert_eq!(starts, vec![1, 3]);
        assert_eq!(rows[3].left, None);
        assert_eq!(rows[3].right.as_ref().map(|r| r.0), Some(3));
        // 変えた行が増えたとき、増えた分は足した行として
        let (rows, _, _) = build_rows("a\nb\n", "a\nB1\nB2\n");
        assert_eq!(rows.iter().map(|r| r.kind).collect::<Vec<_>>(), vec![RowKind::Eq, RowKind::Mod, RowKind::Add]);
    }

    /// 差分は窓の範囲だけを覆う: 欄に移っても残り、欄の大きさを変えられ、隠した欄を出せる。
    /// `:` から戻ると差分へ。本文に向く（木で開いた・飛んだ）と閉じる（開いたものが覆いの下に隠れない）
    #[test]
    fn diff_leaves_panels_usable() {
        let keys = |e: &mut Editor, k: &str| {
            for k in nagi_vim::parse_keys(k, " ") {
                e.key(k);
            }
        };
        let mut e = Editor::new(nagi_core::Buffer::from_str("x\n"), None);
        e.set_screen(120, 40);
        e.open_tree();
        e.show_diff("a".into(), "b".into(), "a\n", "b\n", None);
        let l = e.left_rect().unwrap();
        assert!(matches!(e.sep_at(5, l.x + l.w), Some(crate::resize::Sep::Size(s)) if s.key == "left_width"), "差分の間も左の欄の大きさを変えられる");
        // 欄に移っても差分は残る
        e.set_state(State::List);
        assert!(e.panels.diff.is_some());
        // : から戻ると差分へ
        e.enter_command("");
        keys(&mut e, "<Esc>");
        assert_eq!(e.state(), State::Diff);
        e.enter_command("");
        keys(&mut e, "echo 1<CR>");
        assert_eq!(e.state(), State::Diff);
        assert!(e.panels.diff.is_some());
        // 本文に向くと閉じる
        e.set_state(State::Normal);
        assert!(e.panels.diff.is_none());
        // 左の欄を隠していても、差分の間に出すアイコンがある
        e.show_diff("a".into(), "b".into(), "a\n", "b\n", None);
        e.left_hidden = true;
        let row = e.left_stripe()[0].0;
        assert!(e.chrome_at(row, 0).is_some());
    }

    /// 列のカーソル: ← → で動き（Home・End で行の頭・行末）、行末で止まり、長い行は左端が付いてくる。押した位置へ置く
    #[test]
    fn column_cursor() {
        let mut e = Editor::new(nagi_core::Buffer::from_str(""), None);
        e.set_screen(60, 20);
        let long = "y".repeat(80);
        e.show_diff("a".into(), "b".into(), "ab\n", &format!("ab\n{long}\n"), None);
        assert_eq!(e.state(), State::Diff);
        let d = e.panels.diff.as_ref().unwrap();
        assert_eq!((d.cursor, d.col), (1, 0), "最初の変更の行の頭");
        e.diff_key(Key::End);
        let tw = e.diff_text_w();
        let d = e.panels.diff.as_ref().unwrap();
        assert_eq!(d.shown_col(), 79);
        assert!(d.left > 0 && 79 < d.left + tw, "行末が見える: left {}", d.left);
        e.diff_key(Key::Right);
        assert_eq!(e.panels.diff.as_ref().unwrap().shown_col(), 79, "行末で止まる");
        e.diff_key(Key::Up);
        let d = e.panels.diff.as_ref().unwrap();
        assert_eq!((d.cursor, d.shown_col(), d.left), (0, 1, 1), "短い行では行末に出し、左端も付いてくる");
        e.diff_key(Key::Home);
        assert_eq!(e.panels.diff.as_ref().unwrap().left, 0);
        e.diff_key(Key::Right);
        assert_eq!(e.panels.diff.as_ref().unwrap().shown_col(), 1);
    }
}
