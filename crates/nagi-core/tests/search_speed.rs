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
