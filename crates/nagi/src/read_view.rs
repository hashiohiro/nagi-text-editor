//! 読み取り専用の Vim の画面（下の欄のログ。Maven・:Make の出力）。
//!
//! 入力欄（vim_field.rs）と同じく、中に小さな Editor を持ってキーを流す。移動（hjkl w b e gg G Ctrl-d / 検索 n N）、
//! ビジュアル（v V Ctrl-v）とヤンク（y yy yiw …）はそのまま効く。中身を変える操作（i a o d c x p など）をしても、
//! 元の中身に戻す（挿入にも入らない）。ヤンクしたものはクリップボードにも入れる（レジスタは本文と共通）。

use nagi_vim::Key;

use crate::editor::{Clipboard, Editor, Mode};
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
        // ログは新しい行が下に増えるので、最後の行から
        ed.key(Key::Char('G'));
        ReadView { ed, lines: lines.len(), text }
    }

    /// 行が増えた・変わったら作り直す。カーソルの位置は残す（最後の行にいれば、新しい最後の行へ追いかける）
    pub fn set_lines(&mut self, lines: &[String]) {
        if lines.len() == self.lines && self.ed.buf.line_count() >= lines.len() {
            return;
        }
        let follow = self.ed.cursor.line + 1 >= self.lines.max(1);
        let (cursor, top, mode, anchor) = (self.ed.cursor, self.ed.top, self.ed.mode, self.ed.anchor);
        self.text = lines.join("\n");
        self.lines = lines.len();
        self.rebuild();
        self.ed.mode = mode;
        self.ed.anchor = anchor;
        if follow && !matches!(mode, Mode::Visual { .. }) {
            self.ed.key(Key::Char('G'));
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
        (self.ed.mode == Mode::Search).then(|| (if self.ed.search.forward { "/" } else { "?" }, self.ed.query.text.as_str()))
    }

    /// キーを流す。`regs` は本文のレジスタ（共有するため、流す間だけ入れ替える）
    pub fn key(&mut self, key: Key, regs: &mut Registers, clip: Option<Clipboard>) -> ViewOut {
        let normal = self.ed.mode == Mode::Normal && self.ed.pending_keys.is_empty();
        match key {
            Key::Esc if normal => return ViewOut::Leave,
            Key::Char('q') if normal => return ViewOut::Quit,
            Key::Enter if normal => return ViewOut::Open,
            Key::Char(':') if normal => return ViewOut::Command,
            // Ctrl+C: 選んでいればそこを、選んでいなければ今の行をコピー
            Key::Ctrl('c') => {
                let keys: &[Key] = if matches!(self.ed.mode, Mode::Visual { .. }) { &[Key::Char('y')] } else { &[Key::Char('y'), Key::Char('y')] };
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
        if matches!(self.ed.mode, Mode::Insert | Mode::Command | Mode::Replace) {
            self.ed.mode = Mode::Normal;
            out = ViewOut::ReadOnly;
        }
        if self.ed.buf.version != version {
            let cursor = self.ed.cursor;
            self.rebuild();
            self.ed.mode = Mode::Normal;
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
        if matches!(self.ed.mode, Mode::Visual { .. }) {
            self.ed.mode = Mode::Normal;
        }
        self.place(row, col);
    }

    /// ドラッグ: 押した位置からビジュアルで選ぶ
    pub fn drag(&mut self, row: usize, col: usize) {
        if !matches!(self.ed.mode, Mode::Visual { .. }) {
            self.ed.anchor = self.ed.buf.line_start(self.ed.cursor.line) + self.ed.cursor.byte;
            self.ed.mode = Mode::Visual { line: false, block: false };
        }
        self.place(row, col);
    }

    fn place(&mut self, row: usize, col: usize) {
        let line = (self.ed.top + row).min(self.ed.buf.line_count().saturating_sub(1));
        let text = self.ed.buf.line(line);
        self.ed.cursor.line = line;
        self.ed.cursor.byte = nagi_core::byte_at_col(&text, col, false);
        self.ed.cursor.want_col = col;
        if self.ed.mode == Mode::Normal {
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
        let Mode::Visual { line: whole, block } = self.ed.mode else { return None };
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
