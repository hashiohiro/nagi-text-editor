//! IdeaVim の surround と commentary（vim-surround・vim-commentary と同じキー）。
//!
//! - surround: `ys{動き}{c}`・`yss{c}` で囲む、`ds{c}` で外す、`cs{a}{b}` で変える、Visual の `S{c}` で囲む。
//!   開き括弧（`(` `[` `{`）なら内側に空白も入れ、閉じ括弧・`b` `B` なら入れない。引用符と記号はその文字で。
//! - commentary: `gcc`・`gc{動き}`・Visual の `gc` で行コメントを付ける・外す（plain.rs の行コメントと同じ処理）。

use nagi_core::textops;

use crate::editor::Editor;

/// 囲む文字（開き, 閉じ）。囲めない文字なら None
fn pair(c: char) -> Option<(&'static str, &'static str)> {
    Some(match c {
        '(' => ("( ", " )"),
        ')' | 'b' => ("(", ")"),
        '[' => ("[ ", " ]"),
        ']' | 'r' => ("[", "]"),
        '{' => ("{ ", " }"),
        '}' | 'B' => ("{", "}"),
        '<' | '>' | 'a' => ("<", ">"),
        '"' => ("\"", "\""),
        '\'' => ("'", "'"),
        '`' => ("`", "`"),
        '*' => ("*", "*"),
        '_' => ("_", "_"),
        '|' => ("|", "|"),
        _ => return None,
    })
}

/// 外す・変えるときに探す囲み（開き括弧か引用符）
fn target(c: char) -> Option<char> {
    Some(match c {
        '(' | ')' | 'b' => '(',
        '[' | ']' | 'r' => '[',
        '{' | '}' | 'B' => '{',
        '<' | '>' | 'a' => '<',
        '"' | '\'' | '`' => c,
        _ => return None,
    })
}

impl Editor {
    /// [s, e) を c で囲む（行単位なら行の字下げの後から行末まで）
    pub(crate) fn surround_add(&mut self, s: usize, e: usize, lw: bool, c: char) {
        let Some((open, close)) = pair(c) else {
            self.notify.error(format!("{c} では囲めません"));
            return;
        };
        let (s, e) = if lw {
            let (l1, l2) = (self.buf.pos_of(s).0, self.buf.pos_of(e.saturating_sub(1).max(s)).0);
            let first = self.buf.line(l1);
            let last = self.buf.line(l2);
            (self.buf.line_start(l1) + first.len() - first.trim_start().len(), self.buf.line_start(l2) + last.trim_end_matches(['\r', '\n']).len())
        } else {
            (s, e)
        };
        self.hist.begin();
        self.edit(e, e, close);
        self.edit(s, s, open);
        self.hist.close();
        self.set_abs(s);
    }

    /// カーソルを囲む c の (開きの位置, 閉じの位置)（絶対 byte）
    fn surrounding(&self, c: char) -> Option<(usize, usize)> {
        let t = target(c)?;
        if matches!(t, '"' | '\'' | '`') {
            let line = self.buf.line(self.cursor.line);
            let (a, b) = textops::enclosing_quote(&line, self.cursor.byte, t)?;
            let ls = self.buf.line_start(self.cursor.line);
            return Some((ls + a, ls + b));
        }
        textops::enclosing_block(&self.buf, self.abs(), t)
    }

    /// `ds{c}`: 囲みを外す（開き括弧で指したなら内側の空白も）
    pub(crate) fn surround_del(&mut self, c: char) {
        self.surround_replace(c, None);
    }

    /// `cs{a}{b}`: 囲みを a から b へ
    pub(crate) fn surround_change(&mut self, a: char, b: char) {
        match pair(b) {
            Some(p) => self.surround_replace(a, Some(p)),
            None => self.notify.error(format!("{b} では囲めません")),
        }
    }

    fn surround_replace(&mut self, c: char, with: Option<(&str, &str)>) {
        let Some((a, b)) = self.surrounding(c) else {
            self.notify.error(format!("{c} の囲みが見つかりません"));
            return;
        };
        // 開き括弧で指したら、内側の空白も外す（vim-surround と同じ）
        let trim = matches!(c, '(' | '[' | '{');
        let inner = self.buf.slice_string(a + 1, b);
        let (lead, tail) = if trim { (inner.len() - inner.trim_start_matches(' ').len(), inner.len() - inner.trim_end_matches(' ').len()) } else { (0, 0) };
        let (open, close) = with.unwrap_or(("", ""));
        self.hist.begin();
        self.edit(b - tail.min(b - a - 1), b + 1, close);
        self.edit(a, a + 1 + lead.min(b - a - 1), open);
        self.hist.close();
        self.set_abs(a);
    }

    /// 行 a..=b の行コメントを付ける・外す（commentary の gc）。行コメントの分からない種類のファイルなら false
    pub(crate) fn comment_lines(&mut self, a: usize, b: usize) -> bool {
        let Some(mark) = crate::plain::comment_mark(self.buf.path.as_deref()) else {
            self.notify.info("この種類のファイルの行コメントが分かりません");
            return false;
        };
        let lines: Vec<String> = (a..=b).map(|l| self.buf.line(l)).collect();
        let all = lines.iter().filter(|t| !t.trim().is_empty()).all(|t| t.trim_start().starts_with(mark));
        // 付ける位置は、行のいちばん浅い字下げ
        let indent = lines.iter().filter(|t| !t.trim().is_empty()).map(|t| t.len() - t.trim_start().len()).min().unwrap_or(0);
        self.hist.begin();
        for (i, t) in lines.iter().enumerate() {
            let start = self.buf.line_start(a + i);
            if t.trim().is_empty() {
                continue;
            }
            if all {
                let at = t.len() - t.trim_start().len();
                let n = mark.len() + usize::from(t[at + mark.len()..].starts_with(' '));
                self.edit(start + at, start + at + n, "");
            } else {
                self.edit(start + indent, start + indent, &format!("{mark} "));
            }
        }
        self.hist.close();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    fn run(text: &str, keys: &str) -> String {
        let mut e = Editor::new(Buffer::from_str(text), None);
        e.buf.path = Some("x/a.ts".into());
        for k in nagi_vim::parse_keys(keys, " ") {
            e.key(k);
        }
        e.buf.text()
    }

    #[test]
    fn surround() {
        assert_eq!(run("say hello", "wysiw\""), "say \"hello\"");
        assert_eq!(run("say hello", "wysiw("), "say ( hello )");
        assert_eq!(run("  f(x)", "yss]"), "  [f(x)]");
        assert_eq!(run("a \"b\" c", "fbds\""), "a b c");
        assert_eq!(run("a ( b ) c", "fbds("), "a b c");
        assert_eq!(run("a ( b ) c", "fbds)"), "a  b  c");
        assert_eq!(run("a \"b\" c", "fbcs\"'"), "a 'b' c");
        assert_eq!(run("x [1, 2] y", "f1cs]{"), "x { 1, 2 } y");
        assert_eq!(run("one two", "wvlS)"), "one (tw)o");
    }

    #[test]
    fn commentary() {
        assert_eq!(run("a\n  b\nc\n", "gcj"), "// a\n//   b\nc\n");
        assert_eq!(run("// a\nb\n", "gcc"), "a\nb\n");
        assert_eq!(run("a\nb\nc\n", "jVjgc"), "a\n// b\n// c\n");
        // . で繰り返せる
        assert_eq!(run("a\nb\n", "gccj."), "// a\n// b\n");
    }
}
