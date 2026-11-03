//! git の画面の 3 段目: 左右並べの差分（IntelliJ の差分の画面と同じ形）。
//!
//! エディタのタブ（「名前（差分）」）として窓の領域に出す。ほかのタブ・本文へ移っても閉じずにタブに残り、押すと戻る。
//! 左が HEAD（履歴から開いたときは 1 つ前の版）、右が作業中のファイル（またはその版）。
//! 足した・消した・変えた行を色分けし、対になる行は変わった部分を濃く塗る。本文は構文の色で出す。
//! 見出しの右の「横並びビューアー ▾」で、左右に並べるか 1 列にまとめる（統合ビューアー）かを選ぶ（設定に覚える）。
//! 作業中のファイルとの差分では、右クリックでその変更をステージ・行のロールバック。

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
    /// 出している行（横並びなら `pairs` と同じ、統合なら消した行・足した行を 1 列に）
    pub rows: Vec<Row>,
    /// 左右に並べた行
    pairs: Vec<Row>,
    /// 統合ビューアー（1 列）で出しているか
    pub unified: bool,
    /// タブに残して隠しているか（本文・ほかのタブへ移った）
    pub hidden: bool,
    /// 構文の色（古い版・新しい版の行ごとの (始め, 終わり, 色の種類)。行の中の byte）
    pub hl_old: Vec<Vec<(usize, usize, nagi_syntax::Hl)>>,
    pub hl_new: Vec<Vec<(usize, usize, nagi_syntax::Hl)>>,
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

/// 統合ビューアーの行番号の欄の幅（古い版と新しい版の 2 つ）
pub const UNI_NUM_W: usize = 2 * NUM_W;

/// 見出しの右のビューアーの切り替え
const VIEWER_CHIP: &str = " ▾";

/// バッファのタブの並びで差分のタブを表す番号（buffer_tabs.rs）
pub const DIFF_TAB: usize = usize::MAX;

/// 差分の画面のアクション（ui_action.rs。ADR 0019）
pub static ACTIONS: [crate::ui_action::UiAction; 8] = {
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
        UiAction { id: "diff.sideBySide", label: "横並びビューアー", scope: Diff, enabled: open, run: |e| e.diff_set_unified(false) },
        UiAction { id: "diff.unified", label: "統合ビューアー", scope: Diff, enabled: open, run: |e| e.diff_set_unified(true) },
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

/// 左右に並べた行を 1 列に（変更のまとまりは、消した行を先に、足した行を後に）
pub fn build_unified(pairs: &[Row]) -> Vec<Row> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < pairs.len() {
        let Some(h) = pairs[i].hunk else {
            out.push(pairs[i].clone());
            i += 1;
            continue;
        };
        let end = (i..pairs.len()).find(|&j| pairs[j].hunk != Some(h)).unwrap_or(pairs.len());
        for r in &pairs[i..end] {
            if let Some(l) = &r.left {
                out.push(Row { left: Some(l.clone()), right: None, kind: RowKind::Del, hunk: Some(h) });
            }
        }
        for r in &pairs[i..end] {
            if let Some(n) = &r.right {
                out.push(Row { left: None, right: Some(n.clone()), kind: RowKind::Add, hunk: Some(h) });
            }
        }
        i = end;
    }
    out
}

/// 各まとまりの最初の行（rows の番号）
fn hunk_starts(rows: &[Row]) -> Vec<usize> {
    let mut v = Vec::new();
    let mut last = None;
    for (i, r) in rows.iter().enumerate() {
        if r.hunk.is_some() && r.hunk != last {
            v.push(i);
        }
        last = r.hunk;
    }
    v
}

/// 中身の構文の色を行ごとに（`path` の拡張子で言語を決める。知らない言語なら空）
pub fn line_highlights(text: &str, path: &Path) -> Vec<Vec<(usize, usize, nagi_syntax::Hl)>> {
    let Some(lang) = nagi_syntax::Lang::detect(path) else { return Vec::new() };
    let Some(mut syn) = nagi_syntax::Syntax::new(lang) else { return Vec::new() };
    let mut buf = nagi_core::Buffer::from_str(text);
    syn.parse(&mut buf);
    let n = buf.line_count();
    let mut out = vec![Vec::new(); n];
    for (rg, hl) in syn.highlight(&buf, 0..buf.len_bytes()) {
        let mut l = buf.pos_of(rg.start).0;
        while l < n {
            let ls = buf.line_start(l);
            let le = if l + 1 < n { buf.line_start(l + 1) } else { buf.len_bytes() };
            if ls >= rg.end {
                break;
            }
            let (a, b) = (rg.start.max(ls) - ls, rg.end.min(le) - ls);
            if b > a {
                out[l].push((a, b, hl));
            }
            l += 1;
        }
    }
    out
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

    /// 2 つの中身の差分を開く（題の最後の語はファイルのパス。構文の色とタブの名前に使う）
    pub(crate) fn show_diff(&mut self, left_title: String, right_title: String, old: &str, new: &str, work: Option<(PathBuf, String, PathBuf)>) {
        let (pairs, _, hunks) = build_rows(old, new);
        if hunks.is_empty() {
            self.svc.notify.info("差分はありません");
        }
        let unified = self.settings.get("git.diff_viewer") == "unified";
        let rows = if unified { build_unified(&pairs) } else { pairs.clone() };
        let hunk_rows = hunk_starts(&rows);
        let path = PathBuf::from(right_title.split_whitespace().last().unwrap_or(""));
        let (hl_old, hl_new) = (line_highlights(old, &path), line_highlights(new, &path));
        let cursor = hunk_rows.first().copied().unwrap_or(0);
        // 前の差分のタブにいたなら、戻る先はそのまま
        let back = if self.panels.diff.as_ref().is_some_and(|d| !d.hidden) { self.diff_return.take() } else { Some(self.state()) };
        self.panels.diff = Some(DiffView { left_title, right_title, rows, pairs, unified, hidden: false, hl_old, hl_new, top: cursor.saturating_sub(5), cursor, col: 0, on_left: false, left: 0, hunk_rows, work, hunks, old_text: old.to_string() });
        self.diff_return = back;
        self.set_state(State::Diff);
    }

    /// 差分を窓に出しているか（タブに隠していない）
    pub fn diff_shown(&self) -> bool {
        self.panels.diff.as_ref().is_some_and(|d| !d.hidden)
    }

    /// 差分のタブを押した: 隠していた差分を出して向く
    pub(crate) fn diff_tab_select(&mut self) {
        let Some(d) = &mut self.panels.diff else { return };
        if d.hidden {
            d.hidden = false;
            self.diff_return = Some(State::Normal);
        }
        self.set_state(State::Diff);
    }

    /// 差分のタブの名前（右の題のファイルの名前）
    pub(crate) fn diff_tab_label(&self) -> Option<String> {
        let d = self.panels.diff.as_ref()?;
        let path = d.right_title.split_whitespace().last().unwrap_or("");
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        Some(format!("{name}（差分）"))
    }

    /// 横並び・統合ビューアーを切り替える（カーソルは同じ行のまま。設定に覚える）
    pub(crate) fn diff_set_unified(&mut self, on: bool) {
        let tw0 = self.windows_x().1;
        let Some(d) = &mut self.panels.diff else { return };
        if d.unified != on {
            let cur = d.rows.get(d.cursor).cloned();
            d.unified = on;
            d.rows = if on { build_unified(&d.pairs) } else { d.pairs.clone() };
            d.hunk_rows = hunk_starts(&d.rows);
            let same = |r: &Row, c: &Row| (c.right.is_some() && r.right.as_ref().map(|x| x.0) == c.right.as_ref().map(|x| x.0)) || (c.right.is_none() && r.left.as_ref().map(|x| x.0) == c.left.as_ref().map(|x| x.0));
            d.cursor = cur.and_then(|c| d.rows.iter().position(|r| same(r, &c))).unwrap_or(0);
            let rows = self.screen.1.saturating_sub(2).max(1);
            nagi_ui::list::fit(d.cursor, &mut d.top, rows);
            d.left = 0;
            let tw = if on { tw0.saturating_sub(UNI_NUM_W) } else { (tw0.saturating_sub(1) / 2).saturating_sub(NUM_W) };
            d.follow(tw.max(1));
        }
        self.set_setting("git.diff_viewer", if on { "unified" } else { "side" });
    }

    /// ビューアーを選ぶメニュー（見出しの右の ▾）
    fn diff_viewer_menu(&mut self, row: usize, col: usize) {
        let on = self.panels.diff.as_ref().is_some_and(|d| d.unified);
        let mark = |b: bool| if b { "✓ " } else { "   " };
        let m = crate::menu::Menu::new(row, col)
            .item(format!("{}横並びビューアー", mark(!on)), |e| e.diff_set_unified(false))
            .item(format!("{}統合ビューアー", mark(on)), |e| e.diff_set_unified(true));
        self.open_menu(m);
    }

    pub(crate) fn close_diff(&mut self) {
        let shown = self.diff_shown();
        self.panels.diff = None;
        if !shown {
            // 隠していた差分のタブを閉じた: 今の画面はそのまま
            self.diff_return = None;
            return;
        }
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

    /// 差分の本文の見える幅（左右の狭い方。行番号の欄を除く。統合ビューアーは 1 列）
    fn diff_text_w(&self) -> usize {
        if self.panels.diff.as_ref().is_some_and(|d| d.unified) {
            return self.windows_x().1.saturating_sub(UNI_NUM_W).max(1);
        }
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
        self.svc.notify.info("変更を戻しました（バッファで Ctrl+Z で取り消し）");
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
        if !ed.diff_shown() {
            return None;
        }
        // タブの行の下から
        let (x, w) = ed.windows_x();
        let y = ed.area().y + ed.tabs_row();
        Some(crate::panes::Rect { x, y, w, h: ed.windows_rows().saturating_sub(y) })
    }

    fn draw(&self, ed: &crate::editor::Editor, p: &mut crate::view::Painter, r: crate::panes::Rect, s: crate::layout::Screen) {
        let _ = (r, s);
        if let Some(d) = &ed.panels.diff {
            if d.unified {
                draw::draw_unified(p, d, r);
            } else {
                draw::draw_diff_view(p, d, r);
            }
            draw::draw_viewer_chip(p, d, r);
        }
    }

    fn click(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, at: crate::layout::At, clicks: u32) -> crate::layout::Click {
        let _ = clicks;
        // 窓を覆って出ている差分: 押したら差分へ向き、押した行・列へカーソルを置く（下の本文へは通さない）
        ed.set_state(crate::editor::State::Diff);
        // 見出しの右のビューアーの切り替え
        if at.row == 0 && at.col + viewer_chip_w() + 2 >= r.w {
            ed.diff_viewer_menu(at.screen.0 + 1, at.screen.1);
            return crate::layout::Click::Done;
        }
        let tw = ed.diff_text_w();
        if let Some(d) = &mut ed.panels.diff {
            if d.unified {
                if let Some(k) = at.row.checked_sub(1).map(|k| d.top + k).filter(|&k| k < d.rows.len()) {
                    d.cursor = k;
                    d.col = at.col.saturating_sub(UNI_NUM_W) + d.left;
                    d.col = d.shown_col();
                    d.follow(tw);
                }
                return crate::layout::Click::Done;
            }
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
            .sep()
            .action(ed, "diff.sideBySide")
            .action(ed, "diff.unified")
            .sep()
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
                let hl = if is_left { &d.hl_old } else { &d.hl_new };
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
                    let colors = line_colors(text, hl.get(*n));
                    for (b, g) in text.grapheme_indices(true) {
                        let width = p.cell_width(g);
                        let hit = span.as_ref().is_some_and(|s| b >= s.start && b < s.end);
                        if caret.is_some_and(|cc| cc >= at && cc < at + width) {
                            caret_item = Some(items.len());
                        }
                        at += width;
                        items.push(Item { g: Cow::Borrowed(g), width, fg: colors[b], bg: if hit { Some(word) } else { None }, underline: false });
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

    /// 行の byte ごとの文字色（構文の色。後から塗ったものが勝つ）
    pub(crate) fn line_colors(text: &str, hl: Option<&Vec<(usize, usize, nagi_syntax::Hl)>>) -> Vec<[f32; 4]> {
        let mut c = vec![FG; text.len() + 1];
        for &(a, b, h) in hl.into_iter().flatten() {
            let (a, b) = (a.min(text.len()), b.min(text.len()));
            c[a..b].fill(hl_color(h));
        }
        c
    }

    /// 見出しの右の「横並びビューアー ▾」
    pub(crate) fn draw_viewer_chip(p: &mut Painter, d: &crate::diff_view::DiffView, r: Rect) {
        let cell = p.fonts.cell();
        let label = format!("{}{}", if d.unified { "統合ビューアー" } else { "横並びビューアー" }, super::VIEWER_CHIP);
        let w = super::viewer_chip_w();
        let x = (r.x + r.w).saturating_sub(w + 1);
        let y = r.y as f32 * cell.h;
        p.out.push(Instance::fill(x as f32 * cell.w, y, (w + 1) as f32 * cell.w, cell.h - 1.0, BG));
        let saved = p.cols;
        p.cols = r.x + r.w;
        p.text(&label, x, y, STATUS_FG);
        p.cols = saved;
    }

    /// 統合ビューアー: 1 列に、古い版・新しい版の行番号と本文。消した行は赤、足した行は緑の地
    pub(crate) fn draw_unified(p: &mut Painter, d: &crate::diff_view::DiffView, r: Rect) {
        use crate::diff_view::RowKind;
        let cell = p.fonts.cell();
        let (x0, y0) = (r.x as f32 * cell.w, r.y as f32 * cell.h);
        p.out.push(Instance::fill(x0, y0, r.w as f32 * cell.w, r.h as f32 * cell.h, BG));
        let saved = p.cols;
        p.cols = r.x + r.w;
        p.text(&format!(" {}  →  {}", d.left_title, d.right_title), r.x, y0, FG);
        p.out.push(Instance::fill(x0, y0 + cell.h - 1.0, r.w as f32 * cell.w, 1.0, LINE_NO));
        let body = r.h.saturating_sub(1);
        for (k, row) in d.rows.iter().enumerate().skip(d.top).take(body) {
            let y = y0 + (1 + k - d.top) as f32 * cell.h;
            let bg = match row.kind {
                RowKind::Add => DIFF_ADD_BG,
                RowKind::Del => DIFF_DEL_BG,
                _ => BG,
            };
            p.out.push(Instance::fill(x0, y, r.w as f32 * cell.w, cell.h, bg));
            let cursor = k == d.cursor;
            let num = |n: Option<&(usize, String)>| n.map_or("      ".to_string(), |x| format!("{:>5} ", x.0 + 1));
            let nums = format!("{}{}", num(row.left.as_ref()), num(row.right.as_ref()));
            if cursor {
                p.out.push(Instance::fill(x0, y, (super::UNI_NUM_W - 1) as f32 * cell.w, cell.h, FG));
            }
            let c = p.text(&nums, r.x, y, if cursor { BG } else { LINE_NO });
            let (text, hl) = match (&row.right, &row.left) {
                (Some(n), _) => (n, d.hl_new.get(n.0)),
                (None, Some(o)) => (o, d.hl_old.get(o.0)),
                _ => continue,
            };
            let colors = line_colors(&text.1, hl);
            let caret = cursor.then(|| d.shown_col());
            let mut caret_item = None;
            let mut items = Vec::new();
            let mut at = 0;
            for (b, g) in text.1.grapheme_indices(true) {
                let width = p.cell_width(g);
                if caret.is_some_and(|cc| cc >= at && cc < at + width) {
                    caret_item = Some(items.len());
                }
                at += width;
                items.push(Item { g: Cow::Borrowed(g), width, fg: colors[b], bg: None, underline: false });
            }
            let gone = d.left > 0 && (skip_cols(&mut items, d.left, &mut caret_item) || at < d.left);
            if let Some(i) = caret_item {
                items[i].bg = Some(CURSOR);
                items[i].fg = BG;
            }
            let end = p.draw(&items, c, y);
            if caret.is_some() && caret_item.is_none() && !gone && end < r.x + r.w {
                p.out.push(Instance::fill(end as f32 * cell.w, y, cell.w, cell.h, CURSOR));
            }
        }
        p.cols = saved;
    }
}

/// 見出しの右のビューアーの切り替えの幅（長い方に合わせる）
fn viewer_chip_w() -> usize {
    nagi_core::display_col("横並びビューアー", "横並びビューアー".len(), false) + nagi_core::display_col(VIEWER_CHIP, VIEWER_CHIP.len(), false)
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
    /// `:` から戻ると差分へ。本文に向く（木で開いた・飛んだ）とタブに隠れる（開いたものが覆いの下に隠れない）
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
        // 本文に向くとタブに隠れる
        e.set_state(State::Normal);
        assert!(e.panels.diff.is_some() && !e.diff_shown());
        // 左の欄を隠していても、差分の間に出すアイコンがある
        e.show_diff("a".into(), "b".into(), "a\n", "b\n", None);
        e.left_hidden = true;
        let row = e.left_stripe()[0].0;
        assert!(e.chrome_at(row, 0).is_some());
    }

    /// 差分はエディタのタブ: 開くとタブが増えて作業中のタブになり、ほかのタブを押すと隠れ、差分のタブで戻り、× で閉じる
    #[test]
    fn diff_is_an_editor_tab() {
        let dir = std::env::temp_dir().join(format!("nagi-difftab-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("A.java"), "class A {}\n").unwrap();
        let mut b = nagi_core::Buffer::open(&dir.join("A.java")).unwrap();
        b.path = Some(dir.join("A.java"));
        let mut e = Editor::new(b, None);
        e.settings.set("editor.tabs", "true", false);
        e.set_screen(120, 40);
        e.show_diff("HEAD  src/Main.java".into(), "作業中のファイル  src/Main.java".into(), "a\n", "b\n", None);
        let tabs = e.buffer_tabs(120);
        let t = tabs.iter().find(|t| t.doc == DIFF_TAB).expect("差分のタブ");
        assert_eq!(t.label, "Main.java（差分）");
        assert!(t.active && tabs.iter().filter(|t| t.active).count() == 1, "差分が作業中のタブ");
        assert_eq!(e.tabs_row(), 1, "差分の間もタブの行を出す");
        let r = crate::layout::View::rect(&DiffScreenView, &e, crate::layout::Screen { rows: 40, cols: 120 }).unwrap();
        assert_eq!(r.y, e.area().y + 1, "差分はタブの行の下");
        // ほかのタブを押すと隠れる（閉じない）
        let doc = tabs.iter().find(|t| t.doc != DIFF_TAB).unwrap().doc;
        e.tab_select(doc);
        assert!(!e.diff_shown() && e.panels.diff.is_some());
        assert!(e.buffer_tabs(120).iter().any(|t| t.doc == DIFF_TAB && !t.active));
        // 差分のタブで戻る
        e.tab_select(DIFF_TAB);
        assert!(e.diff_shown());
        assert_eq!(e.state(), State::Diff);
        // × で閉じる
        e.tab_close(DIFF_TAB);
        assert!(e.panels.diff.is_none());
        assert!(!e.buffer_tabs(120).iter().any(|t| t.doc == DIFF_TAB));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 統合ビューアー: 消した行を先に・足した行を後に 1 列で。切り替えてもカーソルは同じ行のまま、設定に覚える。構文の色を付ける
    #[test]
    fn unified_viewer_and_colors() {
        let (pairs, _, _) = build_rows("a\nb\nc\n", "a\nB\nx\nc\n");
        let u = build_unified(&pairs);
        let kinds: Vec<RowKind> = u.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, [RowKind::Eq, RowKind::Del, RowKind::Add, RowKind::Add, RowKind::Eq]);
        assert_eq!(hunk_starts(&u), [1]);
        let mut e = Editor::new(nagi_core::Buffer::from_str(""), None);
        e.set_screen(100, 30);
        e.show_diff("HEAD  A.java".into(), "作業中  A.java".into(), "class A {\n  int a = 1;\n}\n", "class A {\n  int a = 2;\n  int b;\n}\n", None);
        e.diff_key(Key::Down);
        let d = e.panels.diff.as_ref().unwrap();
        assert_eq!(d.rows[d.cursor].right.as_ref().map(|x| x.0), Some(2));
        e.diff_set_unified(true);
        let d = e.panels.diff.as_ref().unwrap();
        assert!(d.unified);
        assert_eq!(d.rows[d.cursor].right.as_ref().map(|x| x.0), Some(2), "同じ行のまま");
        assert_eq!(e.settings.get("git.diff_viewer"), "unified");
        // 次に開く差分も統合
        e.show_diff("a  A.java".into(), "b  A.java".into(), "x\n", "y\n", None);
        assert!(e.panels.diff.as_ref().unwrap().unified);
        e.diff_set_unified(false);
        assert_eq!(e.settings.get("git.diff_viewer"), "side");
        // 構文の色: class はキーワード
        let hl = line_highlights("class A {}\n", Path::new("A.java"));
        assert!(hl[0].iter().any(|&(a, b, _)| (a, b) == (0, 5)), "{:?}", hl[0]);
        assert!(line_highlights("x\n", Path::new("a.unknownext")).is_empty());
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
