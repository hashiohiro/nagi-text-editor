//! 大きなファイルの構文解析とハイライトの速さ。`target/big/` にファイルがあるときだけ測る（無ければ飛ばす）。
//! `cargo test --release -p nagi-syntax --test big_file -- --nocapture`
use std::path::Path;
use std::time::Instant;

use nagi_core::Buffer;
use nagi_syntax::{Lang, Syntax};

#[test]
fn big_files() {
    for name in ["big.ts", "min.json"] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/big").join(name);
        let Ok(mut buf) = Buffer::open(&path) else {
            eprintln!("{name}: 無いので飛ばす");
            continue;
        };
        let mut s = Syntax::new(Lang::detect(&path).unwrap()).unwrap();
        let t = Instant::now();
        s.parse(&mut buf);
        eprintln!("{name}: 解析 {:?}", t.elapsed());
        // 先頭・中ほどの 1 画面ぶん（長い行は見えている 1KB だけ）
        for at in [0, buf.len_bytes() / 2] {
            let line = buf.pos_of(at).0;
            let start = buf.line_start(line).max(at.min(buf.line_start(line) + buf.line_len(line)));
            let end = if buf.line_count() > 1 { buf.line_start((line + 45).min(buf.line_count())) } else { start + 1024 };
            let t = Instant::now();
            let n = s.highlight(&buf, start..end.min(buf.len_bytes())).len();
            eprintln!("{name}: ハイライト @{at} {:?}（{n} 件）", t.elapsed());
        }
        // 1 文字挿入して差分解析（先頭・中ほど。文字列の中なので構文は壊れない）
        for at in [buf.slice_string(0, 400).find('"').map_or(10, |i| i + 1), buf.len_bytes() / 2] {
            let at = (at..).find(|&i| buf.slice_string(i, i + 1).chars().all(|c| c.is_ascii_alphanumeric())).unwrap();
            buf.edit(at, at, "x");
            let t = Instant::now();
            s.sync(&mut buf);
            eprintln!("{name}: 1 文字挿入後の再解析 @{at} {:?}", t.elapsed());
        }
    }
}
