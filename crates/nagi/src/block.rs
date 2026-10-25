//! Visual block（`Ctrl-v`）。選択は表示の列で決める矩形で、行ごとの byte の範囲に直して扱う。
//!
//! - 左端は起点とカーソルの左の方の列、右端は右の方の文字の右端（全角は 2 列）。`$` なら行末まで
//! - 左端をまたぐ全角文字は含めない
//! - `I` `A` `c` は 1 行目で打った文字列を、Insert を抜けるときにほかの行の同じ列へ入れる。
//!   範囲に届かない短い行は `I` `c` では飛ばし、`A` では空白で埋めてから入れる（Vim と同じ）
//! - 矩形で取ったレジスタは「矩形の幅」を覚えておき、`p` で矩形のまま貼る

use nagi_core::display_col;
use nagi_vim::cmd::Op;
use unicode_segmentation::UnicodeSegmentation;

use crate::editor::{Editor, State};

/// 矩形の選択（行は 0 始まり、列は表示の列。right は含む。None は行末まで）
#[derive(Clone, Copy, Debug)]
pub struct BlockSel {
    pub l1: usize,
    pub l2: usize,
    pub left: usize,
    pub right: Option<usize>,
}

/// `I` `A` `c` の、Insert を抜けたときにほかの行へ入れる約束
#[derive(Clone, Debug)]
pub struct BlockInsert {
    /// 入れる行（1 行目は打った行なので含まない）
    pub lines: std::ops::RangeInclusive<usize>,
    /// 入れる表示の列
    pub col: usize,
    /// 行末に足す（`$A`）
    pub eol: bool,
    /// 届かない行を空白で埋めて入れる（`A`）
    pub pad: bool,
    /// 1 行目で打ち始めた位置（絶対 byte）
    pub start: usize,
    /// 抜けた後のカーソル（I と A は矩形の左上の角。c は普通の Esc と同じなので None）
    pub back_to: Option<usize>,
}

/// 列 `col` から始まる書記素の幅（タブは次のタブの位置まで）
fn width(g: &str, col: usize) -> usize {
    nagi_core::width_at(g, col, false)
}

/// 行の中で、表示の列 `left..=right` に収まる書記素の byte 範囲
pub fn span_in_line(line: &str, left: usize, right: Option<usize>) -> (usize, usize) {
    let mut col = 0;
    let mut s = None;
    let mut e = line.len();
    for (i, g) in line.grapheme_indices(true) {
        let w = width(g, col);
        if s.is_none() && col >= left {
            s = Some(i);
        }
        if right.is_some_and(|r| col > r) {
            e = i;
            break;
        }
        col += w;
    }
    let s = s.unwrap_or(line.len());
    (s, e.max(s))
}

/// 表示の列 `col` から始まる位置（行がそこまで無ければ None）
fn byte_at(line: &str, col: usize) -> Option<usize> {
    let mut c = 0;
    for (i, g) in line.grapheme_indices(true) {
        if c >= col {
            return Some(i);
        }
        c += width(g, c);
    }
    (c >= col).then_some(line.len())
}

impl Editor {
    pub fn is_block(&self) -> bool {
        matches!(self.state(), State::Visual { block: true, .. })
    }

    pub fn block_sel(&self) -> BlockSel {
        let a = self.anchor.min(self.buf.len_bytes());
        let (al, ab) = self.buf.pos_of(a);
        let (cl, cb) = (self.cursor.line, self.cursor.byte);
        let cell = |l: usize, b: usize| {
            let line = self.buf.line(l);
            let b = b.min(line.len());
            let c = display_col(&line, b, false);
            let w = line[b..].graphemes(true).next().map_or(1, |g| width(g, c));
            (c, w)
        };
        let (ca, wa) = cell(al, ab);
        let (cc, wc) = cell(cl, cb);
        let right = if self.cursor.want_col == usize::MAX { None } else { Some((ca + wa).max(cc + wc) - 1) };
        BlockSel { l1: al.min(cl), l2: al.max(cl), left: ca.min(cc), right }
    }

    /// 行ごとの (行, 始まり, 終わり)（絶対 byte。終わりは含まない）
    pub fn block_spans(&self) -> Vec<(usize, usize, usize)> {
        let s = self.block_sel();
        (s.l1..=s.l2.min(self.buf.line_count().saturating_sub(1)))
            .map(|l| {
                let line = self.buf.line(l);
                let (a, b) = span_in_line(&line, s.left, s.right);
                let ls = self.buf.line_start(l);
                (l, ls + a, ls + b)
            })
            .collect()
    }

    fn block_width(&self, sel: &BlockSel, spans: &[(usize, usize, usize)]) -> usize {
        match sel.right {
            Some(r) => r + 1 - sel.left,
            None => spans.iter().map(|&(l, s, e)| {
                let line = self.buf.line(l);
                let ls = self.buf.line_start(l);
                nagi_core::display_col(&line, e - ls, false) - nagi_core::display_col(&line, s - ls, false)
            }).max().unwrap_or(0),
        }
    }

    /// カーソルを矩形の左上へ
    fn to_block_top_left(&mut self, sel: &BlockSel) {
        self.cursor.line = sel.l1.min(self.buf.line_count().saturating_sub(1));
        let line = self.buf.line(self.cursor.line);
        self.cursor.byte = byte_at(&line, sel.left).unwrap_or(line.len());
        self.cursor.want_col = sel.left;
    }

    /// 矩形に対するオペレーター（d y c > <）
    pub(crate) fn block_op(&mut self, op: Op, reg: Option<char>) {
        let sel = self.block_sel();
        let spans = self.block_spans();
        self.set_state(State::Normal);
        if matches!(op, Op::Indent | Op::Dedent) {
            let (s, e) = self.lines_range(sel.l1, sel.l2);
            self.apply(op, s, e, true, None);
            return;
        }
        let text: Vec<String> = spans.iter().map(|&(_, s, e)| self.buf.slice_string(s, e)).collect();
        let w = self.block_width(&sel, &spans);
        self.reg_block_next = Some(w);
        self.set_register_pub(reg, text.join("\n"), op == Op::Yank);
        if op == Op::Yank {
            self.to_block_top_left(&sel);
            return;
        }
        self.hist.begin();
        for &(_, s, e) in spans.iter().rev() {
            if e > s {
                self.edit(s, e, "");
            }
        }
        if op == Op::Change {
            self.to_block_top_left(&sel);
            let start = self.abs();
            self.block_insert = Some(BlockInsert { lines: sel.l1 + 1..=sel.l2, col: sel.left, eol: false, pad: false, start, back_to: None });
            // グループは Insert を抜けるまで開いたまま
            self.set_state(State::Insert);
            return;
        }
        self.hist.close();
        self.to_block_top_left(&sel);
        self.clamp_normal();
    }

    /// 矩形の `I`（`append` = false）と `A`
    pub(crate) fn block_insert_cmd(&mut self, append: bool) {
        let sel = self.block_sel();
        self.set_state(State::Normal);
        let corner = {
            let line = self.buf.line(sel.l1);
            self.buf.line_start(sel.l1) + byte_at(&line, sel.left).unwrap_or(line.len())
        };
        let eol = append && sel.right.is_none();
        let col = if append { sel.right.map_or(0, |r| r + 1) } else { sel.left };
        self.hist.begin();
        // 1 行目に打つ位置。A で行が短ければ空白で埋める
        self.cursor.line = sel.l1;
        let line = self.buf.line(sel.l1);
        let ls = self.buf.line_start(sel.l1);
        let at = if eol {
            ls + line.len()
        } else {
            match byte_at(&line, col) {
                Some(b) => ls + b,
                None if append => {
                    let pad = col - display_col(&line, line.len(), false);
                    self.edit(ls + line.len(), ls + line.len(), &" ".repeat(pad));
                    ls + line.len() + pad
                }
                None => ls + line.len(),
            }
        };
        self.set_abs(at);
        self.block_insert = Some(BlockInsert { lines: sel.l1 + 1..=sel.l2, col, eol, pad: append, start: at, back_to: Some(corner) });
        self.set_state(State::Insert);
    }

    /// Insert を抜けるとき: 1 行目で打った文字列をほかの行へ入れる。カーソルを置いたら true
    pub(crate) fn finish_block_insert(&mut self) -> bool {
        let Some(bi) = self.block_insert.take() else { return false };
        let end = self.abs();
        if end <= bi.start || self.buf.pos_of(bi.start).0 != self.cursor.line {
            return false;
        }
        let text = self.buf.slice_string(bi.start, end);
        if text.contains('\n') {
            return false;
        }
        // 下の行から入れる（上の行の位置がずれないように）
        for l in bi.lines.clone().rev() {
            if l >= self.buf.line_count() {
                continue;
            }
            let line = self.buf.line(l);
            let ls = self.buf.line_start(l);
            if bi.eol {
                self.edit(ls + line.len(), ls + line.len(), &text);
                continue;
            }
            match byte_at(&line, bi.col) {
                Some(b) => self.edit(ls + b, ls + b, &text),
                None if bi.pad => {
                    let pad = bi.col - display_col(&line, line.len(), false);
                    self.edit(ls + line.len(), ls + line.len(), &format!("{}{text}", " ".repeat(pad)));
                }
                None => {}
            }
        }
        match bi.back_to {
            Some(at) => {
                self.set_abs(at);
                true
            }
            None => false,
        }
    }

    /// 矩形の `r{c}` と `~`
    pub(crate) fn block_map_chars(&mut self, f: impl Fn(&str) -> String) {
        let sel = self.block_sel();
        let spans = self.block_spans();
        self.set_state(State::Normal);
        self.hist.begin();
        for &(_, s, e) in spans.iter().rev() {
            if e > s {
                let old = self.buf.slice_string(s, e);
                let new: String = old.graphemes(true).map(&f).collect();
                if new != old {
                    self.edit(s, e, &new);
                }
            }
        }
        self.hist.close();
        self.to_block_top_left(&sel);
        self.clamp_normal();
    }

    /// 矩形の `J`（行を結ぶ）
    pub(crate) fn block_join(&mut self) {
        let sel = self.block_sel();
        self.set_state(State::Normal);
        self.join(sel.l1, (sel.l2 - sel.l1).max(1));
    }

    /// 矩形の `O`（同じ行の反対の角へ）
    pub(crate) fn block_swap_corner(&mut self) {
        let sel = self.block_sel();
        let (al, _) = self.buf.pos_of(self.anchor.min(self.buf.len_bytes()));
        let right = sel.right.unwrap_or(sel.left);
        let cur_col = display_col(&self.buf.line(self.cursor.line), self.cursor.byte, false);
        let (new_cur, new_anchor) = if cur_col <= sel.left { (right, sel.left) } else { (sel.left, right) };
        let put = |ed: &Editor, l: usize, col: usize| {
            let line = ed.buf.line(l);
            ed.buf.line_start(l) + byte_at(&line, col).unwrap_or(line.len())
        };
        self.anchor = put(self, al, new_anchor);
        let at = put(self, self.cursor.line, new_cur);
        self.set_abs(at);
        self.cursor.want_col = new_cur;
    }

    /// 矩形で取ったレジスタを、カーソルの列から下の行へ矩形のまま貼る（`after` なら次の列から）
    pub(crate) fn put_block(&mut self, text: &str, w: usize, after: bool) {
        let line0 = self.buf.line(self.cursor.line);
        let mut col = display_col(&line0, self.cursor.byte, false);
        if after && !line0.is_empty() {
            col += line0[self.cursor.byte.min(line0.len())..].graphemes(true).next().map_or(0, |g| width(g, col));
        }
        let nl = self.buf.newline().to_string();
        let top = self.cursor.line;
        self.hist.begin();
        for (i, piece) in text.split('\n').enumerate() {
            let l = top + i;
            // ファイルの終わりを越えたら行を足す
            while l >= self.buf.line_count() {
                let end = self.buf.len_bytes();
                self.edit(end, end, &nl);
            }
            let line = self.buf.line(l);
            let ls = self.buf.line_start(l);
            let pw: usize = piece.graphemes(true).fold(0, |c, g| c + width(g, col + c));
            match byte_at(&line, col) {
                Some(b) => {
                    // 後ろに文字があるときは、矩形の幅まで空白で埋める
                    let fill = if b < line.len() { " ".repeat(w.saturating_sub(pw)) } else { String::new() };
                    self.edit(ls + b, ls + b, &format!("{piece}{fill}"));
                }
                None => {
                    let pad = col - display_col(&line, line.len(), false);
                    self.edit(ls + line.len(), ls + line.len(), &format!("{}{piece}", " ".repeat(pad)));
                }
            }
        }
        self.hist.close();
        self.cursor.line = top;
        let line = self.buf.line(top);
        self.cursor.byte = byte_at(&line, col).unwrap_or(line.len());
        self.clamp_normal();
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

    /// 全角は 2 列として矩形を決める
    #[test]
    fn wide_chars() {
        let mut e = Editor::new(Buffer::from_str("あいう\nabcd\nxyz\n"), None);
        // あ（0〜1 列）から下の b（1 列）まで: 0〜1 列の矩形
        keys(&mut e, "<C-v>jld");
        assert_eq!((e.buf.line(0), e.buf.line(1)), ("いう".to_string(), "cd".to_string()));
        // 矩形で取って貼ると、行ごとに同じ列へ入る
        keys(&mut e, "ugg<C-v>jly");
        assert_eq!(e.reg_blocks.get(&'"'), Some(&2));
        keys(&mut e, "Gp");
        assert_eq!(e.buf.line(2), "xあyz");
        assert_eq!(e.buf.line(3), " ab");
        assert!(e.notify.text.is_empty() || !e.notify.text.starts_with('E'));
        assert_eq!(e.state(), State::Normal);
    }
}
