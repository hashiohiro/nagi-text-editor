//! 1 ファイル分の編集状態とキー処理。
//!
//! Normal / Visual モードのキーは、App がマッピング（nagi-vim の Matcher）を通してから渡す。
//! ここに届くキーは、すでにマッピングを展開した後のもの。Normal / Visual の実行は normal.rs。

use std::collections::HashMap;
use std::path::PathBuf;

use nagi_core::textops::{compile_search, find_matches, next_match};
use nagi_core::{Buffer, Cursor, History, Motion, byte_at_col, display_col};
use nagi_vim::Key;
use unicode_segmentation::UnicodeSegmentation;

/// 行番号の欄の幅（セル）
pub const GUTTER: usize = 6;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Normal,
    Insert,
    Visual { line: bool },
    /// `:` のコマンドライン
    Command,
    /// `/` `?` の検索（最下行）
    Search,
    /// 置換（最下行の 2 行）
    Replace,
}

pub enum Effect {
    None,
    Quit,
    /// 設定（vimrc）を読み直す
    ReloadRc,
    /// `:NagiStats`（`!` 付きは記録を消す）
    Stats { reset: bool },
}

/// OS のクリップボード（`set clipboard+=unnamedplus` のとき無名レジスタの代わりに使う）
pub struct Clipboard {
    pub get: fn() -> Option<String>,
    pub set: fn(&str),
}

/// 1 行の入力欄（コマンドライン・検索語・置換後）
#[derive(Default, Clone)]
pub struct Prompt {
    pub text: String,
    /// 入力欄の中のカーソル（byte）
    pub cursor: usize,
}

impl Prompt {
    pub fn set(&mut self, s: &str) {
        self.text = s.to_string();
        self.cursor = self.text.len();
    }

    pub fn insert(&mut self, s: &str) {
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    /// 編集キーを処理する。処理したら true
    fn key(&mut self, k: Key) -> bool {
        let prev = |t: &str, i: usize| t[..i].chars().next_back().map_or(0, char::len_utf8);
        match k {
            Key::Char(c) => self.insert(c.encode_utf8(&mut [0; 4])),
            Key::Backspace if self.cursor > 0 => {
                let n = prev(&self.text, self.cursor);
                self.cursor -= n;
                self.text.replace_range(self.cursor..self.cursor + n, "");
            }
            Key::Delete if self.cursor < self.text.len() => {
                let n = self.text[self.cursor..].chars().next().map_or(0, char::len_utf8);
                self.text.replace_range(self.cursor..self.cursor + n, "");
            }
            // Ctrl-u: カーソルより前を消す（`:<C-u>` は回数や範囲の自動入力を消す定番）
            Key::Ctrl('u') => {
                self.text.drain(..self.cursor);
                self.cursor = 0;
            }
            Key::Ctrl('w') => {
                let before = &self.text[..self.cursor];
                let start = before.trim_end().rfind(char::is_whitespace).map_or(0, |i| i + 1);
                self.text.replace_range(start..self.cursor, "");
                self.cursor = start;
            }
            Key::Left => self.cursor -= prev(&self.text, self.cursor),
            Key::Right => self.cursor += self.text[self.cursor..].chars().next().map_or(0, char::len_utf8),
            Key::Home | Key::Ctrl('a') => self.cursor = 0,
            Key::End | Key::Ctrl('e') => self.cursor = self.text.len(),
            _ => return false,
        }
        true
    }
}

/// 検索・置換の状態
#[derive(Default)]
pub struct Search {
    pub forward: bool,
    /// 正規表現として扱うか（Alt+R で切り替え）
    pub regex: bool,
    /// 入力を始めたときのカーソルと表示位置（Esc で戻す）
    origin: Option<(Cursor, usize)>,
    /// 一致（絶対 byte の範囲）。`computed` のバッファと検索語に対して計算したもの
    pub matches: Vec<(usize, usize)>,
    /// 今の一致の番号
    pub current: Option<usize>,
    computed: Option<(u64, String, bool)>,
    /// 最後に確定した検索（検索語, 正規表現か, 下方向か）。n / N / hlsearch 用
    pub last: Option<(String, bool, bool)>,
    /// 一致を強調するか（hlsearch。:noh で消える）
    pub highlight: bool,
}

pub struct Editor {
    pub buf: Buffer,
    pub(crate) hist: History,
    pub cursor: Cursor,
    /// 画面最上段の行
    pub top: usize,
    /// スムーズスクロール中の、行に満たない分（0.0〜1.0 行。描画は top + frac から始める）
    pub frac: f32,
    pub mode: Mode,
    pub message: String,
    pub cmdline: Prompt,
    /// 検索語（置換のときも）と置換後
    pub query: Prompt,
    pub repl: Prompt,
    /// 置換の入力欄のどちらにいるか（0: 検索語, 1: 置換後）
    pub replace_field: usize,
    pub search: Search,
    /// IME の変換中の文字列と、その中のカーソル（byte 範囲）
    pub preedit: String,
    pub preedit_cursor: Option<(usize, usize)>,
    /// レジスタ（文字列, 行単位か）
    pub(crate) registers: HashMap<char, (String, bool)>,
    pub clipboard: Option<Clipboard>,
    /// 最後に変更した位置（絶対 byte。g; 用）
    pub(crate) last_change: Option<usize>,
    /// マッピングの続きを待っているキー（showcmd。App が描画前に入れる）
    pub showcmd: String,
    /// 構文木（対応言語のときだけ）
    pub syntax: Option<nagi_syntax::Syntax>,
    /// Normal / Visual で解釈中のキー列
    pub pending_keys: Vec<Key>,
    /// Visual の起点（絶対 byte）
    pub(crate) anchor: usize,
    /// 最後の f / t（; と , 用）
    pub(crate) last_find: Option<(char, bool, bool)>,
    /// `.` で繰り返すキー列と、記録中のキー列
    pub(crate) dot: Vec<Key>,
    pub(crate) dot_rec: Option<Vec<Key>>,
    pub(crate) replaying: bool,
    /// 画面の行数（App が描画前に入れる）
    pub rows: usize,
    /// バッファ全体の文字列（検索・括弧の対応・text object 用）。版が変わるまで使い回す
    text_cache: std::cell::RefCell<Option<(u64, std::rc::Rc<str>)>>,
}

impl Editor {
    pub fn new(buf: Buffer, syntax: Option<nagi_syntax::Syntax>) -> Self {
        Self {
            buf,
            hist: History::default(),
            cursor: Cursor::default(),
            top: 0,
            frac: 0.0,
            mode: Mode::Normal,
            message: String::new(),
            cmdline: Prompt::default(),
            query: Prompt::default(),
            repl: Prompt::default(),
            replace_field: 0,
            search: Search::default(),
            preedit: String::new(),
            preedit_cursor: None,
            registers: HashMap::new(),
            clipboard: None,
            last_change: None,
            showcmd: String::new(),
            syntax,
            pending_keys: Vec::new(),
            anchor: 0,
            last_find: None,
            dot: Vec::new(),
            dot_rec: None,
            replaying: false,
            rows: 1,
            text_cache: Default::default(),
        }
    }

    /// 最下行に使う行数（置換は 2 行）
    pub fn status_rows(&self) -> usize {
        if self.mode == Mode::Replace { 2 } else { 1 }
    }

    /// 入力欄（IME の確定先）があるモードか
    pub fn has_prompt(&self) -> bool {
        matches!(self.mode, Mode::Command | Mode::Search | Mode::Replace)
    }

    /// バッファ全体の文字列。編集されるまでは同じものを返す（1 打ごとに作り直さない）
    pub(crate) fn whole_text(&self) -> std::rc::Rc<str> {
        let mut cache = self.text_cache.borrow_mut();
        match &*cache {
            Some((v, t)) if *v == self.buf.version => t.clone(),
            _ => {
                let t: std::rc::Rc<str> = self.buf.text().into();
                *cache = Some((self.buf.version, t.clone()));
                t
            }
        }
    }

    pub(crate) fn motion(&self) -> Motion<'_> {
        Motion { buf: &self.buf, ambiguous_wide: false }
    }

    /// Motion を写しのカーソルに適用して戻す（Motion は self を借用するため）
    pub(crate) fn with_motion(&mut self, f: impl FnOnce(&Motion, &mut Cursor)) {
        let mut c = self.cursor;
        f(&self.motion(), &mut c);
        self.cursor = c;
    }

    pub(crate) fn abs(&self) -> usize {
        self.buf.line_start(self.cursor.line) + self.cursor.byte
    }

    pub(crate) fn set_abs(&mut self, abs: usize) {
        let (line, byte) = self.buf.pos_of(abs);
        self.cursor.line = line;
        self.cursor.byte = byte;
        let l = self.buf.line(line);
        self.cursor.want_col = display_col(&l, byte.min(l.len()), false);
    }

    /// Normal ではカーソルを最後の書記素より右に置かない
    pub(crate) fn clamp_normal(&mut self) {
        let line = self.buf.line(self.cursor.line);
        if self.cursor.byte >= line.len() {
            self.cursor.byte = line.grapheme_indices(true).last().map_or(0, |(i, _)| i);
        }
    }

    pub(crate) fn edit(&mut self, start: usize, end: usize, text: &str) {
        let c = self.buf.edit(start, end, text);
        self.hist.record(c);
        self.last_change = Some(start);
    }

    pub(crate) fn enter_insert(&mut self) {
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
        self.finish_dot();
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

    pub fn key(&mut self, key: Key) -> Effect {
        if !self.replaying {
            if let Some(rec) = &mut self.dot_rec {
                rec.push(key);
            }
        }
        let effect = match self.mode {
            Mode::Normal | Mode::Visual { .. } => self.normal_key(key),
            Mode::Insert => {
                self.insert(key);
                Effect::None
            }
            Mode::Command => self.command(key),
            Mode::Search => {
                self.search_key(key);
                Effect::None
            }
            Mode::Replace => {
                self.replace_key(key);
                Effect::None
            }
        };
        self.scroll_into_view();
        effect
    }

    fn insert(&mut self, key: Key) {
        match key {
            Key::Esc | Key::Ctrl('c') => self.leave_insert(),
            // 新しい行のインデントは今の行に合わせる（autoindent。Neovim の既定と同じ）
            Key::Enter => {
                let line = self.buf.line(self.cursor.line);
                let indent: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
                let indent = &indent[..indent.len().min(self.cursor.byte)];
                self.insert_text(&format!("\n{indent}"));
            }
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
            Key::Esc | Key::Ctrl('c') => {
                self.cmdline = Prompt::default();
                self.mode = Mode::Normal;
            }
            Key::Backspace if self.cmdline.text.is_empty() => self.mode = Mode::Normal,
            Key::Enter => {
                let line = std::mem::take(&mut self.cmdline).text;
                self.mode = Mode::Normal;
                // `|` で区切った複数のコマンド（`source ~/.ideavimrc | echo "..."`）。:s の中の | は区切らない
                let cmds: Vec<String> = if self.is_substitute(&line) { vec![line] } else { line.split('|').map(str::to_string).collect() };
                let mut effect = Effect::None;
                for cmd in cmds {
                    match self.ex(cmd.trim()) {
                        Effect::None => {}
                        e => effect = e,
                    }
                }
                return effect;
            }
            k => {
                self.cmdline.key(k);
            }
        }
        Effect::None
    }

    // ---------------------------------------------------------------- 検索・置換

    /// 検索語の一致を（バッファか検索語が変わっていれば）計算し直す
    pub fn refresh_matches(&mut self) {
        let (pattern, regex) = match self.mode {
            Mode::Search | Mode::Replace => (self.query.text.clone(), self.search.regex),
            _ => match (&self.search.last, self.search.highlight) {
                (Some((p, r, _)), true) => (p.clone(), *r),
                _ => {
                    self.search.matches.clear();
                    self.search.computed = None;
                    return;
                }
            },
        };
        let key = (self.buf.version, pattern.clone(), regex);
        if self.search.computed.as_ref() == Some(&key) {
            return;
        }
        self.search.matches = find_matches(&self.whole_text(), &pattern, regex);
        self.search.computed = Some(key);
    }

    /// 確定した検索の、[start, end) の中の一致（画面の強調用）
    pub fn visible_matches(&self, start: usize, end: usize) -> Vec<(usize, usize)> {
        let Some((pattern, regex, _)) = &self.search.last else { return Vec::new() };
        let text = self.buf.slice_string(start, end);
        find_matches(&text, pattern, *regex).into_iter().map(|(s, e)| (s + start, e + start)).collect()
    }

    pub(crate) fn start_search(&mut self, forward: bool, replace: bool, initial: &str) {
        self.search.origin = Some((self.cursor, self.top));
        self.search.forward = forward;
        self.search.current = None;
        self.query.set(initial);
        self.repl = Prompt::default();
        self.replace_field = if replace && !initial.is_empty() { 1 } else { 0 };
        self.mode = if replace { Mode::Replace } else { Mode::Search };
        self.message.clear();
        self.update_incsearch();
    }

    /// 入力のたびに、最寄りの一致へカーソルを動かす（incsearch）
    fn update_incsearch(&mut self) {
        let Some((origin, top)) = self.search.origin else { return };
        self.refresh_matches();
        let at = self.buf.line_start(origin.line) + origin.byte;
        self.search.current = next_match(&self.search.matches, at, self.search.forward, self.mode == Mode::Replace);
        match self.search.current {
            Some(i) => self.set_abs(self.search.matches[i].0),
            None => {
                self.cursor = origin;
                self.top = top;
            }
        }
    }

    fn cancel_search(&mut self) {
        if let Some((c, top)) = self.search.origin.take() {
            self.cursor = c;
            self.top = top;
        }
        self.mode = Mode::Normal;
        self.clamp_normal();
    }

    fn search_key(&mut self, key: Key) {
        match key {
            Key::Esc | Key::Ctrl('c') => self.cancel_search(),
            Key::Backspace if self.query.text.is_empty() => self.cancel_search(),
            Key::Enter => {
                let pattern = self.query.text.clone();
                if pattern.is_empty() {
                    // 空なら前回の検索を繰り返す（Vim と同じ）
                    self.cancel_search();
                    if let Some(at) = self.search_next(false, 1) {
                        self.set_abs(at);
                    }
                    return;
                }
                self.mode = Mode::Normal;
                self.search.last = Some((pattern.clone(), self.search.regex, self.search.forward));
                self.search.highlight = true;
                self.search.origin = None;
                if self.search.current.is_none() {
                    self.message = format!("E486: パターンは見つかりませんでした: {pattern}");
                } else {
                    self.show_count();
                }
                self.clamp_normal();
            }
            Key::Alt('r') => {
                self.search.regex = !self.search.regex;
                self.update_incsearch();
            }
            k => {
                if self.query.key(k) {
                    self.update_incsearch();
                }
            }
        }
    }

    fn show_count(&mut self) {
        if let Some(i) = self.search.current {
            let (p, _, f) = self.search.last.clone().unwrap_or_default();
            self.message = format!("{}{p}  [{}/{}]", if f { '/' } else { '?' }, i + 1, self.search.matches.len());
        }
    }

    /// n / N の移動先。`reverse` は検索の向きを逆にする
    pub(crate) fn search_next(&mut self, reverse: bool, count: usize) -> Option<usize> {
        let Some((_, _, forward)) = self.search.last.clone() else {
            self.message = "E35: 前回の検索パターンがありません".into();
            return None;
        };
        self.search.highlight = true;
        self.refresh_matches();
        let fwd = forward != reverse;
        let mut at = self.abs();
        let mut idx = None;
        for _ in 0..count.max(1) {
            idx = next_match(&self.search.matches, at, fwd, false);
            if let Some(i) = idx {
                at = self.search.matches[i].0;
            }
        }
        match idx {
            Some(i) => {
                self.search.current = Some(i);
                self.show_count();
                Some(self.search.matches[i].0)
            }
            None => {
                let p = self.search.last.as_ref().map(|l| l.0.clone()).unwrap_or_default();
                self.message = format!("E486: パターンは見つかりませんでした: {p}");
                None
            }
        }
    }

    fn replace_key(&mut self, key: Key) {
        match key {
            Key::Esc | Key::Ctrl('c') => {
                self.search.origin = None;
                self.mode = Mode::Normal;
                self.clamp_normal();
            }
            Key::Tab | Key::Down | Key::Up => self.replace_field = 1 - self.replace_field,
            Key::Alt('r') => {
                self.search.regex = !self.search.regex;
                self.update_incsearch();
            }
            // 今の 1 件を置き換えて次へ
            Key::Enter => {
                self.refresh_matches();
                let Some(i) = self.search.current else { return };
                let (s, e) = self.search.matches[i];
                let rep = self.replacement(s, e);
                self.hist.begin();
                self.edit(s, e, &rep);
                self.hist.close();
                self.refresh_matches();
                let after = s + rep.len();
                self.search.current = next_match(&self.search.matches, after, true, true);
                match self.search.current {
                    Some(i) => self.set_abs(self.search.matches[i].0),
                    None => {
                        self.message = "置換が終わりました".into();
                        self.mode = Mode::Normal;
                        self.set_abs(s);
                        self.clamp_normal();
                    }
                }
            }
            // すべて置き換える（1 回の u で戻る）
            Key::Alt('a') => {
                self.refresh_matches();
                let ms = self.search.matches.clone();
                if ms.is_empty() {
                    return;
                }
                let reps: Vec<String> = ms.iter().map(|&(s, e)| self.replacement(s, e)).collect();
                self.hist.begin();
                for (&(s, e), rep) in ms.iter().zip(&reps).rev() {
                    self.edit(s, e, rep);
                }
                self.hist.close();
                self.message = format!("{} 箇所を置換しました", ms.len());
                self.mode = Mode::Normal;
                self.search.origin = None;
                self.set_abs(ms[0].0);
                self.clamp_normal();
            }
            k => {
                let field = self.replace_field;
                let changed = if field == 0 { self.query.key(k) } else { self.repl.key(k) };
                if changed && field == 0 {
                    self.update_incsearch();
                }
            }
        }
    }

    /// 一致 [s, e) の置換後の文字列（正規表現なら $1 などを展開する）。予告表示にも使う
    pub fn replacement(&self, s: usize, e: usize) -> String {
        if !self.search.regex {
            return self.repl.text.clone();
        }
        let matched = self.buf.slice_string(s, e);
        match compile_search(&self.query.text, true) {
            Some(re) => re.replace(&matched, self.repl.text.as_str()).into_owned(),
            None => self.repl.text.clone(),
        }
    }

    // ---------------------------------------------------------------- Ex コマンド

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

    fn open(&mut self, path: PathBuf, force: bool) {
        if self.buf.dirty && !force {
            self.message = "E37: 変更が保存されていません（:e! で破棄して開く）".into();
            return;
        }
        let mut message = String::new();
        let mut buf = match Buffer::open(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut b = Buffer::from_str("");
                b.path = Some(path.clone());
                message = format!("\"{}\" [新規]", path.display());
                b
            }
            Err(e) => {
                self.message = format!("{}: {e}", path.display());
                return;
            }
        };
        let syntax = nagi_syntax::Lang::detect(&path).and_then(nagi_syntax::Syntax::new).map(|mut s| {
            s.parse(&mut buf);
            s
        });
        // 一致の計算結果は前のバッファのもの。版の番号は新しいバッファでも 0 から始まるので、必ず捨てる
        let mut search = std::mem::take(&mut self.search);
        search.computed = None;
        search.matches.clear();
        search.current = None;
        let keep = Self {
            clipboard: self.clipboard.take(),
            registers: std::mem::take(&mut self.registers),
            search,
            rows: self.rows,
            message,
            ..Self::new(buf, syntax)
        };
        *self = keep;
    }

    fn ex(&mut self, cmd: &str) -> Effect {
        let cmd = cmd.trim_start_matches(':').trim();
        if self.is_substitute(cmd) {
            self.substitute(cmd);
            return Effect::None;
        }
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
            "e" | "edit" | "e!" | "edit!" if !arg.is_empty() => self.open(PathBuf::from(arg), name.ends_with('!')),
            "e!" | "edit!" => {
                if let Some(p) = self.buf.path.clone() {
                    self.open(p, true);
                }
            }
            "action" => self.action(arg),
            "source" | "so" => return Effect::ReloadRc,
            "NagiStats" => return Effect::Stats { reset: false },
            "NagiStats!" => return Effect::Stats { reset: true },
            "echo" => self.message = arg.trim_matches(['"', '\'']).to_string(),
            "noh" | "nohlsearch" => self.search.highlight = false,
            // ideavimrc の `O`: `:call append(expand('.'), '')`（現在行の下に空行を足す）
            "call" if arg.starts_with("append(") => self.append_blank_line(),
            n if n.chars().all(|c| c.is_ascii_digit()) => {
                let line = n.parse::<usize>().unwrap_or(1).saturating_sub(1);
                self.with_motion(|m, c| m.goto_line(c, line));
            }
            n if n.starts_with('!') => self.message = format!("未実装: 外部コマンド（{cmd}）"),
            _ => self.message = format!("E492: エディタのコマンドではありません: {cmd}"),
        }
        Effect::None
    }

    /// `[範囲]s/検索/置換/フラグ` の形か（範囲は 空 . % 数字 , $）
    fn is_substitute(&self, cmd: &str) -> bool {
        let cmd = cmd.trim().trim_start_matches(':');
        let body = cmd.trim_start_matches(|c: char| c.is_ascii_digit() || matches!(c, ',' | '%' | '.' | '$'));
        let rest = body.strip_prefix("substitute").or(body.strip_prefix("s"));
        rest.and_then(|r| r.chars().next()).is_some_and(|c| !c.is_alphanumeric() && !c.is_whitespace())
    }

    fn substitute(&mut self, cmd: &str) {
        let body_start = cmd.find(|c: char| !(c.is_ascii_digit() || matches!(c, ',' | '%' | '.' | '$'))).unwrap_or(0);
        let range = &cmd[..body_start];
        let rest = &cmd[body_start..];
        let rest = rest.strip_prefix("substitute").or(rest.strip_prefix("s")).unwrap_or(rest);
        let Some(delim) = rest.chars().next() else { return };
        let parts: Vec<&str> = rest[delim.len_utf8()..].splitn(3, delim).collect();
        let pat = parts.first().copied().unwrap_or("");
        let rep = parts.get(1).copied().unwrap_or("");
        let flags = parts.get(2).copied().unwrap_or("");
        let last = self.buf.line_count().saturating_sub(1);
        let n = |s: &str| match s {
            "." | "" => Some(self.cursor.line),
            "$" => Some(last),
            s => s.parse::<usize>().ok().map(|n| n.saturating_sub(1)),
        };
        let (l1, l2) = match range {
            "%" => (0, last),
            r => {
                let (a, b) = r.split_once(',').unwrap_or((r, r));
                match (n(a), n(b)) {
                    (Some(a), Some(b)) => (a, b.min(last)),
                    _ => {
                        self.message = format!("E16: 範囲が不正です: {r}");
                        return;
                    }
                }
            }
        };
        // 確認付き（c）や検索語が空なら、置換の入力欄を開く（ideavimrc の <C-s> = :%s///cg）
        if pat.is_empty() || flags.contains('c') {
            self.start_search(true, true, pat);
            if !rep.is_empty() {
                self.repl.set(rep);
            }
            if !pat.is_empty() {
                self.search.regex = true;
                self.update_incsearch();
            }
            return;
        }
        let Some(re) = compile_search(pat, true) else {
            self.message = format!("E486: パターンが不正です: {pat}");
            return;
        };
        // Vim の置換文字列（\1 と &）を regex の書き方（${1} と ${0}）に直す
        let rep = rep.replace("\\&", "\u{0}").replace('&', "${0}").replace('\u{0}', "&");
        let rep = (1..=9).fold(rep, |r, i| r.replace(&format!("\\{i}"), &format!("${{{i}}}")));
        let global = flags.contains('g');
        let mut count = 0;
        let mut last_hit = None;
        self.hist.begin();
        for l in (l1..=l2).rev() {
            let line = self.buf.line(l);
            let new = if global { re.replace_all(&line, rep.as_str()) } else { re.replace(&line, rep.as_str()) };
            if new != line {
                last_hit = last_hit.or(Some(l));
                count += if global { re.find_iter(&line).count() } else { 1 };
                let start = self.buf.line_start(l);
                let new = new.into_owned();
                self.edit(start, start + line.len(), &new);
            }
        }
        self.hist.close();
        self.message = if count == 0 { format!("E486: パターンは見つかりませんでした: {pat}") } else { format!("{count} 箇所を置換しました") };
        // カーソルは最後に置換した行の最初の文字へ（Vim と同じ）
        if let Some(l) = last_hit {
            self.cursor.line = l;
            self.with_motion(|m, c| m.first_non_blank(c));
        }
        self.clamp_normal();
    }

    /// IdeaVim の `:action <IntelliJ のアクション名>` を Nagi の機能に振り分ける
    fn action(&mut self, name: &str) {
        match nagi_vim::action::ideavim(name) {
            Some((Some("jump.last_change"), _)) => self.jump_last_change(),
            Some((Some("search.buffer"), _)) => self.start_search(true, false, ""),
            Some((Some("replace.buffer"), _)) => self.start_search(true, true, ""),
            Some((Some(cmd), desc)) => self.message = format!("未実装: {desc}（{cmd}）"),
            Some((None, desc)) => self.message = format!("Nagi には無い機能: {desc}"),
            None => self.message = format!("不明なアクション: {name}"),
        }
    }

    pub(crate) fn jump_last_change(&mut self) {
        match self.last_change {
            Some(at) => {
                self.set_abs(at.min(self.buf.len_bytes()));
                self.clamp_normal();
            }
            None => self.message = "E664: 変更履歴がありません".into(),
        }
    }

    /// 現在行の下に空行を足す（ideavimrc の `O` = `:call append(expand('.'), '')`）。カーソルは動かさない
    fn append_blank_line(&mut self) {
        let at = self.buf.line_start(self.cursor.line) + self.buf.line(self.cursor.line).len();
        let nl = self.buf.newline();
        self.hist.begin();
        self.edit(at, at, nl);
        self.hist.close();
    }

    // ---------------------------------------------------------------- IME・マウス

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

    /// IME の確定（winit の Ime::Commit）。Insert なら挿入、入力欄なら欄に入れる
    pub fn ime_commit(&mut self, text: &str) {
        self.preedit.clear();
        self.preedit_cursor = None;
        if !self.replaying {
            if let Some(rec) = &mut self.dot_rec {
                rec.extend(text.chars().map(Key::Char));
            }
        }
        match self.mode {
            Mode::Insert => self.insert_text(text),
            Mode::Search => {
                self.query.insert(text);
                self.update_incsearch();
            }
            Mode::Replace => {
                if self.replace_field == 0 {
                    self.query.insert(text);
                    self.update_incsearch();
                } else {
                    self.repl.insert(text);
                }
            }
            Mode::Command => self.cmdline.insert(text),
            _ => {}
        }
    }

    /// 画面上の (行, セル列) にあたるカーソル位置。行は表示中のスクロール位置から数える
    pub fn pos_at(&self, row: usize, col: usize) -> Cursor {
        let line = (self.top + row).min(self.buf.line_count().saturating_sub(1));
        let text = self.buf.line(line);
        let col = col.saturating_sub(GUTTER);
        let byte = if self.mode == Mode::Insert && col >= display_col(&text, text.len(), false) {
            text.len()
        } else {
            byte_at_col(&text, col, false)
        };
        Cursor { line, byte, want_col: display_col(&text, byte, false) }
    }

    /// マウスのクリック: 画面上の (行, セル列) へカーソルを移す
    pub fn click(&mut self, row: usize, col: usize) {
        if self.has_prompt() {
            return;
        }
        if matches!(self.mode, Mode::Visual { .. }) {
            self.mode = Mode::Normal;
        }
        self.cursor = self.pos_at(row, col);
        if self.mode == Mode::Normal {
            self.clamp_normal();
        }
        self.scroll_into_view();
    }

    /// ドラッグ: 押した位置（絶対 byte）を起点に Visual で選ぶ。Insert 中なら先に抜ける
    pub fn drag(&mut self, anchor: usize, row: usize, col: usize) {
        if self.has_prompt() {
            return;
        }
        if self.mode == Mode::Insert {
            self.key(Key::Esc);
        }
        if !matches!(self.mode, Mode::Visual { .. }) {
            self.anchor = anchor;
            self.mode = Mode::Visual { line: false };
        }
        self.cursor = self.pos_at(row, col);
        self.clamp_normal();
    }

    /// ダブルクリックで単語、トリプルクリックで行を選ぶ（viw / V と同じ）
    pub fn select_at(&mut self, row: usize, col: usize, clicks: u32) {
        if self.has_prompt() {
            return;
        }
        if self.mode == Mode::Insert {
            self.key(Key::Esc);
        }
        self.mode = Mode::Normal;
        self.cursor = self.pos_at(row, col);
        self.clamp_normal();
        if clicks == 2 {
            self.key(Key::Char('v'));
            self.key(Key::Char('i'));
            self.key(Key::Char('w'));
        } else {
            self.key(Key::Char('V'));
        }
    }

    /// ホイール: 表示を動かし、カーソルは画面内に留める（Vim と同じ）
    pub fn scroll(&mut self, lines: isize) {
        let pos = self.top as f64 + lines as f64;
        self.scroll_to(pos);
        self.frac = 0.0;
    }

    /// 表示位置を行単位の小数で決める（スムーズスクロール）。カーソルは見えている行に留める
    pub fn scroll_to(&mut self, pos: f64) {
        let rows = self.rows.max(1);
        let max = self.buf.line_count().saturating_sub(1) as f64;
        let pos = pos.clamp(0.0, max);
        self.top = pos.floor() as usize;
        self.frac = (pos - pos.floor()) as f32;
        // 上端が欠けている行には置かない
        let first = self.top + usize::from(self.frac > 0.0);
        let last = (self.top + rows).saturating_sub(1).max(first);
        let mut c = self.cursor;
        if c.line < first {
            let d = (first - c.line) as isize;
            self.motion().vertical(&mut c, d);
        } else if c.line > last {
            let d = -((c.line - last) as isize);
            self.motion().vertical(&mut c, d);
        }
        self.cursor = c;
        if self.mode == Mode::Normal {
            self.clamp_normal();
        }
    }

    /// 今の表示位置（行単位の小数）
    pub fn scroll_pos(&self) -> f64 {
        self.top as f64 + self.frac as f64
    }

    pub(crate) fn scroll_into_view(&mut self) {
        let rows = self.rows.max(1);
        if self.frac > 0.0 && self.cursor.line <= self.top {
            self.frac = 0.0;
        }
        if self.cursor.line < self.top {
            self.top = self.cursor.line;
        } else if self.cursor.line >= self.top + rows {
            self.top = self.cursor.line + 1 - rows;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(text: &str) -> Editor {
        let mut e = Editor::new(Buffer::from_str(text), None);
        e.rows = 3;
        e
    }

    #[test]
    fn drag_selects_from_press() {
        let mut e = ed("hello world\nsecond line\n");
        e.click(0, GUTTER + 2);
        let anchor = e.abs();
        e.drag(anchor, 1, GUTTER + 3);
        assert_eq!(e.mode, Mode::Visual { line: false });
        assert_eq!((e.cursor.line, e.cursor.byte), (1, 3));
        assert_eq!(e.visual_range(), (2, 16, false));
    }

    #[test]
    fn drag_leaves_insert() {
        let mut e = ed("abc\n");
        e.key(Key::Char('i'));
        e.drag(0, 0, GUTTER + 2);
        assert_eq!(e.mode, Mode::Visual { line: false });
    }

    #[test]
    fn double_and_triple_click() {
        let mut e = ed("let name = 1;\n");
        e.select_at(0, GUTTER + 5, 2);
        assert_eq!(e.visual_range(), (4, 8, false), "単語 name");
        e.select_at(0, GUTTER + 5, 3);
        assert_eq!(e.mode, Mode::Visual { line: true });
    }

    #[test]
    fn smooth_scroll_keeps_cursor_on_full_rows() {
        let text: String = (0..20).map(|i| format!("{i}\n")).collect();
        let mut e = ed(&text);
        e.scroll_to(2.5);
        assert_eq!((e.top, e.frac), (2, 0.5));
        // 上端が半分欠けた 2 行目ではなく、3 行目に置く
        assert_eq!(e.cursor.line, 3);
        e.scroll_to(100.0);
        assert_eq!(e.top, 19, "最終行より先へは行かない");
        assert_eq!(e.cursor.line, 19);
        e.scroll_to(0.0);
        assert_eq!(e.cursor.line, 2, "画面の最下段に留まる");
    }
}
