//! プロジェクトのファイル一覧（ファジーファインダー用）。`.gitignore` などに従い、複数のスレッドで並べて集める。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::Waker;

/// これだけ集まるごとに送る（最初の候補を早く出すため小さめ）
const BATCH: usize = 256;

/// 集めている途中の一覧。落とすと集めるのをやめる
pub struct FileWalk {
    pub rx: Receiver<Vec<String>>,
    cancel: Arc<AtomicBool>,
}

impl Drop for FileWalk {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

struct Batch {
    buf: Vec<String>,
    tx: Sender<Vec<String>>,
    waker: Waker,
}

impl Batch {
    fn flush(&mut self) {
        if !self.buf.is_empty() && self.tx.send(std::mem::take(&mut self.buf)).is_ok() {
            (self.waker)();
        }
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        self.flush();
    }
}

/// `root` 以下のファイルを集める。パスは `root` からの相対で、区切りは `/`
pub fn walk(root: &Path, waker: Waker) -> FileWalk {
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let root = root.to_path_buf();
    let stop = cancel.clone();
    let _ = std::thread::Builder::new().name("files".into()).spawn(move || {
        let walker = ignore::WalkBuilder::new(&root).hidden(true).git_ignore(true).git_global(true).git_exclude(true).require_git(false).build_parallel();
        walker.run(|| {
            let mut b = Batch { buf: Vec::with_capacity(BATCH), tx: tx.clone(), waker: waker.clone() };
            let root = root.clone();
            let stop = stop.clone();
            Box::new(move |entry| {
                if stop.load(Ordering::Relaxed) {
                    return ignore::WalkState::Quit;
                }
                let Ok(e) = entry else { return ignore::WalkState::Continue };
                if e.file_type().is_some_and(|t| t.is_file()) {
                    if let Ok(rel) = e.path().strip_prefix(&root) {
                        b.buf.push(rel.to_string_lossy().replace('\\', "/"));
                        if b.buf.len() >= BATCH {
                            b.flush();
                        }
                    }
                }
                ignore::WalkState::Continue
            })
        });
    });
    FileWalk { rx, cancel }
}

/// `path` を含むプロジェクトの根（`.git` のあるところ）。無ければ None
pub fn project_root(path: &Path) -> Option<PathBuf> {
    let start = if path.is_dir() { path } else { path.parent()? };
    start.ancestors().find(|d| d.join(".git").exists()).map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_and_respects_gitignore() {
        let dir = std::env::temp_dir().join(format!("nagi-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/sub")).unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(dir.join("src/a.rs"), "").unwrap();
        std::fs::write(dir.join("src/sub/b.rs"), "").unwrap();
        std::fs::write(dir.join("target/c.rs"), "").unwrap();
        let w = walk(&dir, Arc::new(|| {}));
        let mut all: Vec<String> = w.rx.iter().flatten().collect();
        all.sort();
        assert_eq!(all, vec!["src/a.rs", "src/sub/b.rs"]);
    }
}
