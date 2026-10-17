//! 大きな木でファイルを集める時間と、ファジーファインダーの 1 打ごとの絞り込みの時間。
//! `NAGI_BIG_TREE` で根を指定したときだけ測る。
//! `NAGI_BIG_TREE=C:\Users\me\.cargo\registry\src cargo test --release -p nagi-workers --test finder_speed -- --nocapture`
use std::time::Instant;

use nagi_core::fuzzy::{Query, rank_ids};

#[test]
fn finder_speed() {
    let Some(root) = std::env::var_os("NAGI_BIG_TREE") else {
        eprintln!("NAGI_BIG_TREE が無いので飛ばす");
        return;
    };
    let t = Instant::now();
    let w = nagi_workers::files::walk(std::path::Path::new(&root), std::sync::Arc::new(|| {}));
    let mut first = None;
    let mut items: Vec<String> = Vec::new();
    for batch in w.rx.iter() {
        first.get_or_insert(t.elapsed());
        items.extend(batch);
    }
    eprintln!("集める: {} 件, 最初の塊 {:?}, 全部 {:?}", items.len(), first, t.elapsed());
    items.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    // 大きなリポジトリの代わりに、同じ一覧を NAGI_BIG_TIMES 回並べて測る
    let times: usize = std::env::var("NAGI_BIG_TIMES").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    let items: Vec<String> = (0..times).flat_map(|k| items.iter().map(move |s| format!("{k}/{s}"))).collect();
    eprintln!("候補: {} 件", items.len());
    // 1 打ずつ打ち足す（打ち足しは前の候補だけを絞る）
    let mut hits: Vec<(u32, i32)> = Vec::new();
    for (n, q) in ["s", "sr", "src", "src/", "src/l", "src/li", "src/lib", "src/lib.", "src/lib.r", "src/lib.rs"].iter().enumerate() {
        let query = Query::new(q);
        let t = Instant::now();
        let ids: Vec<u32> = if n == 0 { (0..items.len() as u32).collect() } else { hits.iter().map(|&(i, _)| i).collect() };
        hits = rank_ids(&query, &items, &ids);
        eprintln!("{q:>12}: {:>6} 件 {:?}", hits.len(), t.elapsed());
    }
}
