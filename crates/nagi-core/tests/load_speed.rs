//! 大きなファイルの読み込みの内訳。`target/big/log100.txt` があるときだけ測る。
//! `cargo test --release -p nagi-core --test load_speed -- --nocapture`
use std::time::Instant;

use ropey::Rope;

#[test]
fn load_breakdown() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/big/log100.txt");
    if !path.exists() {
        eprintln!("log100.txt が無いので飛ばす");
        return;
    }
    for _ in 0..3 {
        let t = Instant::now();
        let bytes = std::fs::read(&path).unwrap();
        let read = t.elapsed();
        let t = Instant::now();
        let text = String::from_utf8_lossy(&bytes);
        let utf8 = t.elapsed();
        let t = Instant::now();
        let crlf = text.contains("\r\n");
        let crlf_t = t.elapsed();
        let t = Instant::now();
        let rope = Rope::from_str(&text);
        let build = t.elapsed();
        let t = Instant::now();
        let all = nagi_core::Buffer::open(&path).unwrap();
        let open = t.elapsed();
        let t = Instant::now();
        let mut head_at = None;
        let streamed = nagi_core::Buffer::open_with_head(&path, |h| head_at = Some((t.elapsed(), h.line_count()))).unwrap();
        let stream = t.elapsed();
        assert_eq!(streamed.text(), all.text());
        assert_eq!((streamed.crlf, streamed.bom), (all.crlf, all.bom));
        eprintln!("open_with_head {stream:?}（先頭 {head_at:?}）");
        eprintln!("read {read:?}  utf8 {utf8:?}  crlf {crlf_t:?}({crlf})  rope {build:?}  Buffer::open {open:?}  ({} 行)", all.line_count());
        drop(rope);
    }
}
