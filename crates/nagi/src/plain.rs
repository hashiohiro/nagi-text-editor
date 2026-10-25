//! Vim を使わないときのバッファの編集（設定の「Vim のキー操作」がオフ。既定）。
//!
//! 普通のエディタと同じく、打てばそのまま入る（Normal の状態は無い。`set_state(State::Normal)` は Insert になる）。
//! - 矢印・Home・End・PageUp・PageDown で動き、Shift を足すと選ぶ。Ctrl+←→ は単語、Ctrl+Home・End はファイルの端
//! - 選んでいる間に打つ・Backspace・Delete・Enter は、選んだ所を置き換える。Tab・Shift+Tab は行の字下げ
//! - Ctrl+C・X・V・Z・Shift+Z・A・S・D・Y・/ などはキーマップ（keymap.rs の PLAIN_ACTIONS。設定で付け替えられる）から
//!   コマンドの表（Copy・Cut・Paste・Undo・Redo・SelectAll・DuplicateLine・DeleteLine・CommentLine など）を動かす
//!
//! 選んだ所は Visual の状態（起点 `anchor` とカーソル）で持つが、Vim と違って終わりの文字を含まない（visual_range）。
//! 取り消しは、打つ・消すの続きを 1 回にまとめ、動いたり種類が変わったりしたら区切る。

use nagi_vim::{Key, Mods, Nav};
use unicode_segmentation::UnicodeSegmentation;

use crate::editor::{Editor, State};

/// 取り消しをまとめている操作の種類
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Run {
    #[default]
    None,
    Typing,
    Deleting,
}

impl Editor {
    /// Vim のキー操作を使わない（普通のエディタ）
    pub fn plain(&self) -> bool {
        !self.vim_on
    }

    /// Vim を使わないとき、このキーを受けないか: 一覧のパネル・ダイアログでは文字のキーを受けない（操作はマウス・右クリックの
    /// メニュー・矢印・Enter・Esc・Tab・Space・Ctrl の組み合わせで）。入力欄に打っている間は受ける
    pub(crate) fn plain_drops(&self, key: Key) -> bool {
        use crate::editor::Panel;
        if self.vim_on || !matches!(key, Key::Char(c) if c != ' ') {
            return false;
        }
        if !matches!(self.focus, Some(Panel::List | Panel::Log | Panel::Diff | Panel::DbTable | Panel::Settings | Panel::RunDialog)) {
            return false;
        }
        let typing = self.has_prompt()
            || self.in_text_field()
            || self.log.as_ref().is_some_and(|l| l.filter.is_some() && self.state() == State::Log)
            || self.db_table.as_ref().is_some_and(|t| (t.editing.is_some() || t.filter_edit.is_some()) && self.state() == State::DbTable);
        !typing
    }

    /// 本文で何もしていない（Vim なら Normal、使わないなら打てる状態で選んでいない）。
    /// 変更の差分・blame の小窓、カーソルの診断の文はこのときに出す
    pub fn idle(&self) -> bool {
        match self.state() {
            State::Normal => true,
            State::Insert => self.plain() && self.completion.is_none(),
            _ => false,
        }
    }

    /// Vim のキー操作の有効・無効を切り替える（設定を効かせるとき）
    pub(crate) fn set_vim(&mut self, on: bool) {
        if self.vim_on == on {
            return;
        }
        self.vim_on = on;
        crate::vim_field::set_vim(on);
        // 今の状態を読み替える（Vim を切ったら打てる状態へ、入れたら Normal へ）
        match self.state() {
            State::Normal if !on => self.set_state(State::Insert),
            State::Insert if on => self.leave_insert_pub(),
            _ => {}
        }
    }

    /// 選んでいる所 [始め, 終わり)（選んでいなければ None）
    /// Vim を使わないときは終わりの文字を含まない。Vim の Visual（コマンドパレットから呼んだとき）はその範囲
    pub(crate) fn plain_selection(&self) -> Option<(usize, usize)> {
        if !matches!(self.state(), State::Visual { .. }) {
            return None;
        }
        let (s, e, _) = self.visual_range();
        (s < e).then_some((s, e))
    }

    /// 選んだ所の文字（選んでいなければ None）
    pub(crate) fn plain_selected_text(&self) -> Option<String> {
        self.plain_selection().map(|(s, e)| self.buf.slice_string(s, e))
    }

    /// 選んだ所を消して、打てる状態へ（消したら true）
    pub(crate) fn plain_delete_selection(&mut self) -> bool {
        let Some((s, e)) = self.plain_selection() else {
            if matches!(self.state(), State::Visual { .. }) {
                self.set_state(State::Normal);
            }
            return false;
        };
        self.plain_run(Run::Deleting);
        self.edit(s, e, "");
        self.set_abs(s);
        self.set_state(State::Normal);
        true
    }

    /// 取り消しのまとまりを始める・続ける（種類が変わったら区切る）
    fn plain_run(&mut self, run: Run) {
        if self.plain_run != run {
            self.hist.close();
            if run != Run::None {
                self.hist.begin();
            }
            self.plain_run = run;
        }
    }

    /// 取り消しのまとまりを閉じる（動いた・コマンドを動かした）
    pub(crate) fn plain_break(&mut self) {
        self.plain_run(Run::None);
    }

    /// 本文のキー（Vim を使わないとき）。扱ったら true（扱わなければ Insert の処理へ）
    pub(crate) fn plain_key(&mut self, key: Key) -> bool {
        // 入力補完の一覧を出している間の ↑↓・Enter・Tab・Esc は補完へ
        if self.completion.is_some() && matches!(key, Key::Up | Key::Down | Key::Enter | Key::Tab | Key::Esc | Key::PageUp | Key::PageDown) {
            if key == Key::Esc {
                self.completion = None;
                return true;
            }
            return false;
        }
        let none = Mods::default();
        let (nav, mods) = match key {
            Key::Mod(n, m) => (Some(n), m),
            // 修飾の無い移動・削除のキーも同じ表で（Shift を付けてみて、移動のキーか見分ける）
            k => match k.with_mods(Mods { shift: true, ..none }) {
                Some(Key::Mod(n, _)) => (Some(n), none),
                _ => (None, none),
            },
        };
        match (nav, key) {
            (_, Key::Esc) => {
                // 選んでいれば外す（Vim のように 1 つ左へは動かない）
                self.plain_break();
                if matches!(self.state(), State::Visual { .. }) {
                    self.set_state(State::Normal);
                }
                self.git.popup = None;
                true
            }
            (Some(Nav::Backspace | Nav::Delete), _) if self.plain_selection().is_some() => {
                self.plain_delete_selection();
                true
            }
            (Some(Nav::Backspace), _) if mods.ctrl => {
                self.plain_delete_word(false);
                true
            }
            (Some(Nav::Delete), _) if mods.ctrl => {
                self.plain_delete_word(true);
                true
            }
            (Some(Nav::Backspace), _) => {
                self.set_state(State::Normal);
                self.plain_run(Run::Deleting);
                self.backspace_pub();
                true
            }
            (Some(Nav::Delete), _) => {
                self.set_state(State::Normal);
                self.plain_run(Run::Deleting);
                let at = self.abs();
                let next = self.plain_next(at);
                if next > at {
                    self.edit(at, next, "");
                    self.set_abs(at);
                }
                true
            }
            (Some(Nav::Tab), _) => {
                let lines = self.plain_selection().map(|(s, e)| (self.buf.pos_of(s).0, self.buf.pos_of(e.saturating_sub(1).max(s)).0));
                match lines {
                    // 何行かを選んでいれば、その行を字下げ（Shift なら戻す）
                    Some((a, b)) if a != b || mods.shift => self.plain_indent(a, b, !mods.shift),
                    _ if mods.shift => self.plain_indent(self.cursor.line, self.cursor.line, false),
                    _ => {
                        self.plain_delete_selection();
                        self.plain_run(Run::Typing);
                        self.insert_tab();
                    }
                }
                true
            }
            (Some(n), _) => {
                self.plain_move(n, mods);
                true
            }
            (None, Key::Enter) => {
                self.plain_delete_selection();
                self.plain_run(Run::Typing);
                false
            }
            (None, Key::Char(c)) => {
                self.plain_delete_selection();
                // 単語の切れ目で取り消しを区切る（打った文字をまとめて消しすぎない）
                if c == ' ' && self.plain_run == Run::Typing {
                    self.plain_break();
                }
                self.plain_run(Run::Typing);
                false
            }
            _ => false,
        }
    }

    /// `at` の次の書記素の始まり（行末なら改行の後）
    fn plain_next(&self, at: usize) -> usize {
        let (line, byte) = self.buf.pos_of(at);
        let text = self.buf.line(line);
        match text[byte.min(text.len())..].graphemes(true).next() {
            Some(g) => at + g.len(),
            None if line + 1 < self.buf.line_count() => self.buf.line_start(line + 1),
            None => at,
        }
    }

    /// `at` の前の書記素の始まり（行頭なら前の行の末尾）
    fn plain_prev(&self, at: usize) -> usize {
        let (line, byte) = self.buf.pos_of(at);
        let text = self.buf.line(line);
        match text[..byte.min(text.len())].grapheme_indices(true).next_back() {
            Some((i, _)) => self.buf.line_start(line) + i,
            None if line > 0 => self.buf.line_start(line - 1) + self.buf.line_len(line - 1),
            None => at,
        }
    }

    /// 動く（Shift なら選ぶ・広げる）
    fn plain_move(&mut self, n: Nav, m: Mods) {
        self.plain_break();
        let sel = self.plain_selection();
        let from = self.abs();
        // 選んでいて Shift なしの ←→ は、動かずに選んだ所の端へ
        if !m.shift {
            if let (Some((s, e)), Nav::Left | Nav::Right) = (sel, n) {
                if !m.ctrl {
                    self.set_abs(if n == Nav::Left { s } else { e });
                    self.set_state(State::Normal);
                    return;
                }
            }
        }
        let rows = self.rows.max(1);
        match (n, m.ctrl) {
            (Nav::Left, false) => self.set_abs(self.plain_prev(from)),
            (Nav::Right, false) => self.set_abs(self.plain_next(from)),
            (Nav::Left, true) => self.with_motion(|mo, c| mo.word_backward(c, false)),
            (Nav::Right, true) => {
                let line_end = self.buf.line_start(self.cursor.line) + self.buf.line_len(self.cursor.line);
                if from == line_end {
                    self.set_abs(self.plain_next(from));
                } else {
                    self.with_motion(|mo, c| mo.word_forward(c, false));
                    // 次の単語が次の行なら、この行の末尾で止まる
                    if self.abs() > line_end || self.abs() <= from {
                        self.set_abs(line_end);
                    }
                }
            }
            (Nav::Up, _) if self.cursor.line == 0 => self.set_abs(0),
            (Nav::Down, _) if self.cursor.line + 1 >= self.buf.line_count() => self.set_abs(self.buf.len_bytes()),
            (Nav::Up, _) => self.plain_vertical(-1),
            (Nav::Down, _) => self.plain_vertical(1),
            (Nav::PageUp, _) => self.plain_vertical(-(rows as isize)),
            (Nav::PageDown, _) => self.plain_vertical(rows as isize),
            (Nav::Home, true) => self.set_abs(0),
            (Nav::End, true) => self.set_abs(self.buf.len_bytes()),
            // Home は行頭の空白の後へ（そこにいれば行頭へ）
            (Nav::Home, false) => {
                let byte = self.cursor.byte;
                self.with_motion(|mo, c| mo.first_non_blank(c));
                if self.cursor.byte == byte {
                    self.with_motion(|mo, c| mo.line_start(c));
                }
            }
            (Nav::End, false) => self.set_abs(self.buf.line_start(self.cursor.line) + self.buf.line_len(self.cursor.line)),
            _ => {}
        }
        if m.shift {
            if sel.is_none() && !matches!(self.state(), State::Visual { .. }) {
                self.anchor = from;
                self.set_state(State::Visual { line: false, block: false });
            }
        } else if matches!(self.state(), State::Visual { .. }) {
            self.set_state(State::Normal);
        }
    }

    /// 上下に動く（表示の列を保つ。行末より右なら行末）
    fn plain_vertical(&mut self, delta: isize) {
        let want = self.cursor.want_col;
        self.with_motion(|mo, c| mo.vertical(c, delta));
        let len = self.buf.line_len(self.cursor.line);
        self.cursor.byte = self.cursor.byte.min(len);
        self.cursor.want_col = want;
    }

    /// Ctrl+Backspace・Ctrl+Delete: 単語を消す
    fn plain_delete_word(&mut self, forward: bool) {
        self.set_state(State::Normal);
        self.plain_run(Run::Deleting);
        let at = self.abs();
        let mut c = self.cursor;
        if forward {
            self.motion().word_forward(&mut c, false);
        } else {
            self.motion().word_backward(&mut c, false);
        }
        let to = self.buf.line_start(c.line) + c.byte;
        // 動かなければ 1 文字（行の端なら改行）
        let to = if to == at { if forward { self.plain_next(at) } else { self.plain_prev(at) } } else { to };
        let (s, e) = (at.min(to), at.max(to));
        if s < e {
            self.edit(s, e, "");
            self.set_abs(s);
        }
    }

    /// 行 `a`〜`b` を字下げ（`add` でなければ戻す）。選んだ所は行に合わせて保つ
    fn plain_indent(&mut self, a: usize, b: usize, add: bool) {
        self.plain_break();
        self.hist.begin();
        let unit = self.indent_unit();
        let had_sel = matches!(self.state(), State::Visual { .. });
        for l in a..=b {
            let start = self.buf.line_start(l);
            if add {
                if self.buf.line_len(l) > 0 {
                    self.edit(start, start, &unit);
                }
            } else {
                let text = self.buf.line(l);
                let n = if text.starts_with('\t') { 1 } else { text.chars().take(unit.len().max(1)).take_while(|c| *c == ' ').count() };
                if n > 0 {
                    self.edit(start, start + n, "");
                }
            }
        }
        self.hist.close();
        if had_sel {
            self.anchor = self.buf.line_start(a);
            self.set_abs(self.buf.line_start(b) + self.buf.line_len(b));
        } else {
            let len = self.buf.line_len(self.cursor.line);
            self.cursor.byte = self.cursor.byte.min(len);
        }
    }

    /// ダブルクリックで単語、トリプルクリックで行（改行まで）を選ぶ
    pub(crate) fn plain_select_at(&mut self, row: usize, col: usize, clicks: u32) {
        self.plain_break();
        self.set_state(State::Normal);
        self.cursor = self.pos_at(row, col);
        let l = self.cursor.line;
        let start = self.buf.line_start(l);
        let (s, e) = if clicks == 2 {
            let text = self.buf.line(l);
            match nagi_core::textops::word_object(&text, self.cursor.byte.min(text.len().saturating_sub(1)), false, false) {
                Some((a, b)) => (start + a, start + b),
                None => return,
            }
        } else {
            (start, start + self.buf.line_len_with_break(l))
        };
        self.anchor = s;
        self.set_abs(e);
        self.set_state(State::Visual { line: false, block: false });
    }

    /// 本文の右クリック（窓の中の行・列と、メニューを出す画面の位置）。選んでいる所の外なら、そこへカーソルを移してから
    pub(crate) fn text_context(&mut self, row: usize, col: usize, at: (usize, usize)) {
        let pos = self.pos_at(row, col);
        let abs = self.buf.line_start(pos.line) + pos.byte;
        let inside = self.plain_selection().is_some_and(|(s, e)| abs >= s && abs < e);
        if !inside {
            self.click(row, col);
        }
        let run = |name: &'static str| move |e: &mut Editor| {
            e.shortcut(name);
        };
        let m = crate::menu::Menu::new(at.0, at.1)
            .item("切り取り", run("Cut"))
            .item("コピー", run("Copy"))
            .item("貼り付け", run("Paste"))
            .item("すべて選ぶ", run("SelectAll"))
            .sep()
            .item("定義へ", run("GotoDefinition"))
            .item("実装へ", run("GotoImplementation"))
            .item("使っている所", run("Refs"))
            .sep()
            .item("行コメント", run("CommentLine"))
            .item("行のコミット（blame）", run("Gblame"))
            .item("HEAD との差分", run("Gdiff"))
            .sep()
            // どのコマンドもマウスからたどれるように
            .item("コマンドパレット…", run("Commands"));
        self.open_menu(m);
    }

    // ---------------------------------------------------------------- コマンドの表から（キーマップ・メニュー）

    /// 選んだ所をコピー（選んでいなければ今の行）
    pub(crate) fn plain_copy(&mut self, cut: bool) {
        self.plain_break();
        let (s, e, text) = match self.plain_selection() {
            Some((s, e)) => (s, e, self.buf.slice_string(s, e)),
            None => {
                let l = self.cursor.line;
                let s = self.buf.line_start(l);
                let e = s + self.buf.line_len_with_break(l);
                let mut t = self.buf.slice_string(s, e);
                if !t.ends_with('\n') {
                    t.push('\n');
                }
                (s, e, t)
            }
        };
        match &self.clipboard {
            Some(cb) => (cb.set)(&text),
            None => crate::clipboard::set(&text),
        }
        self.registers.insert('"', (text, false));
        if cut && s < e {
            self.hist.begin();
            self.edit(s, e, "");
            self.hist.close();
            self.set_abs(s);
            self.set_state(State::Normal);
        }
    }

    /// クリップボードを貼る（選んでいれば置き換える）
    pub(crate) fn plain_paste(&mut self) {
        let text = match &self.clipboard {
            Some(cb) => (cb.get)(),
            None => crate::clipboard::get(),
        };
        let Some(text) = text.filter(|t| !t.is_empty()) else { return };
        self.plain_break();
        self.hist.begin();
        if let Some((s, e)) = self.plain_selection() {
            self.edit(s, e, "");
            self.set_abs(s);
        }
        self.set_state(State::Normal);
        self.insert_text(&text);
        self.hist.close();
    }

    pub(crate) fn plain_undo(&mut self, redo: bool) {
        self.plain_break();
        self.set_state(State::Normal);
        let at = if redo { self.hist.redo(&mut self.buf) } else { self.hist.undo(&mut self.buf) };
        match at {
            Some(at) => self.set_abs(at),
            None if !redo => self.notify.info("これ以上は元に戻せません"),
            None => {}
        }
    }

    pub(crate) fn plain_select_all(&mut self) {
        self.plain_break();
        self.anchor = 0;
        self.set_abs(self.buf.len_bytes());
        self.set_state(State::Visual { line: false, block: false });
    }

    /// 行を複製する（選んでいれば、その文字を後ろに）
    pub(crate) fn plain_duplicate(&mut self) {
        self.plain_break();
        self.hist.begin();
        match self.plain_selection() {
            Some((s, e)) => {
                let t = self.buf.slice_string(s, e);
                self.edit(e, e, &t);
                self.anchor = e;
                self.set_abs(e + t.len());
            }
            None => {
                let l = self.cursor.line;
                let end = self.buf.line_start(l) + self.buf.line_len(l);
                let text = format!("{}{}", self.buf.newline(), self.buf.line(l));
                self.edit(end, end, &text);
                self.cursor.line += 1;
            }
        }
        self.hist.close();
    }

    /// 行を消す（選んでいれば、かかる行をすべて）
    pub(crate) fn plain_delete_lines(&mut self) {
        self.plain_break();
        let (a, b) = match self.plain_selection() {
            Some((s, e)) => (self.buf.pos_of(s).0, self.buf.pos_of(e.saturating_sub(1).max(s)).0),
            None => (self.cursor.line, self.cursor.line),
        };
        let mut s = self.buf.line_start(a);
        let mut e = self.buf.line_start(b) + self.buf.line_len_with_break(b);
        // 最後の行なら、前の改行から消す
        if e == self.buf.len_bytes() && a > 0 && self.buf.line_len_with_break(b) == self.buf.line_len(b) {
            s = self.buf.line_start(a - 1) + self.buf.line_len(a - 1);
            e = self.buf.len_bytes();
        }
        self.hist.begin();
        self.edit(s, e, "");
        self.hist.close();
        self.set_state(State::Normal);
        let line = a.min(self.buf.line_count().saturating_sub(1));
        self.set_abs(self.buf.line_start(line));
    }

    /// 行コメントを付ける・外す（選んでいれば、かかる行をすべて。全部に付いていれば外す）
    pub(crate) fn plain_comment(&mut self) {
        self.plain_break();
        let (a, b) = match self.plain_selection() {
            Some((s, e)) => (self.buf.pos_of(s).0, self.buf.pos_of(e.saturating_sub(1).max(s)).0),
            None => (self.cursor.line, self.cursor.line),
        };
        // 付ける・外すのは commentary の gc と同じ（surround.rs）
        if !self.comment_lines(a, b) {
            return;
        }
        if self.plain_selection().is_none() {
            let len = self.buf.line_len(self.cursor.line);
            self.cursor.byte = self.cursor.byte.min(len);
            // 1 行なら次の行へ（IntelliJ と同じ）
            if a == b && self.cursor.line + 1 < self.buf.line_count() {
                self.cursor.line += 1;
            }
        }
    }
}

/// 拡張子から行コメントの記号
pub(crate) fn comment_mark(path: Option<&std::path::Path>) -> Option<&'static str> {
    let ext = path?.extension()?.to_string_lossy().to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" | "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "java" | "kt" | "kts" | "js" | "mjs" | "cjs" | "jsx" | "ts" | "tsx" | "go" | "cs" | "swift" | "scala" | "dart" | "zig" | "groovy" | "gradle" | "php" | "css" | "scss" | "json" | "jsonc" | "proto" | "fs" => "//",
        "py" | "rb" | "sh" | "bash" | "zsh" | "ps1" | "psm1" | "yml" | "yaml" | "toml" | "r" | "pl" | "ex" | "exs" | "cmake" | "dockerfile" | "properties" | "conf" | "mk" => "#",
        "sql" | "lua" | "hs" => "--",
        "ini" => ";",
        "vim" => "\"",
        "bat" | "cmd" => "REM",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    fn plain(text: &str) -> Editor {
        let mut e = Editor::new(Buffer::from_str(text), None);
        e.set_screen(80, 20);
        e.set_vim(false);
        e
    }

    fn shift(n: Nav) -> Key {
        Key::Mod(n, Mods { shift: true, ..Default::default() })
    }

    #[test]
    fn types_moves_and_selects() {
        let mut e = plain("hello world\nsecond\n");
        assert_eq!(e.state(), State::Insert, "Vim を切ると打てる状態");
        for c in "Hi ".chars() {
            e.key(Key::Char(c));
        }
        assert_eq!(e.buf.line(0), "Hi hello world");
        // End で行末（最後の文字の後ろ）、→ で次の行の頭
        e.key(Key::End);
        assert_eq!(e.cursor.byte, 14);
        e.key(Key::Right);
        assert_eq!((e.cursor.line, e.cursor.byte), (1, 0));
        e.key(Key::Left);
        assert_eq!((e.cursor.line, e.cursor.byte), (0, 14));
        // Shift+Home で行頭まで選び、打つと置き換わる
        e.key(Key::Home);
        e.key(Key::Home);
        e.key(shift(Nav::End));
        assert_eq!(e.plain_selected_text().as_deref(), Some("Hi hello world"), "終わりの文字を含まない");
        e.key(Key::Char('X'));
        assert_eq!(e.buf.line(0), "X");
        assert_eq!(e.state(), State::Insert);
        // Esc は何も動かさない（Vim のように左へ寄らない）
        e.key(Key::Esc);
        assert_eq!((e.state(), e.cursor.byte), (State::Insert, 1));
        // パネルから戻っても Normal にはならない
        e.set_state(State::Normal);
        assert_eq!(e.state(), State::Insert);
    }

    #[test]
    fn undo_groups_and_clipboard_ops() {
        let mut e = plain("abc\n");
        e.key(Key::End);
        for c in "def ghi".chars() {
            e.key(Key::Char(c));
        }
        assert_eq!(e.buf.line(0), "abcdef ghi");
        // 単語の切れ目で区切る: 1 回目で " ghi"、2 回目で "def" が消える
        e.plain_undo(false);
        assert_eq!(e.buf.line(0), "abcdef");
        e.plain_undo(false);
        assert_eq!(e.buf.line(0), "abc");
        e.plain_undo(true);
        assert_eq!(e.buf.line(0), "abcdef");
        // 行の複製・削除・コメント（拡張子が無いので分からない）
        e.plain_duplicate();
        assert_eq!((e.buf.line(0), e.buf.line(1), e.cursor.line), ("abcdef".to_string(), "abcdef".to_string(), 1));
        e.plain_delete_lines();
        assert_eq!(e.buf.text(), "abcdef\n");
        // 全部を選んで切り取る
        e.plain_select_all();
        e.plain_copy(true);
        assert_eq!(e.buf.text(), "");
        assert_eq!(e.registers.get(&'"').map(|r| r.0.as_str()), Some("abcdef\n"));
    }

    #[test]
    fn tab_indents_selected_lines_and_comment() {
        let mut e = plain("a\nb\nc\n");
        e.buf.path = Some(std::path::PathBuf::from("x.py"));
        e.key(shift(Nav::Down));
        e.key(shift(Nav::Down));
        e.key(Key::Tab);
        assert_eq!(e.buf.text(), "    a\n    b\nc\n", "c の行は頭までしか選んでいないので含めない（普通のエディタと同じ）");
        e.key(shift(Nav::Tab));
        assert_eq!(e.buf.text(), "a\nb\nc\n");
        e.plain_comment();
        assert!(e.buf.text().starts_with("# a\n# b\n"), "{:?}", e.buf.text());
        e.plain_comment();
        assert!(e.buf.text().starts_with("a\nb\n"), "{:?}", e.buf.text());
    }

    #[test]
    fn double_click_selects_word_and_line() {
        let mut e = plain("hello world\nnext\n");
        let x = e.active_rect().x + e.gutter();
        e.select_at(0, x + 7, 2);
        assert_eq!(e.plain_selected_text().as_deref(), Some("world"));
        e.select_at(0, x + 1, 3);
        assert_eq!(e.plain_selected_text().as_deref(), Some("hello world\n"));
        // 打つと置き換わる
        e.key(Key::Char('Z'));
        assert_eq!(e.buf.text(), "Znext\n");
    }

    #[test]
    fn shift_home_after_typing() {
        let mut e = plain("# nagi\n\nA line\n");
        for c in "abc".chars() {
            e.key(Key::Char(c));
        }
        e.key(shift(Nav::Home));
        assert_eq!((e.cursor.line, e.cursor.byte), (0, 0));
        assert_eq!(e.plain_selected_text().as_deref(), Some("abc"));
    }

    /// パネルでは文字のキーを受けず、右クリックのメニューからは同じ操作ができる。確認は窓で答える
    #[test]
    fn panels_take_no_letters_but_menus_work() {
        let dir = std::env::temp_dir().join(format!("nagi-plain-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        let mut buf = Buffer::open(&dir.join("a.txt")).unwrap();
        buf.path = Some(dir.join("a.txt"));
        let mut e = Editor::new(buf, None);
        e.set_screen(120, 40);
        e.set_vim(false);
        e.open_tree();
        for _ in 0..200 {
            e.poll_background();
            if e.tree.as_ref().is_some_and(|t| t.rows.len() >= 2) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(e.state(), State::List);
        // q（Vim なら木を閉じる）は受けない
        e.key(Key::Char('q'));
        assert!(e.tree.is_some(), "文字のキーは受けない");
        // 右クリックのメニューの「木を閉じる」は同じ操作
        let r = e.tree_rect().unwrap();
        let s = crate::layout::Screen { rows: 40, cols: 120 };
        assert!(crate::layout::context(&mut e, s, r.y + 1, r.x + 2));
        let i = e.menu.as_ref().unwrap().items.iter().position(|it| it.label == "木を閉じる").unwrap();
        e.menu_run(i);
        assert!(e.tree.is_none());
        // 確認の窓はクリックでも答えられる（はい）
        let hit = std::rc::Rc::new(std::cell::Cell::new(false));
        let h2 = hit.clone();
        e.ask("消しますか", move |_| h2.set(true));
        let m = e.menu.as_ref().unwrap();
        let b = m.rect(e.screen.0, e.screen.1);
        crate::layout::press(&mut e, s, b.y + 1, b.x + 2, |_| 1);
        assert!(hit.get(), "1 行目（題の下）の「はい」を押した");
        assert!(e.menu.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
