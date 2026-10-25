//! 設定の「エディタ」の効き目（インデント・保存時の処理・新しいファイル）。行番号・波線は view.rs、外部の変更は autoread.rs、
//! スクロールと押しっぱなしの速さは main.rs で設定を読む。

use nagi_core::Buffer;

use crate::editor::Editor;

/// 拡張子をインデントの設定の名前にそろえる（tsx → ts、jsx → js、yml → yaml、htm → html）
fn ext_key(ext: &str) -> String {
    match ext.to_ascii_lowercase().as_str() {
        "tsx" | "mts" | "cts" => "ts".into(),
        "jsx" | "mjs" | "cjs" => "js".into(),
        "yml" => "yaml".into(),
        "htm" => "html".into(),
        e => e.into(),
    }
}

/// ファイルのインデントを見分ける（頭の 1000 行）。(タブか, 空白の幅) で、タブなら幅は 0。行が少なければ None。
/// タブで始まる行が多ければタブ、空白なら、深くなる行の増え方（2・4・8・3）でいちばん多いもの
pub fn detect_indent(buf: &Buffer) -> Option<(bool, usize)> {
    let (mut tabs, mut spaces, mut prev) = (0, 0, 0);
    let mut grow = [0usize; 9];
    for l in 0..buf.line_count().min(1000) {
        let line = buf.line(l);
        let body = line.trim_start_matches([' ', '\t']);
        // 空の行と、注釈の続き（/** の下の ` * `。1 つずれる）は数えない
        if body.trim_end().is_empty() || body.starts_with('*') {
            continue;
        }
        let ws = &line[..line.len() - body.len()];
        if ws.starts_with('\t') {
            tabs += 1;
            continue;
        }
        let n = ws.len();
        if n > 0 {
            spaces += 1;
        }
        if n > prev && n - prev < grow.len() {
            grow[n - prev] += 1;
        }
        prev = n;
    }
    if tabs + spaces < 3 {
        return None;
    }
    if tabs > spaces {
        return Some((true, 0));
    }
    let w = [4, 2, 8, 3].into_iter().fold(0, |best, w| if grow[w] > grow[best] { w } else { best });
    (w > 0).then_some((false, w))
}

/// 行末の空白を消し、最後に改行を入れる（どちらも設定でオンのとき）ための直し（始め, 終わり, 文字）。
/// 後ろから並べるので、この順に当てれば位置はずれない
pub fn tidy(buf: &Buffer, trim: bool, final_newline: bool) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    if final_newline {
        let len = buf.len_bytes();
        if len > 0 && !buf.slice_string(len - 1, len).ends_with('\n') {
            out.push((len, len, buf.newline().to_string()));
        }
    }
    if trim {
        for l in (0..buf.line_count()).rev() {
            let line = buf.line(l);
            let t = line.trim_end_matches([' ', '\t']).len();
            if t < line.len() {
                let s = buf.line_start(l);
                out.push((s + t, s + line.len(), String::new()));
            }
        }
    }
    out
}

impl Editor {
    /// 今のファイルから見分けたインデント（設定の「ファイルのインデントを見分ける」がオンのとき）
    fn detected_indent(&self) -> Option<(bool, usize)> {
        self.settings.bool("editor.detect_indent").then(|| detect_indent(&self.buf)).flatten()
    }

    /// インデントにタブを使うか（:set → ファイルから見分けたもの → 設定）
    pub fn indent_tabs(&self) -> bool {
        if let Some(v) = self.settings.session.get("editor.indent_style") {
            return v == "tabs";
        }
        if let Some((tabs, _)) = self.detected_indent() {
            return tabs;
        }
        self.settings.get("editor.indent_style") == "tabs"
    }

    /// タブの表示幅（:set ts → 設定）
    pub fn tab_width(&self) -> usize {
        self.settings.int("editor.tab_width") as usize
    }

    /// タブの表示幅を描く・動かす処理に入れる（窓・ファイルごとに違うので、キーを処理する前・描く前に）
    pub fn sync_tab(&self) {
        nagi_core::set_tab_width(self.tab_width());
    }

    /// 今のファイルのインデントの幅（:set sw → ファイルから見分けたもの → 言語ごとの設定 → 全体の設定）
    pub fn indent_width(&self) -> usize {
        if let Some(w) = self.settings.session.get("editor.indent_width").and_then(|v| v.parse::<usize>().ok()) {
            return w.clamp(1, 16);
        }
        if let Some((false, w)) = self.detected_indent() {
            return w;
        }
        let ext = self.buf.path.as_deref().and_then(|p| p.extension()).map(|e| ext_key(&e.to_string_lossy())).unwrap_or_default();
        for pair in self.settings.list("editor.indent_overrides") {
            if let Some((k, v)) = pair.split_once('=') {
                if ext_key(k.trim()) == ext {
                    if let Ok(w) = v.trim().parse::<usize>() {
                        return w.clamp(1, 16);
                    }
                }
            }
        }
        self.settings.int("editor.indent_width") as usize
    }

    /// インデント 1 段（タブか空白）
    pub fn indent_unit(&self) -> String {
        if self.indent_tabs() { "\t".into() } else { " ".repeat(self.indent_width()) }
    }

    /// Insert の Tab: タブか、次のインデントの位置までの空白
    pub(crate) fn insert_tab(&mut self) {
        if self.indent_tabs() {
            self.insert_text("\t");
            return;
        }
        let w = self.indent_width();
        let line = self.buf.line(self.cursor.line);
        let col = nagi_core::display_col(&line, self.cursor.byte.min(line.len()), false);
        self.insert_text(&" ".repeat(w - col % w));
    }

    /// 改行で入れる文字（自動インデント・{ の後で 1 段深く）と、入れた後にカーソルを戻す byte 数。
    /// カーソルの後ろが閉じ括弧なら、それを次の行へ送ってカーソルを間の行に置く
    pub(crate) fn newline_text(&self) -> (String, usize) {
        let line = self.buf.line(self.cursor.line);
        let at = self.cursor.byte.min(line.len());
        if !self.settings.bool("editor.auto_indent") {
            return ("\n".into(), 0);
        }
        let ws_len = line.len() - line.trim_start_matches([' ', '\t']).len();
        let indent = &line[..ws_len.min(at)];
        let before = line[..at].trim_end();
        let open = before.chars().last().filter(|_| self.settings.bool("editor.smart_indent"));
        let py = self.buf.path.as_deref().and_then(|p| p.extension()).is_some_and(|e| e == "py");
        if !matches!(open, Some('{' | '(' | '[')) && !(py && open == Some(':')) {
            return (format!("\n{indent}"), 0);
        }
        let inner = format!("{indent}{}", self.indent_unit());
        let closer = match open {
            Some('{') => Some('}'),
            Some('(') => Some(')'),
            Some('[') => Some(']'),
            _ => None,
        };
        if closer.is_some() && line[at..].trim_start().starts_with(closer.unwrap()) {
            let tail = format!("\n{indent}");
            return (format!("\n{inner}{tail}"), tail.len());
        }
        (format!("\n{inner}"), 0)
    }

    /// 空白だけの行で閉じ括弧を打ったら 1 段浅くする（設定の「括弧で字下げ」）
    pub(crate) fn dedent_for_closer(&mut self, c: char) {
        if !matches!(c, '}' | ')' | ']') || !self.settings.bool("editor.smart_indent") || !self.settings.bool("editor.auto_indent") {
            return;
        }
        let line = self.buf.line(self.cursor.line);
        let before = &line[..self.cursor.byte.min(line.len())];
        if before.is_empty() || !before.chars().all(|c| c == ' ' || c == '\t') {
            return;
        }
        let cut = if before.ends_with('\t') { 1 } else { before.len().min(self.indent_width()) };
        let ls = self.buf.line_start(self.cursor.line);
        let end = ls + before.len();
        self.edit(end - cut, end, "");
        self.set_abs(end - cut);
    }

    /// ソフトタブ: 空白のインデントの中の Backspace で、前のインデントの位置まで消す範囲（行内 byte）。当てはまらなければ None
    pub(crate) fn soft_tab_back(&self) -> Option<usize> {
        if !self.settings.bool("editor.soft_tabs") || self.indent_tabs() || self.cursor.byte == 0 {
            return None;
        }
        let line = self.buf.line(self.cursor.line);
        let before = &line[..self.cursor.byte.min(line.len())];
        if !before.bytes().all(|b| b == b' ') {
            return None;
        }
        let n = before.len();
        Some(n - ((n - 1) % self.indent_width() + 1))
    }

    /// 保存の前に、設定の「保存時の処理」を今のバッファに当てる（取り消せる）
    pub(crate) fn tidy_before_save(&mut self) {
        let (trim, nl) = (self.settings.bool("editor.trim_trailing"), self.settings.bool("editor.final_newline"));
        if !trim && !nl {
            return;
        }
        let edits = tidy(&self.buf, trim, nl);
        if !edits.is_empty() {
            self.hist.begin();
            for (s, e, t) in edits {
                self.edit(s, e, &t);
            }
            self.hist.close();
            let len = self.buf.line(self.cursor.line).len();
            self.cursor.byte = self.cursor.byte.min(len);
        }
    }

    /// 折り返す幅（設定の「折り返す」がオンのとき。作業中の窓の本文の幅）
    pub fn wrap_width(&self) -> Option<usize> {
        if !self.settings.bool("editor.wrap") {
            return None;
        }
        let w = self.active_rect().w.saturating_sub(self.gutter());
        (w > 4).then_some(w)
    }

    /// 行 `ln` が折り返して使う画面の行数
    pub fn line_rows(&self, ln: usize, w: usize) -> usize {
        let t = self.buf.line(ln);
        (nagi_core::display_col(&t, t.len(), false) + 1).div_ceil(w).max(1)
    }

    /// (行, byte) が作業中の窓のどこに出るか（窓の中の画面の行, 本文の中の列）。見えていなければ None
    pub fn screen_pos(&self, line: usize, byte: usize) -> Option<(usize, usize)> {
        if line < self.top {
            return None;
        }
        let t = self.buf.line(line);
        let col = nagi_core::display_col(&t, byte.min(t.len()), false);
        match self.wrap_width() {
            Some(w) if self.folds.is_empty() => {
                let above: usize = (self.top..line).map(|l| self.line_rows(l, w)).sum();
                Some((above + col / w, col % w))
            }
            // 横に送っていれば、左端からの列（左に隠れていれば None）
            _ => Some((line - self.top, col.checked_sub(self.left)?)),
        }
    }

    /// 新しいファイル（まだ無いパス）のバッファに、設定の改行と BOM を当てる
    pub(crate) fn new_file_defaults(&self, b: &mut Buffer) {
        b.crlf = self.settings.get("editor.new_newline") == "crlf";
        b.bom = self.settings.bool("editor.new_bom");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tidy_trims_and_adds_newline() {
        let mut b = Buffer::from_str("a  \nb\t\nc  ");
        for (s, e, t) in tidy(&b, true, true) {
            b.edit(s, e, &t);
        }
        assert_eq!(b.text(), "a\nb\nc\n");
        assert!(tidy(&b, true, true).is_empty(), "もう直すところは無い");
    }

    #[test]
    fn indent_by_language() {
        let mut e = Editor::new(Buffer::from_str(""), None);
        e.buf.path = Some("x/app.tsx".into());
        assert_eq!(e.indent_unit(), "  ", "ts=2 が tsx にも効く");
        e.buf.path = Some("x/App.java".into());
        assert_eq!(e.indent_unit(), "    ");
        e.settings.set("editor.indent_style", "tabs", false);
        assert_eq!(e.indent_unit(), "\t");
    }

    #[test]
    fn detects_file_indent() {
        let java = "class A {\n  void f() {\n    if (x) {\n      y();\n    }\n  }\n  /**\n   * doc\n   */\n}\n";
        assert_eq!(detect_indent(&Buffer::from_str(java)), Some((false, 2)));
        assert_eq!(detect_indent(&Buffer::from_str("a {\n\tb {\n\t\tc\n\t}\n}\n")), Some((true, 0)));
        assert_eq!(detect_indent(&Buffer::from_str("a\nb\n")), None, "インデントの行が無ければ設定のまま");
        let mut e = Editor::new(Buffer::from_str(java), None);
        e.buf.path = Some("x/A.java".into());
        assert_eq!(e.indent_unit(), "  ", "設定は 4 でも、ファイルが 2 なら 2");
        e.settings.set("editor.detect_indent", "false", false);
        assert_eq!(e.indent_unit(), "    ");
    }

    /// 改行の自動インデント・{ の後で 1 段深く・閉じ括弧で浅く・ソフトタブの Backspace・タブの表示幅
    #[test]
    fn indent_while_typing() {
        use nagi_vim::Key;
        let mut e = Editor::new(Buffer::from_str("    f() {}"), None);
        e.buf.path = Some("x/A.java".into());
        e.settings.set("editor.detect_indent", "false", false);
        e.key(Key::Char('0'));
        e.key(Key::Char('f'));
        e.key(Key::Char('{'));
        e.key(Key::Char('a'));
        e.key(Key::Enter);
        assert_eq!(e.buf.text(), "    f() {\n        \n    }");
        assert_eq!((e.cursor.line, e.cursor.byte), (1, 8));
        // ソフトタブ: 空白のインデントの中の Backspace は 1 段（4）ずつ
        e.key(Key::Backspace);
        assert_eq!(e.buf.line(1), "    ");
        e.key(Key::Char(' '));
        e.key(Key::Backspace);
        e.key(Key::Backspace);
        assert_eq!(e.buf.line(1), "", "5 つなら 1 つ、次に 4 つ");
        // 閉じ括弧で 1 段浅く
        for c in "        }".chars() {
            e.key(Key::Char(c));
        }
        assert_eq!(e.buf.line(1), "    }");
        e.key(Key::Esc);
        // 自動インデントを切れば、改行は何も足さない
        e.settings.set("editor.auto_indent", "false", false);
        e.key(Key::Char('A'));
        e.key(Key::Enter);
        assert_eq!(e.buf.line(2), "");
        e.key(Key::Esc);
        // タブの表示幅
        e.settings.set("editor.tab_width", "8", false);
        e.sync_tab();
        assert_eq!(nagi_core::display_col("\tx", 1, false), 8);
        nagi_core::set_tab_width(4);
    }

    /// 折り返し: 長い行は画面の行を何行も使い、表示位置とマウスの位置もそれに合わせる
    #[test]
    fn wrap_positions() {
        let long = "x".repeat(250);
        let mut e = Editor::new(Buffer::from_str(&format!("a\n{long}\nb\nc\n")), None);
        e.set_screen(100 + crate::editor::GUTTER, 6);
        e.settings.set("editor.wrap", "true", false);
        let w = e.wrap_width().unwrap();
        assert_eq!(w, 100);
        assert_eq!(e.line_rows(1, w), 3, "250 字 + カーソルの 1 で 3 行");
        assert_eq!(e.screen_pos(2, 0), Some((4, 0)));
        assert_eq!(e.screen_pos(1, 120), Some((2, 20)));
        // 押した位置: 3 行目の画面の行（長い行の 2 段目）の 10 列目 → 長い行の 110 文字目
        let c = e.pos_at(2, crate::editor::GUTTER + 10);
        assert_eq!((c.line, c.byte), (1, 110));
        // カーソルを最後の行へ: 長い行が 3 行使うので、上の行が送られる
        e.cursor.line = 3;
        e.scroll_into_view();
        let used: usize = (e.top..=3).map(|l| e.line_rows(l, w)).sum();
        assert!(used <= e.rows, "top {} rows {} used {used}", e.top, e.rows);
    }
}
