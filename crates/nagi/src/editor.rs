//! 1 ファイル分の編集状態とキー処理（M1 の範囲）。Vim のキーパーサの本格版は nagi-vim（M1 第3段）に移す。

use nagi_core::{Buffer, Cursor, History, Motion, byte_at_col, display_col};
use unicode_segmentation::UnicodeSegmentation;
use winit::keyboard::NamedKey;

/// 行番号の欄の幅（セル）
pub const GUTTER: usize = 6;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Normal,
    Insert,
    /// `:` のコマンドライン
    Command,
}

pub enum Input<'a> {
    Text(&'a str),
    Named(NamedKey),
    Ctrl(char),
}

pub enum Effect {
    None,
    Quit,
}

pub struct Editor {
    pub buf: Buffer,
    hist: History,
    pub cursor: Cursor,
    /// 画面最上段の行
    pub top: usize,
    pub mode: Mode,
    pub count: usize,
    /// 2 打鍵目を待っている前置キー（g, d）
    pub pending: Option<char>,
    pub message: String,
    pub cmdline: String,
    /// IME の変換中の文字列と、その中のカーソル（byte 範囲）
    pub preedit: String,
    pub preedit_cursor: Option<(usize, usize)>,
    /// 無名レジスタ（文字列, 行単位か）
    register: (String, bool),
}

impl Editor {
    pub fn new(buf: Buffer) -> Self {
        Self {
            buf,
            hist: History::default(),
            cursor: Cursor::default(),
            top: 0,
            mode: Mode::Normal,
            count: 0,
            pending: None,
            message: String::new(),
            cmdline: String::new(),
            preedit: String::new(),
            preedit_cursor: None,
            register: (String::new(), false),
        }
    }

    fn motion(&self) -> Motion<'_> {
        Motion { buf: &self.buf, ambiguous_wide: false }
    }

    fn abs(&self) -> usize {
        self.buf.line_start(self.cursor.line) + self.cursor.byte
    }

    fn set_abs(&mut self, abs: usize) {
        let (line, byte) = self.buf.pos_of(abs);
        self.cursor.line = line;
        self.cursor.byte = byte;
        let l = self.buf.line(line);
        self.cursor.want_col = display_col(&l, byte.min(l.len()), false);
    }

    /// Normal ではカーソルを最後の書記素より右に置かない
    fn clamp_normal(&mut self) {
        let line = self.buf.line(self.cursor.line);
        if self.cursor.byte >= line.len() {
            self.cursor.byte = line.grapheme_indices(true).last().map_or(0, |(i, _)| i);
        }
    }

    fn edit(&mut self, start: usize, end: usize, text: &str) {
        let c = self.buf.edit(start, end, text);
        self.hist.record(c);
    }

    fn enter_insert(&mut self) {
        self.hist.begin();
        self.mode = Mode::Insert;
        self.message.clear();
    }

    fn leave_insert(&mut self) {
        self.hist.close();
        self.mode = Mode::Normal;
        self.preedit.clear();
        self.preedit_cursor = None;
        // Vim と同じく、Insert を抜けるとカーソルは 1 つ左へ
        if self.cursor.byte > 0 {
            let mut c = self.cursor;
            self.motion().left(&mut c, 1);
            self.cursor = c;
        }
        self.clamp_normal();
    }

    /// 入力された文字列を挿入する。改行はこのファイルの改行コードにする
    pub fn insert_text(&mut self, s: &str) {
        let nl = self.buf.newline();
        let text: String = s.replace("\r\n", "\n").replace('\r', "\n").replace('\n', nl);
        let at = self.abs();
        self.edit(at, at, &text);
        self.set_abs(at + text.len());
    }

    fn backspace(&mut self) {
        let line = self.buf.line(self.cursor.line);
        if self.cursor.byte > 0 {
            let prev = line[..self.cursor.byte].grapheme_indices(true).last().map_or(0, |(i, _)| i);
            let start = self.buf.line_start(self.cursor.line) + prev;
            self.edit(start, self.abs(), "");
            self.set_abs(start);
        } else if self.cursor.line > 0 {
            // 前の行と結合する（前の行の改行を消す）
            let prev = self.cursor.line - 1;
            let brk = self.buf.line_len_with_break(prev) - self.buf.line(prev).len();
            let end = self.buf.line_start(self.cursor.line);
            self.edit(end - brk, end, "");
            self.set_abs(end - brk);
        }
    }

    /// 行全体（改行込み）の範囲。最終行で改行が無ければ、前の行の改行を含める
    fn line_range(&self, line: usize, n: usize) -> (usize, usize) {
        let last = (line + n).min(self.buf.line_count()) - 1;
        let start = self.buf.line_start(line);
        let end = self.buf.line_start(last) + self.buf.line_len_with_break(last);
        if end == self.buf.len_bytes() && !self.buf.slice_string(start, end).ends_with('\n') && line > 0 {
            let brk = self.buf.line_len_with_break(line - 1) - self.buf.line(line - 1).len();
            (start - brk, end)
        } else {
            (start, end)
        }
    }

    fn delete_lines(&mut self, n: usize) {
        let (start, end) = self.line_range(self.cursor.line, n);
        let mut text = self.buf.slice_string(start, end);
        if !text.ends_with('\n') {
            text = text.trim_start_matches(['\r', '\n']).to_string() + self.buf.newline();
        }
        self.register = (text, true);
        self.edit(start, end, "");
        let line = self.cursor.line.min(self.buf.line_count().saturating_sub(1));
        self.cursor.line = line;
        let mut c = self.cursor;
        self.motion().first_non_blank(&mut c);
        self.cursor = c;
    }

    fn put(&mut self, after: bool) {
        let (text, linewise) = self.register.clone();
        if text.is_empty() {
            return;
        }
        self.hist.begin();
        if linewise {
            let line = if after { self.cursor.line + 1 } else { self.cursor.line };
            let at = if line >= self.buf.line_count() {
                // 最終行の後ろ: 改行が無ければ先に足す
                let end = self.buf.len_bytes();
                if !self.buf.slice_string(end.saturating_sub(1), end).ends_with('\n') {
                    let nl = self.buf.newline();
                    self.edit(end, end, nl);
                }
                self.buf.len_bytes()
            } else {
                self.buf.line_start(line)
            };
            self.edit(at, at, &text);
            self.set_abs(at);
            let mut c = self.cursor;
            self.motion().first_non_blank(&mut c);
            self.cursor = c;
        } else {
            let mut at = self.abs();
            if after && !self.buf.line(self.cursor.line).is_empty() {
                let g = self.buf.line(self.cursor.line)[self.cursor.byte..].graphemes(true).next().map_or(0, str::len);
                at += g;
            }
            self.edit(at, at, &text);
            self.set_abs(at + text.len());
            let mut c = self.cursor;
            self.motion().left(&mut c, 1);
            self.cursor = c;
        }
        self.hist.close();
    }

    pub fn key(&mut self, input: Input, rows: usize) -> Effect {
        let effect = match self.mode {
            Mode::Normal => self.normal(input, rows),
            Mode::Insert => {
                self.insert(input);
                Effect::None
            }
            Mode::Command => self.command(input),
        };
        self.scroll_into_view(rows);
        effect
    }

    fn insert(&mut self, input: Input) {
        match input {
            Input::Named(NamedKey::Escape) => self.leave_insert(),
            Input::Named(NamedKey::Enter) => self.insert_text("\n"),
            Input::Named(NamedKey::Backspace) => self.backspace(),
            Input::Named(NamedKey::Tab) => self.insert_text("\t"),
            Input::Named(NamedKey::ArrowRight) => {
                let line = self.buf.line(self.cursor.line);
                if let Some(g) = line[self.cursor.byte..].graphemes(true).next() {
                    self.cursor.byte += g.len();
                    self.cursor.want_col = display_col(&line, self.cursor.byte, false);
                }
            }
            Input::Text(t) => self.insert_text(t),
            _ => {}
        }
        // 左・上・下の矢印（Motion は借用中の self を触れないので、写しに適用してから戻す）
        if let Input::Named(k @ (NamedKey::ArrowLeft | NamedKey::ArrowUp | NamedKey::ArrowDown)) = input {
            let mut c = self.cursor;
            match k {
                NamedKey::ArrowLeft => self.motion().left(&mut c, 1),
                NamedKey::ArrowUp => self.motion().vertical(&mut c, -1),
                _ => self.motion().vertical(&mut c, 1),
            }
            self.cursor = c;
        }
    }

    fn command(&mut self, input: Input) -> Effect {
        match input {
            Input::Named(NamedKey::Escape) => {
                self.cmdline.clear();
                self.mode = Mode::Normal;
            }
            Input::Named(NamedKey::Backspace) => {
                if self.cmdline.pop().is_none() {
                    self.mode = Mode::Normal;
                }
            }
            Input::Named(NamedKey::Enter) => {
                let cmd = std::mem::take(&mut self.cmdline);
                self.mode = Mode::Normal;
                return self.ex(cmd.trim());
            }
            Input::Text(t) => self.cmdline.push_str(t),
            _ => {}
        }
        Effect::None
    }

    fn write(&mut self) -> bool {
        match self.buf.save() {
            Ok(n) => {
                let name = self.buf.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                self.message = format!("\"{name}\" {}L, {n}B 書き込み", self.buf.line_count());
                true
            }
            Err(e) => {
                self.message = format!("E212: 書き込めません: {e}");
                false
            }
        }
    }

    fn ex(&mut self, cmd: &str) -> Effect {
        match cmd {
            "w" => {
                self.write();
            }
            "q" if self.buf.dirty => self.message = "E37: 変更が保存されていません（:q! で破棄して終了）".into(),
            "q" | "q!" => return Effect::Quit,
            "wq" | "x" => {
                if self.write() {
                    return Effect::Quit;
                }
            }
            "" => {}
            n if n.chars().all(|c| c.is_ascii_digit()) => {
                let line = n.parse::<usize>().unwrap_or(1).saturating_sub(1);
                let mut c = self.cursor;
                self.motion().goto_line(&mut c, line);
                self.cursor = c;
            }
            other => self.message = format!("E492: エディタのコマンドではありません: {other}"),
        }
        Effect::None
    }

    fn normal(&mut self, input: Input, rows: usize) -> Effect {
        let n = self.count.max(1);
        let mut keep = false;
        let mut c = self.cursor;
        let pending = self.pending.take();
        match input {
            Input::Ctrl('d') => self.motion().vertical(&mut c, (rows / 2) as isize),
            Input::Ctrl('u') => self.motion().vertical(&mut c, -((rows / 2) as isize)),
            Input::Ctrl('f') => self.motion().vertical(&mut c, rows as isize),
            Input::Ctrl('b') => self.motion().vertical(&mut c, -(rows as isize)),
            Input::Ctrl('r') => {
                if let Some(at) = self.hist.redo(&mut self.buf) {
                    self.set_abs(at);
                    c = self.cursor;
                }
            }
            Input::Named(NamedKey::Escape) => self.message.clear(),
            Input::Text(t) => match (pending, t) {
                (Some('g'), "g") => self.motion().goto_line(&mut c, self.count.saturating_sub(1)),
                (Some('d'), "d") => {
                    self.delete_lines(n);
                    c = self.cursor;
                }
                (Some(_), _) => {}
                (None, d) if d.len() == 1 && d.as_bytes()[0].is_ascii_digit() && (d != "0" || self.count > 0) => {
                    self.count = self.count * 10 + (d.as_bytes()[0] - b'0') as usize;
                    keep = true;
                }
                (None, "g" | "d") => {
                    self.pending = t.chars().next();
                    keep = true;
                }
                (None, "h") => self.motion().left(&mut c, n),
                (None, "l") => self.motion().right(&mut c, n),
                (None, "j") => self.motion().vertical(&mut c, n as isize),
                (None, "k") => self.motion().vertical(&mut c, -(n as isize)),
                (None, "0") => self.motion().line_start(&mut c),
                (None, "^") => self.motion().first_non_blank(&mut c),
                (None, "$") => self.motion().line_end(&mut c),
                (None, "w") => (0..n).for_each(|_| self.motion().word_forward(&mut c)),
                (None, "b") => (0..n).for_each(|_| self.motion().word_backward(&mut c)),
                (None, "G") => self.motion().goto_line(&mut c, if self.count > 0 { self.count - 1 } else { usize::MAX }),
                (None, "i") => self.enter_insert(),
                (None, "a") => {
                    self.enter_insert();
                    let line = self.buf.line(c.line);
                    if let Some(g) = line[c.byte.min(line.len())..].graphemes(true).next() {
                        c.byte += g.len();
                    }
                }
                (None, "I") => {
                    self.motion().first_non_blank(&mut c);
                    self.enter_insert();
                }
                (None, "A") => {
                    c.byte = self.buf.line(c.line).len();
                    self.enter_insert();
                }
                (None, "o" | "O") => {
                    self.enter_insert();
                    let nl = self.buf.newline();
                    if t == "o" {
                        let at = self.buf.line_start(c.line) + self.buf.line(c.line).len();
                        self.edit(at, at, nl);
                        self.set_abs(at + nl.len());
                    } else {
                        let at = self.buf.line_start(c.line);
                        self.edit(at, at, nl);
                        self.set_abs(at);
                    }
                    c = self.cursor;
                }
                (None, "x") => {
                    let line = self.buf.line(c.line);
                    let start = self.buf.line_start(c.line) + c.byte;
                    let len: usize = line[c.byte.min(line.len())..].graphemes(true).take(n).map(str::len).sum();
                    if len > 0 {
                        self.register = (self.buf.slice_string(start, start + len), false);
                        self.edit(start, start + len, "");
                    }
                }
                (None, "p") => {
                    self.put(true);
                    c = self.cursor;
                }
                (None, "P") => {
                    self.put(false);
                    c = self.cursor;
                }
                (None, "u") => {
                    match self.hist.undo(&mut self.buf) {
                        Some(at) => self.set_abs(at),
                        None => self.message = "これ以上は元に戻せません".into(),
                    }
                    c = self.cursor;
                }
                (None, ":") => {
                    self.mode = Mode::Command;
                    self.cmdline.clear();
                    self.message.clear();
                }
                (None, other) => self.message = format!("未実装: {other}"),
            },
            _ => keep = self.count > 0 || pending.is_some(),
        }
        if keep && self.pending.is_none() {
            self.pending = pending;
        }
        if !keep {
            self.count = 0;
        }
        self.cursor = c;
        if self.mode == Mode::Normal {
            self.clamp_normal();
        }
        Effect::None
    }

    /// IME の変換中の文字列（winit の Ime::Preedit）
    pub fn ime_preedit(&mut self, text: String, cursor: Option<(usize, usize)>) {
        // winit 0.30.13 の preedit カーソルは UTF-16 単位を char 数として返すことがある（#4772）。境界に丸める
        let fix = |i: usize| {
            let mut i = i.min(text.len());
            while !text.is_char_boundary(i) {
                i -= 1;
            }
            i
        };
        self.preedit_cursor = cursor.map(|(a, b)| (fix(a), fix(b)));
        self.preedit = text;
    }

    /// IME の確定（winit の Ime::Commit）。Insert グループの中の 1 つの変更になる
    pub fn ime_commit(&mut self, text: &str) {
        self.preedit.clear();
        self.preedit_cursor = None;
        if self.mode == Mode::Insert {
            self.insert_text(text);
        }
    }

    /// マウスのクリック: 画面上の (行, セル列) へカーソルを移す
    pub fn click(&mut self, row: usize, col: usize, rows: usize) {
        if self.mode == Mode::Command {
            return;
        }
        let line = (self.top + row).min(self.buf.line_count().saturating_sub(1));
        let text = self.buf.line(line);
        let col = col.saturating_sub(GUTTER);
        self.cursor.line = line;
        self.cursor.byte = if self.mode == Mode::Insert && col >= display_col(&text, text.len(), false) {
            text.len()
        } else {
            byte_at_col(&text, col, false)
        };
        self.cursor.want_col = display_col(&text, self.cursor.byte, false);
        if self.mode == Mode::Normal {
            self.clamp_normal();
        }
        self.scroll_into_view(rows);
    }

    /// ホイール: 表示を動かし、カーソルは画面内に留める（Vim と同じ）
    pub fn scroll(&mut self, lines: isize, rows: usize) {
        let max_top = self.buf.line_count().saturating_sub(1);
        self.top = (self.top as isize + lines).clamp(0, max_top as isize) as usize;
        let mut c = self.cursor;
        if c.line < self.top {
            let d = (self.top - c.line) as isize;
            self.motion().vertical(&mut c, d);
        } else if c.line >= self.top + rows {
            let d = -((c.line + 1 - (self.top + rows)) as isize);
            self.motion().vertical(&mut c, d);
        }
        self.cursor = c;
        if self.mode == Mode::Normal {
            self.clamp_normal();
        }
    }

    fn scroll_into_view(&mut self, rows: usize) {
        if self.cursor.line < self.top {
            self.top = self.cursor.line;
        } else if self.cursor.line >= self.top + rows {
            self.top = self.cursor.line + 1 - rows;
        }
    }
}
