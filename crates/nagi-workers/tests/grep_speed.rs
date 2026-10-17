//! 全文検索の最初の結果と全部が届くまでの時間。`NAGI_BIG_TREE` で根を指定したときだけ測る。
use std::time::Instant;

#[test]
fn grep_speed() {
    let Some(root) = std::env::var_os("NAGI_BIG_TREE") else {
        eprintln!("NAGI_BIG_TREE が無いので飛ばす");
        return;
    };
    for (pattern, regex) in [("fn main", false), ("unsafe impl", false), ("zzqqxx_not_found", false), (r"impl\s+Drop", true)] {
        let t = Instant::now();
        let job = nagi_workers::grep::search(std::path::Path::new(&root), pattern, regex, std::sync::Arc::new(|| {})).unwrap();
        let mut first = None;
        let mut n = 0;
        for batch in job.rx.iter() {
            first.get_or_insert(t.elapsed());
            n += batch.len();
        }
        eprintln!("{pattern:>18}: {n:>6} 件  最初 {first:?}  全部 {:?}", t.elapsed());
    }
}
