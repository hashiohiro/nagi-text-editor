//! ユーザーの実際の .ideavimrc（tests/fixtures/ideavimrc は ~/IdeaProjects/env/.ideavimrc の写し）を読めること

use nagi_vim::{Key, action, parse_keys, parse_rc};

#[test]
fn real_ideavimrc() {
    let rc = parse_rc(include_str!("fixtures/ideavimrc"));
    assert_eq!(rc.leader, " ");
    assert!(rc.has("clipboard=unnamedplus"));
    // <expr> の j/k（2 行）だけが解釈されない
    assert_eq!(rc.notes.len(), 2, "{:#?}", rc.notes);
    let ew = rc.keymap.normal.iter().find(|m| m.lhs == parse_keys("ew", " ")).unwrap();
    assert_eq!(ew.rhs, parse_keys(":<C-u>w<CR>", " "));

    // 右辺の `:action X` は、すべて対応表に載っていること（載っていなければ表に足す）
    let mut unknown = Vec::new();
    for m in &rc.keymap.normal {
        let rhs: String = m.rhs.iter().map(|k| match k {
            Key::Char(c) => c.to_string(),
            _ => " ".into(),
        }).collect();
        if let Some(i) = rhs.find("action ") {
            let name = rhs[i + 7..].split_whitespace().next().unwrap_or("");
            if action::ideavim(name).is_none() {
                unknown.push(name.to_string());
            }
        }
    }
    assert!(unknown.is_empty(), "対応表にないアクション: {unknown:?}");
}

/// 読み込み時間の目安（cargo test --release -- --nocapture で表示）
#[test]
fn parse_speed() {
    let text = include_str!("fixtures/ideavimrc");
    let n = 1000;
    let t = std::time::Instant::now();
    for _ in 0..n {
        std::hint::black_box(parse_rc(text));
    }
    let per = t.elapsed() / n;
    let rc = parse_rc(text);
    let km = &rc.keymap;
    let mut m = nagi_vim::Matcher::default();
    let t = std::time::Instant::now();
    for _ in 0..n {
        for k in parse_keys("<leader>zLjj", " ") {
            std::hint::black_box(m.feed(km, k));
        }
    }
    let feed = t.elapsed() / (n * 6);
    println!("parse_rc: {per:?}/回（{} 行, マッピング {} 件）、キー 1 打の照合: {feed:?}", text.lines().count(), km.normal.len());
}
