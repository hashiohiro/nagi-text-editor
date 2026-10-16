//! 1 ファイル分の編集状態とキー処理（M1 の範囲）。
//!
//! Normal モードのキーは、App がマッピング（nagi-vim の Matcher）を通してから渡す。
//! ここに届くキーは、すでにマッピングを展開した後のもの。

use nagi_core::{Buffer, Cursor, History, Motion, byte_at_col, display_col};
use nagi_vim::Key;
use unicode_segmentation::UnicodeSegmentation;

/// 行番号の欄の幅（セル）
pub const GUTTER: usize = 6;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Normal,
    Insert,
    /// `:` のコマンドライン
    Command,
}

pub enum Effect {
    None,
    Quit,
    /// 設定（vimrc）を読み直す
    ReloadRc,
}

/// OS のクリップボード（`set clipboard+=unnamedplus` のとき無名レジスタの代わりに使う）
pub struct Clipboard {
    pub get: fn() -> Option<String>,
    pub set: fn(&str),
}

pub struct Editor {
    pub buf: Buffer,
    hist: History,
    pub cursor: Cursor,
    /// 画面最上段の行
    pub top: usize,
    pub mode: Mode,
    pub count: usize,
    /// 2 打鍵目を待っている前置キー（g, d, ]）
    pub pending: Option<char>,
    pub message: String,
    pub cmdline: String,
    /// コマンドライン内のカーソル（byte）
    pub cmd_cursor: usize,
    /// IME の変換中の文字列と、その中のカーソル（byte 範囲）
    pub preedit: String,
    pub preedit_cursor: Option<(usize, usize)>,
    /// 無名レジスタ（文字列, 行単位か）
    register: (String, bool),
    pub clipboard: Option<Clipboard>,
    /// 最後に変更した位置（絶対 byte。g; 用）
    last_change: Option<usize>,
    /// マッピングの続きを待っているキー（showcmd。App が描画前に入れる）
    pub showcmd: String,
    /// 構文木（対応言語のときだけ）
    pub syntax: Option<nagi_syntax::Syntax>,
}

impl Editor {
    pub fn new(buf: Buffer, syntax: Option<nagi_syntax::Syntax>) -> Self {
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
            cmd_cursor: 0,
            preedit: String::new(),
            preedit_cursor: None,
            register: (String::new(), false),
            clipboard: None,
            last_change: None,
            showcmd: String::new(),
            syntax,
        }
    }

    fn motion(&self) -> Motion<'_> {
        Motion { buf: &self.buf, ambiguous_wide: false }
    }

    /// Motion を写しのカーソルに適用して戻す（Motion は self を借用するため）
    fn with_motion(&mut self, f: impl FnOnce(&Motion, &mut Cursor)) {
        let mut c = self.cursor;
        f(&self.motion(), &mut c);
        self.cursor = c;
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
        self.last_change = Some(start);
    }

    fn set_register(&mut self, text: String, linewise: bool) {
        if let Some(cb) = &self.clipboard {
            // Windows のクリップボードは CRLF が普通
            (cb.set)(&text.replace("\r\n", "\n").replace('\n', "\r\n"));
        }
        self.register = (text, linewise);
    }

    fn get_register(&self) -> (String, bool) {
        if let Some(cb) = &self.clipboard {
            if let Some(t) = (cb.get)() {
                let t = t.replace("\r\n", "\n");
                let linewise = t.ends_with('\n');
                return (t, linewise);
            }
        }
        (self.register.0.replace("\r\n", "\n"), self.register.1)
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
            self.with_motion(|m, c| m.left(c, 1));
        }
        self.clamp_normal();
    }

    /// 文字列を挿入する。改行はこのファイルの改行コードにする
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
            text = text.trim_start_matches(['\r', '\n']).to_string() + "\n";
        }
        self.set_register(text, true);
        self.edit(start, end, "");
        self.cursor.line = self.cursor.line.min(self.buf.line_count().saturating_sub(1));
        self.with_motion(|m, c| m.first_non_blank(c));
    }

    /// 現在行の下に空行を足す（ideavimrc の `O` = `:call append(expand('.'), '')`）。カーソルは動かさない
    fn append_blank_line(&mut self) {
        let at = self.buf.line_start(self.cursor.line) + self.buf.line(self.cursor.line).len();
        let nl = self.buf.newline();
        self.hist.begin();
        self.edit(at, at, nl);
        self.hist.close();
    }

    /// p / P。`indent` は ]p（インデントを現在行に合わせる）
    fn put(&mut self, after: bool, indent: bool) {
        let (text, linewise) = self.get_register();
        if text.is_empty() {
            return;
        }
        let nl = self.buf.newline();
        self.hist.begin();
        if linewise {
            let mut text = text;
            if indent {
                text = reindent(&text, &leading_ws(&self.buf.line(self.cursor.line)));
            }
            let text = text.replace('\n', nl);
            let line = if after { self.cursor.line + 1 } else { self.cursor.line };
            let at = if line >= self.buf.line_count() {
                // 最終行の後ろ: 改行が無ければ先に足す
                let end = self.buf.len_bytes();
                if !self.buf.slice_string(end.saturating_sub(1), end).ends_with('\n') {
                    self.edit(end, end, nl);
                }
                self.buf.len_bytes()
            } else {
                self.buf.line_start(line)
            };
            self.edit(at, at, &text);
            self.set_abs(at);
            self.with_motion(|m, c| m.first_non_blank(c));
        } else {
            let text = text.replace('\n', nl);
            let mut at = self.abs();
            if after && !self.buf.line(self.cursor.line).is_empty() {
                let g = self.buf.line(self.cursor.line)[self.cursor.byte..].graphemes(true).next().map_or(0, str::len);
                at += g;
            }
            self.edit(at, at, &text);
            self.set_abs(at + text.len());
            self.with_motion(|m, c| m.left(c, 1));
        }
        self.hist.close();
    }

    pub fn key(&mut self, key: Key, rows: usize) -> Effect {
        let effect = match self.mode {
            Mode::Normal => self.normal(key, rows),
            Mode::Insert => {
                self.insert(key);
                Effect::None
            }
            Mode::Command => self.command(key),
        };
        self.scroll_into_view(rows);
        effect
    }

    fn insert(&mut self, key: Key) {
        match key {
            Key::Esc => self.leave_insert(),
            Key::Enter => self.insert_text("\n"),
            Key::Backspace => self.backspace(),
            Key::Tab => self.insert_text("\t"),
            Key::Left => self.with_motion(|m, c| m.left(c, 1)),
            Key::Up => self.with_motion(|m, c| m.vertical(c, -1)),
            Key::Down => self.with_motion(|m, c| m.vertical(c, 1)),
            Key::Right => {
                let line = self.buf.line(self.cursor.line);
                if let Some(g) = line[self.cursor.byte..].graphemes(true).next() {
                    self.cursor.byte += g.len();
                    self.cursor.want_col = display_col(&line, self.cursor.byte, false);
                }
            }
            Key::Char(c) => self.insert_text(c.encode_utf8(&mut [0; 4])),
            _ => {}
        }
    }

    fn command(&mut self, key: Key) -> Effect {
        match key {
            Key::Esc => {
                self.cmdline.clear();
                self.mode = Mode::Normal;
            }
            Key::Backspace => {
                if self.cmdline.is_empty() {
                    self.mode = Mode::Normal;
                } else if self.cmd_cursor > 0 {
                    let prev = self.cmdline[..self.cmd_cursor].chars().next_back().map_or(0, char::len_utf8);
                    self.cmd_cursor -= prev;
                    self.cmdline.remove(self.cmd_cursor);
                }
            }
            // Ctrl-u: カーソルより前を消す（`:<C-u>` は回数や範囲の自動入力を消す定番）
            Key::Ctrl('u') => {
                self.cmdline.drain(..self.cmd_cursor);
                self.cmd_cursor = 0;
            }
            Key::Left => self.cmd_cursor -= self.cmdline[..self.cmd_cursor].chars().next_back().map_or(0, char::len_utf8),
            Key::Right => self.cmd_cursor += self.cmdline[self.cmd_cursor..].chars().next().map_or(0, char::len_utf8),
            Key::Home => self.cmd_cursor = 0,
            Key::End => self.cmd_cursor = self.cmdline.len(),
            Key::Enter => {
                let line = std::mem::take(&mut self.cmdline);
                self.cmd_cursor = 0;
                self.mode = Mode::Normal;
                // `|` で区切った複数のコマンド（`source ~/.ideavimrc | echo "..."`）
                let mut effect = Effect::None;
                for cmd in line.split('|') {
                    match self.ex(cmd.trim()) {
                        Effect::None => {}
                        e => effect = e,
                    }
                }
                return effect;
            }
            Key::Char(c) => {
                self.cmdline.insert(self.cmd_cursor, c);
                self.cmd_cursor += c.len_utf8();
            }
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
        let cmd = cmd.trim_start_matches(':').trim();
        let (name, arg) = cmd.split_once(char::is_whitespace).map_or((cmd, ""), |(n, a)| (n, a.trim()));
        match name {
            "" => {}
            "w" | "write" => {
                self.write();
            }
            "q" | "quit" if self.buf.dirty => self.message = "E37: 変更が保存されていません（:q! で破棄して終了）".into(),
            "q" | "quit" | "q!" | "quit!" => return Effect::Quit,
            "wq" | "x" | "xit" => {
                if self.write() {
                    return Effect::Quit;
                }
            }
            "action" => self.action(arg),
            "source" | "so" => return Effect::ReloadRc,
            "echo" => self.message = arg.trim_matches(['"', '\'']).to_string(),
            "noh" | "nohlsearch" => {}
            // ideavimrc の `O`: `:call append(expand('.'), '')`（現在行の下に空行を足す）
            "call" if arg.starts_with("append(") => self.append_blank_line(),
            n if n.chars().all(|c| c.is_ascii_digit()) => {
                let line = n.parse::<usize>().unwrap_or(1).saturating_sub(1);
                self.with_motion(|m, c| m.goto_line(c, line));
            }
            n if n.starts_with('!') => self.message = format!("未実装: 外部コマンド（{cmd}）"),
            n if n.starts_with('%') || n.starts_with("s/") => self.message = format!("未実装: 置換（{cmd}）"),
            _ => self.message = format!("E492: エディタのコマンドではありません: {cmd}"),
        }
        Effect::None
    }

    /// IdeaVim の `:action <IntelliJ のアクション名>` を Nagi の機能に振り分ける
    fn action(&mut self, name: &str) {
        match nagi_vim::action::ideavim(name) {
            Some((Some("jump.last_change"), _)) => self.jump_last_change(),
            Some((Some(cmd), desc)) => self.message = format!("未実装: {desc}（{cmd}）"),
            Some((None, desc)) => self.message = format!("Nagi には無い機能: {desc}"),
            None => self.message = format!("不明なアクション: {name}"),
        }
    }

    fn jump_last_change(&mut self) {
        match self.last_change {
            Some(at) => {
                self.set_abs(at.min(self.buf.len_bytes()));
                self.clamp_normal();
            }
            None => self.message = "E664: 変更履歴がありません".into(),
        }
    }

    fn normal(&mut self, key: Key, rows: usize) -> Effect {
        let n = self.count.max(1);
        let mut keep = false;
        let pending = self.pending.take();
        let mut c = self.cursor;
        match (pending, key) {
            (_, Key::Esc) => self.message.clear(),
            (None, Key::Ctrl('d')) => self.motion().vertical(&mut c, (rows / 2) as isize),
            (None, Key::Ctrl('u')) => self.motion().vertical(&mut c, -((rows / 2) as isize)),
            (None, Key::Ctrl('f')) => self.motion().vertical(&mut c, rows as isize),
            (None, Key::Ctrl('b')) => self.motion().vertical(&mut c, -(rows as isize)),
            (None, Key::Ctrl('r')) => {
                if let Some(at) = self.hist.redo(&mut self.buf) {
                    self.set_abs(at);
                    c = self.cursor;
                }
            }
            (None, Key::Down) => self.motion().vertical(&mut c, n as isize),
            (None, Key::Up) => self.motion().vertical(&mut c, -(n as isize)),
            (None, Key::Left) => self.motion().left(&mut c, n),
            (None, Key::Right) => self.motion().right(&mut c, n),
            (Some('g'), Key::Char('g')) => self.motion().goto_line(&mut c, self.count.saturating_sub(1)),
            // 折り返しをしない間は gj / gk は j / k と同じ
            (Some('g'), Key::Char('j')) => self.motion().vertical(&mut c, n as isize),
            (Some('g'), Key::Char('k')) => self.motion().vertical(&mut c, -(n as isize)),
            (Some('g'), Key::Char(';')) => {
                self.jump_last_change();
                c = self.cursor;
            }
            (Some('d'), Key::Char('d')) => {
                self.delete_lines(n);
                c = self.cursor;
            }
            (Some(']'), Key::Char(p @ ('p' | 'P'))) => {
                self.put(p == 'p', true);
                c = self.cursor;
            }
            (Some(_), _) => {}
            (None, Key::Char(d)) if d.is_ascii_digit() && (d != '0' || self.count > 0) => {
                self.count = self.count * 10 + (d as u8 - b'0') as usize;
                keep = true;
            }
            (None, Key::Char(p @ ('g' | 'd' | ']'))) => {
                self.pending = Some(p);
                keep = true;
            }
            (None, Key::Char(ch)) => match ch {
                'h' => self.motion().left(&mut c, n),
                'l' | ' ' => self.motion().right(&mut c, n),
                'j' => self.motion().vertical(&mut c, n as isize),
                'k' => self.motion().vertical(&mut c, -(n as isize)),
                '0' => self.motion().line_start(&mut c),
                '^' => self.motion().first_non_blank(&mut c),
                '$' => self.motion().line_end(&mut c),
                'w' => (0..n).for_each(|_| self.motion().word_forward(&mut c)),
                'b' => (0..n).for_each(|_| self.motion().word_backward(&mut c)),
                'e' => (0..n).for_each(|_| self.motion().word_end(&mut c)),
                'G' => self.motion().goto_line(&mut c, if self.count > 0 { self.count - 1 } else { usize::MAX }),
                'i' => self.enter_insert(),
                'a' => {
                    self.enter_insert();
                    let line = self.buf.line(c.line);
                    if let Some(g) = line[c.byte.min(line.len())..].graphemes(true).next() {
                        c.byte += g.len();
                    }
                }
                'I' => {
                    self.motion().first_non_blank(&mut c);
                    self.enter_insert();
                }
                'A' => {
                    c.byte = self.buf.line(c.line).len();
                    self.enter_insert();
                }
                'o' | 'O' => {
                    self.enter_insert();
                    let nl = self.buf.newline();
                    if ch == 'o' {
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
                'x' => {
                    let line = self.buf.line(c.line);
                    let start = self.buf.line_start(c.line) + c.byte;
                    let len: usize = line[c.byte.min(line.len())..].graphemes(true).take(n).map(str::len).sum();
                    if len > 0 {
                        let t = self.buf.slice_string(start, start + len);
                        self.set_register(t, false);
                        self.edit(start, start + len, "");
                    }
                }
                'p' | 'P' => {
                    self.put(ch == 'p', false);
                    c = self.cursor;
                }
                'u' => {
                    match self.hist.undo(&mut self.buf) {
                        Some(at) => self.set_abs(at),
                        None => self.message = "これ以上は元に戻せません".into(),
                    }
                    c = self.cursor;
                }
                ':' => {
                    self.mode = Mode::Command;
                    self.cmdline.clear();
                    self.cmd_cursor = 0;
                    self.message.clear();
                }
                other => self.message = format!("未実装: {other}"),
            },
            _ => keep = self.count > 0,
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

fn leading_ws(line: &str) -> String {
    line.chars().take_while(|c| *c == ' ' || *c == '\t').collect()
}

/// ]p: 行単位のテキストのインデントを、先頭行が `indent` になるようにずらす
fn reindent(text: &str, indent: &str) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let base = lines.first().map(|l| leading_ws(l)).unwrap_or_default();
    lines
        .iter()
        .map(|l| {
            if l.trim().is_empty() {
                l.to_string()
            } else if let Some(rest) = l.strip_prefix(base.as_str()) {
                format!("{indent}{rest}")
            } else {
                format!("{indent}{}", l.trim_start_matches([' ', '\t']))
            }
        })
        .collect()
}
