//! 畳める木（プロジェクト・ブランチ・データベース・Maven）の共通の状態と振る舞い。
//!
//! 行の中身の取り方は部品ごとに違う（ファイル・スキーマは後から読み込む、Maven は最初からある、ブランチは名前を / で分ける）ので、
//! 行は持ち主が作る。ここは「開いているフォルダ」（`Folds`）と、行の並びに対する ← → の動き（`left`・`right`）、
//! 名前を区切りで分けた木（`path_rows`）だけを持つ。動きは IntelliJ と同じ:
//! - ← は、開いたフォルダなら閉じる。それ以外は親の行へ移る（畳まない）
//! - → は、閉じたフォルダなら開く（開いたフォルダ・葉でどうするかは部品が決める）

use std::collections::HashSet;
use std::hash::Hash;

/// 開いているフォルダ。`default_open` なら書いていないフォルダは開いている（書いたものは閉じている）
#[derive(Clone, Debug)]
pub struct Folds<K> {
    default_open: bool,
    flipped: HashSet<K>,
}

impl<K: Hash + Eq + Clone> Folds<K> {
    /// 初めはどのフォルダも閉じている
    pub fn closed() -> Self {
        Folds { default_open: false, flipped: HashSet::new() }
    }

    /// 初めはどのフォルダも開いている
    pub fn open() -> Self {
        Folds { default_open: true, flipped: HashSet::new() }
    }

    pub fn is_open(&self, k: &K) -> bool {
        self.default_open != self.flipped.contains(k)
    }

    pub fn set_open(&mut self, k: &K, open: bool) {
        if open == self.default_open {
            self.flipped.remove(k);
        } else {
            self.flipped.insert(k.clone());
        }
    }

    pub fn toggle(&mut self, k: &K) {
        let now = self.is_open(k);
        self.set_open(k, !now);
    }

    /// 既定と違う状態にしたフォルダ（閉じた木なら開いているもの。読み直すときに使う）
    pub fn flipped(&self) -> impl Iterator<Item = &K> {
        self.flipped.iter()
    }

    /// すべて既定に戻す
    pub fn clear(&mut self) {
        self.flipped.clear();
    }
}

/// 木の 1 行（字下げの段と、フォルダならその鍵）
pub trait TreeRow {
    type Key;
    fn depth(&self) -> usize;
    fn folder(&self) -> Option<&Self::Key>;
}

/// ← → で行うこと
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step<K> {
    /// フォルダを開く（true）・閉じる（false）
    Fold(K, bool),
    /// その行を選ぶ
    Select(usize),
    /// 何もしない（→ で開いたフォルダ・葉のときなど。部品が決める）
    None,
}

/// ←: 開いたフォルダなら閉じる。それ以外は親の行（手前でいちばん近い、字下げの浅い行）へ
pub fn left<R: TreeRow>(rows: &[R], sel: usize, folds: &Folds<R::Key>) -> Step<R::Key>
where
    R::Key: Hash + Eq + Clone,
{
    let Some(r) = rows.get(sel) else { return Step::None };
    if let Some(k) = r.folder().filter(|k| folds.is_open(k)) {
        return Step::Fold(k.clone(), false);
    }
    match rows[..sel].iter().rposition(|x| x.depth() < r.depth()) {
        Some(p) => Step::Select(p),
        None => Step::None,
    }
}

/// →: 閉じたフォルダなら開く
pub fn right<R: TreeRow>(rows: &[R], sel: usize, folds: &Folds<R::Key>) -> Step<R::Key>
where
    R::Key: Hash + Eq + Clone,
{
    match rows.get(sel).and_then(|r| r.folder()) {
        Some(k) if !folds.is_open(k) => Step::Fold(k.clone(), true),
        _ => Step::None,
    }
}

/// 名前を区切りで分けた木の 1 行
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathRow {
    /// フォルダ（`key` は鍵の頭と道筋。`name` はその段の名前）
    Folder { key: String, name: String, depth: usize },
    /// 葉（渡した並びの番号）
    Leaf { i: usize, depth: usize },
}

impl TreeRow for PathRow {
    type Key = String;
    fn depth(&self) -> usize {
        match self {
            PathRow::Folder { depth, .. } | PathRow::Leaf { depth, .. } => *depth,
        }
    }
    fn folder(&self) -> Option<&String> {
        match self {
            PathRow::Folder { key, .. } => Some(key),
            PathRow::Leaf { .. } => None,
        }
    }
}

/// 名前（`names`）を `sep` で分けたフォルダの木を、字下げ `depth` から並べる。フォルダは名前の順でその段の葉より先、
/// 葉は渡した順。フォルダの鍵は `key` の後ろに道筋（`ローカル/feature`・`L:feature` のように頭は持ち主が決める）。
/// 閉じたフォルダの中は出さない
pub fn path_rows(names: &[(usize, &str)], sep: char, key: &str, depth: usize, folds: &Folds<String>) -> Vec<PathRow> {
    let items: Vec<(usize, Vec<&str>)> = names.iter().map(|&(i, n)| (i, n.split(sep).collect())).collect();
    let mut out = Vec::new();
    push_level(&items, sep, key, "", 0, depth, folds, &mut out);
    out
}

#[allow(clippy::too_many_arguments)]
fn push_level(items: &[(usize, Vec<&str>)], sep: char, key: &str, path: &str, at: usize, depth: usize, folds: &Folds<String>, out: &mut Vec<PathRow>) {
    let mut folders: Vec<&str> = items.iter().filter(|(_, p)| p.len() > at + 1).map(|(_, p)| p[at]).collect();
    folders.sort_unstable();
    folders.dedup();
    for f in folders {
        let sub = if path.is_empty() { f.to_string() } else { format!("{path}{sep}{f}") };
        let k = format!("{key}{sub}");
        out.push(PathRow::Folder { key: k.clone(), name: f.to_string(), depth });
        if folds.is_open(&k) {
            let inside: Vec<(usize, Vec<&str>)> = items.iter().filter(|(_, p)| p.len() > at + 1 && p[at] == f).cloned().collect();
            push_level(&inside, sep, key, &sub, at + 1, depth + 1, folds, out);
        }
    }
    out.extend(items.iter().filter(|(_, p)| p.len() == at + 1).map(|(i, _)| PathRow::Leaf { i: *i, depth }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(rows: &[PathRow], names: &[(usize, &str)]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                PathRow::Folder { name, depth, .. } => format!("{}{name}/", " ".repeat(*depth)),
                PathRow::Leaf { i, depth } => format!("{}{}", " ".repeat(*depth), names.iter().find(|n| n.0 == *i).unwrap().1.rsplit('/').next().unwrap()),
            })
            .collect()
    }

    #[test]
    fn folds() {
        let mut f = Folds::closed();
        assert!(!f.is_open(&"a"));
        f.toggle(&"a");
        assert!(f.is_open(&"a"));
        let mut o = Folds::open();
        assert!(o.is_open(&"a"));
        o.set_open(&"a", false);
        assert_eq!((o.is_open(&"a"), o.flipped().count()), (false, 1));
        o.set_open(&"a", true);
        assert_eq!(o.flipped().count(), 0, "既定に戻したら書かない");
    }

    #[test]
    fn paths_and_keys() {
        let names = [(0, "main"), (1, "feature/a"), (2, "feature/b/c"), (3, "bugfix/x")];
        let mut f = Folds::open();
        let rows = path_rows(&names, '/', "L:", 1, &f);
        assert_eq!(show(&rows, &names), [" bugfix/", "  x", " feature/", "  b/", "   c", "  a", " main"]);
        assert_eq!(rows[2], PathRow::Folder { key: "L:feature".into(), name: "feature".into(), depth: 1 });
        assert_eq!(rows[3], PathRow::Folder { key: "L:feature/b".into(), name: "b".into(), depth: 2 });
        f.set_open(&"L:feature".to_string(), false);
        assert_eq!(show(&path_rows(&names, '/', "L:", 1, &f), &names), [" bugfix/", "  x", " feature/", " main"]);
    }

    #[test]
    fn left_right() {
        let names = [(0, "feature/a"), (1, "main")];
        let mut f = Folds::closed();
        let rows = path_rows(&names, '/', "", 0, &f);
        // 閉じたフォルダで → は開く、← は親が無いので何もしない
        assert_eq!(right(&rows, 0, &f), Step::Fold("feature".into(), true));
        assert_eq!(left(&rows, 0, &f), Step::None);
        f.set_open(&"feature".to_string(), true);
        let rows = path_rows(&names, '/', "", 0, &f);
        assert_eq!(right(&rows, 0, &f), Step::None, "開いたフォルダで → は部品が決める");
        assert_eq!(left(&rows, 0, &f), Step::Fold("feature".into(), false));
        // 葉で ← は親の行へ（畳まない）
        assert_eq!(left(&rows, 1, &f), Step::Select(0));
        assert_eq!(left(&rows, 2, &f), Step::None, "根の葉");
    }
}
