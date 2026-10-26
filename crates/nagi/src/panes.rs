//! ウィンドウの分割（Vim の `:sp` `:vs` と `Ctrl-w`）。
//!
//! 作業中の窓のカーソルと表示位置は Editor の欄（cursor・top・frac）にそのまま置き、ほかの窓は `Win` として脇に置く。
//! 窓を移るときに入れ替えるのは、バッファ（buffers.rs）と同じ考え方。
//! 大きさは Vim の equalalways と同じく、窓を足したり閉じたりしたら同じ並びの窓で等分する。
//! 区切り（左右の間の線、上下の間の状態行）をマウスでドラッグすると比率が変わる（resize.rs）。

use nagi_core::Cursor;

use crate::buffers::Slot;
use crate::editor::{Editor, State};

/// 脇に置いた窓
#[derive(Clone, Copy)]
pub(crate) struct Win {
    pub doc: usize,
    pub cursor: Cursor,
    pub top: usize,
    /// 左端の列（横に送った位置）
    pub left: usize,
}

pub(crate) enum WinSlot {
    Active,
    Parked(Win),
    Closed,
}

/// 窓の並び。`vertical` は左右に並べる（`:vs`）
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Layout {
    Leaf(usize),
    /// `weights` は子の大きさの比率（等しければ等分）
    Split { vertical: bool, kids: Vec<Layout>, weights: Vec<f32> },
}

fn parse_layout(t: &[&str], i: &mut usize, n: usize) -> Option<Layout> {
    let tok = *t.get(*i)?;
    *i += 1;
    if tok != "(" {
        return tok.parse::<usize>().ok().filter(|w| *w < n).map(Layout::Leaf);
    }
    let vertical = match *t.get(*i)? {
        "v" => true,
        "h" => false,
        _ => return None,
    };
    *i += 1;
    let mut weights = Vec::new();
    while *t.get(*i)? != ":" {
        weights.push(t[*i].parse::<f32>().ok()?);
        *i += 1;
    }
    *i += 1;
    let mut kids = Vec::new();
    while *t.get(*i)? != ")" {
        kids.push(parse_layout(t, i, n)?);
    }
    *i += 1;
    (kids.len() == weights.len() && kids.len() >= 2).then_some(Layout::Split { vertical, kids, weights })
}

/// 並びの子の大きさ（セル）。等しい比率なら余りを前の子から 1 つずつ配る（前からの割り方と同じ）
pub(crate) fn split_sizes(avail: usize, weights: &[f32]) -> Vec<usize> {
    let n = weights.len();
    if weights.windows(2).all(|w| w[0] == w[1]) {
        return (0..n).map(|i| avail / n + usize::from(i < avail % n)).collect();
    }
    let total: f32 = weights.iter().sum();
    let mut acc = 0.0;
    let mut prev = 0;
    weights
        .iter()
        .enumerate()
        .map(|(i, w)| {
            acc += w;
            let end = if i + 1 == n { avail } else { ((avail as f32) * acc / total).round() as usize };
            let size = end.saturating_sub(prev);
            prev = end;
            size
        })
        .collect()
}

/// セル単位の矩形。分割中は最後の行が窓ごとの状態行
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Rect {
    pub fn contains(&self, row: usize, col: usize) -> bool {
        row >= self.y && row < self.y + self.h && col >= self.x && col < self.x + self.w
    }
}

impl Layout {
    /// 書き出す形（開き直すため。session.rs）: 窓は並びの順の番号、分割は `(v 1 1 : 0 1)`（向き・比率・子）
    pub(crate) fn render(&self, index: &dyn Fn(usize) -> usize) -> String {
        match self {
            Layout::Leaf(w) => index(*w).to_string(),
            Layout::Split { vertical, kids, weights } => {
                let ws: Vec<String> = weights.iter().map(|w| format!("{w}")).collect();
                let ks: Vec<String> = kids.iter().map(|k| k.render(index)).collect();
                format!("({} {} : {})", if *vertical { "v" } else { "h" }, ws.join(" "), ks.join(" "))
            }
        }
    }

    /// `render` の形を読む（窓の番号は `n` 未満）
    pub(crate) fn parse(s: &str, n: usize) -> Option<Layout> {
        let spaced = s.replace('(', " ( ").replace(')', " ) ");
        let toks: Vec<&str> = spaced.split_whitespace().collect();
        let mut i = 0;
        let l = parse_layout(&toks, &mut i, n)?;
        (i == toks.len()).then_some(l)
    }

    fn leaves(&self, out: &mut Vec<usize>) {
        match self {
            Layout::Leaf(w) => out.push(*w),
            Layout::Split { kids, .. } => kids.iter().for_each(|k| k.leaves(out)),
        }
    }

    /// `area` を等分して各窓の矩形を返す。左右の間には区切り線の 1 列を空ける
    fn rects(&self, area: Rect, out: &mut Vec<(usize, Rect)>) {
        match self {
            Layout::Leaf(w) => out.push((*w, area)),
            Layout::Split { vertical, kids, weights } => {
                let n = kids.len();
                let (total, gap) = if *vertical { (area.w, 1) } else { (area.h, 0) };
                let avail = total.saturating_sub(gap * (n - 1));
                let sizes = split_sizes(avail, weights);
                let mut at = 0;
                for (k, &size) in kids.iter().zip(&sizes) {
                    let r = if *vertical { Rect { x: area.x + at, w: size, ..area } } else { Rect { y: area.y + at, h: size, ..area } };
                    k.rects(r, out);
                    at += size + gap;
                }
            }
        }
    }

    /// 窓 `target` の隣に `new` を入れる（`before` なら前に）。同じ向きの並びの中ならその並びに足す
    fn insert(&mut self, target: usize, new: usize, vertical: bool, before: bool) -> bool {
        match self {
            Layout::Leaf(w) if *w == target => {
                let kids = if before { vec![Layout::Leaf(new), Layout::Leaf(target)] } else { vec![Layout::Leaf(target), Layout::Leaf(new)] };
                *self = Layout::Split { vertical, kids, weights: vec![1.0, 1.0] };
                true
            }
            Layout::Leaf(_) => false,
            Layout::Split { vertical: v, kids, weights } => {
                if *v == vertical {
                    if let Some(i) = kids.iter().position(|k| *k == Layout::Leaf(target)) {
                        kids.insert(if before { i } else { i + 1 }, Layout::Leaf(new));
                        // equalalways: 足したら等分に戻す
                        *weights = vec![1.0; kids.len()];
                        return true;
                    }
                }
                kids.iter_mut().any(|k| k.insert(target, new, vertical, before))
            }
        }
    }

    /// 窓 `w` を外す。並びに 1 つだけ残ったら、その並びをほどく
    fn remove(&mut self, w: usize) -> bool {
        let Layout::Split { kids, weights, .. } = self else { return false };
        let removed = if let Some(i) = kids.iter().position(|k| *k == Layout::Leaf(w)) {
            kids.remove(i);
            *weights = vec![1.0; kids.len()];
            true
        } else {
            kids.iter_mut().any(|k| k.remove(w))
        };
        if kids.len() == 1 {
            *self = kids.pop().unwrap();
        }
        removed
    }
}

impl Editor {
    /// 窓が 2 つ以上あるか（窓ごとの状態行を出す）
    pub fn is_split(&self) -> bool {
        matches!(self.layout, Layout::Split { .. })
    }

    /// 窓と矩形（画面の本文の範囲 `screen` を分けたもの）
    pub fn window_rects(&self) -> Vec<(usize, Rect)> {
        // 下にパネルを開いている間は、その分を除いた範囲を窓で分ける
        // 左にツリーを開いている間は、その分と区切りの 1 列を除く
        let ((x, cols), rows) = (self.windows_x(), self.windows_rows());
        // SQL コンソールの上の帯の分、1 行下げる
        let top = self.windows_top();
        let mut out = Vec::new();
        self.layout.rects(Rect { x, y: top, w: cols, h: rows.saturating_sub(top) }, &mut out);
        out
    }

    /// 窓の領域の上端の行（枠の内側の上から、バッファのタブと SQL コンソールの帯を除いた所）
    pub fn windows_top(&self) -> usize {
        self.area().y + self.tabs_row() + self.console_row()
    }

    /// 枠（上の帯と左右のアイコンの列。chrome.rs）の内側
    pub fn area(&self) -> Rect {
        crate::chrome::area(self.screen)
    }

    /// 差分・ログ・テーブルを画面いっぱいに出していないか（タブ・帯を出す）
    fn editor_shown(&self) -> bool {
        self.diff.is_none() && !self.log_fullscreen() && self.db_table.is_none()
    }

    /// バッファのタブの行の数（0 か 1。buffer_tabs.rs。設定の「タブ」で隠せる）。行の位置は area().y
    pub fn tabs_row(&self) -> usize {
        usize::from(self.editor_shown() && self.settings.bool("editor.tabs"))
    }

    /// SQL コンソールの帯の行（タブの下）
    pub fn console_row(&self) -> usize {
        usize::from(self.console_source().is_some() && self.editor_shown())
    }

    pub fn active_rect(&self) -> Rect {
        self.window_rects().into_iter().find(|(w, _)| *w == self.cur_win).map_or(Rect { x: self.windows_x().0, y: self.windows_top(), w: self.windows_x().1, h: self.windows_rows().saturating_sub(self.windows_top()) }, |(_, r)| r)
    }

    /// 窓の本文の行数（分割中は状態行の 1 行を除く）
    pub fn text_rows(&self, r: Rect) -> usize {
        r.h.saturating_sub(usize::from(self.is_split())).max(1)
    }

    /// 画面の本文の範囲（列, 行）を入れ、作業中の窓の行数を合わせる（描画とマウスの前に App が呼ぶ）
    pub fn set_screen(&mut self, cols: usize, rows: usize) {
        self.screen = (cols, rows);
        self.rows = self.text_rows(self.active_rect());
    }

    pub fn window_at(&self, row: usize, col: usize) -> Option<(usize, Rect)> {
        self.window_rects().into_iter().find(|(_, r)| r.contains(row, col))
    }

    /// 窓 `w` の表示状態 (バッファ番号, カーソル, 表示位置)
    pub(crate) fn win_state(&self, w: usize) -> Option<Win> {
        match self.wins.get(w)? {
            WinSlot::Active => Some(Win { doc: self.cur_doc, cursor: self.cursor, top: self.top, left: self.left }),
            WinSlot::Parked(win) => Some(*win),
            WinSlot::Closed => None,
        }
    }

    /// 窓 `w` へ移る
    pub(crate) fn focus_window(&mut self, w: usize) {
        if w == self.cur_win || !matches!(self.wins.get(w), Some(WinSlot::Parked(_))) {
            return;
        }
        let WinSlot::Parked(next) = std::mem::replace(&mut self.wins[w], WinSlot::Active) else { unreachable!() };
        self.wins[self.cur_win] = WinSlot::Parked(Win { doc: self.cur_doc, cursor: self.cursor, top: self.top, left: self.left });
        self.prev_win = Some(self.cur_win);
        self.cur_win = w;
        if next.doc != self.cur_doc {
            // 窓の移動はジャンプにしない
            self.jump_lock = true;
            self.switch_doc(next.doc);
            self.jump_lock = false;
        }
        if matches!(self.state(), State::Visual { .. } | State::Insert) {
            self.set_state(State::Normal);
        }
        self.pending_keys.clear();
        self.cursor = next.cursor;
        self.top = next.top;
        self.left = next.left;
        self.frac = 0.0;
        let last = self.buf.line_count().saturating_sub(1);
        self.cursor.line = self.cursor.line.min(last);
        self.top = self.top.min(last);
        self.clamp_normal();
        self.rows = self.text_rows(self.active_rect());
    }

    /// `:sp` / `:vs`。今の窓を分けて、新しい窓（同じバッファ）へ移る。新しい窓は上か左（Vim の既定）
    pub(crate) fn split(&mut self, vertical: bool) {
        let rect = self.active_rect();
        // 狭すぎる窓は分けない（Vim の E36）
        if (vertical && rect.w < 2 * 12 + 1) || (!vertical && rect.h < 2 * 3) {
            self.notify.error("E36: 分割する余地がありません");
            return;
        }
        let new = self.wins.len();
        self.wins.push(WinSlot::Parked(Win { doc: self.cur_doc, cursor: self.cursor, top: self.top, left: self.left }));
        // 新しい窓は上・左に（Vim と同じ）。設定の splitbelow・splitright なら下・右に
        let after = if vertical { self.settings.bool("editor.split_right") } else { self.settings.bool("editor.split_below") };
        self.layout.insert(self.cur_win, new, vertical, !after);
        self.focus_window(new);
    }

    /// 窓 `w` を閉じる。最後の 1 つは閉じない（呼ぶ側で終了を扱う）
    pub(crate) fn close_window(&mut self, w: usize) {
        if !self.is_split() || matches!(self.wins.get(w), None | Some(WinSlot::Closed)) {
            return;
        }
        if w == self.cur_win {
            // 隣（直前の窓があればそこ）へ移ってから閉じる
            let mut order = Vec::new();
            self.layout.leaves(&mut order);
            let i = order.iter().position(|&x| x == w).unwrap_or(0);
            let next = self.prev_win.filter(|p| order.contains(p) && *p != w).unwrap_or(if i + 1 < order.len() { order[i + 1] } else { order[i - 1] });
            self.focus_window(next);
        }
        self.wins[w] = WinSlot::Closed;
        self.layout.remove(w);
        if self.prev_win == Some(w) {
            self.prev_win = None;
        }
        self.rows = self.text_rows(self.active_rect());
    }

    /// `:only` / `Ctrl-w o`
    pub(crate) fn only_window(&mut self) {
        let mut order = Vec::new();
        self.layout.leaves(&mut order);
        for w in order.into_iter().filter(|&w| w != self.cur_win) {
            self.wins[w] = WinSlot::Closed;
        }
        self.layout = Layout::Leaf(self.cur_win);
        self.prev_win = None;
        self.rows = self.text_rows(self.active_rect());
    }

    /// 分けている窓（並びの形, 窓ごとの (バッファの番号, カーソル, 表示の上, 作業中か)）。分けていなければ None
    pub(crate) fn window_snapshot(&self) -> Option<(String, Vec<(usize, Cursor, usize, bool)>)> {
        if !self.is_split() {
            return None;
        }
        let mut order = Vec::new();
        self.layout.leaves(&mut order);
        let wins: Vec<(usize, Cursor, usize, bool)> = order
            .iter()
            .filter_map(|&w| match self.wins.get(w)? {
                WinSlot::Active => Some((self.cur_doc, self.cursor, self.top, true)),
                WinSlot::Parked(p) => Some((p.doc, p.cursor, p.top, false)),
                WinSlot::Closed => None,
            })
            .collect();
        if wins.len() != order.len() {
            return None;
        }
        let shape = self.layout.render(&|w| order.iter().position(|o| *o == w).unwrap_or(0));
        Some((shape, wins))
    }

    /// 窓の分け方を戻す（`docs` は窓ごとの (バッファの番号, カーソル, 表示の上)。`active` の窓を作業中に）
    pub(crate) fn restore_windows(&mut self, layout: Layout, docs: Vec<(usize, Cursor, usize)>, active: usize) {
        self.only_window();
        // 今の窓（0 でなくてもよい）は使わずに閉じ、窓を作り直す
        let base = self.wins.len();
        for (doc, cursor, top) in &docs {
            self.wins.push(WinSlot::Parked(Win { doc: *doc, cursor: *cursor, top: *top, left: 0 }));
        }
        fn shift(l: Layout, by: usize) -> Layout {
            match l {
                Layout::Leaf(w) => Layout::Leaf(w + by),
                Layout::Split { vertical, kids, weights } => Layout::Split { vertical, kids: kids.into_iter().map(|k| shift(k, by)).collect(), weights },
            }
        }
        let old = self.cur_win;
        self.layout = shift(layout, base);
        self.focus_window(base + active.min(docs.len().saturating_sub(1)));
        self.wins[old] = WinSlot::Closed;
        self.prev_win = None;
        self.rows = self.text_rows(self.active_rect());
    }

    /// `Ctrl-w w` / `W`（順に巡る）
    pub(crate) fn cycle_window(&mut self, forward: bool, count: usize) {
        let mut order = Vec::new();
        self.layout.leaves(&mut order);
        let n = order.len();
        let i = order.iter().position(|&w| w == self.cur_win).unwrap_or(0);
        let to = if forward { (i + count) % n } else { (i + n - count % n) % n };
        self.focus_window(order[to]);
    }

    /// `Ctrl-w h/j/k/l`: その向きで隣り合う窓のうち、カーソルの行（左右）か列（上下）に重なるものへ移る
    pub(crate) fn focus_direction(&mut self, dir: char, count: usize) {
        for _ in 0..count.max(1) {
            let rects = self.window_rects();
            let Some(&(_, cur)) = rects.iter().find(|(w, _)| *w == self.cur_win) else { return };
            let crow = cur.y + self.cursor.line.saturating_sub(self.top).min(cur.h.saturating_sub(1));
            let ccol = cur.x + crate::editor::GUTTER.min(cur.w.saturating_sub(1));
            let pick = rects
                .iter()
                .filter(|(w, _)| *w != self.cur_win)
                .filter(|(_, r)| match dir {
                    'h' => r.x + r.w < cur.x + 1 && r.x + r.w + 1 >= cur.x,
                    'l' => r.x > cur.x + cur.w.saturating_sub(1) && r.x <= cur.x + cur.w + 1,
                    'k' => r.y + r.h == cur.y,
                    'j' => r.y == cur.y + cur.h,
                    _ => false,
                })
                // 重なる窓を優先し、無ければ一番近い窓
                .min_by_key(|(_, r)| match dir {
                    'h' | 'l' => if crow >= r.y && crow < r.y + r.h { 0 } else { 1 + crow.abs_diff(r.y) },
                    _ => if ccol >= r.x && ccol < r.x + r.w { 0 } else { 1 + ccol.abs_diff(r.x) },
                })
                .map(|(w, _)| *w);
            match pick {
                Some(w) => self.focus_window(w),
                // 一番下の窓から下へは、パネルがあればパネルへ
                None if dir == 'j' && self.grep.is_some() => {
                    self.focus_panel();
                    return;
                }
                // 一番左の窓から左へは、ツリーがあればツリーへ
                None if dir == 'h' && (self.tree.is_some() || self.commit.is_some()) => {
                    self.select_left_tab(self.left_last());
                    return;
                }
                None => return,
            }
        }
    }

    /// `Ctrl-w` の続きのキー
    pub(crate) fn window_command(&mut self, c: char, count: Option<usize>) {
        let n = count.unwrap_or(1);
        match c {
            'h' | 'j' | 'k' | 'l' => self.focus_direction(c, n),
            'w' => match count {
                Some(i) => {
                    let mut order = Vec::new();
                    self.layout.leaves(&mut order);
                    if let Some(&w) = order.get(i.saturating_sub(1)) {
                        self.focus_window(w);
                    }
                }
                None => self.cycle_window(true, 1),
            },
            'W' => self.cycle_window(false, 1),
            't' => {
                let mut order = Vec::new();
                self.layout.leaves(&mut order);
                self.focus_window(order[0]);
            }
            'b' => {
                let mut order = Vec::new();
                self.layout.leaves(&mut order);
                self.focus_window(*order.last().unwrap());
            }
            'p' => {
                if let Some(p) = self.prev_win {
                    self.focus_window(p);
                }
            }
            's' | 'S' => self.split(false),
            'v' => self.split(true),
            'n' => {
                self.split(false);
                self.new_empty_buffer();
            }
            'c' | 'q' => {
                if self.is_split() {
                    self.close_window(self.cur_win);
                } else if c == 'c' {
                    self.notify.error("E444: 最後のウィンドウは閉じられません");
                }
            }
            'o' => self.only_window(),
            // 大きさは常に等分なので、= は何もしない
            '=' => {}
            _ => {}
        }
    }

    /// 窓を閉じたときなど、どの窓にも出ていないバッファを数える用。`doc` を表示している窓
    pub(crate) fn windows_showing(&self, doc: usize) -> Vec<usize> {
        (0..self.wins.len()).filter(|&w| self.win_state(w).is_some_and(|s| s.doc == doc)).collect()
    }

    /// `:bd` で消したバッファを出していたほかの窓を閉じる（Vim と同じ）。残す窓が無ければ作業中の窓を残す
    pub(crate) fn close_windows_of(&mut self, doc: usize) {
        for w in self.windows_showing(doc) {
            if w != self.cur_win && self.is_split() {
                self.close_window(w);
            }
        }
    }

    /// 脇に置いたバッファの中身（描画用）。作業中のバッファなら None（Editor の欄を使う）
    pub(crate) fn parked_doc(&self, doc: usize) -> Option<&crate::buffers::Doc> {
        match self.docs.get(doc)? {
            Slot::Parked(d) => Some(d),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Effect;
    use nagi_core::Buffer;

    fn keys(e: &mut Editor, k: &str) -> Option<Effect> {
        let mut last = None;
        for key in nagi_vim::parse_keys(k, " ") {
            last = Some(e.key(key));
        }
        last
    }

    fn ed() -> Editor {
        let text: String = (1..=50).map(|i| format!("line{i}\n")).collect();
        let mut e = Editor::new(Buffer::from_str(&text), None);
        e.set_screen(100, 40);
        e
    }

    #[test]
    fn split_rects_divide_screen() {
        let mut e = ed();
        keys(&mut e, ":vs<CR>");
        let r = e.window_rects();
        assert_eq!(r.len(), 2);
        // 枠の内側を左右で等分し、間に区切りの 1 列
        let a = e.area();
        assert_eq!((r[0].1.x, r[1].1.x, r[0].1.w + r[1].1.w + 1), (a.x, a.x + r[0].1.w + 1, a.w));
        assert!(r[0].1.w.abs_diff(r[1].1.w) <= 1);
        assert_eq!(r[0].0, e.cur_win, "新しい窓は左で、そこへ移る");
        keys(&mut e, ":sp<CR>");
        let r = e.window_rects();
        assert_eq!(r.len(), 3);
        assert_eq!((r[0].1.y, r[1].1.y, r[0].1.h + r[1].1.h), (a.y, a.y + r[0].1.h, a.h));
        assert_eq!(e.rows, r[0].1.h - 1, "状態行の 1 行を除く");
    }

    #[test]
    fn windows_keep_own_cursor_on_same_buffer() {
        let mut e = ed();
        keys(&mut e, ":vs<CR>");
        keys(&mut e, "10j");
        assert_eq!(e.cursor.line, 10);
        keys(&mut e, "<C-w>l");
        assert_eq!(e.cursor.line, 0, "右の窓のカーソルはそのまま");
        keys(&mut e, "x");
        keys(&mut e, "<C-w>h");
        assert_eq!(e.cursor.line, 10);
        assert_eq!(e.buf.line(0), "ine1", "同じバッファなので編集は両方に出る");
        keys(&mut e, "<C-w>p");
        assert_eq!(e.cursor.line, 0);
        keys(&mut e, "<C-w><C-w>");
        assert_eq!(e.cursor.line, 10);
    }

    #[test]
    fn direction_moves_and_close() {
        let mut e = ed();
        // 左に 1 つ、右を上下に分ける: [A | B/C]
        keys(&mut e, ":vs<CR>");
        let left = e.cur_win;
        keys(&mut e, "<C-w>l:sp<CR>");
        let top_right = e.cur_win;
        keys(&mut e, "<C-w>j");
        let bottom_right = e.cur_win;
        assert_ne!(top_right, bottom_right);
        keys(&mut e, "<C-w>h");
        assert_eq!(e.cur_win, left);
        keys(&mut e, "<C-w>l<C-w>k");
        assert_eq!(e.cur_win, top_right);
        // :q は窓だけを閉じ、終了しない
        assert!(matches!(keys(&mut e, ":q<CR>"), Some(Effect::None)));
        assert_eq!(e.window_rects().len(), 2);
        keys(&mut e, "<C-w>o");
        assert_eq!(e.window_rects().len(), 1);
        assert!(!e.is_split());
        // 分割していなくても :q で nagi は終わらない（バッファを閉じる）
        assert!(matches!(keys(&mut e, ":q<CR>"), Some(Effect::None)));
    }

    #[test]
    fn split_with_other_buffer() {
        let dir = std::env::temp_dir().join(format!("nagi-panes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("other.txt");
        std::fs::write(&f, "other\n").unwrap();
        let mut e = ed();
        keys(&mut e, &format!(":sp {}<CR>", f.display()));
        assert_eq!(e.buf.line(0), "other");
        keys(&mut e, "<C-w>j");
        assert_eq!(e.buf.line(0), "line1");
        keys(&mut e, "<C-w>k");
        assert_eq!(e.buf.line(0), "other");
        // 別の窓に出ているバッファを :bd すると、その窓も閉じる
        keys(&mut e, "<C-w>j:bd 2<CR>");
        assert_eq!(e.window_rects().len(), 1);
        assert_eq!(e.buf.line(0), "line1");
    }
}
