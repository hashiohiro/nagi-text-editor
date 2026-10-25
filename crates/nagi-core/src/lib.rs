//! バッファ（rope）とカーソル移動。GPU・OS に依存しない（architecture.md §4, §5）。
//!
//! 位置はすべて byte オフセット。表示列は表示のときだけ計算する。

use std::io;
use std::path::{Path, PathBuf};

use ropey::{LineType, Rope};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const LT: LineType = LineType::LF_CR;
thread_local! {
    static TAB_WIDTH: std::cell::Cell<usize> = const { std::cell::Cell::new(4) };
}

/// タブの表示幅（設定の「タブの表示幅」・vim の tabstop。窓とファイルで違うので、描く前・キーを処理する前にエディタが入れる）
pub fn tab_width() -> usize {
    TAB_WIDTH.with(std::cell::Cell::get)
}

pub fn set_tab_width(w: usize) {
    TAB_WIDTH.with(|t| t.set(w.clamp(1, 16)));
}

mod history;
pub mod error;
pub use error::{Context, Error, Result};
pub mod hash;
pub mod log;
pub mod mmap;
pub mod task;
pub mod fuzzy;
pub mod linediff;
pub mod textops;
pub use history::{Change, History};

/// 構文木の更新用に記録する編集（位置は (行, 行内 byte)）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditLog {
    pub start: usize,
    pub old_end: usize,
    pub new_end: usize,
    pub start_pos: (usize, usize),
    pub old_end_pos: (usize, usize),
    pub new_end_pos: (usize, usize),
}

/// `start` から `text` を進んだ先の (行, 行内 byte)
fn advance(start: (usize, usize), text: &str) -> (usize, usize) {
    match text.rfind('\n') {
        Some(i) => (start.0 + text.matches('\n').count(), text.len() - i - 1),
        None => (start.0, start.1 + text.len()),
    }
}

pub struct Buffer {
    rope: Rope,
    /// 編集のたびに増える（表示やハイライトのキャッシュの鍵）
    pub version: u64,
    /// まだ構文木に反映していない編集
    edits: Vec<EditLog>,
    /// 保存後に変更があったか
    pub dirty: bool,
    pub path: Option<PathBuf>,
    /// 読み込み時の改行が CRLF だった（保存時に維持する）
    pub crlf: bool,
    /// 読み込み時に UTF-8 の BOM があった（保存時に維持する）
    pub bom: bool,
    /// 最後に読み込んだ・保存したときのファイルの (更新時刻, 大きさ)。外部での変更を見分ける
    pub disk: Option<DiskStamp>,
}

/// ファイルの (更新時刻, 大きさ)
pub type DiskStamp = (std::time::SystemTime, u64);

/// `path` の今の (更新時刻, 大きさ)
pub fn disk_stamp(path: &Path) -> Option<DiskStamp> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// 検索は rope から窓ごとに切り出して行う（ファイル全体の文字列を作らない）
impl textops::Source for Buffer {
    fn len(&self) -> usize {
        self.rope.len()
    }
    fn window(&self, lo: usize, hi: usize) -> (usize, std::borrow::Cow<'_, str>) {
        let lo = self.rope.floor_char_boundary(lo.min(self.rope.len()));
        let hi = self.rope.floor_char_boundary(hi.min(self.rope.len())).max(lo);
        (lo, std::borrow::Cow::Owned(self.slice_string(lo, hi)))
    }
    fn line_start_near(&self, p: usize) -> usize {
        let p = self.rope.floor_char_boundary(p.min(self.rope.len()));
        let (_, col) = self.pos_of(p);
        if col <= textops::LINE_CONTEXT { p - col } else { p }
    }
    fn scan_fwd(&self, at: usize, needles: &[u8], f: &mut dyn FnMut(usize, u8) -> bool) -> Option<usize> {
        let at = at.min(self.rope.len());
        let (mut chunks, mut start) = self.rope.chunks_at(at);
        let mut skip = at - start;
        while let Some(c) = chunks.next() {
            let base = start + skip;
            if let Some(i) = textops::scan_bytes(&c.as_bytes()[skip..], needles, false, &mut |i, b| f(base + i, b)) {
                return Some(base + i);
            }
            start += c.len();
            skip = 0;
        }
        None
    }
    fn scan_back(&self, at: usize, needles: &[u8], f: &mut dyn FnMut(usize, u8) -> bool) -> Option<usize> {
        let at = at.min(self.rope.len());
        // `at` を含む塊の前半を見てから、それより前の塊を逆順に見る（prev は `at` を含む塊の一つ前を返す）
        let (mut chunks, mut start) = self.rope.chunks_at(at);
        let mut part = chunks.clone().next().map_or("", |c| &c[..at - start]);
        loop {
            let base = start;
            if let Some(i) = textops::scan_bytes(part.as_bytes(), needles, true, &mut |i, b| f(base + i, b)) {
                return Some(base + i);
            }
            part = chunks.prev()?;
            start -= part.len();
        }
    }
}

/// 末尾で途中までしか無い UTF-8 の文字の byte 数（0〜3）
fn incomplete_tail(b: &[u8]) -> usize {
    for back in 1..=b.len().min(3) {
        let c = b[b.len() - back];
        if c & 0xC0 == 0x80 {
            continue; // 続きの byte。さらに前を見る
        }
        let need = match c {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => 1,
        };
        return if need > back { back } else { 0 };
    }
    0
}

impl Buffer {
    pub fn from_str(text: &str) -> Self {
        Self { rope: Rope::from_str(text), version: 0, edits: Vec::new(), dirty: false, path: None, crlf: text.contains("\r\n"), bom: false, disk: None }
    }

    pub fn open(path: &Path) -> io::Result<Self> {
        let disk = disk_stamp(path);
        let bytes = std::fs::read(path)?;
        let (bom, body) = match bytes.strip_prefix(b"\xEF\xBB\xBF") {
            Some(rest) => (true, rest),
            None => (false, &bytes[..]),
        };
        let text = String::from_utf8_lossy(body);
        let mut b = Self::from_str(&text);
        b.path = Some(path.to_path_buf());
        b.bom = bom;
        b.disk = disk;
        Ok(b)
    }

    /// 大きなファイルを読む。読み込みと UTF-8 の確認（読み込み用のスレッド）と、rope の組み立て（このスレッド）を
    /// 並べて進める。最初の 1MB を読んだところで、先頭部分だけのバッファを `head` に渡す（表示を先に出すため）。
    /// 小さなファイルは `open` と同じで、`head` は呼ばない
    pub fn open_with_head(path: &Path, head: impl FnOnce(Buffer)) -> io::Result<Self> {
        use std::io::Read;
        const STREAM_LIMIT: u64 = 8 << 20;
        const CHUNK: u64 = 1 << 20;
        const HEAD_BYTES: usize = 256 << 10;
        let mut file = std::fs::File::open(path)?;
        if file.metadata()?.len() < STREAM_LIMIT {
            return Self::open(path);
        }
        // 読む → UTF-8 と改行コードを確かめる → rope に組み立てる、の 3 段を別々のスレッドで並べる
        let (btx, brx) = std::sync::mpsc::sync_channel::<Vec<u8>>(8);
        let reader = std::thread::Builder::new().name("read".into()).spawn(move || -> io::Result<()> {
            loop {
                let mut buf = Vec::with_capacity(CHUNK as usize);
                if (&mut file).take(CHUNK).read_to_end(&mut buf)? == 0 || btx.send(buf).is_err() {
                    return Ok(());
                }
            }
        })?;
        let (tx, rx) = std::sync::mpsc::sync_channel::<String>(8);
        let checker = std::thread::Builder::new().name("utf8".into()).spawn(move || -> (bool, bool) {
            let (mut carry, mut first, mut crlf, mut bom) = (Vec::new(), true, false, false);
            let mut chunks = brx.into_iter().peekable();
            while let Some(next) = chunks.next() {
                let eof = chunks.peek().is_none();
                let mut buf = std::mem::take(&mut carry);
                buf.extend_from_slice(&next);
                if first {
                    first = false;
                    if buf.starts_with(b"\xEF\xBB\xBF") {
                        buf.drain(..3);
                        bom = true;
                    }
                }
                // 塊の終わりで切れた文字は次の塊へ回す
                if !eof {
                    carry = buf.split_off(buf.len() - incomplete_tail(&buf));
                }
                let s = match String::from_utf8(buf) {
                    Ok(s) => s,
                    Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
                };
                // CRLF を塊の境目で切ると、rope が \r と \n を別々の改行として数える。末尾の \r は次の塊へ回す
                let mut s = s;
                if !eof && s.ends_with('\r') {
                    s.pop();
                    carry.insert(0, b'\r');
                }
                crlf = crlf || s.contains("\r\n");
                if !s.is_empty() && tx.send(s).is_err() {
                    break;
                }
            }
            (bom, crlf)
        })?;
        let mut builder = ropey::RopeBuilder::new();
        let mut head = Some(head);
        for chunk in rx {
            if let Some(h) = head.take() {
                // 先頭部分は行の途中で切らない
                let mut end = chunk.len().min(HEAD_BYTES);
                while !chunk.is_char_boundary(end) {
                    end -= 1;
                }
                let end = chunk[..end].rfind('\n').map_or(end, |i| i + 1);
                let mut b = Self::from_str(&chunk[..end]);
                b.path = Some(path.to_path_buf());
                h(b);
            }
            builder.append(&chunk);
        }
        let (bom, crlf) = checker.join().map_err(|_| io::Error::other("読み込みスレッドが止まった"))?;
        reader.join().map_err(|_| io::Error::other("読み込みスレッドが止まった"))??;
        let mut b = Self::from_str("");
        b.rope = builder.finish();
        b.crlf = crlf;
        b.bom = bom;
        b.path = Some(path.to_path_buf());
        b.disk = disk_stamp(path);
        Ok(b)
    }

    /// 行数。末尾が改行で終わる場合、その後ろの空行は数えない（Vim と同じ）。
    pub fn line_count(&self) -> usize {
        let n = self.rope.len_lines(LT);
        if n > 1 && self.rope.trailing_line_break_idx(LT).is_some() { n - 1 } else { n }
    }

    /// 改行を除いた行の内容
    pub fn line(&self, i: usize) -> String {
        let slice = self.rope.line(i, LT);
        let mut s = String::with_capacity(slice.len());
        for chunk in slice.chunks() {
            s.push_str(chunk);
        }
        while s.ends_with('\n') || s.ends_with('\r') {
            s.pop();
        }
        s
    }

    /// 行の先頭から最大 `max` byte（文字の境目で切る）。長い行の表示で、画面に入らない部分の文字列を作らない。
    /// 2 つ目は途中で切ったかどうか
    pub fn line_head(&self, i: usize, max: usize) -> (String, bool) {
        if self.line_len(i) <= max {
            return (self.line(i), false);
        }
        let mut s = String::with_capacity(max);
        for chunk in self.rope.line(i, LT).chunks() {
            if s.len() + chunk.len() > max {
                let mut end = max - s.len();
                while !chunk.is_char_boundary(end) {
                    end -= 1;
                }
                s.push_str(&chunk[..end]);
                break;
            }
            s.push_str(chunk);
        }
        (s, true)
    }

    /// 行の長さ（byte、改行を除く）。文字列を作らずに求める
    pub fn line_len(&self, i: usize) -> usize {
        let slice = self.rope.line(i, LT);
        let mut n = slice.len();
        while n > 0 && matches!(slice.byte(n - 1), b'\n' | b'\r') {
            n -= 1;
        }
        n
    }

    /// 同じ内容の別のバッファ（rope の複製は O(1)）。裏のスレッドで構文解析するときに渡す
    pub fn snapshot(&self) -> Buffer {
        Buffer { rope: self.rope.clone(), version: self.version, edits: Vec::new(), dirty: false, path: self.path.clone(), crlf: self.crlf, bom: self.bom, disk: self.disk }
    }

    pub fn len_bytes(&self) -> usize {
        self.rope.len()
    }

    /// 行の先頭の絶対 byte 位置
    pub fn line_start(&self, line: usize) -> usize {
        self.rope.line_to_byte_idx(line.min(self.rope.len_lines(LT)), LT)
    }

    /// 絶対 byte 位置 → (行, 行内 byte)
    pub fn pos_of(&self, abs: usize) -> (usize, usize) {
        let abs = abs.min(self.rope.len());
        let line = self.rope.byte_to_line_idx(abs, LT);
        (line, abs - self.line_start(line))
    }

    /// この行を区切る改行（このファイルの改行コード）
    pub fn newline(&self) -> &'static str {
        if self.crlf { "\r\n" } else { "\n" }
    }

    /// 行末の改行を含めた行の長さ（byte）
    pub fn line_len_with_break(&self, line: usize) -> usize {
        self.rope.line(line, LT).len()
    }

    pub fn slice_string(&self, start: usize, end: usize) -> String {
        let mut s = String::with_capacity(end.saturating_sub(start));
        for c in self.rope.slice(start..end).chunks() {
            s.push_str(c);
        }
        s
    }

    /// [start, end) を text で置き換え、その変更を返す（undo とハイライト更新に使う）
    pub fn edit(&mut self, start: usize, end: usize, text: &str) -> Change {
        let removed = self.slice_string(start, end);
        let (line, byte) = self.pos_of(start);
        let start_pos = (line, byte);
        self.edits.push(EditLog {
            start,
            old_end: end,
            new_end: start + text.len(),
            start_pos,
            old_end_pos: advance(start_pos, &removed),
            new_end_pos: advance(start_pos, text),
        });
        self.version += 1;
        if end > start {
            self.rope.remove(start..end);
        }
        if !text.is_empty() {
            self.rope.insert(start, text);
        }
        self.dirty = true;
        Change { at: start, removed, inserted: text.to_string() }
    }

    /// バッファ全体の文字列（検索・括弧の対応・text object の計算用）
    pub fn text(&self) -> String {
        self.slice_string(0, self.rope.len())
    }

    pub fn has_edits(&self) -> bool {
        !self.edits.is_empty()
    }

    /// まだ構文木に反映していない編集を取り出す
    pub fn take_edits(&mut self) -> Vec<EditLog> {
        std::mem::take(&mut self.edits)
    }

    /// `byte` を含むチャンクの、`byte` から後ろ（tree-sitter に分割して渡す用）
    pub fn chunk_from(&self, byte: usize) -> &[u8] {
        if byte >= self.rope.len() {
            return &[];
        }
        let (chunk, start) = self.rope.chunk(byte);
        &chunk.as_bytes()[byte - start..]
    }

    /// 範囲のテキストをチャンクごとに返す
    pub fn chunks_in(&self, start: usize, end: usize) -> impl Iterator<Item = &[u8]> {
        self.rope.slice(start.min(self.rope.len())..end.min(self.rope.len())).chunks().map(str::as_bytes)
    }

    /// 読み込み時の BOM と改行を保って書き出す（改行は編集時にファイルの改行コードで入れている）
    pub fn save(&mut self) -> io::Result<usize> {
        let path = self.path.clone().ok_or_else(|| io::Error::other("ファイル名がない"))?;
        let mut out = Vec::with_capacity(self.rope.len() + 3);
        if self.bom {
            out.extend_from_slice(b"\xEF\xBB\xBF");
        }
        for c in self.rope.chunks() {
            out.extend_from_slice(c.as_bytes());
        }
        // 途中で失敗しても元のファイルを壊さないよう、一時ファイルに書いてから置き換える
        let tmp = path.with_extension("nagi-tmp");
        std::fs::write(&tmp, &out)?;
        std::fs::rename(&tmp, &path)?;
        self.dirty = false;
        self.disk = disk_stamp(&path);
        Ok(out.len())
    }
}

/// 書記素の表示幅（1 か 2。タブは tab_width）
pub fn grapheme_width(g: &str, ambiguous_wide: bool) -> usize {
    if g == "\t" {
        return tab_width();
    }
    if ambiguous_wide { g.width_cjk() } else { g.width() }.clamp(1, 2)
}

/// 行内 byte オフセット `byte` の表示列（文字の途中なら、その文字の頭までの列。落ちない）
pub fn display_col(line: &str, byte: usize, ambiguous_wide: bool) -> usize {
    let mut b = byte.min(line.len());
    while !line.is_char_boundary(b) {
        b -= 1;
    }
    line[..b].graphemes(true).map(|g| grapheme_width(g, ambiguous_wide)).sum()
}

/// 表示列 `col` を含む書記素の開始 byte（行末を越えたら最後の書記素）
pub fn byte_at_col(line: &str, col: usize, ambiguous_wide: bool) -> usize {
    let mut c = 0;
    let mut last = 0;
    for (i, g) in line.grapheme_indices(true) {
        let w = grapheme_width(g, ambiguous_wide);
        if col < c + w {
            return i;
        }
        c += w;
        last = i;
    }
    last
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub line: usize,
    /// 行内 byte オフセット（書記素の先頭）
    pub byte: usize,
    /// j / k で保つ表示列
    pub want_col: usize,
}

/// Normal モードのカーソル移動。Vim と同じく、Normal では行末の改行の上には置かない。
pub struct Motion<'a> {
    pub buf: &'a Buffer,
    pub ambiguous_wide: bool,
}

use textops::{Class, class};

impl Motion<'_> {
    fn set_byte(&self, cur: &mut Cursor, line: &str, byte: usize) {
        cur.byte = byte;
        cur.want_col = display_col(line, byte, self.ambiguous_wide);
    }

    // h / l / w / e / b はカーソルの前後だけを見る（行全体を書記素に分けると、数 MB の 1 行で数百 ms かかる）

    pub fn left(&self, cur: &mut Cursor, n: usize) {
        let line = self.buf.line(cur.line);
        let Some(head) = line.get(..cur.byte.min(line.len())) else { return };
        let b = head.grapheme_indices(true).rev().nth(n.saturating_sub(1)).map_or(0, |(i, _)| i);
        self.set_byte(cur, &line, b);
    }

    pub fn right(&self, cur: &mut Cursor, n: usize) {
        let line = self.buf.line(cur.line);
        let Some(tail) = line.get(cur.byte..) else { return };
        let starts: Vec<usize> = tail.grapheme_indices(true).map(|(i, _)| cur.byte + i).take(n + 1).collect();
        let Some(&b) = starts.get(n.min(starts.len().saturating_sub(1))) else { return };
        self.set_byte(cur, &line, b);
    }

    pub fn vertical(&self, cur: &mut Cursor, delta: isize) {
        let last = self.buf.line_count().saturating_sub(1) as isize;
        cur.line = (cur.line as isize + delta).clamp(0, last) as usize;
        let line = self.buf.line(cur.line);
        cur.byte = byte_at_col(&line, cur.want_col, self.ambiguous_wide);
    }

    pub fn line_start(&self, cur: &mut Cursor) {
        cur.byte = 0;
        cur.want_col = 0;
    }

    /// `^`: 行頭の空白を飛ばした最初の文字
    pub fn first_non_blank(&self, cur: &mut Cursor) {
        let line = self.buf.line(cur.line);
        let b = line.grapheme_indices(true).find(|(_, g)| class(g, false) != Class::Space).map_or(0, |(i, _)| i);
        self.set_byte(cur, &line, b);
    }

    /// `$`: 行末の書記素。以降の j / k も行末に付く
    pub fn line_end(&self, cur: &mut Cursor) {
        let line = self.buf.line(cur.line);
        cur.byte = line.grapheme_indices(true).next_back().map_or(0, |(i, _)| i);
        cur.want_col = usize::MAX;
    }

    pub fn goto_line(&self, cur: &mut Cursor, line: usize) {
        cur.line = line.min(self.buf.line_count().saturating_sub(1));
        self.first_non_blank(cur);
    }

    /// `w`: 次の単語の先頭（行をまたぐ）
    pub fn word_forward(&self, cur: &mut Cursor, big: bool) {
        let mut line_no = cur.line;
        let mut line = self.buf.line(line_no);
        let from = if line.is_char_boundary(cur.byte.min(line.len())) { cur.byte.min(line.len()) } else { 0 };
        let mut gs = line[from..].grapheme_indices(true).map(|(i, g)| (from + i, g)).peekable();
        let cls = gs.peek().map_or(Class::Space, |(_, g)| class(g, big));
        // 同じ種類を飛ばし、続く空白を飛ばす
        while cls != Class::Space && gs.next_if(|(_, g)| class(g, big) == cls).is_some() {}
        while gs.next_if(|(_, g)| class(g, big) == Class::Space).is_some() {}
        if let Some(&(b, _)) = gs.peek() {
            self.set_byte(cur, &line, b);
            return;
        }
        // 次の空でない行の最初の非空白
        while line_no + 1 < self.buf.line_count() {
            line_no += 1;
            line = self.buf.line(line_no);
            if let Some((i, _)) = line.grapheme_indices(true).find(|(_, g)| class(g, big) != Class::Space) {
                cur.line = line_no;
                self.set_byte(cur, &line, i);
                return;
            }
            if line.is_empty() {
                cur.line = line_no;
                self.set_byte(cur, &line, 0);
                return;
            }
        }
    }

    /// `e`: 単語の末尾（今が末尾なら次の単語の末尾。行をまたぐ）
    pub fn word_end(&self, cur: &mut Cursor, big: bool) {
        let mut line_no = cur.line;
        let mut line = self.buf.line(line_no);
        // 1 つ進めてから、空白を飛ばし、同じ種類の終わりまで進む
        let (mut from, mut skip) = match cur.byte.min(line.len()) {
            b if line.is_char_boundary(b) => (b, true),
            _ => (0, false),
        };
        loop {
            let mut gs = line[from..].grapheme_indices(true).map(|(i, g)| (from + i, g)).peekable();
            if skip {
                gs.next();
                skip = false;
            }
            while gs.next_if(|(_, g)| class(g, big) == Class::Space).is_some() {}
            if let Some((mut b, g)) = gs.next() {
                let cls = class(g, big);
                while let Some((nb, _)) = gs.next_if(|(_, g)| class(g, big) == cls) {
                    b = nb;
                }
                cur.line = line_no;
                self.set_byte(cur, &line, b);
                return;
            }
            if line_no + 1 >= self.buf.line_count() {
                return;
            }
            line_no += 1;
            line = self.buf.line(line_no);
            from = 0;
        }
    }

    /// `b`: 前の単語の先頭（行をまたぐ）
    pub fn word_backward(&self, cur: &mut Cursor, big: bool) {
        let mut line_no = cur.line;
        let mut line = self.buf.line(line_no);
        let mut end = cur.byte;
        loop {
            let end_b = (0..=end.min(line.len())).rev().find(|&b| line.is_char_boundary(b)).unwrap_or(0);
            let mut gs = line[..end_b].grapheme_indices(true).rev().peekable();
            while gs.next_if(|(_, g)| class(g, big) == Class::Space).is_some() {}
            if let Some((mut b, g)) = gs.next() {
                let cls = class(g, big);
                while let Some((nb, _)) = gs.next_if(|(_, g)| class(g, big) == cls) {
                    b = nb;
                }
                cur.line = line_no;
                self.set_byte(cur, &line, b);
                return;
            }
            if line_no == 0 {
                cur.line = 0;
                self.set_byte(cur, &line, 0);
                return;
            }
            line_no -= 1;
            line = self.buf.line(line_no);
            end = line.len();
            if line.is_empty() {
                cur.line = line_no;
                self.set_byte(cur, &line, 0);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 括弧の対応を rope の塊ごとに数えても、全体の文字列で数えたのと同じになる
    #[test]
    fn bracket_scan_matches_str() {
        use textops::{enclosing_block, match_pair, Source};
        // 塊の境目をまたぐように長くし、多 byte 文字も混ぜる
        let unit = "fn 関数(a: [u8; 3]) {\n    if (x) { y(\"日本語\") }\n    <T>\n}\n";
        let text = unit.repeat(400);
        let buf = Buffer::from_str(&text);
        let t: &str = &text;
        for at in (0..text.len()).step_by(7).filter(|&i| text.is_char_boundary(i)) {
            let (l, _) = buf.pos_of(at);
            let end = buf.line_start(l) + buf.line_len(l);
            assert_eq!(match_pair(&buf, at, end), match_pair(t, at, end), "% at {at}");
            for open in ['(', '[', '{', '<'] {
                assert_eq!(enclosing_block(&buf, at, open), enclosing_block(t, at, open), "{open} at {at}");
            }
            let mut a = Vec::new();
            let mut b = Vec::new();
            for needles in [&b"("[..], b"()", b"(){", b"([{)]}\n"] {
                a.clear();
                b.clear();
                buf.scan_back(at, needles, &mut |i, c| { a.push((i, c)); a.len() > 300 });
                t.scan_back(at, needles, &mut |i, c| { b.push((i, c)); b.len() > 300 });
                assert_eq!(a, b, "scan_back at {at}");
                a.clear();
                b.clear();
                buf.scan_fwd(at, needles, &mut |i, c| { a.push((i, c)); a.len() > 300 });
                t.scan_fwd(at, needles, &mut |i, c| { b.push((i, c)); b.len() > 300 });
                assert_eq!(a, b, "scan_fwd at {at}");
            }
        }
    }

    /// 分けて読んだ大きなファイルが、まとめて読んだのと同じ内容・改行コード・BOM になる。
    /// 1MB の境目で多 byte 文字や CRLF が切れても、壊れた byte があってもよい
    #[test]
    fn open_with_head_matches_open() {
        let dir = std::env::temp_dir().join("nagi-load-test");
        std::fs::create_dir_all(&dir).unwrap();
        let cases: [(&str, Vec<u8>); 3] = [
            ("lf", "あいう漢字 line\n".repeat(500_000).into_bytes()),
            ("crlf-bom", [b"\xEF\xBB\xBF".as_slice(), "x\r\n".repeat(3_000_000).as_bytes()].concat()),
            ("invalid", {
                let mut v = "ok 日本\n".repeat(900_000).into_bytes();
                v.splice(5_000_000..5_000_000, [0xFF, 0xFE]);
                v
            }),
        ];
        for (name, bytes) in cases {
            let path = dir.join(name);
            std::fs::write(&path, &bytes).unwrap();
            let want = Buffer::open(&path).unwrap();
            let mut head = None;
            let got = Buffer::open_with_head(&path, |h| head = Some(h)).unwrap();
            assert!(got.text() == want.text(), "{name}: 内容が違う（{} / {} byte）", got.len_bytes(), want.len_bytes());
            assert_eq!((got.crlf, got.bom, got.line_count()), (want.crlf, want.bom, want.line_count()), "{name}");
            let head = head.expect("大きなファイルは先頭を先に渡す");
            assert!(want.text().starts_with(&head.text()) && head.text().ends_with('\n'), "{name}: 先頭は行の途中で切らない");
            let _ = std::fs::remove_file(&path);
        }
    }

    #[test]
    fn line_count_ignores_trailing_newline() {
        assert_eq!(Buffer::from_str("a\nb\n").line_count(), 2);
        assert_eq!(Buffer::from_str("a\nb").line_count(), 2);
        assert_eq!(Buffer::from_str("").line_count(), 1);
        assert_eq!(Buffer::from_str("a\r\nb\r\n").line(0), "a");
    }

    #[test]
    fn wide_columns() {
        assert_eq!(display_col("a漢b", 4, false), 3);
        assert_eq!(display_col("a漢b", 2, false), 1, "文字の途中（漢の 2 byte 目）は、その文字の頭まで");
        assert_eq!(byte_at_col("a漢b", 2, false), 1);
        assert_eq!(byte_at_col("a漢b", 3, false), 4);
    }

    #[test]
    fn word_motions() {
        let b = Buffer::from_str("foo bar.baz\n\n  qux");
        let m = Motion { buf: &b, ambiguous_wide: false };
        let mut c = Cursor::default();
        m.word_forward(&mut c, false);
        assert_eq!((c.line, c.byte), (0, 4));
        m.word_forward(&mut c, false);
        assert_eq!((c.line, c.byte), (0, 7));
        m.word_forward(&mut c, false);
        assert_eq!((c.line, c.byte), (0, 8));
        m.word_forward(&mut c, false);
        assert_eq!((c.line, c.byte), (1, 0));
        m.word_forward(&mut c, false);
        assert_eq!((c.line, c.byte), (2, 2));
        m.word_backward(&mut c, false);
        assert_eq!((c.line, c.byte), (1, 0));
        m.word_backward(&mut c, false);
        assert_eq!((c.line, c.byte), (0, 8));
        let mut c = Cursor::default();
        m.word_end(&mut c, false);
        assert_eq!((c.line, c.byte), (0, 2));
        m.word_end(&mut c, false);
        assert_eq!((c.line, c.byte), (0, 6));
        m.word_end(&mut c, false);
        assert_eq!((c.line, c.byte), (0, 7));
    }
}
