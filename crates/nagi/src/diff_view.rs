//! git の画面の 3 段目: 左右並べの差分（IntelliJ の差分の画面と同じ形）。
//!
//! 窓の領域全体に出す。左が HEAD（履歴から開いたときは 1 つ前の版）、右が作業中のファイル（またはその版）。
//! 足した・消した・変えた行を色分けし、対になる行は変わった部分を濃く塗る。作業中のファイルとの差分では
//! `u` でその変更を戻し、`s` でステージする。

use std::path::{Path, PathBuf};

use nagi_core::linediff::{Hunk, HunkKind, diff_lines, split_lines};
use nagi_vim::Key;

use crate::editor::{Editor, Mode};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RowKind {
    Eq,
    Add,
    Del,
    Mod,
}

#[derive(Clone, Debug)]
pub struct Row {
    /// (行番号 0 始まり, 本文)
    pub left: Option<(usize, String)>,
    pub right: Option<(usize, String)>,
    pub kind: RowKind,
    /// 変更のまとまりの番号
    pub hunk: Option<usize>,
}

pub struct DiffView {
    pub left_title: String,
    pub right_title: String,
    pub rows: Vec<Row>,
    pub top: usize,
    pub cursor: usize,
    /// 各まとまりの最初の行（rows の番号）
    pub hunk_rows: Vec<usize>,
    /// 作業中のファイルとの差分なら (根, 相対パス, 絶対パス)。戻す・ステージができる
    pub work: Option<(PathBuf, String, PathBuf)>,
    hunks: Vec<Hunk>,
    old_text: String,
}

/// 2 つの中身から、左右に並べた行を作る
pub fn build_rows(old: &str, new: &str) -> (Vec<Row>, Vec<usize>, Vec<Hunk>) {
    let (a, b) = (split_lines(old), split_lines(new));
    let hunks = diff_lines(&a, &b);
    let mut rows = Vec::new();
    let mut starts = Vec::new();
    let (mut i, mut j) = (0, 0);
    let eq = |rows: &mut Vec<Row>, i: &mut usize, j: &mut usize, until_i: usize| {
        while *i < until_i {
            rows.push(Row { left: Some((*i, a[*i].to_string())), right: Some((*j, b[*j].to_string())), kind: RowKind::Eq, hunk: None });
            *i += 1;
            *j += 1;
        }
    };
    for (k, h) in hunks.iter().enumerate() {
        eq(&mut rows, &mut i, &mut j, h.old.start);
        starts.push(rows.len());
        let (lo, ln) = (h.old.len(), h.new.len());
        let kind = match h.kind {
            HunkKind::Add => RowKind::Add,
            HunkKind::Del => RowKind::Del,
            HunkKind::Mod => RowKind::Mod,
        };
        for r in 0..lo.max(ln) {
            let left = (r < lo).then(|| (h.old.start + r, a[h.old.start + r].to_string()));
            let right = (r < ln).then(|| (h.new.start + r, b[h.new.start + r].to_string()));
            // 変えたまとまりで片方しか無い行は、足した・消した行として塗る
            let kind = match (kind, &left, &right) {
                (RowKind::Mod, None, _) => RowKind::Add,
                (RowKind::Mod, _, None) => RowKind::Del,
                (k, _, _) => k,
            };
            rows.push(Row { left, right, kind, hunk: Some(k) });
        }
        i = h.old.end;
        j = h.new.end;
    }
    eq(&mut rows, &mut i, &mut j, a.len());
    (rows, starts, hunks)
}

impl Editor {
    /// 作業中のファイル `rel` と HEAD の差分を開く（コミットのパネルの d、:Gdiff）
    pub(crate) fn open_file_diff(&mut self, root: &Path, rel: &str) {
        let path = root.join(rel);
        let old = nagi_workers::git::head_blob(root, rel).unwrap_or_default();
        // 開いているバッファがあればその中身（保存前の変更も見せる）、無ければファイル
        let new = match self.find_doc(&path) {
            Some(d) if d == self.cur_doc => self.buf.text(),
            Some(d) => self.parked_doc(d).map(|x| x.buf.text()).unwrap_or_default(),
            None => std::fs::read_to_string(&path).unwrap_or_default(),
        };
        self.show_diff(format!("HEAD  {rel}"), format!("作業中のファイル  {rel}"), &old, &new, Some((root.to_path_buf(), rel.to_string(), path)));
    }

    /// 今のファイルの差分（`:Gdiff`）
    pub(crate) fn diff_current(&mut self) {
        let (Some(root), Some(p)) = (self.git.root.clone(), self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok())) else {
            self.message = "git のリポジトリのファイルではありません".into();
            return;
        };
        let Ok(rel) = p.strip_prefix(&root) else { return };
        let rel = rel.to_string_lossy().replace('\\', "/");
        self.open_file_diff(&root, &rel);
    }

    /// 2 つの中身の差分を開く
    pub(crate) fn show_diff(&mut self, left_title: String, right_title: String, old: &str, new: &str, work: Option<(PathBuf, String, PathBuf)>) {
        let (rows, hunk_rows, hunks) = build_rows(old, new);
        if hunks.is_empty() {
            self.message = "差分はありません".into();
        }
        let cursor = hunk_rows.first().copied().unwrap_or(0);
        self.diff = Some(DiffView { left_title, right_title, rows, top: cursor.saturating_sub(5), cursor, hunk_rows, work, hunks, old_text: old.to_string() });
        self.diff_return = Some(self.mode);
        self.mode = Mode::Diff;
    }

    fn close_diff(&mut self) {
        self.diff = None;
        self.mode = match self.diff_return.take() {
            Some(m @ (Mode::List | Mode::Normal)) => m,
            _ => Mode::Normal,
        };
    }

    /// 差分の画面のキー
    pub(crate) fn diff_key(&mut self, key: Key) {
        let rows = self.screen.1.saturating_sub(2).max(1);
        let Some(d) = &mut self.diff else {
            self.mode = Mode::Normal;
            return;
        };
        let last = d.rows.len().saturating_sub(1);
        let pending = std::mem::take(&mut self.diff_pending);
        match (pending, key) {
            (Some(']'), Key::Char('c')) => {
                if let Some(&r) = d.hunk_rows.iter().find(|&&r| r > d.cursor) {
                    d.cursor = r;
                }
            }
            (Some('['), Key::Char('c')) => {
                if let Some(&r) = d.hunk_rows.iter().rev().find(|&&r| r < d.cursor) {
                    d.cursor = r;
                }
            }
            (Some('g'), Key::Char('g')) => d.cursor = 0,
            (_, Key::Char(c @ (']' | '[' | 'g'))) => self.diff_pending = Some(c),
            (_, Key::Char('j') | Key::Down) => d.cursor = (d.cursor + 1).min(last),
            (_, Key::Char('k') | Key::Up) => d.cursor = d.cursor.saturating_sub(1),
            (_, Key::Ctrl('d')) => d.cursor = (d.cursor + rows / 2).min(last),
            (_, Key::Ctrl('u')) => d.cursor = d.cursor.saturating_sub(rows / 2),
            (_, Key::Char('G')) => d.cursor = last,
            (_, Key::Char('q') | Key::Esc) => self.close_diff(),
            (_, Key::Char('u')) => self.diff_apply(false),
            (_, Key::Char('s')) => self.diff_apply(true),
            (_, Key::Enter) => {
                // その行を作業中のファイルで開く
                let target = d.rows[d.cursor].right.as_ref().map(|r| r.0).or_else(|| d.rows[..d.cursor].iter().rev().find_map(|r| r.right.as_ref().map(|x| x.0 + 1)));
                let path = d.work.as_ref().map(|w| w.2.clone());
                self.close_diff();
                self.mode = Mode::Normal;
                if let (Some(p), Some(l)) = (path, target) {
                    self.jump_to(&p, l, 0);
                }
            }
            _ => {}
        }
        if let Some(d) = &mut self.diff {
            if d.cursor < d.top {
                d.top = d.cursor;
            } else if d.cursor >= d.top + rows {
                d.top = d.cursor + 1 - rows;
            }
        }
    }

    /// カーソルの変更を戻す（`stage` ならステージ）。作業中のファイルとの差分だけ
    fn diff_apply(&mut self, stage: bool) {
        let Some(d) = &self.diff else { return };
        let Some((root, rel, path)) = d.work.clone() else {
            self.message = "この差分では戻せません（作業中のファイルとの差分だけ）".into();
            return;
        };
        let Some(k) = d.rows[d.cursor].hunk.or_else(|| d.hunk_rows.iter().rposition(|&r| r <= d.cursor)) else {
            self.message = "カーソルの行に変更はありません".into();
            return;
        };
        let h = d.hunks[k].clone();
        let old_lines: Vec<String> = split_lines(&d.old_text)[h.old.clone()].iter().map(|s| s.to_string()).collect();
        if stage {
            let new_lines: Vec<String> = d.rows.iter().filter(|r| r.hunk == Some(k)).filter_map(|r| r.right.as_ref().map(|x| x.1.clone())).collect();
            let o: Vec<&str> = old_lines.iter().map(String::as_str).collect();
            let n: Vec<&str> = new_lines.iter().map(String::as_str).collect();
            let new_file = d.old_text.is_empty();
            let patch = nagi_workers::git::hunk_patch(&rel, h.old.start, &o, h.new.start, &n, new_file);
            self.message = match nagi_workers::git::apply_cached(&root, &patch) {
                Ok(()) => "変更をステージしました".into(),
                Err(e) => format!("ステージできません: {}", e.lines().next().unwrap_or("")),
            };
            self.refresh_status();
            return;
        }
        // 戻す: ファイルをバッファで開いて、その範囲を HEAD の行に置き換える（u で取り消せる）
        let keep = self.diff.take();
        let ret = self.diff_return.take();
        self.edit_file(path.clone());
        let nl = self.buf.newline().to_string();
        let total = self.buf.line_count();
        let start = if h.new.start < total { self.buf.line_start(h.new.start) } else { self.buf.len_bytes() };
        let end = if h.new.end < total { self.buf.line_start(h.new.end) } else { self.buf.len_bytes() };
        let text: String = old_lines.iter().map(|l| format!("{l}{nl}")).collect();
        self.hist.begin();
        self.edit(start, end, &text);
        self.hist.close();
        self.message = "変更を戻しました（バッファで u で取り消し）".into();
        drop(keep);
        // 差分を作り直して開き直す
        let new = self.buf.text();
        let old = self.git.root.as_ref().and_then(|r| nagi_workers::git::head_blob(r, &rel)).unwrap_or_default();
        self.show_diff(format!("HEAD  {rel}"), format!("作業中のファイル  {rel}"), &old, &new, Some((root, rel, path)));
        self.diff_return = ret;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_align() {
        let (rows, starts, _) = build_rows("a\nb\nc\nd\n", "a\nB\nc\nx\nd\n");
        let k: Vec<RowKind> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(k, vec![RowKind::Eq, RowKind::Mod, RowKind::Eq, RowKind::Add, RowKind::Eq]);
        assert_eq!(starts, vec![1, 3]);
        assert_eq!(rows[3].left, None);
        assert_eq!(rows[3].right.as_ref().map(|r| r.0), Some(3));
        // 変えた行が増えたとき、増えた分は足した行として
        let (rows, _, _) = build_rows("a\nb\n", "a\nB1\nB2\n");
        assert_eq!(rows.iter().map(|r| r.kind).collect::<Vec<_>>(), vec![RowKind::Eq, RowKind::Mod, RowKind::Add]);
    }
}
