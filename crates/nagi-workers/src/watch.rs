//! ファイルの見張り（開いているファイル・設定・ツリーのフォルダ）。notify の知らせを受けたスレッドで
//! そのファイルの (更新時刻, 大きさ) を読んでから UI へ送る（UI スレッドではファイルを読まない）。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use notify::{EventKind, RecursiveMode, Watcher as _};

use crate::Waker;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// ファイルが書き換わった（今の更新時刻と大きさ）
    File(PathBuf, (SystemTime, u64)),
    /// ファイルが無くなった
    Removed(PathBuf),
    /// 見張っているフォルダの中身が変わった
    Dir(PathBuf),
}

pub struct Watcher {
    inner: notify::RecommendedWatcher,
    pub rx: Receiver<Change>,
    files: Arc<Mutex<HashSet<PathBuf>>>,
    dirs: Arc<Mutex<HashSet<PathBuf>>>,
    /// 実際に notify に渡しているフォルダ（ファイルは親のフォルダごと見張る）
    watched: HashSet<PathBuf>,
}

fn key(p: &Path) -> PathBuf {
    let p = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    PathBuf::from(p.to_string_lossy().to_lowercase())
}

impl Watcher {
    pub fn new(waker: Waker) -> Option<Self> {
        let (tx, rx) = channel();
        let files: Arc<Mutex<HashSet<PathBuf>>> = Default::default();
        let dirs: Arc<Mutex<HashSet<PathBuf>>> = Default::default();
        let (f2, d2) = (files.clone(), dirs.clone());
        let inner = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else { return };
            if matches!(ev.kind, EventKind::Access(_)) {
                return;
            }
            let mut sent = false;
            for p in &ev.paths {
                let k = key(p);
                if f2.lock().is_ok_and(|f| f.contains(&k)) {
                    let change = match nagi_stamp(p) {
                        Some(s) => Change::File(p.clone(), s),
                        None => Change::Removed(p.clone()),
                    };
                    sent |= tx.send(change).is_ok();
                }
                if let Some(parent) = p.parent() {
                    if d2.lock().is_ok_and(|d| d.contains(&key(parent))) {
                        sent |= tx.send(Change::Dir(parent.to_path_buf())).is_ok();
                    }
                }
            }
            if sent {
                waker();
            }
        })
        .ok()?;
        Some(Self { inner, rx, files, dirs, watched: HashSet::new() })
    }

    fn watch_dir(&mut self, dir: &Path) {
        let k = key(dir);
        if self.watched.insert(k) {
            let _ = self.inner.watch(dir, RecursiveMode::NonRecursive);
        }
    }

    /// ファイルを見張る（親のフォルダを見張り、そのファイルの知らせだけを送る）
    pub fn watch_file(&mut self, path: &Path) {
        let Ok(abs) = std::path::absolute(path) else { return };
        if let Ok(mut f) = self.files.lock() {
            f.insert(key(&abs));
        }
        if let Some(parent) = abs.parent() {
            self.watch_dir(parent);
        }
    }

    /// フォルダの中身の増減を見張る（ツリー用）
    pub fn watch_listing(&mut self, dir: &Path) {
        let Ok(abs) = std::path::absolute(dir) else { return };
        if let Ok(mut d) = self.dirs.lock() {
            d.insert(key(&abs));
        }
        self.watch_dir(&abs);
    }
}

fn nagi_stamp(p: &Path) -> Option<(SystemTime, u64)> {
    let m = std::fs::metadata(p).ok()?;
    Some((m.modified().ok()?, m.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_file_and_dir_changes() {
        let dir = std::env::temp_dir().join(format!("nagi-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let f = dir.join("a.txt");
        std::fs::write(&f, "1").unwrap();
        let mut w = Watcher::new(Arc::new(|| {})).unwrap();
        w.watch_file(&f);
        w.watch_listing(&dir.join("sub"));
        std::fs::write(&f, "22").unwrap();
        std::fs::write(dir.join("other.txt"), "x").unwrap();
        std::fs::write(dir.join("sub/new.txt"), "x").unwrap();
        let mut got = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            if let Ok(c) = w.rx.recv_timeout(std::time::Duration::from_millis(100)) {
                got.push(c);
            }
            let file = got.iter().any(|c| matches!(c, Change::File(p, (_, 2)) if p.ends_with("a.txt")));
            let sub = got.iter().any(|c| matches!(c, Change::Dir(p) if p.ends_with("sub")));
            if file && sub {
                break;
            }
        }
        assert!(got.iter().any(|c| matches!(c, Change::File(p, (_, 2)) if p.ends_with("a.txt"))), "{got:?}");
        assert!(got.iter().any(|c| matches!(c, Change::Dir(p) if p.ends_with("sub"))), "{got:?}");
        // 見張っていない other.txt の知らせは来ない
        assert!(!got.iter().any(|c| matches!(c, Change::File(p, _) if p.ends_with("other.txt"))));
        std::fs::remove_file(&f).unwrap();
        let removed = (0..30).any(|_| matches!(w.rx.recv_timeout(std::time::Duration::from_millis(100)), Ok(Change::Removed(_))));
        assert!(removed);
    }
}
