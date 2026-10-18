//! 折りたたみ（tree-sitter の構文木から。構文木の無いファイルでは Visual の `zf` で行を選んで畳む）。
//!
//! 閉じた折りたたみは行の範囲で持つ（バッファごと）。入れ子は外側が勝つ。
//! - `j` `k` は閉じた折りたたみを 1 行として飛ばし、`dd` などの行の操作は折りたたみ全体にかかる
//! - 中を編集したら、その折りたたみは開く（Vim と同じ）
//! - 検索やマークで中へ跳んだら開く（Vim の foldopen の既定と同じ）

use crate::editor::{Editor, State};

#[derive(Default, Clone, Debug)]
pub struct Folds {
    /// 閉じた折りたたみ（行の範囲。両端を含む）
    pub closed: Vec<(usize, usize)>,
}

impl Folds {
    pub fn is_empty(&self) -> bool {
        self.closed.is_empty()
    }

    /// `line` を含む一番外側の閉じた折りたたみ
    pub fn outer_at(&self, line: usize) -> Option<(usize, usize)> {
        self.closed.iter().filter(|&&(s, e)| s <= line && line <= e).copied().min_by_key(|&(s, e)| (s, usize::MAX - e))
    }

    /// 見える行に寄せる（閉じた折りたたみの中なら、その頭の行）
    pub fn snap(&self, line: usize) -> usize {
        self.outer_at(line).map_or(line, |(s, _)| s)
    }

    /// 次の見える行
    pub fn next_visible(&self, line: usize, total: usize) -> Option<usize> {
        let n = match self.outer_at(line) {
            Some((_, e)) => e + 1,
            None => line + 1,
        };
        (n < total).then_some(n)
    }

    pub fn prev_visible(&self, line: usize) -> Option<usize> {
        let line = self.snap(line);
        (line > 0).then(|| self.snap(line - 1))
    }

    /// `top` から下へ `count` 本の見える行
    pub fn visible_from(&self, top: usize, count: usize, total: usize) -> Vec<usize> {
        let mut out = Vec::with_capacity(count);
        let mut l = self.snap(top);
        while out.len() < count && l < total {
            out.push(l);
            match self.next_visible(l, total) {
                Some(n) => l = n,
                None => break,
            }
        }
        out
    }

    /// 編集の前に呼ぶ: `sl..=el` の行を `nl` 本の改行を含む文字列に置き換える
    pub fn shift(&mut self, sl: usize, el: usize, nl: usize) {
        // 中を編集した折りたたみは開き、後ろのものはずらす
        self.closed.retain(|&(s, e)| !(sl <= e && el >= s));
        for (s, e) in self.closed.iter_mut() {
            if *s > el {
                *s = *s + nl - (el - sl);
                *e = *e + nl - (el - sl);
            }
        }
    }
}

impl Editor {
    /// `z` のコマンド（c o a M R E j k と、Visual の f）
    pub(crate) fn fold_cmd(&mut self, c: char, visual: Option<(usize, usize)>) {
        if c != 'f' {
            self.ensure_syntax();
        }
        let line = self.cursor.line;
        let syntax_folds = |ed: &Editor, all: bool| -> Option<Vec<(usize, usize)>> {
            let s = ed.syntax.as_ref()?;
            Some(if all { s.all_folds() } else { s.folds_at(line) })
        };
        match c {
            'f' => {
                if let Some((l1, l2)) = visual {
                    if l2 > l1 {
                        self.folds.closed.push((l1, l2));
                    }
                }
                self.set_state(State::Normal);
                self.cursor.line = visual.map_or(line, |v| v.0);
            }
            'c' | 'a' if c == 'c' || self.folds.outer_at(line).is_none() => {
                if let Some(s) = &mut self.syntax {
                    s.sync(&mut self.buf);
                }
                let Some(cands) = syntax_folds(self, false) else {
                    self.notify.error("E490: 折りたたみがありません（構文木の無いファイルは Visual の zf で作る）");
                    return;
                };
                // 内側から、まだ閉じていないものを閉じる（閉じていれば外側を）
                let outer = self.folds.outer_at(line);
                match cands.into_iter().find(|f| !self.folds.closed.contains(f) && outer.is_none_or(|o| f.0 <= o.0 && f.1 >= o.1)) {
                    Some(f) => self.folds.closed.push(f),
                    None if outer.is_none() => self.notify.error("E490: 折りたたみがありません"),
                    None => {}
                }
            }
            'o' | 'a' => {
                // 見えている（一番外側の）閉じた折りたたみを開く
                match self.folds.outer_at(line) {
                    Some(f) => self.folds.closed.retain(|&x| x != f),
                    None if c == 'o' => self.notify.error("E490: 閉じた折りたたみはありません"),
                    None => {}
                }
            }
            'M' => {
                if let Some(s) = &mut self.syntax {
                    s.sync(&mut self.buf);
                }
                if let Some(all) = syntax_folds(self, true) {
                    for f in all {
                        if !self.folds.closed.contains(&f) {
                            self.folds.closed.push(f);
                        }
                    }
                }
            }
            'R' | 'E' => self.folds.closed.clear(),
            'j' | 'k' => {
                let mut all = syntax_folds(self, true).unwrap_or_default();
                all.extend(self.folds.closed.iter().copied());
                let to = if c == 'j' { all.iter().map(|f| f.0).filter(|&s| s > line).min() } else { all.iter().map(|f| f.1).filter(|&e| e < line).max() };
                if let Some(l) = to {
                    self.cursor.line = l;
                }
            }
            _ => {}
        }
        self.cursor.line = self.folds.snap(self.cursor.line);
        self.clamp_normal();
    }

    /// 行の操作の範囲: 見える行で `n` 本（閉じた折りたたみは丸ごと 1 本）の最後の行
    pub(crate) fn fold_extend(&self, from: usize, n: usize) -> usize {
        let total = self.buf.line_count();
        let mut l = self.folds.snap(from);
        for _ in 1..n {
            match self.folds.next_visible(l, total) {
                Some(x) => l = x,
                None => break,
            }
        }
        self.folds.outer_at(l).map_or(l, |(_, e)| e)
    }

    /// 縦の移動の行き先（閉じた折りたたみを 1 行として数える）
    pub(crate) fn fold_vertical(&self, from: usize, d: isize) -> usize {
        let total = self.buf.line_count();
        let mut l = self.folds.snap(from);
        for _ in 0..d.unsigned_abs() {
            let next = if d > 0 { self.folds.next_visible(l, total) } else { self.folds.prev_visible(l) };
            match next {
                Some(x) => l = x,
                None => break,
            }
        }
        l
    }

    /// カーソルが閉じた折りたたみの途中にあれば、その折りたたみを開く（跳んだ先を見せる）
    pub(crate) fn open_folds_at_cursor(&mut self) {
        let line = self.cursor.line;
        self.folds.closed.retain(|&(s, e)| !(s < line && line <= e));
    }

    /// `zz` `zt` `zb`: カーソルの行を画面の中央・上・下へ
    pub(crate) fn scroll_cursor(&mut self, c: char) {
        let rows = self.rows.max(1);
        let line = self.cursor.line;
        self.top = match c {
            't' => line,
            'b' => line.saturating_sub(rows - 1),
            _ => line.saturating_sub(rows / 2),
        };
        self.frac = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    fn ed(src: &str) -> Editor {
        let mut buf = Buffer::from_str(src);
        let mut syn = nagi_syntax::Syntax::new(nagi_syntax::Lang::Rust).unwrap();
        syn.parse(&mut buf);
        let mut e = Editor::new(buf, Some(syn));
        e.rows = 20;
        e
    }

    fn keys(e: &mut Editor, k: &str) {
        for key in nagi_vim::parse_keys(k, " ") {
            e.key(key);
        }
    }

    const RS: &str = "fn a() {\n    let x = 1;\n    let y = 2;\n}\nfn b() {\n    b1();\n}\nend\n";

    #[test]
    fn close_move_delete() {
        let mut e = ed(RS);
        // 関数 a の中で zc → a 全体（0〜3 行）を閉じ、カーソルは頭の行へ
        keys(&mut e, "jzc");
        assert_eq!(e.folds.closed, vec![(0, 3)]);
        assert_eq!(e.cursor.line, 0);
        // j は折りたたみを 1 行として飛ばす
        keys(&mut e, "j");
        assert_eq!(e.cursor.line, 4);
        keys(&mut e, "k");
        assert_eq!(e.cursor.line, 0);
        assert_eq!(e.folds.visible_from(0, 10, e.buf.line_count()), vec![0, 4, 5, 6, 7]);
        // dd は折りたたみ全体を消す（折りたたみも消える）
        keys(&mut e, "dd");
        assert_eq!(e.buf.line(0), "fn b() {");
        assert!(e.folds.is_empty());
        // zM で全部、zR で全部開く。za で切り替え
        keys(&mut e, "zM");
        assert_eq!(e.folds.closed, vec![(0, 2)]);
        keys(&mut e, "za");
        assert!(e.folds.is_empty());
        keys(&mut e, "zaj");
        assert_eq!(e.cursor.line, 3, "閉じた b を飛ばして end へ");
        keys(&mut e, "zR");
        assert!(e.folds.is_empty());
    }

    #[test]
    fn open_on_edit_and_jump() {
        let mut e = ed(RS);
        keys(&mut e, "zc");
        // 検索で中へ跳ぶと開く
        keys(&mut e, "/let y<CR>");
        assert_eq!(e.cursor.line, 2);
        assert!(e.folds.is_empty());
        // 中を編集したら開く（a の頭の行を消すと a だけ開く）
        keys(&mut e, "ggzc5Gzc");
        assert_eq!(e.folds.closed.len(), 2);
        keys(&mut e, "ggx");
        assert_eq!(e.folds.closed, vec![(4, 6)]);
    }

    #[test]
    fn manual_fold_without_syntax() {
        let mut e = Editor::new(Buffer::from_str("1\n2\n3\n4\n"), None);
        keys(&mut e, "zc");
        assert!(e.notify.text.starts_with("E490"));
        keys(&mut e, "jVjzf");
        assert_eq!(e.folds.closed, vec![(1, 2)]);
        keys(&mut e, "ggjj");
        assert_eq!(e.cursor.line, 3);
    }
}
