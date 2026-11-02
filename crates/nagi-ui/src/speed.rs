//! 打って探すスピードサーチ（IntelliJ の木・一覧と同じ）。
//!
//! 一覧の上で文字を打つと、名前に打った字を含む行を選ぶ（大文字小文字は区別しない）。↑↓ で前後の当たりへ、
//! Backspace で 1 字消し、Esc で閉じる。ほかのキー（Enter・←→ など）は検索を閉じて、一覧にそのまま渡す。
//! 一覧の中身（行の名前）は呼ぶ側が渡す。

use nagi_vim::Key;

#[derive(Default, Debug, Clone)]
pub struct SpeedSearch {
    pub query: String,
}

/// キーを受けたか
#[derive(Debug, PartialEq, Eq)]
pub enum Out {
    /// 検索のキーだった（選び直した行。当たりが無ければ None で、選びはそのまま）
    Took(Option<usize>),
    /// 検索のキーではない（検索は閉じた）。一覧に渡す
    Pass,
}

impl SpeedSearch {
    pub fn active(&self) -> bool {
        !self.query.is_empty()
    }

    pub fn clear(&mut self) {
        self.query.clear();
    }

    /// 名前の中の当たり（byte の範囲）
    pub fn find(&self, name: &str) -> Option<std::ops::Range<usize>> {
        find(name, &self.query)
    }

    /// キーを受ける（`sel` は今選んでいる行、`len` は行の数、`name` は行の名前）
    pub fn key<'a>(&mut self, key: Key, sel: usize, len: usize, name: impl Fn(usize) -> &'a str) -> Out {
        let hit = |q: &str, i: usize| find(name(i), q).is_some();
        match key {
            // 空白は探している途中だけ字にする（初めの空白は一覧のもの）
            Key::Char(c) if !c.is_control() && (c != ' ' || self.active()) => {
                self.query.push(c);
                let q = self.query.clone();
                Out::Took((sel..len).chain(0..sel).find(|&i| hit(&q, i)))
            }
            Key::Backspace if self.active() => {
                self.query.pop();
                let q = self.query.clone();
                Out::Took(if q.is_empty() { None } else { (sel..len).chain(0..sel).find(|&i| hit(&q, i)) })
            }
            Key::Down if self.active() => {
                let q = self.query.clone();
                Out::Took((sel + 1..len).find(|&i| hit(&q, i)))
            }
            Key::Up if self.active() => {
                let q = self.query.clone();
                Out::Took((0..sel).rev().find(|&i| hit(&q, i)))
            }
            Key::Esc if self.active() => {
                self.clear();
                Out::Took(None)
            }
            _ => {
                self.clear();
                Out::Pass
            }
        }
    }
}

/// `name` の中の `q` の当たり（大文字小文字を区別しない。byte の範囲）
pub fn find(name: &str, q: &str) -> Option<std::ops::Range<usize>> {
    if q.is_empty() {
        return None;
    }
    let same = |a: char, b: char| a == b || a.to_lowercase().eq(b.to_lowercase());
    name.char_indices().find_map(|(at, _)| {
        let mut rest = name[at..].char_indices();
        let mut end = at;
        for qc in q.chars() {
            let (i, c) = rest.next()?;
            if !same(c, qc) {
                return None;
            }
            end = at + i + c.len_utf8();
        }
        Some(at..end)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_moves_and_closes() {
        let names = ["Cargo.toml", "src", "main.rs", "lib.rs", "README.md"];
        let mut s = SpeedSearch::default();
        // 打つと、今の行から下へ最初の当たりを選ぶ（大文字小文字は区別しない）
        assert_eq!(s.key(Key::Char('r'), 0, 5, |i| names[i]), Out::Took(Some(0)));
        assert_eq!(s.key(Key::Char('s'), 0, 5, |i| names[i]), Out::Took(Some(2)));
        assert_eq!(s.find("main.rs"), Some(5..7));
        // ↓ は次の当たり、↑ は前の当たり。無ければ動かない
        assert_eq!(s.key(Key::Down, 2, 5, |i| names[i]), Out::Took(Some(3)));
        assert_eq!(s.key(Key::Down, 3, 5, |i| names[i]), Out::Took(None));
        assert_eq!(s.key(Key::Up, 3, 5, |i| names[i]), Out::Took(Some(2)));
        // 当たりの無い字: 選びはそのまま
        assert_eq!(s.key(Key::Char('z'), 2, 5, |i| names[i]), Out::Took(None));
        assert_eq!(s.key(Key::Backspace, 2, 5, |i| names[i]), Out::Took(Some(2)));
        // Esc は検索だけ閉じる。閉じていれば一覧に渡す
        assert_eq!(s.key(Key::Esc, 2, 5, |i| names[i]), Out::Took(None));
        assert!(!s.active());
        assert_eq!(s.key(Key::Esc, 2, 5, |i| names[i]), Out::Pass);
        // 初めの空白・Enter は一覧のもの（Enter は検索を閉じて渡す）
        assert_eq!(s.key(Key::Char(' '), 0, 5, |i| names[i]), Out::Pass);
        s.key(Key::Char('l'), 0, 5, |i| names[i]);
        assert_eq!(s.key(Key::Enter, 3, 5, |i| names[i]), Out::Pass);
        assert!(!s.active());
    }
}
