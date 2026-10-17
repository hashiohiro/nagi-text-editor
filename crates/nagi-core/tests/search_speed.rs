//! 検索の速さ（incsearch で 1 打ごとにかかる時間の内訳）。`cargo test --release -p nagi-core --test search_speed -- --nocapture`
use std::time::Instant;

use nagi_core::Buffer;
use nagi_core::textops::{compile_search, find_all, find_matches};

#[test]
fn incsearch_cost() {
    let src = include_str!("../../nagi/src/normal.rs");
    let buf = Buffer::from_str(&src.repeat(10));
    for pat in ["s", "se", "sel", "self", "日本", "Self.dot"] {
        let t = Instant::now();
        let re = compile_search(pat, false).unwrap();
        let compile = t.elapsed();
        let t = Instant::now();
        let text = buf.text();
        let gather = t.elapsed();
        let t = Instant::now();
        let n = find_all(&text, &re).len();
        let find = t.elapsed();
        let t = Instant::now();
        let n2 = find_matches(&text, pat, false).len();
        let fm = t.elapsed();
        assert_eq!(n, n2);
        eprintln!("{pat:>10}: find_matches {fm:>10?}");
        eprintln!("{pat:>10}: compile {compile:>10?}  text {gather:>10?}  find {find:>10?}  ({n} 件, {} KB)", text.len() / 1024);
    }
}

#[test]
fn compile_unicode_vs_ascii() {
    for pat in ["s", "self", "Self.dot"] {
        let src = regex::escape(pat);
        for unicode in [true, false] {
            let t = Instant::now();
            for _ in 0..50 {
                std::hint::black_box(regex::RegexBuilder::new(&src).case_insensitive(true).multi_line(true).unicode(unicode).build().unwrap());
            }
            eprintln!("{pat:>10} unicode={unicode:<5}: {:?}/回", t.elapsed() / 50);
        }
    }
}

/// 大きなファイルでの 1 打ごとの内訳（`target/big/log100.txt` があるときだけ）
#[test]
fn big_file_incsearch() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/big/log100.txt");
    let Ok(buf) = Buffer::open(&path) else {
        eprintln!("log100.txt が無いので飛ばす");
        return;
    };
    let t = Instant::now();
    let text = buf.text();
    eprintln!("全体の文字列 {:?}（{} MB）", t.elapsed(), text.len() >> 20);
    let mut prev: Vec<(usize, usize)> = Vec::new();
    let mut prev_pat = String::new();
    for pat in ["i", "it", "ite", "item", "items", "e", "el", "ela", "elapsed=99", "完", "完了"] {
        let t = Instant::now();
        let all = find_matches(&text, pat, false);
        let full = t.elapsed();
        // 打ち足しなら前回の一致から絞り込む
        let t = Instant::now();
        let narrowed: Option<Vec<(usize, usize)>> = (!prev_pat.is_empty() && pat.starts_with(prev_pat.as_str()) && pat.is_ascii()).then(|| {
            prev.iter().filter(|&&(s, _)| text.get(s..s + pat.len()).is_some_and(|w| w.eq_ignore_ascii_case(pat))).map(|&(s, _)| (s, s + pat.len())).collect()
        });
        let narrow = t.elapsed();
        if let Some(n) = &narrowed {
            assert_eq!(n, &all, "{pat}");
        }
        // 最寄りの 1 件だけ（中ほどから先）
        let t = Instant::now();
        let ac = aho_corasick::AhoCorasick::builder().ascii_case_insensitive(true).build([pat]).unwrap();
        let near = ac.find(&text[text.len() / 2..]).map(|m| m.start());
        let nearest = t.elapsed();
        eprintln!("{pat:>12}: 全件 {full:>10?}（{} 件）  絞り込み {:>10?}  最寄り {nearest:>8?} {near:?}", all.len(), narrowed.as_ref().map(|_| narrow));
        prev = all;
        prev_pat = pat.to_string();
    }
}
