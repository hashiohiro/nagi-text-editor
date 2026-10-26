//! 読み取り専用の Vim の画面（下の欄のログ。Maven・:Make の出力）。
//!
//! 入力欄（vim_field.rs）と同じく、中に小さな Editor を持ってキーを流す。移動（hjkl w b e gg G Ctrl-d / 検索 n N）、
//! ビジュアル（v V Ctrl-v）とヤンク（y yy yiw …）はそのまま効く。中身を変える操作（i a o d c x p など）をしても、
//! 元の中身に戻す（挿入にも入らない）。ヤンクしたものはクリップボードにも入れる（レジスタは本文と共通）。

use nagi_vim::Key;

use crate::editor::{Clipboard, Editor, State};
use crate::vim_field::Registers;

/// キーを流した結果
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewOut {
    None,
    /// 本文へ戻る（ノーマルで Esc）
    Leave,
    /// 欄を閉じる（ノーマルで q）
    Quit,
    /// カーソルの行を開く（ノーマルで Enter）
    Open,
    /// Ex のコマンドライン（ノーマルで :）
    Command,
    /// 中身を変えようとした（元に戻した）
    ReadOnly,
    /// ヤンクした（クリップボードに入れた。行数）
    Yanked(usize),
}

pub struct ReadView {
    pub ed: Box<Editor>,
    /// 元の行の数と中身（変えられたら戻す）
    lines: usize,
    text: String,
}

impl ReadView {
    pub fn new(lines: &[String]) -> ReadView {
        let text = lines.join("\n");
        let mut ed = Box::new(Editor::new(nagi_core::Buffer::from_str(&text), None));
        ed.set_screen(400, 12);
        // ログは新しい行が下に増えるので、最後の行の行末から（追いかける）
        ed.key(Key::Char('G'));
        ed.key(Key::Char('$'));
        ReadView { ed, lines: lines.len(), text }
    }

    /// 行が増えた・変わったら作り直す。カーソルが最後の行にいれば、新しい最後の行の行末へ追いかけ、表示も合わせる。
    /// カーソルを上へ動かせば追いかけるのをやめ（その場に残す）、最後の行へ戻せばまた追いかける
    pub fn set_lines(&mut self, lines: &[String]) {
        if lines.len() == self.lines && self.ed.buf.line_count() >= lines.len() {
            return;
        }
        let follow = self.ed.cursor.line + 1 >= self.lines.max(1);
        let (cursor, top, mode, anchor) = (self.ed.cursor, self.ed.top, self.ed.state(), self.ed.anchor);
        self.text = lines.join("\n");
        self.lines = lines.len();
        self.rebuild();
        self.ed.set_state(mode);
        self.ed.anchor = anchor;
        if follow && !matches!(mode, State::Visual { .. }) {
            self.ed.key(Key::Char('G'));
            self.ed.key(Key::Char('$'));
        } else {
            self.ed.cursor = cursor;
            self.ed.top = top;
            self.ed.cursor.line = self.ed.cursor.line.min(self.ed.buf.line_count().saturating_sub(1));
            self.ed.clamp_normal();
        }
        self.ed.scroll_into_view();
    }

    fn rebuild(&mut self) {
        let (screen, registers) = (self.ed.screen, std::mem::take(&mut self.ed.registers));
        self.ed.buf = nagi_core::Buffer::from_str(&self.text);
        self.ed.registers = registers;
        self.ed.screen = screen;
    }

    /// 見える行数（欄の中身の高さ）と幅
    pub fn resize(&mut self, cols: usize, rows: usize) {
        if self.ed.screen != (cols, rows + 1) {
            // 最下行は入力・状態の行なので 1 行足す
            self.ed.set_screen(cols, rows + 1);
            self.ed.scroll_into_view();
        }
    }

    /// 検索の入力中なら (前の印, 打っている語)
    pub fn prompt(&self) -> Option<(&'static str, &str)> {
        (self.ed.state() == State::Search).then(|| (if self.ed.search.forward { "/" } else { "?" }, self.ed.query.text.as_str()))
    }

    /// キーを流す。`regs` は本文のレジスタ（共有するため、流す間だけ入れ替える）
    pub fn key(&mut self, key: Key, regs: &mut Registers, clip: Option<Clipboard>) -> ViewOut {
        let normal = self.ed.state() == State::Normal && self.ed.pending_keys.is_empty();
        match key {
            Key::Esc if normal => return ViewOut::Leave,
            Key::Char('q') if normal => return ViewOut::Quit,
            Key::Enter if normal => return ViewOut::Open,
            Key::Char(':') if normal => return ViewOut::Command,
            // Ctrl+C: 選んでいればそこを、選んでいなければ今の行をコピー
            Key::Ctrl('c') => {
                let keys: &[Key] = if matches!(self.ed.state(), State::Visual { .. }) { &[Key::Char('y')] } else { &[Key::Char('y'), Key::Char('y')] };
                let mut out = ViewOut::None;
                for &k in keys {
                    out = self.key(k, regs, clip);
                }
                return out;
            }
            _ => {}
        }
        let version = self.ed.buf.version;
        let yank_before = regs.get(&'0').cloned();
        std::mem::swap(&mut self.ed.registers, regs);
        self.ed.clipboard = clip;
        self.ed.key(key);
        std::mem::swap(&mut self.ed.registers, regs);
        // 読み取り専用: 挿入・置き換え・コマンドラインには入らない。中身が変わったら戻す
        let mut out = ViewOut::None;
        if matches!(self.ed.state(), State::Insert | State::Command | State::Replace) {
            self.ed.set_state(State::Normal);
            out = ViewOut::ReadOnly;
        }
        if self.ed.buf.version != version {
            let cursor = self.ed.cursor;
            self.rebuild();
            self.ed.set_state(State::Normal);
            self.ed.cursor = cursor;
            self.ed.cursor.line = cursor.line.min(self.ed.buf.line_count().saturating_sub(1));
            self.ed.clamp_normal();
            out = ViewOut::ReadOnly;
        }
        self.ed.pending_keys.retain(|_| out == ViewOut::None);
        // ヤンクしたら、クリップボードにも入れる
        let yank = regs.get(&'0').cloned();
        if out == ViewOut::None && yank.is_some() && yank != yank_before {
            let (text, _) = yank.unwrap();
            let text = text.trim_end_matches('\n').replace('\n', "\r\n");
            match &clip {
                Some(cb) => (cb.set)(&text),
                None => crate::clipboard::set(&text),
            }
            return ViewOut::Yanked(text.lines().count().max(1));
        }
        out
    }

    /// 押した位置（見えている行・表示の列）へカーソルを置く。ビジュアルなら抜ける
    pub fn click(&mut self, row: usize, col: usize) {
        if matches!(self.ed.state(), State::Visual { .. }) {
            self.ed.set_state(State::Normal);
        }
        self.place(row, col);
    }

    /// ドラッグ: 押した位置からビジュアルで選ぶ
    pub fn drag(&mut self, row: usize, col: usize) {
        if !matches!(self.ed.state(), State::Visual { .. }) {
            self.ed.anchor = self.ed.buf.line_start(self.ed.cursor.line) + self.ed.cursor.byte;
            self.ed.set_state(State::Visual { line: false, block: false });
        }
        self.place(row, col);
    }

    fn place(&mut self, row: usize, col: usize) {
        let line = (self.ed.top + row).min(self.ed.buf.line_count().saturating_sub(1));
        let text = self.ed.buf.line(line);
        self.ed.cursor.line = line;
        self.ed.cursor.byte = nagi_core::byte_at_col(&text, col, false);
        self.ed.cursor.want_col = col;
        if self.ed.state() == State::Normal {
            self.ed.clamp_normal();
        }
        self.ed.scroll_into_view();
    }

    /// 選んでいる所（ビジュアルなら選び、そうでなければ今の行）をクリップボードへ。行数を返す
    pub fn copy(&mut self, regs: &mut Registers, clip: Option<Clipboard>) -> usize {
        match self.key(Key::Ctrl('c'), regs, clip) {
            ViewOut::Yanked(n) => n,
            _ => 0,
        }
    }

    /// 描く用: 行 `line` の中のビジュアルの範囲（byte。無ければ None）
    pub fn visual_in(&self, line: usize) -> Option<(usize, usize)> {
        let State::Visual { line: whole, block } = self.ed.state() else { return None };
        let start = self.ed.buf.line_start(line);
        let text = self.ed.buf.line(line);
        if block {
            // 矩形: 列の範囲
            let a = self.ed.anchor;
            let al = line_of(&self.ed.buf, a);
            let (l0, l1) = (al.min(self.ed.cursor.line), al.max(self.ed.cursor.line));
            if line < l0 || line > l1 {
                return None;
            }
            let ac = nagi_core::display_col(&self.ed.buf.line(al), a - self.ed.buf.line_start(al), false);
            let cc = nagi_core::display_col(&self.ed.buf.line(self.ed.cursor.line), self.ed.cursor.byte, false);
            let (c0, c1) = (ac.min(cc), ac.max(cc));
            return Some((nagi_core::byte_at_col(&text, c0, false), nagi_core::byte_at_col(&text, c1 + 1, false)));
        }
        let (a, b, _) = self.ed.visual_range();
        if whole {
            let (la, lb) = (line_of(&self.ed.buf, a), line_of(&self.ed.buf, b.saturating_sub(1).max(a)));
            return (line >= la && line <= lb).then_some((0, text.len().max(1)));
        }
        let (s, e) = (a.max(start), b.min(start + text.len() + 1));
        (s < e).then(|| (s - start, e - start))
    }
}

/// 絶対 byte の位置を含む行
fn line_of(buf: &nagi_core::Buffer, abs: usize) -> usize {
    let (mut lo, mut hi) = (0, buf.line_count().saturating_sub(1));
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if buf.line_start(mid) <= abs {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

/// 描き方（view.rs の Painter で描く）
pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    /// ログ: 読み取り専用の Vim の画面。1 行目は見出し（検索の入力中は検索語）、2 行目から本文。
    /// カーソルは四角、ビジュアルで選んだ所は反転（本文と同じ）。[ERROR]・error: の行は赤、[WARNING]・warning: の行は黄色
    pub(crate) fn draw_read_view(p: &mut Painter, g: &crate::grep_panel::GrepPanel, v: &crate::read_view::ReadView, r: Rect, focused: bool) {
        let cell = p.fonts.cell();
        let (x, y0, w) = (r.x as f32 * cell.w, r.y as f32 * cell.h, r.w as f32 * cell.w);
        p.out.push(Instance::fill(x, y0, w, r.h as f32 * cell.h, PANEL_BG));
        p.out.push(Instance::fill(x, y0, w, 1.0, LINE_NO));
        let ed = &v.ed;
        let saved = p.cols;
        p.cols = (r.x + r.w).saturating_sub(3);
        match v.prompt() {
            Some((lead, q)) => {
                let end = p.text(&format!("{lead}{q}"), r.x, y0, FG);
                p.caret(end, y0);
            }
            None => {
                let mode = match ed.state() {
                    State::Visual { block: true, .. } => "   -- VISUAL BLOCK --",
                    State::Visual { line: true, .. } => "   -- VISUAL LINE --",
                    State::Visual { .. } => "   -- VISUAL --",
                    _ => "",
                };
                let head = format!(
                    "{}   {} 行{}   {}/{}{mode}",
                    g.title.as_deref().unwrap_or(""),
                    g.hits.len(),
                    if g.searching() { " …" } else { "" },
                    ed.cursor.line + 1,
                    ed.buf.line_count()
                );
                p.text(&head, r.x, y0, STATUS_FG);
            }
        }
        p.cols = saved;
        let n = g.hits.len();
        for k in 0..r.h.saturating_sub(1) {
            let line = ed.top + k;
            if line >= n.max(1) || line >= ed.buf.line_count() {
                break;
            }
            let y = (r.y + 1 + k) as f32 * cell.h;
            let text = ed.buf.line(line);
            let t = text.trim_start();
            let base = if t.starts_with("[ERROR]") || t.starts_with("error") {
                DIAG_ERROR
            } else if t.starts_with("[WARNING]") || t.starts_with("[WARN]") || t.starts_with("warning") {
                DIAG_WARN
            } else {
                FG
            };
            let vis = v.visual_in(line);
            let cur = (focused && line == ed.cursor.line).then_some(ed.cursor.byte);
            let mut items = Vec::new();
            for (b, gr) in text.grapheme_indices(true) {
                let width = p.cell_width(gr);
                let sel = vis.is_some_and(|(s, e)| b >= s && b < e);
                let (fg, bg) = if cur == Some(b) {
                    (BG, Some(if sel { CURSOR_IN_SELECTION } else { CURSOR }))
                } else if sel {
                    (FG, Some(SEL_BG))
                } else {
                    (base, None)
                };
                items.push(Item { g: Cow::Borrowed(gr), width, fg, bg, underline: false });
            }
            // 横に送っていれば、左端より左を落とす（行が左端より短ければ、行末のカーソルも隠れている）
            let width: usize = items.iter().map(|i| i.width).sum();
            skip_cols(&mut items, ed.left, &mut None);
            let end = p.draw(&items, r.x, y);
            // 空の行・行末のカーソル
            if cur.is_some_and(|c| c >= text.len()) && width >= ed.left {
                p.out.push(Instance::fill(end as f32 * cell.w, y, cell.w, cell.h, if vis.is_some() { CURSOR_IN_SELECTION } else { CURSOR }));
            } else if vis.is_some() && text.is_empty() {
                p.out.push(Instance::fill(end as f32 * cell.w, y, cell.w, cell.h, SEL_BG));
            }
        }
    }
}
