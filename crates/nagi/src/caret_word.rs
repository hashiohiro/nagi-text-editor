//! キャレットの所の識別子と同じ表記のものを強調する（IntelliJ の「キャレット位置の要素の使用箇所を強調表示」）。
//!
//! - カーソルが識別子の上（直後も含む）にあれば、作業中の窓の見えている範囲で、同じ表記の識別子
//!   （大文字小文字を区別し、単語の単位）に地の色を付ける。名前の一致に基づく（意味は見ない）
//! - クリックでもキーでも、カーソルを動かしただけならすぐ出す。打っている間は消し、
//!   打つのを止めて少し待ってから出す（IntelliJ と同じ）
//! - キーワード・文字列・注釈・数の上では出さない
//! - 出すのは Normal・Insert の間だけ（選択・検索の入力中は出さない）

use std::path::PathBuf;
use std::time::{Duration, Instant};

use nagi_syntax::Hl;

use crate::editor::{Editor, State};
use crate::nav::ident_char;

/// 打つのを止めてから出すまで
const AFTER_EDIT: Duration = Duration::from_millis(300);

#[derive(Default)]
pub struct CaretWord {
    /// 最後に見た (ファイル, バッファの版, カーソルの位置（絶対 byte）)
    seen: Option<(Option<PathBuf>, u64, usize)>,
    /// 強調している名前
    name: Option<String>,
    /// 編集の後、出す時刻
    due: Option<Instant>,
}

impl Editor {
    /// 描く前に呼ぶ: ファイル・版・カーソルが変わったら強調を決め直す
    pub(crate) fn update_caret_word(&mut self, now: Instant) {
        let at = self.core.buf.line_start(self.core.cursor.line) + self.core.cursor.byte;
        let key = (self.core.buf.path.clone(), self.core.buf.version, at);
        if self.caret_word.seen.as_ref() == Some(&key) {
            return;
        }
        let edited = self.caret_word.seen.as_ref().is_some_and(|(p, v, _)| *p == key.0 && *v != key.1);
        self.caret_word.seen = Some(key);
        if edited {
            self.caret_word.name = None;
            self.caret_word.due = Some(now + AFTER_EDIT);
        } else if self.caret_word.due.is_none() {
            self.caret_word.name = self.caret_word_name();
        }
    }

    /// 打つのを止めて待ったら出す。出したら true（描き直す）
    pub(crate) fn fire_caret_word(&mut self, now: Instant) -> bool {
        if self.caret_word.due.is_none_or(|d| now < d) {
            return false;
        }
        self.caret_word.due = None;
        self.caret_word.name = self.caret_word_name();
        self.caret_word.name.is_some()
    }

    pub(crate) fn caret_word_deadline(&self) -> Option<Instant> {
        self.caret_word.due
    }

    /// カーソルの所の、強調する識別子
    fn caret_word_name(&self) -> Option<String> {
        let (name, at) = self.word_at_cursor()?;
        if name.starts_with(|c: char| c.is_ascii_digit()) {
            return None;
        }
        if let Some(syn) = &self.syntax {
            // 一番内側（後に来るもの）の色分けで見る
            let kind = syn.highlight(&self.core.buf, at..at + name.len()).into_iter().filter(|(r, _)| r.start <= at && at < r.end).last().map(|(_, h)| h);
            if matches!(kind, Some(Hl::Keyword | Hl::String | Hl::Comment | Hl::Number | Hl::Escape)) {
                return None;
            }
        }
        Some(name)
    }

    /// 作業中の窓に出す名前（設定がオンで、Normal・Insert の間だけ）
    pub(crate) fn caret_word_shown(&self) -> Option<&str> {
        let name = self.caret_word.name.as_deref()?;
        (matches!(self.state(), State::Normal | State::Insert) && self.settings.bool("editor.highlight_usages")).then_some(name)
    }
}

/// `line` の中で `name` と同じ表記の識別子の範囲（byte。前後が識別子の文字でないもの）
pub(crate) fn occurrences(line: &str, name: &str) -> Vec<(usize, usize)> {
    line.match_indices(name)
        .filter(|(i, _)| {
            let before = line[..*i].chars().next_back().is_none_or(|c| !ident_char(c));
            let after = line[i + name.len()..].chars().next().is_none_or(|c| !ident_char(c));
            before && after
        })
        .map(|(i, _)| (i, i + name.len()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::{Buffer, Cursor};
    use nagi_vim::Key;

    fn ed(text: &str) -> Editor {
        let mut e = Editor::new(Buffer::from_str(text), None);
        e.rows = 5;
        e
    }

    fn put(e: &mut Editor, line: usize, byte: usize) {
        e.core.cursor = Cursor { line, byte, want_col: 0 };
    }

    #[test]
    fn whole_words_case_sensitive() {
        assert_eq!(occurrences("foo fooBar Foo foo_x (foo) $foo foo", "foo"), vec![(0, 3), (22, 25), (32, 35)]);
        assert_eq!(occurrences("名前 名前の", "名前"), vec![(0, 6)], "日本語の識別子も単語の単位");
        assert!(occurrences("bar", "foo").is_empty());
    }

    #[test]
    fn shows_on_caret_move() {
        let mut e = ed("let count = 1;\ncount += Count;\n");
        let now = Instant::now();
        put(&mut e, 1, 2);
        e.update_caret_word(now);
        assert_eq!(e.caret_word_shown(), Some("count"));
        // 識別子の直後（IntelliJ と同じく、その識別子として扱う）
        put(&mut e, 0, 9);
        e.update_caret_word(now);
        assert_eq!(e.caret_word_shown(), Some("count"));
        // 識別子でない所・数の上では出さない
        put(&mut e, 0, 10);
        e.update_caret_word(now);
        assert_eq!(e.caret_word_shown(), None);
        put(&mut e, 0, 12);
        e.update_caret_word(now);
        assert_eq!(e.caret_word_shown(), None);
    }

    #[test]
    fn hidden_while_typing_then_shown_after_delay() {
        let mut e = ed("abc abc\n");
        let now = Instant::now();
        e.update_caret_word(now);
        assert_eq!(e.caret_word_shown(), Some("abc"));
        e.key(Key::Char('i'));
        e.key(Key::Char('x'));
        e.update_caret_word(now);
        assert_eq!(e.caret_word_shown(), None, "打った直後は出さない");
        assert!(!e.fire_caret_word(now + Duration::from_millis(100)));
        assert!(e.fire_caret_word(now + AFTER_EDIT));
        assert_eq!(e.caret_word_shown(), Some("xabc"));
        assert!(e.caret_word_deadline().is_none());
    }

    #[test]
    fn not_in_visual() {
        let mut e = ed("abc abc\n");
        e.update_caret_word(Instant::now());
        e.key(Key::Char('v'));
        assert_eq!(e.caret_word_shown(), None);
        e.key(Key::Esc);
        assert_eq!(e.caret_word_shown(), Some("abc"));
    }

    #[test]
    fn setting_off() {
        let mut e = ed("abc abc\n");
        e.settings.set("editor.highlight_usages", "false", false);
        e.update_caret_word(Instant::now());
        assert_eq!(e.caret_word_shown(), None);
    }
}
