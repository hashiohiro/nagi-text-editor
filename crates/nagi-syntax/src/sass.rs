//! Sass（インデントで書く形、.sass）のハイライト。tree-sitter の文法が無いので、行ごとに読む。
//!
//! 色は CSS・SCSS と同じ: プロパティとクラス・ID は属性の色、タグと @規則は青、$変数は型の色、文字列・数・コメント。
//! `=名前`（mixin の定義）と `+名前`（include）は記号を青、名前を関数の色に。

use std::ops::Range;

use crate::Hl;

/// 文章全体のハイライト（位置は文章の byte）
pub fn highlight(text: &str) -> Vec<(Range<usize>, Hl)> {
    let mut out = Vec::new();
    let mut at = 0;
    let mut in_block = false;
    for line in text.split_inclusive('\n') {
        line_spans(line.trim_end_matches(['\n', '\r']), at, &mut in_block, &mut out);
        at += line.len();
    }
    out
}

fn line_spans(line: &str, base: usize, in_block: &mut bool, out: &mut Vec<(Range<usize>, Hl)>) {
    let b = line.as_bytes();
    let indent = line.len() - line.trim_start().len();
    let body = &line[indent..];
    // 複数行の /* */（閉じるまで）
    if *in_block {
        match line.find("*/") {
            Some(e) => {
                out.push((base..base + e + 2, Hl::Comment));
                *in_block = false;
            }
            None => {
                out.push((base..base + line.len(), Hl::Comment));
            }
        }
        return;
    }
    if body.starts_with("//") {
        out.push((base + indent..base + line.len(), Hl::Comment));
        return;
    }
    if body.starts_with("/*") {
        match body.find("*/") {
            Some(e) => out.push((base + indent..base + indent + e + 2, Hl::Comment)),
            None => {
                out.push((base + indent..base + line.len(), Hl::Comment));
                *in_block = true;
            }
        }
        return;
    }
    let word_end = |from: usize| -> usize {
        let mut i = from;
        while i < b.len() && (b[i].is_ascii_alphanumeric() || matches!(b[i], b'-' | b'_')) {
            i += 1;
        }
        i
    };
    let s = indent;
    // @規則（@import @use @mixin @include @if @each など）
    if body.starts_with('@') {
        let e = word_end(s + 1);
        out.push((base + s..base + e, Hl::Keyword));
        // @mixin / @function の名前
        if matches!(&line[s..e], "@mixin" | "@function" | "@include") {
            let n0 = e + (line[e..].len() - line[e..].trim_start().len());
            let n1 = word_end(n0);
            if n1 > n0 {
                out.push((base + n0..base + n1, Hl::Function));
            }
            value_spans(line, n1, base, out);
        } else {
            value_spans(line, e, base, out);
        }
        return;
    }
    // =名前（mixin の定義）、+名前（include）
    if body.starts_with('=') || (body.starts_with('+') && body.len() > 1 && (b[s + 1].is_ascii_alphabetic() || b[s + 1] == b'_')) {
        out.push((base + s..base + s + 1, Hl::Keyword));
        let e = word_end(s + 1);
        out.push((base + s + 1..base + e, Hl::Function));
        value_spans(line, e, base, out);
        return;
    }
    // $変数: 値
    if body.starts_with('$') {
        let e = word_end(s + 1);
        out.push((base + s..base + e, Hl::Type));
        value_spans(line, e, base, out);
        return;
    }
    // プロパティ: 「名前:」の後が空白か行末（a:hover のような選択子と分ける）、古い形の「:名前 値」
    let e = word_end(s);
    let colon_prop = e > s && line[e..].starts_with(':') && line[e + 1..].chars().next().is_none_or(char::is_whitespace);
    let old_prop = body.starts_with(':') && word_end(s + 1) > s + 1 && line[word_end(s + 1)..].starts_with(' ');
    if colon_prop || (old_prop && indent > 0) {
        let (n0, n1) = if old_prop { (s + 1, word_end(s + 1)) } else { (s, e) };
        out.push((base + n0..base + n1, Hl::Attribute));
        value_spans(line, n1 + 1, base, out);
        return;
    }
    // 選択子: .class #id は属性の色、タグは青、& と :hover の類は控えめに
    let mut i = s;
    while i < b.len() {
        match b[i] {
            b'.' | b'#' => {
                let e = word_end(i + 1);
                out.push((base + i..base + e, Hl::Attribute));
                i = e.max(i + 1);
            }
            b':' => {
                let mut e = i + 1;
                if e < b.len() && b[e] == b':' {
                    e += 1;
                }
                let e = word_end(e);
                out.push((base + i..base + e, Hl::Attribute));
                i = e.max(i + 1);
            }
            b'&' => {
                out.push((base + i..base + i + 1, Hl::Number));
                i += 1;
            }
            c if c.is_ascii_alphabetic() => {
                let e = word_end(i);
                out.push((base + i..base + e, Hl::Keyword));
                i = e;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                out.push((base + i..base + line.len(), Hl::Comment));
                break;
            }
            _ => i += 1,
        }
    }
}

/// 値の部分（文字列・数と単位・#色・$変数・関数・!important・行末の // コメント）
fn value_spans(line: &str, from: usize, base: usize, out: &mut Vec<(Range<usize>, Hl)>) {
    let b = line.as_bytes();
    let mut i = from;
    while i < b.len() {
        let c = b[i];
        match c {
            b'"' | b'\'' => {
                let mut e = i + 1;
                while e < b.len() && b[e] != c {
                    if b[e] == b'\\' {
                        e += 1;
                    }
                    e += 1;
                }
                let e = (e + 1).min(b.len());
                out.push((base + i..base + e, Hl::String));
                i = e;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                out.push((base + i..base + line.len(), Hl::Comment));
                return;
            }
            b'$' => {
                let mut e = i + 1;
                while e < b.len() && (b[e].is_ascii_alphanumeric() || matches!(b[e], b'-' | b'_')) {
                    e += 1;
                }
                out.push((base + i..base + e, Hl::Type));
                i = e;
            }
            b'#' if b.get(i + 1).is_some_and(u8::is_ascii_hexdigit) => {
                let mut e = i + 1;
                while e < b.len() && b[e].is_ascii_hexdigit() {
                    e += 1;
                }
                out.push((base + i..base + e, Hl::Number));
                i = e;
            }
            b'!' => {
                let mut e = i + 1;
                while e < b.len() && b[e].is_ascii_alphabetic() {
                    e += 1;
                }
                out.push((base + i..base + e, Hl::Keyword));
                i = e;
            }
            c if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit)) || (c == b'-' && b.get(i + 1).is_some_and(u8::is_ascii_digit) && (i == 0 || !b[i - 1].is_ascii_alphanumeric())) => {
                let mut e = i + 1;
                while e < b.len() && (b[e].is_ascii_alphanumeric() || b[e] == b'.' || b[e] == b'%') {
                    e += 1;
                }
                out.push((base + i..base + e, Hl::Number));
                i = e;
            }
            c if c.is_ascii_alphabetic() || c == b'-' || c == b'_' => {
                let mut e = i;
                while e < b.len() && (b[e].is_ascii_alphanumeric() || matches!(b[e], b'-' | b'_')) {
                    e += 1;
                }
                // 関数の呼び出し（darken( など）
                if b.get(e) == Some(&b'(') {
                    out.push((base + i..base + e, Hl::Function));
                }
                i = e.max(i + 1);
            }
            _ => i += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sass() {
        let src = "// コメント\n@use 'base'\n$main: #333\n=round($r: 4px)\n  border-radius: $r\n.card > a:hover\n  +round(8px)\n  color: darken($main, 10%) !important\n  &.active\n    margin: 0 -2px\n/* 長い\n   コメント */\n";
        let hl = highlight(src);
        let at = |s: &str| -> Option<Hl> {
            let i = src.find(s).unwrap();
            hl.iter().rev().find(|(r, _)| r.start <= i && i < r.end).map(|x| x.1)
        };
        assert_eq!(at("// コメント"), Some(Hl::Comment));
        assert_eq!(at("@use"), Some(Hl::Keyword));
        assert_eq!(at("'base'"), Some(Hl::String));
        assert_eq!(at("$main:"), Some(Hl::Type));
        assert_eq!(at("#333"), Some(Hl::Number));
        assert_eq!(at("=round"), Some(Hl::Keyword));
        assert_eq!(at("round($r"), Some(Hl::Function));
        assert_eq!(at("border-radius"), Some(Hl::Attribute));
        assert_eq!(at(".card"), Some(Hl::Attribute));
        assert_eq!(at("a:hover"), Some(Hl::Keyword), "選択子のタグ（a:hover はプロパティではない）");
        assert_eq!(at(":hover"), Some(Hl::Attribute));
        assert_eq!(at("+round(8px)"), Some(Hl::Keyword));
        assert_eq!(at("8px"), Some(Hl::Number));
        assert_eq!(at("color:"), Some(Hl::Attribute));
        assert_eq!(at("darken"), Some(Hl::Function));
        assert_eq!(at("!important"), Some(Hl::Keyword));
        assert_eq!(at("&.active"), Some(Hl::Number));
        assert_eq!(at("-2px"), Some(Hl::Number));
        assert_eq!(at("コメント */"), Some(Hl::Comment), "複数行のコメント");
    }
}
