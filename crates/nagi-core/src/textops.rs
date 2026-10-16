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

const PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];

/// `%`: `at` 以降で行内の最初の括弧を探し、対応する括弧の位置を返す
pub fn match_pair(text: &str, at: usize, line_end: usize) -> Option<usize> {
    let (start, open, close, forward) = text[at..line_end].char_indices().find_map(|(i, c)| {
        PAIRS.iter().find_map(|&(o, cl)| {
            if c == o {
                Some((at + i, o, cl, true))
            } else if c == cl {
                Some((at + i, o, cl, false))
            } else {
                None
            }
        })
    })?;
    let mut depth = 0i32;
    if forward {
        for (i, c) in text[start..].char_indices() {
            if c == open {
                depth += 1;
            } else if c == close {
                depth -= 1;
                if depth == 0 {
                    return Some(start + i);
                }
            }
        }
    } else {
        for (i, c) in text[..=start].char_indices().rev() {
            if c == close {
                depth += 1;
            } else if c == open {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
        }
    }
    None
}

/// 開き括弧 `open` で囲まれた、`at` を含む最も内側の範囲 (開き括弧の位置, 閉じ括弧の位置)
pub fn enclosing_block(text: &str, at: usize, open: char) -> Option<(usize, usize)> {
    let close = match open {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '<' => '>',
        _ => return None,
    };
    // カーソルが開き括弧の上なら、その括弧から始める
    let mut depth = 0i32;
    let mut start = None;
    if text[at..].starts_with(open) {
        start = Some(at);
    } else {
        for (i, c) in text[..at].char_indices().rev() {
            if c == close {
                depth += 1;
            } else if c == open {
                if depth == 0 {
                    start = Some(i);
                    break;
                }
                depth -= 1;
            }
        }
    }
    let start = start?;
    let mut depth = 0i32;
    for (i, c) in text[start..].char_indices() {
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Some((start, start + i));
            }
        }
    }
    None
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
    RegexBuilder::new(&src).case_insensitive(true).multi_line(true).build().ok()
}

/// すべての一致（絶対 byte の範囲）。空の一致は除く
pub fn find_all(text: &str, re: &Regex) -> Vec<(usize, usize)> {
    re.find_iter(text).filter(|m| !m.is_empty()).map(|m| (m.start(), m.end())).collect()
}

/// `at` より後ろ（前）で最初の一致の番号。端に来たら反対側から続ける（wrapscan）
pub fn next_match(matches: &[(usize, usize)], at: usize, forward: bool, include_at: bool) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    if forward {
        matches.iter().position(|m| if include_at { m.0 >= at } else { m.0 > at }).or(Some(0))
    } else {
        matches.iter().rposition(|m| if include_at { m.0 <= at } else { m.0 < at }).or(Some(matches.len() - 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let m = find_all(t, &compile_search("hover", false).unwrap());
        assert_eq!(next_match(&m, 1, true, false), Some(1));
        assert_eq!(next_match(&m, 13, true, false), Some(0));
    }
}
