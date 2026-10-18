//! プロジェクトの全文検索。`ignore` で `.gitignore` に従ってファイルを並べて歩き、`grep-searcher` で探す。
//! 1 ファイル分の一致がそろうたびに送るので、最初の結果はすぐ届く。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use grep_matcher::Matcher;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::sinks::UTF8;
use grep_searcher::{BinaryDetection, SearcherBuilder};

use crate::Waker;

/// これより多い一致は集めない（メモリと描画を抑える）
pub const MAX_HITS: usize = 20_000;
/// 1 行の本文として持つ最大の byte 数（長い行は切る）
const MAX_TEXT: usize = 400;

/// 1 つの一致
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// 根からの相対パス（区切りは `/`）
    pub path: String,
    /// 1 始まりの行番号
    pub line: usize,
    /// 行の中の一致の範囲（byte。`text` の中の位置）
    pub range: (usize, usize),
    /// 行の本文（行頭の空白を除き、長ければ切る）
    pub text: String,
}

/// 探している途中の検索。落とすと探すのをやめる
pub struct GrepJob {
    /// 見つけたもの（送り口が閉じたら探し終えた）。捨てると探すのをやめる
    pub rx: nagi_core::task::Task<Vec<Hit>>,
}

/// `root` 以下で `pattern` を探す。大文字小文字は区別しない。`regex` でなければ文字どおりに探す。
/// パターンが正しくなければ Err（正規表現の誤り）
pub fn search(root: &Path, pattern: &str, regex: bool, waker: Waker) -> Result<GrepJob, String> {
    let matcher = RegexMatcherBuilder::new().case_insensitive(true).fixed_strings(!regex).build(pattern).map_err(|e| e.to_string())?;
    let root: PathBuf = root.to_path_buf();
    let rx = nagi_core::task::spawn("grep", waker, move |ctx| {
        let (tx, waker, stop) = (ctx.sender(), ctx.waker(), ctx.cancel_flag());
        let count = Arc::new(AtomicUsize::new(0));
        let walker = crate::files::walker(&root).build_parallel();
        walker.run(|| {
            let matcher = matcher.clone();
            let tx = tx.clone();
            let waker = waker.clone();
            let stop = stop.clone();
            let count = count.clone();
            let root = root.clone();
            let mut searcher = SearcherBuilder::new().binary_detection(BinaryDetection::quit(0)).line_number(true).build();
            Box::new(move |entry| {
                if stop.load(Ordering::Relaxed) || count.load(Ordering::Relaxed) >= MAX_HITS {
                    return ignore::WalkState::Quit;
                }
                let Ok(e) = entry else { return ignore::WalkState::Continue };
                if !e.file_type().is_some_and(|t| t.is_file()) {
                    return ignore::WalkState::Continue;
                }
                let rel = e.path().strip_prefix(&root).unwrap_or(e.path()).to_string_lossy().replace('\\', "/");
                let mut hits = Vec::new();
                let _ = searcher.search_path(
                    &matcher,
                    e.path(),
                    UTF8(|line_no, line| {
                        let line = line.trim_end_matches(['\r', '\n']);
                        let lead = line.len() - line.trim_start().len();
                        let body = &line[lead..];
                        let Ok(Some(m)) = matcher.find(body.as_bytes()) else { return Ok(true) };
                        let mut cut = body.len().min(MAX_TEXT);
                        while !body.is_char_boundary(cut) {
                            cut -= 1;
                        }
                        let range = (m.start().min(cut), m.end().min(cut));
                        hits.push(Hit { path: rel.clone(), line: line_no as usize, range, text: body[..cut].to_string() });
                        Ok(!stop.load(Ordering::Relaxed))
                    }),
                );
                if !hits.is_empty() {
                    count.fetch_add(hits.len(), Ordering::Relaxed);
                    if tx.send(hits).is_err() {
                        return ignore::WalkState::Quit;
                    }
                    waker();
                }
                ignore::WalkState::Continue
            })
        });
        // 送り口を閉じて起こすのは task が行う（受け取る側が「終わった」と分かる）
        drop(tx);
    });
    Ok(GrepJob { rx })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_case_insensitive_and_literal() {
        let dir = std::env::temp_dir().join(format!("nagi-grep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join(".gitignore"), "out/\n").unwrap();
        std::fs::create_dir_all(dir.join("out")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn main() {\n    let Foo = 1;\n}\n").unwrap();
        std::fs::write(dir.join("src/b.txt"), "日本語の foo.bar\n").unwrap();
        std::fs::write(dir.join("out/c.rs"), "foo\n").unwrap();
        let job = search(&dir, "foo", false, Arc::new(|| {})).unwrap();
        let mut hits: Vec<Hit> = job.rx.iter().flatten().collect();
        hits.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(hits.len(), 2);
        assert_eq!((hits[0].path.as_str(), hits[0].line, hits[0].text.as_str(), hits[0].range), ("src/a.rs", 2, "let Foo = 1;", (4, 7)));
        assert_eq!(hits[1].text, "日本語の foo.bar");
        // 文字どおり: `.` は任意の 1 文字ではない
        let job = search(&dir, "o.b", false, Arc::new(|| {})).unwrap();
        assert_eq!(job.rx.iter().flatten().count(), 1);
        let job = search(&dir, "f.o", true, Arc::new(|| {})).unwrap();
        assert_eq!(job.rx.iter().flatten().count(), 2);
        assert!(search(&dir, "(", true, Arc::new(|| {})).is_err());
    }
}
