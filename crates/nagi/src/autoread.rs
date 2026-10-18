//! 外部での変更の検知（Vim の autoread に近い）と、設定ファイルの即時反映。
//!
//! - 開いているファイルが外部で書き換わったら、バッファに変更が無ければ裏で読み直して差し替える。
//!   変更があれば差し替えずに知らせる（W12）。消されたら知らせる。
//! - 自分の保存では反応しない（保存したときの更新時刻と大きさを覚えていて、同じなら無視する）。
//! - 設定ファイル（ideavimrc）が書き換わったら読み直す（App が行う）。
//! - ツリーで読み込んだフォルダの中身が変わったら、そのフォルダを読み直す。

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use nagi_core::{Buffer, History};
use nagi_workers::watch::{Change, Watcher};

use crate::buffers::Slot;
use crate::editor::Editor;

/// 裏で読み直したバッファの受け渡し
pub struct Reloads {
    tx: Sender<(PathBuf, Buffer)>,
    rx: Receiver<(PathBuf, Buffer)>,
}

impl Default for Reloads {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self { tx, rx }
    }
}

fn same(a: &Path, b: &Path) -> bool {
    let n = |p: &Path| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().to_lowercase();
    n(a) == n(b)
}

impl Editor {
    /// 見張りを始める（App が waker を入れた後に呼ぶ）。今のバッファのファイルと設定ファイルを見張る
    pub fn start_watch(&mut self, rc_files: &[PathBuf]) {
        let waker = self.waker.clone().unwrap_or_else(|| std::sync::Arc::new(|| {}));
        let Some(mut w) = Watcher::new(waker) else { return };
        if let Some(p) = &self.buf.path {
            w.watch_file(p);
        }
        for p in rc_files {
            w.watch_file(p);
        }
        self.rc_files = rc_files.to_vec();
        self.watcher = Some(w);
    }

    pub(crate) fn watch_file(&mut self, p: &Path) {
        if let Some(w) = &mut self.watcher {
            w.watch_file(p);
        }
    }

    pub(crate) fn watch_listing(&mut self, dir: &Path) {
        if let Some(w) = &mut self.watcher {
            w.watch_listing(dir);
        }
    }

    /// バッファ `doc` の (変更あり, 覚えている時刻と大きさ)
    fn doc_disk(&self, doc: usize) -> Option<(bool, Option<nagi_core::DiskStamp>)> {
        match self.docs.get(doc)? {
            Slot::Active => Some((self.buf.dirty, self.buf.disk)),
            Slot::Parked(d) => Some((d.buf.dirty, d.buf.disk)),
            Slot::Deleted => None,
        }
    }

    fn set_doc_disk(&mut self, doc: usize, s: Option<nagi_core::DiskStamp>) {
        match self.docs.get_mut(doc) {
            Some(Slot::Active) => self.buf.disk = s,
            Some(Slot::Parked(d)) => d.buf.disk = s,
            _ => {}
        }
    }

    /// 届いた知らせを処理する。戻り値は (描き直す, 設定を読み直す)
    pub fn poll_watch(&mut self) -> (bool, bool) {
        let mut changes = Vec::new();
        if let Some(w) = &self.watcher {
            while let Ok(c) = w.rx.try_recv() {
                changes.push(c);
            }
        }
        let mut redraw = false;
        let mut reload_rc = false;
        for c in changes {
            match c {
                Change::File(p, stamp) => {
                    if self.rc_files.iter().any(|r| same(r, &p)) {
                        reload_rc = true;
                    }
                    let Some(doc) = self.find_doc(&p) else { continue };
                    let Some((dirty, disk)) = self.doc_disk(doc) else { continue };
                    if disk == Some(stamp) {
                        continue; // 自分の保存か、同じ変更の 2 回目の知らせ
                    }
                    self.set_doc_disk(doc, Some(stamp));
                    let name = p.file_name().map_or(p.display().to_string(), |n| n.to_string_lossy().into_owned());
                    // 設定の「外部での変更」: 何もしない／知らせるだけ／自動で読み直す
                    let mode = self.settings.get("editor.autoread");
                    if mode == "off" {
                        continue;
                    }
                    if mode == "ask" && !dirty {
                        self.message = format!("\"{name}\" が外部で変更されました（:e! で読み直す）");
                        redraw = true;
                        continue;
                    }
                    if dirty {
                        self.message = format!("W12: \"{name}\" が外部で変更されました。バッファにも変更があります（:e! で読み直す）");
                    } else {
                        // 裏で読み直し、届いたら差し替える
                        let tx = self.reloads.tx.clone();
                        let waker = self.waker.clone();
                        let _ = std::thread::Builder::new().name("reload".into()).spawn(move || {
                            if let Ok(b) = Buffer::open(&p) {
                                if tx.send((p, b)).is_ok() {
                                    if let Some(w) = waker {
                                        w();
                                    }
                                }
                            }
                        });
                    }
                    redraw = true;
                }
                Change::Removed(p) => {
                    if let Some(doc) = self.find_doc(&p) {
                        if self.doc_disk(doc).is_some_and(|(_, d)| d.is_some()) {
                            self.set_doc_disk(doc, None);
                            self.message = format!("E211: \"{}\" は削除されました", p.display());
                            redraw = true;
                        }
                    }
                }
                Change::Dir(d) => redraw |= self.tree_reload_dir(&d),
            }
        }
        while let Ok((p, b)) = self.reloads.rx.try_recv() {
            redraw |= self.apply_reload(&p, b);
        }
        (redraw, reload_rc)
    }

    /// 読み直したバッファを差し替える。読み直している間に編集していたら差し替えない
    fn apply_reload(&mut self, p: &Path, b: Buffer) -> bool {
        let Some(doc) = self.find_doc(p) else { return false };
        // 古い読み直し（同じ変更の知らせが 2 回来て、遅れて届いたもの。その後に自分で保存していれば元に戻してしまう）は当てない。
        // 読んだときのファイルの時刻と大きさが、今覚えているものと同じときだけ
        if self.doc_disk(doc).is_some_and(|(_, d)| d != b.disk) {
            return false;
        }
        let name = p.file_name().map_or(p.display().to_string(), |n| n.to_string_lossy().into_owned());
        match self.docs.get_mut(doc) {
            Some(Slot::Active) => {
                if self.buf.dirty {
                    return false;
                }
                let path = self.buf.path.clone();
                self.buf = b;
                self.buf.path = path;
                self.hist = History::default();
                self.syntax = None;
                self.pending_syntax = None;
                let last = self.buf.line_count().saturating_sub(1);
                self.cursor.line = self.cursor.line.min(last);
                self.top = self.top.min(last);
                self.clamp_normal();
                self.reset_search_cache();
                self.start_syntax();
            }
            Some(Slot::Parked(d)) => {
                if d.buf.dirty {
                    return false;
                }
                let path = d.buf.path.clone();
                d.buf = b;
                d.buf.path = path;
                d.hist = History::default();
                // 構文木は、そのバッファに戻ったときに作り直す
                d.syntax = None;
                d.pending_syntax = None;
            }
            _ => return false,
        }
        self.message = format!("\"{name}\" を読み直しました（外部で変更）");
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(e: &mut Editor, k: &str) {
        for key in nagi_vim::parse_keys(k, " ") {
            e.key(key);
        }
    }

    /// 知らせが届いて条件を満たすまで待つ（最大 3 秒）
    fn wait(e: &mut Editor, mut done: impl FnMut(&Editor, bool) -> bool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            let (_, rc) = e.poll_watch();
            if done(e, rc) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn reloads_external_changes() {
        let dir = std::env::temp_dir().join(format!("nagi-autoread-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.txt");
        let rc = dir.join("rc.vim");
        std::fs::write(&f, "one\ntwo\n").unwrap();
        std::fs::write(&rc, "set number\n").unwrap();
        let mut e = Editor::new(Buffer::open(&f).unwrap(), None);
        e.start_watch(&[rc.clone()]);
        keys(&mut e, "j");
        // 外部で書き換え → 変更が無いので読み直す（カーソルの行は保つ）
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&f, "ONE\nTWO\nTHREE\n").unwrap();
        assert!(wait(&mut e, |e, _| e.buf.line(0) == "ONE"), "読み直されない: {}", e.message);
        assert!(e.message.contains("読み直しました"));
        assert_eq!(e.cursor.line, 1);
        // 自分の保存では反応しない
        keys(&mut e, "x:w<CR>");
        e.message.clear();
        assert!(!wait(&mut e, |e, _| !e.message.is_empty()), "自分の保存に反応した: {}", e.message);
        // 編集中に外部で書き換え → 差し替えずに知らせる
        keys(&mut e, "x");
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&f, "outside\n").unwrap();
        assert!(wait(&mut e, |e, _| e.message.starts_with("W12")), "{}", e.message);
        assert_eq!(e.buf.line(1), "O");
        // 設定ファイルの書き換えは App へ知らせる
        std::fs::write(&rc, "set nonumber\n").unwrap();
        assert!(wait(&mut e, |_, rc| rc));
    }
}
