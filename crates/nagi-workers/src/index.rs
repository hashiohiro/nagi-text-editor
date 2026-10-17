//! プロジェクトの定義の索引（tags の代わり。architecture.md §10）。
//!
//! `.gitignore` に従って対応する言語のファイルを集め、複数のスレッドで構文木を作って定義を取り出す。
//! 結果はファイルの (更新時刻, 大きさ) と一緒に `%LOCALAPPDATA%\nagi\index\` に保存し、次からは変わったファイルだけを
//! 解析し直す（差分更新）。保存したファイルは 1 つずつ索引し直す。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use nagi_syntax::Lang;
use nagi_syntax::defs::{DefKind, DefParser};

use crate::Waker;

/// 索引の 1 項目
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub kind: DefKind,
    pub line: u32,
    pub col: u32,
}

#[derive(Clone, Debug, Default)]
struct FileDefs {
    stamp: (u64, u64),
    defs: Vec<Entry>,
}

/// 名前で引く定義の場所
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loc {
    /// 根からの相対パス（区切りは `/`）
    pub path: Arc<str>,
    pub line: u32,
    pub col: u32,
    pub kind: DefKind,
}

#[derive(Default)]
pub struct Index {
    pub root: PathBuf,
    files: HashMap<Arc<str>, FileDefs>,
    by_name: HashMap<String, Vec<Loc>>,
}

impl Index {
    pub fn lookup(&self, name: &str) -> &[Loc] {
        self.by_name.get(name).map_or(&[], Vec::as_slice)
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn def_count(&self) -> usize {
        self.by_name.values().map(Vec::len).sum()
    }

    /// すべての定義（シンボル検索用）: (名前, 場所)
    pub fn all(&self) -> impl Iterator<Item = (&str, &Loc)> {
        self.by_name.iter().flat_map(|(n, v)| v.iter().map(move |l| (n.as_str(), l)))
    }

    /// 識別子の名前の一覧（補完用）
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.by_name.keys().map(String::as_str)
    }

    fn rebuild(&mut self) {
        let mut by: HashMap<String, Vec<Loc>> = HashMap::new();
        for (path, f) in &self.files {
            for d in &f.defs {
                by.entry(d.name.clone()).or_default().push(Loc { path: path.clone(), line: d.line, col: d.col, kind: d.kind });
            }
        }
        self.by_name = by;
    }

    /// 1 ファイルの索引を差し替える（保存したとき）
    pub fn replace_file(&mut self, rel: &str, stamp: (u64, u64), defs: Vec<Entry>) {
        let key: Arc<str> = rel.into();
        if let Some(old) = self.files.remove(rel) {
            for d in &old.defs {
                if let Some(v) = self.by_name.get_mut(&d.name) {
                    v.retain(|l| &*l.path != rel);
                    if v.is_empty() {
                        self.by_name.remove(&d.name);
                    }
                }
            }
        }
        for d in &defs {
            self.by_name.entry(d.name.clone()).or_default().push(Loc { path: key.clone(), line: d.line, col: d.col, kind: d.kind });
        }
        self.files.insert(key, FileDefs { stamp, defs });
    }

    /// 保存用の写し（名前の表は持たない）
    pub fn snapshot(&self) -> Index {
        Index { root: self.root.clone(), files: self.files.clone(), by_name: HashMap::new() }
    }

    /// 索引を保存する（裏のスレッドから呼ぶ）
    pub fn save(&self) {
        let Some(p) = cache_path(&self.root) else { return };
        let mut out = String::from("nagi-index 1\n");
        for (path, f) in &self.files {
            out.push_str(&format!("F\t{path}\t{}\t{}\n", f.stamp.0, f.stamp.1));
            for d in &f.defs {
                out.push_str(&format!("D\t{}\t{}\t{}\t{}\n", d.kind.code(), d.line, d.col, d.name));
            }
        }
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = p.with_extension("tmp");
        if std::fs::write(&tmp, out).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

/// 索引の保存先（根のパスの FNV-1a を名前にする）
fn cache_path(root: &Path) -> Option<PathBuf> {
    let base = std::env::var_os("NAGI_INDEX_DIR").map(PathBuf::from).or_else(|| std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("nagi").join("index")))?;
    let key = root.to_string_lossy().to_lowercase();
    let mut h: u64 = 0xcbf29ce484222325;
    for b in key.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    Some(base.join(format!("{h:016x}.idx")))
}

fn load_cache(root: &Path) -> HashMap<Arc<str>, FileDefs> {
    let mut out = HashMap::new();
    let Some(p) = cache_path(root) else { return out };
    let Ok(text) = std::fs::read_to_string(p) else { return out };
    let mut lines = text.lines();
    if lines.next() != Some("nagi-index 1") {
        return out;
    }
    let mut cur: Option<(Arc<str>, FileDefs)> = None;
    for l in lines {
        let mut it = l.split('\t');
        match it.next() {
            Some("F") => {
                if let Some((k, v)) = cur.take() {
                    out.insert(k, v);
                }
                let (Some(path), Some(m), Some(sz)) = (it.next(), it.next(), it.next()) else { continue };
                cur = Some((path.into(), FileDefs { stamp: (m.parse().unwrap_or(0), sz.parse().unwrap_or(0)), defs: Vec::new() }));
            }
            Some("D") => {
                let (Some(k), Some(line), Some(col), Some(name)) = (it.next(), it.next(), it.next(), it.next()) else { continue };
                let Some(kind) = k.parse().ok().and_then(DefKind::from_code) else { continue };
                if let Some((_, f)) = &mut cur {
                    f.defs.push(Entry { name: name.to_string(), kind, line: line.parse().unwrap_or(0), col: col.parse().unwrap_or(0) });
                }
            }
            _ => {}
        }
    }
    if let Some((k, v)) = cur.take() {
        out.insert(k, v);
    }
    out
}

/// ファイルの (更新時刻の ns, 大きさ)
pub fn stamp(path: &Path) -> Option<(u64, u64)> {
    let m = std::fs::metadata(path).ok()?;
    let t = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_nanos() as u64;
    Some((t, m.len()))
}

/// 1 ファイルの定義を取り出す
pub fn index_file(path: &Path, parser: &mut DefParser) -> Option<((u64, u64), Vec<Entry>)> {
    let lang = Lang::detect(path)?;
    let st = stamp(path)?;
    let bytes = std::fs::read(path).ok()?;
    // 大きすぎるもの（生成物など）は入れない
    if bytes.len() > 2 << 20 {
        return Some((st, Vec::new()));
    }
    let src = String::from_utf8_lossy(&bytes);
    let defs = parser.defs(lang, &src).into_iter().map(|d| Entry { name: d.name, kind: d.kind, line: d.line as u32, col: d.col as u32 }).collect();
    Some((st, defs))
}

/// 作っている途中の知らせ
pub enum Progress {
    /// (解析したファイル, 解析が要るファイル)
    Step(usize, usize),
    Done(Box<Index>),
}

/// 索引を作る（保存したものがあれば、変わったファイルだけ解析し直す）
pub fn build(root: &Path, waker: Waker) -> Receiver<Progress> {
    let (tx, rx) = channel();
    let root = root.to_path_buf();
    let _ = std::thread::Builder::new().name("index".into()).spawn(move || {
        let cached = load_cache(&root);
        // 対応する言語のファイルを集める
        let mut paths = Vec::new();
        for e in ignore::WalkBuilder::new(&root).hidden(true).require_git(false).build().flatten() {
            if e.file_type().is_some_and(|t| t.is_file()) && Lang::detect(e.path()).is_some() {
                paths.push(e.into_path());
            }
        }
        let mut files: HashMap<Arc<str>, FileDefs> = HashMap::new();
        let mut todo = Vec::new();
        for p in paths {
            let rel: Arc<str> = p.strip_prefix(&root).unwrap_or(&p).to_string_lossy().replace('\\', "/").into();
            match (cached.get(&rel), stamp(&p)) {
                (Some(c), Some(st)) if c.stamp == st => {
                    files.insert(rel, c.clone());
                }
                _ => todo.push((rel, p)),
            }
        }
        let total = todo.len();
        let changed = total > 0 || files.len() != cached.len();
        // 解析は CPU の数だけスレッドに分ける
        let done = Arc::new(Mutex::new(Vec::with_capacity(total)));
        let queue = Arc::new(Mutex::new(todo));
        let n = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 8);
        std::thread::scope(|s| {
            for _ in 0..n {
                let (queue, done, tx, waker) = (queue.clone(), done.clone(), tx.clone(), waker.clone());
                s.spawn(move || {
                    let mut parser = DefParser::default();
                    loop {
                        let Some((rel, p)) = queue.lock().ok().and_then(|mut q| q.pop()) else { break };
                        let r = index_file(&p, &mut parser);
                        let k = {
                            let mut d = done.lock().unwrap();
                            if let Some((st, defs)) = r {
                                d.push((rel, FileDefs { stamp: st, defs }));
                            } else {
                                d.push((rel, FileDefs::default()));
                            }
                            d.len()
                        };
                        if k % 200 == 0 {
                            let _ = tx.send(Progress::Step(k, total));
                            waker();
                        }
                    }
                });
            }
        });
        for (rel, f) in Arc::try_unwrap(done).map(|m| m.into_inner().unwrap_or_default()).unwrap_or_default() {
            files.insert(rel, f);
        }
        let mut idx = Index { root, files, by_name: HashMap::new() };
        idx.rebuild();
        if changed {
            idx.save();
        }
        let _ = tx.send(Progress::Done(Box::new(idx)));
        waker();
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_save_and_incremental() {
        let dir = std::env::temp_dir().join(format!("nagi-index-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        // 索引の保存先も一時フォルダへ
        unsafe { std::env::set_var("NAGI_INDEX_DIR", dir.join("cache")) };
        std::fs::write(dir.join("src/A.java"), "class A { void run() {} }\n").unwrap();
        std::fs::write(dir.join("src/b.ts"), "export function helper() {}\nexport class B {}\n").unwrap();
        std::fs::write(dir.join("readme.md"), "# x\n").unwrap();
        let wait = |rx: Receiver<Progress>| loop {
            if let Ok(Progress::Done(i)) = rx.recv() {
                return i;
            }
        };
        let idx = wait(build(&dir, Arc::new(|| {})));
        assert_eq!(idx.file_count(), 2);
        assert_eq!(idx.lookup("run").len(), 1);
        assert_eq!(&*idx.lookup("helper")[0].path, "src/b.ts");
        assert_eq!(idx.lookup("B")[0].kind, DefKind::Class);
        // 2 回目は保存したものを使う。変えたファイルだけ解析し直す
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.join("src/b.ts"), "export function renamed() {}\n").unwrap();
        let idx = wait(build(&dir, Arc::new(|| {})));
        assert!(idx.lookup("helper").is_empty());
        assert_eq!(idx.lookup("renamed").len(), 1);
        assert_eq!(idx.lookup("run").len(), 1, "変えていないファイルは保存したものから");
        // 1 ファイルの差し替え
        let mut idx = idx;
        idx.replace_file("src/A.java", (1, 1), vec![Entry { name: "stop".into(), kind: DefKind::Method, line: 0, col: 0 }]);
        assert!(idx.lookup("run").is_empty());
        assert_eq!(idx.lookup("stop").len(), 1);
    }
}
