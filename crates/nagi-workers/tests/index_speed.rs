//! 索引を作る時間（初回と 2 回目）。`NAGI_BIG_TREE` で根を指定したときだけ測る。保存先は `NAGI_INDEX_DIR`。
use std::time::Instant;

#[test]
fn index_speed() {
    let Some(root) = std::env::var_os("NAGI_BIG_TREE") else {
        eprintln!("NAGI_BIG_TREE が無いので飛ばす");
        return;
    };
    let root = std::path::PathBuf::from(root);
    for round in ["初回", "2 回目"] {
        let t = Instant::now();
        let rx = nagi_workers::index::build(&root, std::sync::Arc::new(|| {}));
        let idx = loop {
            if let Ok(nagi_workers::index::Progress::Done(i)) = rx.recv() {
                break i;
            }
        };
        eprintln!("{round}: {} ファイル, 定義 {} 件, {:?}", idx.file_count(), idx.def_count(), t.elapsed());
    }
}
