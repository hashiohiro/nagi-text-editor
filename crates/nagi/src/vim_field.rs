//! Vim 風の入力欄（パネルの入力欄とコミットメッセージ）。
//!
//! 中に小さな Editor（Vim のエディタ部そのもの）を持ち、キーをそこへ流す。エディタと違って Insert モードで始め、
//! Esc でノーマルモード（`w` `b` `dw` `ciw` `.` `u` `p` やテキストオブジェクトがそのまま効く）、ノーマルモードで
//! もう一度 Esc を押すと閉じる。1 行の欄は Enter で決める。複数行の欄（コミットメッセージ）は Enter で改行し、
//! Ctrl+Enter で決める。レジスタとクリップボードはエディタと共通（欄で yiw したものを本文で p できる）。

use std::collections::HashMap;

use nagi_core::Buffer;
use nagi_vim::Key;

use crate::editor::{Clipboard, Editor, State};

/// 欄に流したキーの結果
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldOut {
    /// 何も変わらない（カーソルが動いただけなど）
    None,
    /// 中身が変わった（絞り込みを出し直す）
    Changed,
    /// 決めた（Enter）
    Submit,
    /// 閉じた（ノーマルモードで Esc）
    Cancel,
}

pub type Registers = HashMap<char, (String, bool)>;

thread_local! {
    /// Vim のキー操作を使うか（エディタの設定に合わせる。使わなければ普通の入力欄: Esc で閉じ、Shift+矢印で選ぶ）。
    /// UI の糸ごと（試験は糸ごとに別の Editor を持つ）
    static VIM: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

pub fn set_vim(on: bool) {
    VIM.with(|v| v.set(on));
}

pub struct VimField {
    pub ed: Box<Editor>,
    pub multiline: bool,
}

impl VimField {
    pub fn new(text: &str, multiline: bool) -> VimField {
        let mut ed = Box::new(Editor::new(Buffer::from_str(text), None));
        ed.set_screen(400, 50);
        if !VIM.with(|v| v.get()) {
            ed.set_vim(false);
            let end = ed.buf.len_bytes();
            ed.set_abs(end);
            return VimField { ed, multiline };
        }
        // 最後の行の終わりで Insert
        for k in [Key::Char('G'), Key::Char('A')] {
            ed.key(k);
        }
        VimField { ed, multiline }
    }

    /// 中身（1 行の欄は行を空白でつなぐ）
    pub fn text(&self) -> String {
        let n = self.ed.buf.line_count();
        let lines: Vec<String> = (0..n).map(|l| self.ed.buf.line(l)).collect();
        let mut s = lines.join(if self.multiline { "\n" } else { " " });
        while s.ends_with('\n') {
            s.pop();
        }
        s
    }

    pub fn is_insert(&self) -> bool {
        // Vim を使わない欄は、選んでいる間も打てる
        self.ed.state() == State::Insert || (!self.ed.vim_on && matches!(self.ed.state(), State::Visual { .. }))
    }

    /// 選んでいる文字（Vim を使わない欄で選んでいるときだけ）
    pub fn selected_text(&self) -> Option<String> {
        if self.ed.vim_on { None } else { self.ed.plain_selected_text() }
    }

    /// 最下行に出すモードの名前
    pub fn mode_label(&self) -> &'static str {
        match self.ed.state() {
            State::Insert => "INSERT",
            State::Visual { .. } => "VISUAL",
            _ => "NORMAL",
        }
    }

    /// IME の確定や貼り付けを入れる（Insert のときだけ）
    pub fn insert_text(&mut self, text: &str) -> FieldOut {
        if !self.is_insert() {
            return FieldOut::None;
        }
        let t = if self.multiline { text.replace("\r\n", "\n") } else { text.trim_end_matches(['\r', '\n']).replace("\r\n", " ").replace(['\n', '\r'], " ") };
        // Vim を使わない欄: 選んでいれば置き換える
        if !self.ed.vim_on {
            self.ed.plain_delete_selection();
        }
        self.ed.insert_text(&t);
        FieldOut::Changed
    }

    /// キーを流す。`regs` はエディタのレジスタ（共有するため、流す間だけ入れ替える）
    pub fn key(&mut self, key: Key, regs: &mut Registers, clip: Option<Clipboard>) -> FieldOut {
        let insert = self.is_insert();
        let normal = self.ed.state() == State::Normal && self.ed.pending_keys.is_empty();
        match key {
            Key::Ctrl('m') => return FieldOut::Submit,
            Key::Enter if !self.multiline && (insert || normal) => return FieldOut::Submit,
            Key::Esc if normal => return FieldOut::Cancel,
            // Vim を使わない欄は、選んでいなければ Esc 1 回で閉じる
            Key::Esc if !self.ed.vim_on && self.ed.state() == State::Insert => return FieldOut::Cancel,
            // Ex のコマンドラインは入力欄の中では使わない
            Key::Char(':') if normal => return FieldOut::None,
            // Vim を使わない欄の Ctrl+A は全部を選ぶ
            Key::Ctrl('a') if !self.ed.vim_on => {
                self.ed.plain_select_all();
                return FieldOut::None;
            }
            Key::Paste | Key::Ctrl('v') if insert => {
                let text = match &clip {
                    Some(cb) => (cb.get)(),
                    None => crate::clipboard::get(),
                };
                return text.map_or(FieldOut::None, |t| self.insert_text(&t));
            }
            _ => {}
        }
        let before = self.text();
        std::mem::swap(&mut self.ed.registers, regs);
        self.ed.clipboard = clip;
        self.ed.key(key);
        std::mem::swap(&mut self.ed.registers, regs);
        if self.ed.state() == State::Command {
            self.ed.set_state(State::Normal);
        }
        if self.text() != before { FieldOut::Changed } else { FieldOut::None }
    }

    /// 描く用: カーソルの (行, 行の中の byte)、Visual の範囲（絶対 byte）
    pub fn cursor(&self) -> (usize, usize) {
        (self.ed.cursor.line, self.ed.cursor.byte)
    }

    pub fn visual(&self) -> Option<(usize, usize)> {
        matches!(self.ed.state(), State::Visual { .. }).then(|| {
            let (a, b, _) = self.ed.visual_range();
            (a, b)
        })
    }

    /// 行の先頭の絶対 byte
    pub fn line_start(&self, l: usize) -> usize {
        self.ed.buf.line_start(l)
    }
}

impl VimField {
    /// 欄に移ったとき: ノーマルになっていれば行末から Insert に戻す
    pub fn reinsert(&mut self) {
        if !self.is_insert() {
            self.ed.set_state(State::Normal);
            self.ed.pending_keys.clear();
            self.ed.key(Key::Char('A'));
        }
    }
}

impl Editor {
    /// `pick` で選んだ入力欄にキーを流す（レジスタとクリップボードはエディタと共有）
    pub(crate) fn feed_field(&mut self, key: Key, pick: fn(&mut Editor) -> Option<&mut VimField>) -> FieldOut {
        let mut regs = std::mem::take(&mut self.registers);
        let clip = self.clipboard;
        let (out, modes) = match pick(self) {
            Some(f) => {
                let before = f.mode_label();
                let out = f.key(key, &mut regs, clip);
                (out, Some((before, f.mode_label())))
            }
            None => (FieldOut::None, None),
        };
        self.registers = regs;
        // 欄の中でモードが変わったら最下行に出す
        if let (Some((a, b)), FieldOut::None | FieldOut::Changed) = (modes, out) {
            if a != b {
                // モードの表示はすぐ書き換わるので履歴に残さない
                self.notify.progress(match b {
                    "NORMAL" => "-- NORMAL --（入力欄。Enter で決める・もう一度 Esc で閉じる）".into(),
                    m => format!("-- {m} --"),
                });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(f: &mut VimField, regs: &mut Registers, k: &str) -> FieldOut {
        let mut last = FieldOut::None;
        for key in nagi_vim::parse_keys(k, " ") {
            last = f.key(key, regs, None);
        }
        last
    }

    /// Vim を使わない欄: Ctrl+A で全部を選び、打つ・貼ると置き換わる
    #[test]
    fn plain_select_all() {
        set_vim(false);
        let mut regs = Registers::new();
        let mut f = VimField::new("abc", false);
        f.key(Key::Ctrl('a'), &mut regs, None);
        assert_eq!(f.selected_text().as_deref(), Some("abc"));
        assert!(f.is_insert());
        f.key(Key::Char('x'), &mut regs, None);
        assert_eq!(f.text(), "x");
        f.key(Key::Ctrl('a'), &mut regs, None);
        f.insert_text("yz");
        assert_eq!(f.text(), "yz");
        set_vim(true);
    }

    #[test]
    fn insert_then_normal_commands() {
        let mut regs = Registers::new();
        let mut f = VimField::new("status = 'OPEN'", false);
        assert!(f.is_insert(), "Insert で始める");
        assert_eq!(keys(&mut f, &mut regs, " and total > 1000"), FieldOut::Changed);
        assert_eq!(f.text(), "status = 'OPEN' and total > 1000");
        // Esc でノーマル → b で語の頭へ、ciw で書き換え
        keys(&mut f, &mut regs, "<Esc>bciw5000<Esc>");
        assert_eq!(f.text(), "status = 'OPEN' and total > 5000");
        // 0 で行頭、dw で 1 語消す、u で戻す
        keys(&mut f, &mut regs, "0dw");
        assert_eq!(f.text(), "= 'OPEN' and total > 5000");
        keys(&mut f, &mut regs, "u");
        assert_eq!(f.text(), "status = 'OPEN' and total > 5000");
        // yiw したものはエディタのレジスタに残る
        keys(&mut f, &mut regs, "yiw");
        assert_eq!(regs.get(&'"').map(|r| r.0.as_str()), Some("status"));
        // ci' で引用の中を書き換える
        keys(&mut f, &mut regs, "fOci'CLOSED<Esc>");
        assert_eq!(f.text(), "status = 'CLOSED' and total > 5000");
        // : は使わない、Enter で決める、ノーマルの Esc で閉じる
        assert_eq!(keys(&mut f, &mut regs, ":"), FieldOut::None);
        assert_eq!(keys(&mut f, &mut regs, "<CR>"), FieldOut::Submit);
        assert_eq!(keys(&mut f, &mut regs, "<Esc>"), FieldOut::Cancel);
    }

    #[test]
    fn visual() {
        let mut regs = Registers::new();
        let mut f = VimField::new("alpha beta gamma", false);
        keys(&mut f, &mut regs, "<Esc>0v");
        assert!(f.visual().is_some(), "v で Visual に入る: {:?}", f.ed.state());
        keys(&mut f, &mut regs, "e");
        assert_eq!(f.visual(), Some((0, 5)), "alpha を選ぶ");
        keys(&mut f, &mut regs, "y");
        assert_eq!(regs.get(&'"').map(|r| r.0.as_str()), Some("alpha"));
        keys(&mut f, &mut regs, "wviwd");
        assert_eq!(f.text(), "alpha  gamma");
        keys(&mut f, &mut regs, "0vee");
        assert_eq!(f.visual(), Some((0, 12)));
        keys(&mut f, &mut regs, "<Esc>");
        assert!(f.visual().is_none(), "Esc で Visual をやめる（欄は閉じない）");
    }

    #[test]
    fn multiline() {
        let mut regs = Registers::new();
        let mut f = VimField::new("", true);
        keys(&mut f, &mut regs, "件名<CR><CR>本文");
        assert_eq!(f.text(), "件名\n\n本文");
        assert_eq!(keys(&mut f, &mut regs, "<Esc>ggA を直す<C-m>"), FieldOut::Submit);
        assert_eq!(f.text(), "件名 を直す\n\n本文");
    }
}
