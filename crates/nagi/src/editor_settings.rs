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
    /// 今のファイルのインデントの幅（言語ごとの設定 → 全体の設定）
    pub fn indent_width(&self) -> usize {
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
        if self.settings.get("editor.indent_style") == "tabs" { "\t".into() } else { " ".repeat(self.indent_width()) }
    }

    /// Insert の Tab: タブか、次のインデントの位置までの空白
    pub(crate) fn insert_tab(&mut self) {
        if self.settings.get("editor.indent_style") == "tabs" {
            self.insert_text("\t");
            return;
        }
        let w = self.indent_width();
        let line = self.buf.line(self.cursor.line);
        let col = nagi_core::display_col(&line, self.cursor.byte.min(line.len()), false);
        self.insert_text(&" ".repeat(w - col % w));
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
            _ => Some((line - self.top, col)),
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
