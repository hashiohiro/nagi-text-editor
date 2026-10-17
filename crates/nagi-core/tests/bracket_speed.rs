//! 大きなファイルで `%` と括弧の text object にかかる時間。`target/big/` があるときだけ測る。
//! `cargo test --release -p nagi-core --test bracket_speed -- --nocapture`
use std::time::Instant;

use nagi_core::textops::{enclosing_block, match_pair};

#[test]
fn bracket_speed() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/big");
    for name in ["big.ts", "min.json", "log100.txt"] {
        let path = dir.join(name);
        if !path.exists() {
            eprintln!("{name} が無いので飛ばす");
            continue;
        }
        let buf = nagi_core::Buffer::open(&path).unwrap();
        let len = buf.len_bytes();
        // 先頭・中ほど・末尾の手前で、最悪の場合（ファイルの端まで探す）も含めて測る
        for at in [0, len / 2, len.saturating_sub(100)] {
            let at = buf.line_start(buf.pos_of(at).0);
            let (l, _) = buf.pos_of(at);
            let end = at + buf.line_len(l);
            let t = Instant::now();
            let p = match_pair(&buf, at, end);
            let pct = t.elapsed();
            let t = Instant::now();
            let b = enclosing_block(&buf, at, '{');
            let blk = t.elapsed();
            let t = Instant::now();
            let r = enclosing_block(&buf, at, '<');
            let angle = t.elapsed();
            eprintln!("{name} @{at}: % {pct:?} ({p:?})  i{{ {blk:?} ({b:?})  i< {angle:?} ({r:?})");
        }
    }
}
