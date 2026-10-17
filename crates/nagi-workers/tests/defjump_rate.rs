//! 定義ジャンプの的中率（M4 の受け入れ条件）。`NAGI_RATE_TREE` のプロジェクトを読むだけで、何も書き換えない
//! （索引の保存先は `NAGI_INDEX_DIR`）。
//! 参照（メソッド呼び出し・型）ごとに、ファイルの中で決まる / 索引で 1 か所に決まる / 候補が複数 / 見つからない を数える。
use std::collections::HashMap;

use nagi_syntax::Lang;
use nagi_syntax::defs::{DefParser, RefKind};

#[test]
fn defjump_rate() {
    let Some(root) = std::env::var_os("NAGI_RATE_TREE") else {
        eprintln!("NAGI_RATE_TREE が無いので飛ばす");
        return;
    };
    let root = std::path::PathBuf::from(root);
    let t0 = std::time::Instant::now();
    let rx = nagi_workers::index::build(&root, std::sync::Arc::new(|| {}));
    let idx = loop {
        match rx.recv() {
            Ok(nagi_workers::index::Progress::Done(i)) => break i,
            Ok(_) => {}
            Err(_) => panic!("索引を作る処理が終わりを知らせずに止まった"),
        }
    };
    eprintln!("索引: {:?}", t0.elapsed());
    let mut parser = DefParser::default();
    // 種類ごとに [ファイル内, 1 か所, 複数, なし]
    let mut counts: HashMap<RefKind, [usize; 4]> = HashMap::new();
    let mut same_name_file = (0usize, 0usize);
    // 型で候補が複数になった名前（名前 → (回数, 候補の数)）
    let mut multi: HashMap<String, (usize, usize)> = HashMap::new();
    // 候補が複数だったが import・同じモジュールで 1 か所に絞れた数
    let mut narrowed: HashMap<RefKind, usize> = HashMap::new();
    let mut files = 0;
    for e in ignore::WalkBuilder::new(&root).build().flatten() {
        let Some(lang) = Lang::detect(e.path()) else { continue };
        let Ok(src) = std::fs::read_to_string(e.path()) else { continue };
        files += 1;
        if std::env::var_os("NAGI_RATE_TRACE").is_some() {
            eprintln!("解析: {}", e.path().display());
        }
        let tf = std::time::Instant::now();
        let report = parser.reference_report(lang, &src);
        if tf.elapsed().as_millis() > 200 {
            eprintln!("遅い: {} {:?}", e.path().display(), tf.elapsed());
        }
        for (name, kind, local) in report {
            let c = counts.entry(kind).or_default();
            if local {
                c[0] += 1;
                continue;
            }
            let all = idx.lookup(&name).to_vec();
            let many = all.len() > 1;
            let rel = e.path().strip_prefix(&root).map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_default();
            let locs = nagi_workers::index::narrow_candidates(&rel, &src, &name, all);
            if many && locs.len() == 1 {
                *narrowed.entry(kind).or_default() += 1;
            }
            match locs.len() {
                0 => c[3] += 1,
                1 => {
                    c[1] += 1;
                    if kind == RefKind::Type {
                        same_name_file.1 += 1;
                        let stem = std::path::Path::new(&*locs[0].path).file_stem().and_then(|s| s.to_str()).unwrap_or("");
                        if stem == name {
                            same_name_file.0 += 1;
                        }
                    }
                }
                n => {
                    c[2] += 1;
                    if kind == RefKind::Type {
                        *multi.entry(name.clone()).or_insert((0, n)) = (multi.get(&name).map_or(0, |x| x.0) + 1, n);
                    }
                }
            }
        }
    }
    eprintln!("ファイル {files}、索引の定義 {}", idx.def_count());
    let mut m: Vec<_> = multi.iter().collect();
    m.sort_by(|a, b| b.1.0.cmp(&a.1.0));
    for (n, (k, c)) in m.iter().take(8) {
        let locs: Vec<String> = idx.lookup(n).iter().take(3).map(|l| l.path.rsplit(['\\', '/']).take(2).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("/")).collect();
        eprintln!("候補が複数の型: {n} ×{k}（定義 {c} か所: {}）", locs.join(", "));
    }
    for (kind, label) in [(RefKind::Type, "型"), (RefKind::Call, "呼び出し")] {
        eprintln!("{label}: 絞り込みで 1 か所に決まった {}", narrowed.get(&kind).copied().unwrap_or(0));
        let c = counts.get(&kind).copied().unwrap_or_default();
        let all: usize = c.iter().sum();
        let project = c[0] + c[1] + c[2];
        let pct = |x: usize, d: usize| if d == 0 { 0.0 } else { x as f64 * 100.0 / d as f64 };
        eprintln!(
            "{label}: 参照 {all}（ファイル内 {} / 1 か所 {} / 複数 {} / なし {}）",
            c[0], c[1], c[2], c[3]
        );
        eprintln!(
            "  全体: すぐ跳べる {:.1}%、選べば跳べる {:.1}%、見つからない {:.1}%",
            pct(c[0] + c[1], all),
            pct(c[2], all),
            pct(c[3], all)
        );
        eprintln!("  プロジェクト内の名前に限る: すぐ跳べる {:.1}%、複数から選ぶ {:.1}%", pct(c[0] + c[1], project), pct(c[2], project));
    }
    eprintln!("型で 1 か所に決まったうち、同じ名前のファイルへ跳ぶもの: {}/{}", same_name_file.0, same_name_file.1);
}
