//! プロジェクトのファイル一覧（ファジーファインダー用）。`.gitignore` などに従い、複数のスレッドで並べて集める。

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;

use crate::Waker;

/// これだけ集まるごとに送る（最初の候補を早く出すため小さめ）
const BATCH: usize = 256;

/// 集めている途中の一覧。落とすと集めるのをやめる
pub struct FileWalk {
    /// 集めたもの（送り口が閉じたら集め終えた）。捨てると集めるのをやめる
    pub rx: nagi_core::task::Task<Vec<String>>,
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
    let root = root.to_path_buf();
    let rx = nagi_core::task::spawn("files", waker, move |ctx| {
        let (tx, waker, stop) = (ctx.sender(), ctx.waker(), ctx.cancel_flag());
        let walker = walker(&root).build_parallel();
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
        // 送り口を閉じて起こすのは task が行う（受け取る側が「集め終えた」と分かる）
        drop(tx);
    });
    FileWalk { rx }
}

/// フォルダの中身（ファイルツリー用）。(名前, フォルダか) をフォルダ・名前の順に並べる。`.gitignore` に従い、`.git` は出さない
pub type DirList = (PathBuf, Vec<(String, bool)>);

/// `dir` の中身を裏のスレッドで読み、`tx` に送って UI を起こす
pub fn list_dir(dir: PathBuf, tx: Sender<DirList>, waker: Waker) {
    nagi_core::task::spawn_to("list-dir", tx, waker, move || {
        let names = crate::config::for_path(&dir).exclude;
        let mut out: Vec<(String, bool)> = ignore::WalkBuilder::new(&dir)
            .max_depth(Some(1))
            .hidden(false)
            .require_git(false)
            .filter_entry(move |e| e.file_name() != ".git" && !excluded_name(e, &names))
            .build()
            .filter_map(Result::ok)
            .filter(|e| e.depth() == 1)
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.file_type().is_some_and(|t| t.is_dir())))
            .collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
        (dir, out)
    });
}

/// `path` を含むプロジェクトの根（`.git` のあるところ）。無ければ None
pub fn project_root(path: &Path) -> Option<PathBuf> {
    crate::project::root_of(path)
}

/// プロジェクトのファイルをたどる WalkBuilder（隠しファイルと .gitignore に加え、IntelliJ・Eclipse の除外フォルダ・出力先を外す）
pub fn walker(root: &Path) -> ignore::WalkBuilder {
    let ex = crate::project::detect(root).excludes;
    let mut b = ignore::WalkBuilder::new(root);
    b.hidden(true).git_ignore(true).git_global(true).git_exclude(true).require_git(false);
    // 設定の「除外するフォルダ」（このプロジェクトのもの）
    let names = crate::config::for_path(root).exclude;
    if !ex.is_empty() || !names.is_empty() {
        b.filter_entry(move |e| !ex.iter().any(|x| e.path() == x) && !excluded_name(e, &names));
    }
    b
}

/// 設定の「除外するフォルダ」の名前のフォルダか
fn excluded_name(e: &ignore::DirEntry, names: &[String]) -> bool {
    if !e.file_type().is_some_and(|t| t.is_dir()) {
        return false;
    }
    let name = e.file_name().to_string_lossy();
    names.iter().any(|x| x.eq_ignore_ascii_case(&name))
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
        let w = walk(&dir, nagi_core::task::no_waker());
        let mut all: Vec<String> = w.rx.iter().flatten().collect();
        all.sort();
        assert_eq!(all, vec!["src/a.rs", "src/sub/b.rs"]);
    }
}
