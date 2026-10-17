//! 移動先・範囲の計算（f / t、%、{ }、text object）と検索。位置はすべて絶対 byte。
//!
//! M1 では、バッファ全体の文字列（`Buffer::text`）に対して計算する。大きなファイルで遅くなったら、
//! rope のチャンクを直接たどる形に置き換える。

use regex::{Regex, RegexBuilder};
use unicode_segmentation::UnicodeSegmentation;

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum Class {
    Space,
    Word,
    Punct,
}

/// 単語の種類。`big`（W / B / E / iW）では空白以外はすべて同じ種類
pub fn class(g: &str, big: bool) -> Class {
    let c = g.chars().next().unwrap_or(' ');
    if c.is_whitespace() {
        Class::Space
    } else if big || c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

/// `line` の中で、`from`（行内 byte）より後ろ（前）にある `ch` を `n` 個目まで探す。
/// 戻り値は見つかった文字の行内 byte。`till` なら 1 つ手前（後ろ）の書記素
pub fn find_in_line(line: &str, from: usize, ch: char, forward: bool, till: bool, n: usize) -> Option<usize> {
    let gs: Vec<(usize, &str)> = line.grapheme_indices(true).collect();
    let pos = gs.iter().position(|(i, _)| *i == from)?;
    let is = |g: &str| g.starts_with(ch);
    let mut found = 0;
    if forward {
        // till で直後の文字が対象のとき、その文字は飛ばす（Vim の ; の挙動に合わせる簡略化はしない）
        for k in pos + 1..gs.len() {
            if is(gs[k].1) {
                found += 1;
                if found == n {
                    return Some(if till { gs[k - 1].0 } else { gs[k].0 });
                }
            }
        }
    } else {
        for k in (0..pos).rev() {
            if is(gs[k].1) {
                found += 1;
                if found == n {
                    return Some(if till { gs[k + 1].0 } else { gs[k].0 });
                }
            }
        }
    }
    None
}

const PAIRS: [(u8, u8); 3] = [(b'(', b')'), (b'[', b']'), (b'{', b'}')];

/// 括弧はどれも ASCII なので、UTF-8 の byte のまま数えてよい（多 byte 文字の byte は 0x80 以上）
fn depth_scan(depth: &mut i32, up: u8, down: u8) -> impl FnMut(usize, u8) -> bool + '_ {
    move |_, c| {
        if c == up {
            *depth += 1;
        } else if c == down {
            *depth -= 1;
            return *depth == 0;
        }
        false
    }
}

/// `%`: `at` 以降で行内の最初の括弧を探し、対応する括弧の位置を返す
pub fn match_pair<S: Source + ?Sized>(text: &S, at: usize, line_end: usize) -> Option<usize> {
    let mut found = None;
    text.scan_fwd(at, b"([{)]}\n", &mut |i, c| {
        if i >= line_end {
            return true;
        }
        found = PAIRS.iter().find_map(|&(o, cl)| (c == o || c == cl).then_some((i, o, cl, c == o)));
        found.is_some()
    });
    let (start, open, close, forward) = found?;
    let mut depth = 0;
    if forward {
        text.scan_fwd(start, &[open, close], &mut depth_scan(&mut depth, open, close))
    } else {
        text.scan_back(start + 1, &[open, close], &mut depth_scan(&mut depth, close, open))
    }
}

/// 開き括弧 `open` で囲まれた、`at` を含む最も内側の範囲 (開き括弧の位置, 閉じ括弧の位置)
pub fn enclosing_block<S: Source + ?Sized>(text: &S, at: usize, open: char) -> Option<(usize, usize)> {
    let (open, close) = match open {
        '(' => (b'(', b')'),
        '[' => (b'[', b']'),
        '{' => (b'{', b'}'),
        '<' => (b'<', b'>'),
        _ => return None,
    };
    // カーソルが開き括弧の上なら、その括弧から始める
    let start = if byte_at(text, at) == Some(open) {
        at
    } else {
        let mut depth = 0i32;
        text.scan_back(at, &[open, close], &mut |_, c| {
            if c == close {
                depth += 1;
            } else if c == open {
                if depth == 0 {
                    return true;
                }
                depth -= 1;
            }
            false
        })?
    };
    let mut depth = 0;
    let end = text.scan_fwd(start, &[open, close], &mut depth_scan(&mut depth, open, close))?;
    Some((start, end))
}

/// `i` の byte
pub fn byte_at<S: Source + ?Sized>(text: &S, i: usize) -> Option<u8> {
    text.window(i, i + 1).1.bytes().next()
}

/// 引用符 `q` で囲まれた、`at` を含む範囲（行内）。(開きの位置, 閉じの位置)
pub fn enclosing_quote(line: &str, at: usize, q: char) -> Option<(usize, usize)> {
    let idx: Vec<usize> = line.char_indices().filter(|(i, c)| *c == q && (*i == 0 || !line[..*i].ends_with('\\'))).map(|(i, _)| i).collect();
    // 組にして、at を含む組を探す。無ければ at より後ろの最初の組（Vim と同じ）
    let pairs: Vec<(usize, usize)> = idx.chunks(2).filter(|c| c.len() == 2).map(|c| (c[0], c[1])).collect();
    pairs.iter().find(|(a, b)| *a <= at && at <= *b).or_else(|| pairs.iter().find(|(a, _)| *a > at)).copied()
}

/// 単語の text object（行内 byte の範囲 [start, end)）。`around` は後ろ（無ければ前）の空白を含める
pub fn word_object(line: &str, at: usize, big: bool, around: bool) -> Option<(usize, usize)> {
    let gs: Vec<(usize, &str)> = line.grapheme_indices(true).collect();
    let pos = gs.iter().position(|(i, _)| *i == at).or(if gs.is_empty() { None } else { Some(gs.len() - 1) })?;
    let cls = class(gs[pos].1, big);
    let (mut a, mut b) = (pos, pos);
    while a > 0 && class(gs[a - 1].1, big) == cls {
        a -= 1;
    }
    while b + 1 < gs.len() && class(gs[b + 1].1, big) == cls {
        b += 1;
    }
    if around {
        if cls != Class::Space && b + 1 < gs.len() && class(gs[b + 1].1, big) == Class::Space {
            while b + 1 < gs.len() && class(gs[b + 1].1, big) == Class::Space {
                b += 1;
            }
        } else if cls != Class::Space {
            while a > 0 && class(gs[a - 1].1, big) == Class::Space {
                a -= 1;
            }
        } else {
            while b + 1 < gs.len() && class(gs[b + 1].1, big) != Class::Space {
                b += 1;
            }
        }
    }
    let end = gs.get(b + 1).map_or(line.len(), |(i, _)| *i);
    Some((gs[a].0, end))
}

/// 検索。大文字小文字を区別しない。`regex` が false なら文字どおりに探す
pub fn compile_search(pattern: &str, regex: bool) -> Option<Regex> {
    if pattern.is_empty() {
        return None;
    }
    let src = if regex { pattern.to_string() } else { regex::escape(pattern) };
    // 英数字だけの検索語は ASCII の範囲で大文字小文字を同一視する。Unicode の対応表を使うと組み立てに
    // 数百 µs かかり、1 打ごとに組み立て直す incsearch で効いてくる。日本語には大文字小文字がないので結果は同じ
    let ascii = !regex && pattern.is_ascii();
    RegexBuilder::new(&src).case_insensitive(true).multi_line(true).unicode(!ascii).build().ok()
}

/// 検索語のすべての一致（絶対 byte の範囲）。大文字小文字は区別しない。
pub fn find_matches(text: &str, pattern: &str, regex: bool) -> Vec<(usize, usize)> {
    find_matches_in(text, pattern, regex)
}

/// `find_matches_in` の、`cancel` が立ったら途中でやめる版（やめたら None）
pub fn find_matches_cancellable<S: Source + ?Sized>(src: &S, pattern: &str, regex: bool, cancel: &std::sync::atomic::AtomicBool) -> Option<Vec<(usize, usize)>> {
    use std::sync::atomic::Ordering::Relaxed;
    let Some(f) = Finder::new(pattern, regex) else { return Some(Vec::new()) };
    let mut out = Vec::new();
    f.each_from_while(src, 0, || !cancel.load(Relaxed), |m| {
        out.push(m);
        true
    });
    (!cancel.load(Relaxed)).then_some(out)
}

/// `find_matches` の、ファイル全体の文字列を作らずにバッファから探す版
pub fn find_matches_in<S: Source + ?Sized>(src: &S, pattern: &str, regex: bool) -> Vec<(usize, usize)> {
    let Some(f) = Finder::new(pattern, regex) else { return Vec::new() };
    let mut out = Vec::new();
    f.each_from(src, 0, |m| {
        out.push(m);
        true
    });
    out
}

/// すべての一致（絶対 byte の範囲）。空の一致は除く
pub fn find_all(text: &str, re: &Regex) -> Vec<(usize, usize)> {
    re.find_iter(text).filter(|m| !m.is_empty()).map(|m| (m.start(), m.end())).collect()
}

/// 検索する文字列の出どころ。ファイル全体の文字列を作らずに、窓ごとに切り出して探せるようにする
pub trait Source {
    fn len(&self) -> usize;
    /// [lo, hi) を切り出す（両端は文字の境目へ切り下げる）。戻り値は（実際の lo, 文字列）
    fn window(&self, lo: usize, hi: usize) -> (usize, std::borrow::Cow<'_, str>);
    /// `p` を含む行の先頭。`LINE_CONTEXT` より遠ければ `p` のまま
    fn line_start_near(&self, p: usize) -> usize;
    /// `at` から後ろへ、`needles` のどれかに当たる byte を順に `f` へ渡し、`f` が true を返した位置
    fn scan_fwd(&self, at: usize, needles: &[u8], f: &mut dyn FnMut(usize, u8) -> bool) -> Option<usize>;
    /// `at` より前を逆順に見る。ほかは `scan_fwd` と同じ
    fn scan_back(&self, at: usize, needles: &[u8], f: &mut dyn FnMut(usize, u8) -> bool) -> Option<usize>;
}

/// `hay` の中で `needles` のどれかに当たる位置を順に（`rev` なら逆順に）`f` へ渡し、`f` が true を返した位置。
/// 3 種類までは memchr で間を飛ばす
pub fn scan_bytes(hay: &[u8], needles: &[u8], rev: bool, f: &mut dyn FnMut(usize, u8) -> bool) -> Option<usize> {
    let mut hit = |i: usize| f(i, hay[i]);
    match (needles, rev) {
        (&[a], false) => memchr::memchr_iter(a, hay).find(|&i| hit(i)),
        (&[a], true) => memchr::memchr_iter(a, hay).rev().find(|&i| hit(i)),
        (&[a, b], false) => memchr::memchr2_iter(a, b, hay).find(|&i| hit(i)),
        (&[a, b], true) => memchr::memchr2_iter(a, b, hay).rev().find(|&i| hit(i)),
        (&[a, b, c], false) => memchr::memchr3_iter(a, b, c, hay).find(|&i| hit(i)),
        (&[a, b, c], true) => memchr::memchr3_iter(a, b, c, hay).rev().find(|&i| hit(i)),
        (_, false) => (0..hay.len()).find(|&i| needles.contains(&hay[i]) && hit(i)),
        (_, true) => (0..hay.len()).rev().find(|&i| needles.contains(&hay[i]) && hit(i)),
    }
}

/// 窓を行頭から切り出すために戻る最大の長さ（`^` や `\b` を正しく判定するため）
pub const LINE_CONTEXT: usize = 4096;

impl Source for str {
    fn len(&self) -> usize {
        str::len(self)
    }
    fn scan_fwd(&self, at: usize, needles: &[u8], f: &mut dyn FnMut(usize, u8) -> bool) -> Option<usize> {
        let at = at.min(self.len());
        scan_bytes(&self.as_bytes()[at..], needles, false, &mut |i, c| f(at + i, c)).map(|i| at + i)
    }
    fn scan_back(&self, at: usize, needles: &[u8], f: &mut dyn FnMut(usize, u8) -> bool) -> Option<usize> {
        scan_bytes(&self.as_bytes()[..at.min(self.len())], needles, true, f)
    }
    fn window(&self, lo: usize, hi: usize) -> (usize, std::borrow::Cow<'_, str>) {
        let (lo, hi) = (floor_boundary(self, lo), floor_boundary(self, hi));
        (lo, std::borrow::Cow::Borrowed(&self[lo..hi.max(lo)]))
    }
    fn line_start_near(&self, p: usize) -> usize {
        let p = floor_boundary(self, p);
        let lo = floor_boundary(self, p.saturating_sub(LINE_CONTEXT));
        match self[lo..p].rfind('\n') {
            Some(i) => lo + i + 1,
            None if lo == 0 => 0,
            None => p,
        }
    }
}

/// 検索語の探し手。大文字小文字は区別しない。
///
/// 正規表現でない検索語は、組み立てが数 µs の aho-corasick で探す（regex は検索語ごとの組み立てに
/// 数十〜数百 µs かかり、1 打ごとに組み立て直す incsearch で効く）。aho-corasick は ASCII の範囲でしか
/// 大文字小文字を同一視しないので、大文字小文字のある非 ASCII 文字（É など）を含むときだけ regex を使う。
///
/// 探すときはファイル全体の文字列を作らず、窓（最初は 64KB、見つからなければ倍に）ごとに切り出して探す。
/// incsearch や n / N の移動先は、カーソルの近くだけを見れば済む
pub enum Finder {
    Ac(aho_corasick::AhoCorasick),
    Re(Regex),
}

/// 最初の窓の幅（見つからなければ倍にしていく）
const FIRST_WINDOW: usize = 1 << 16;
/// 窓の幅の上限
const MAX_WINDOW: usize = 1 << 22;
/// 窓の端で始まる一致を拾うために、窓の先まで見る長さ（これより長い一致は窓の端で取りこぼしうる）
const SPAN_MARGIN: usize = 4096;

fn floor_boundary(text: &str, mut i: usize) -> usize {
    i = i.min(text.len());
    while !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn next_boundary(text: &str, i: usize) -> usize {
    text[i.min(text.len())..].chars().next().map_or(text.len(), |c| i + c.len_utf8())
}

impl Finder {
    pub fn new(pattern: &str, regex: bool) -> Option<Self> {
        if pattern.is_empty() {
            return None;
        }
        let caseless_beyond_ascii = pattern.chars().all(|c| c.is_ascii() || (!c.is_lowercase() && !c.is_uppercase()));
        if !regex && caseless_beyond_ascii {
            return aho_corasick::AhoCorasick::builder().ascii_case_insensitive(true).build([pattern]).ok().map(Finder::Ac);
        }
        compile_search(pattern, regex).map(Finder::Re)
    }

    /// 文字列 `text` の [from, text.len()) にある一致を順に `f` へ渡す（`f` が false を返したら止める）。空の一致は除く
    fn each_in(&self, text: &str, from: usize, mut f: impl FnMut((usize, usize)) -> bool) {
        let from = floor_boundary(text, from);
        match self {
            Finder::Ac(ac) => {
                for m in ac.find_iter(aho_corasick::Input::new(text).span(from..text.len())) {
                    if !f((m.start(), m.end())) {
                        return;
                    }
                }
            }
            Finder::Re(re) => {
                let mut p = from;
                while p <= text.len() {
                    let Some(m) = re.find_at(text, p) else { return };
                    if m.is_empty() {
                        if m.start() >= text.len() {
                            return;
                        }
                        p = next_boundary(text, m.start());
                        continue;
                    }
                    if !f((m.start(), m.end())) {
                        return;
                    }
                    p = m.end();
                }
            }
        }
    }

    /// 始まりが [lo, hi) にある一致を順に `f` へ渡す。窓は行頭から切り出す。`f` が false を返したら false
    fn each_window<S: Source + ?Sized>(&self, src: &S, lo: usize, hi: usize, mut f: impl FnMut((usize, usize)) -> bool) -> bool {
        let base = src.line_start_near(lo);
        let (b, text) = src.window(base, (hi + SPAN_MARGIN).min(src.len()));
        let mut cont = true;
        self.each_in(&text, lo.saturating_sub(b), |(s, e)| {
            let (s, e) = (s + b, e + b);
            if s < lo {
                return true;
            }
            if s >= hi {
                return false;
            }
            cont = f((s, e));
            cont
        });
        cont
    }

    /// `from` から先の一致を順に `f` へ渡す（`f` が false を返したら止める）
    pub fn each_from<S: Source + ?Sized>(&self, src: &S, from: usize, f: impl FnMut((usize, usize)) -> bool) {
        self.each_from_while(src, from, || true, f);
    }

    /// `each_from` の、窓ごとに `go` を確かめ、false なら途中でやめる版（裏で数えている検索の取り消し用）
    pub fn each_from_while<S: Source + ?Sized>(&self, src: &S, from: usize, go: impl Fn() -> bool, mut f: impl FnMut((usize, usize)) -> bool) {
        let len = src.len();
        let mut lo = from;
        let mut last_end = 0;
        while lo < len && go() {
            let hi = (lo + MAX_WINDOW).min(len);
            let cont = self.each_window(src, lo, hi, |m| {
                last_end = m.1;
                f(m)
            });
            if !cont {
                return;
            }
            // 窓の終わりをまたいだ一致の途中からは数え直さない（全体を 1 回で探したのと同じ数え方にする）
            lo = hi.max(last_end);
        }
    }

    /// `at` より後ろ（前）で最初の一致。`include_at` なら `at` から始まる一致も含める。
    /// 端に来たら反対側から続ける（wrapscan）。`at` の近くから探し、一致が無くてもファイルを 1 回なめるだけで済ませる
    pub fn next<S: Source + ?Sized>(&self, src: &S, at: usize, forward: bool, include_at: bool) -> Option<(usize, usize)> {
        let len = src.len();
        let at = at.min(len);
        if forward {
            let from = if include_at { at } else { at + 1 };
            return self.first_in(src, from, len).or_else(|| self.first_in(src, 0, from.min(len)));
        }
        let hi = if include_at { at + 1 } else { at };
        self.last_in(src, 0, hi.min(len)).or_else(|| self.last_in(src, hi.min(len), len))
    }

    /// `next` の、`at` から `reach` byte の範囲だけを見る版（端で反対側へは回らない）。
    /// 大きなファイルの incsearch で、1 打ごとにファイル全体をなめないために使う
    pub fn next_near<S: Source + ?Sized>(&self, src: &S, at: usize, forward: bool, include_at: bool, reach: usize) -> Option<(usize, usize)> {
        let len = src.len();
        let at = at.min(len);
        // 端を越える分は反対側の端から続ける（wrapscan。ファイルの末尾近くで下へ探すと先頭の一致に回る）
        if forward {
            let from = (if include_at { at } else { at + 1 }).min(len);
            let over = (from + reach).saturating_sub(len);
            return self.first_in(src, from, (from + reach).min(len)).or_else(|| self.first_in(src, 0, over.min(from)));
        }
        let hi = (if include_at { at + 1 } else { at }).min(len);
        let over = reach.saturating_sub(hi);
        self.last_in(src, hi.saturating_sub(reach), hi).or_else(|| self.last_in(src, len.saturating_sub(over).max(hi), len))
    }

    /// 始まりが [lo, hi) にある最初の一致。窓を倍にしながら先へ進む
    fn first_in<S: Source + ?Sized>(&self, src: &S, mut lo: usize, hi: usize) -> Option<(usize, usize)> {
        let mut window = FIRST_WINDOW;
        while lo < hi {
            let end = (lo + window).min(hi);
            let mut found = None;
            self.each_window(src, lo, end, |m| {
                found = Some(m);
                false
            });
            if found.is_some() {
                return found;
            }
            lo = end;
            window = (window * 2).min(MAX_WINDOW);
        }
        None
    }

    /// 始まりが [floor, hi) にある最後の一致。`hi` の手前の窓から、重ねずに倍の幅で手前へ進む
    fn last_in<S: Source + ?Sized>(&self, src: &S, floor: usize, mut hi: usize) -> Option<(usize, usize)> {
        let mut window = FIRST_WINDOW;
        while hi > floor {
            let lo = hi.saturating_sub(window).max(floor);
            let mut last = None;
            self.each_window(src, lo, hi, |m| {
                last = Some(m);
                true
            });
            if last.is_some() {
                return last;
            }
            hi = lo;
            window = (window * 2).min(MAX_WINDOW);
        }
        None
    }
}

/// `at` より後ろ（前）で最初の一致の番号。端に来たら反対側から続ける（wrapscan）
pub fn next_match(matches: &[(usize, usize)], at: usize, forward: bool, include_at: bool) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    // 一致は位置順に並んでいるので二分探索（大きなファイルでは数百万件になる）
    if forward {
        let i = matches.partition_point(|m| if include_at { m.0 < at } else { m.0 <= at });
        Some(if i < matches.len() { i } else { 0 })
    } else {
        let i = matches.partition_point(|m| if include_at { m.0 <= at } else { m.0 < at });
        Some(if i > 0 { i - 1 } else { matches.len() - 1 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 近くだけ探す `next_near` は、範囲内なら `next` と同じ一致を返し、範囲外なら None。端では反対側へ回る
    #[test]
    fn finder_next_near() {
        let text = format!("needle{}needle{}", "x".repeat(10_000), "y".repeat(10_000));
        let f = Finder::new("needle", false).unwrap();
        let second = 6 + 10_000;
        assert_eq!(f.next_near(text.as_str(), 0, true, false, 20_000), Some((second, second + 6)));
        assert_eq!(f.next_near(text.as_str(), 0, true, false, 5_000), None, "範囲外");
        // 末尾近くから下へ: 先頭の一致に回る
        assert_eq!(f.next_near(text.as_str(), text.len() - 10, true, false, 100), Some((0, 6)));
        // 先頭近くから上へ: 末尾側の一致に回る
        assert_eq!(f.next_near(text.as_str(), 3, false, false, 20_000), Some((0, 6)));
        assert_eq!(f.next_near(text.as_str(), 0, false, false, 20_000), Some((second, second + 6)));
        assert_eq!(f.next_near(text.as_str(), 0, false, false, 100), None);
    }

    /// 近くから探す `Finder::next` は、全件を並べてから選ぶやり方と同じ一致を返す
    #[test]
    fn finder_next_matches_full_scan() {
        let unit = "alpha Beta 日本語 beta\nfoo.bar ALPHA\n\n  gamma alphabet ";
        let text = unit.repeat(3000); // 窓（64KB）をまたぐ大きさ
        let buf = crate::Buffer::from_str(&text);
        for (pat, re) in [("alpha", false), ("BETA", false), ("日本", false), ("a.b", false), ("^foo", true), ("al?pha", true), ("zzz", false)] {
            let all = find_matches(&text, pat, re);
            assert_eq!(find_matches_in(&buf, pat, re), all, "buffer: {pat}");
            let f = Finder::new(pat, re).unwrap();
            for at in (0..text.len()).step_by(7919).chain([0, text.len() - 1, text.len()]) {
                let at = floor_boundary(&text, at);
                for (fwd, inc) in [(true, false), (true, true), (false, false), (false, true)] {
                    let want = next_match(&all, at, fwd, inc).map(|i| all[i]);
                    assert_eq!(f.next(text.as_str(), at, fwd, inc), want, "{pat} at={at} fwd={fwd} inc={inc}");
                    assert_eq!(f.next(&buf, at, fwd, inc), want, "buffer: {pat} at={at} fwd={fwd} inc={inc}");
                }
            }
        }
    }

    #[test]
    fn find_and_pairs() {
        assert_eq!(find_in_line("a,b,c", 0, ',', true, false, 2), Some(3));
        assert_eq!(find_in_line("a,b,c", 0, ',', true, true, 1), Some(0));
        assert_eq!(find_in_line("a,b,c", 4, ',', false, false, 1), Some(3));
        let t = "f(a, (b)) [x]";
        assert_eq!(match_pair(t, 0, t.len()), Some(8));
        assert_eq!(match_pair(t, 8, t.len()), Some(1));
        assert_eq!(enclosing_block(t, 6, '('), Some((5, 7)));
        assert_eq!(enclosing_block(t, 3, '('), Some((1, 8)));
        assert_eq!(enclosing_quote(r#"x = "ab\"c" + 'd'"#, 5, '"'), Some((4, 10)));
    }

    #[test]
    fn words() {
        assert_eq!(word_object("foo bar.baz", 5, false, false), Some((4, 7)));
        assert_eq!(word_object("foo bar baz", 4, false, true), Some((4, 8)));
        assert_eq!(word_object("foo bar.baz", 5, true, false), Some((4, 11)));
        assert_eq!(word_object("日本語 です", 3, false, false), Some((0, 9)));
    }

    #[test]
    fn search_ignores_case_and_handles_japanese() {
        let t = "Hover hover HOVER 日本語の検索、日本語";
        let re = compile_search("hover", false).unwrap();
        assert_eq!(find_all(t, &re).len(), 3);
        let re = compile_search("日本語", false).unwrap();
        assert_eq!(find_all(t, &re).len(), 2);
        let re = compile_search("a.b(", false).unwrap();
        assert!(find_all("a.b( axb(", &re).len() == 1);
        assert_eq!(find_matches(t, "hover", false).len(), 3);
        assert_eq!(find_matches(t, "日本語", false).len(), 2);
        assert_eq!(find_matches("a.b( axb(", "a.b(", false).len(), 1);
        assert_eq!(find_matches("CAFÉ café", "café", false).len(), 2, "É は regex で同一視");
        assert_eq!(find_matches("ab12 AB34", "ab\\d", true).len(), 2);
        let m = find_all(t, &compile_search("hover", false).unwrap());
        assert_eq!(next_match(&m, 1, true, false), Some(1));
        assert_eq!(next_match(&m, 13, true, false), Some(0));
    }
}
