//! git の画面の 1 段目: 行頭の変更マーカー、`]c` `[c`、マーカーから出る差分のポップアップ、状態行のブランチ。
//!
//! status と HEAD の版は裏のスレッドで git から取り、差分（HEAD ↔ 作業中のバッファ）は描く前にバッファの版が
//! 変わったときだけ計算し直す。マーカーは HEAD と比べる（IntelliJ と同じ。ステージしても消えない）。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Sender;

use nagi_core::linediff::{Hunk, HunkKind, diff_lines, split_lines};
use nagi_workers::git::{FileState, Status};

use crate::editor::Editor;

/// これより大きなファイルは差分を出さない（打つたびに全体を比べるため）
const MAX_DIFF_BYTES: usize = 2 << 20;

#[derive(Default)]
pub struct GitState {
    pub root: Option<PathBuf>,
    pub status: Option<Status>,
    status_rx: Option<nagi_core::task::Task<nagi_core::Result<Status>>>,
    /// 管理されているファイルのあるフォルダ（根からの相対パス、/ 区切り）。木で管理対象外のフォルダを赤にする
    pub tracked_dirs: Option<HashSet<String>>,
    tracked_rx: Option<nagi_core::task::Task<HashSet<String>>>,
    /// HEAD の版の中身（絶対パス → 中身。HEAD に無ければ None）
    heads: HashMap<PathBuf, Option<Arc<String>>>,
    head_chan: Option<(Sender<(PathBuf, Option<String>)>, nagi_core::task::Task<(PathBuf, Option<String>)>)>,
    pending: HashSet<PathBuf>,
    /// 計算した差分（絶対パス → (バッファの版, まとまり)）
    hunks: HashMap<PathBuf, (u64, Vec<Hunk>)>,
    /// ポップアップで見せている変更（作業中のバッファのまとまりの番号）
    pub popup: Option<usize>,
    /// 見張っている .git のフォルダ（ターミナルなど外で git を動かしたら読み直す）
    pub git_dir: Option<PathBuf>,
    /// status を取っている間に .git が変わった（届いたらもう一度取る）
    stale: bool,
}

/// 行頭のマーカー
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Marker {
    Add,
    Mod,
    /// この行の手前で消した
    DelAbove,
}

fn abs(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

impl Editor {
    /// 作業ツリーの根を決めて、status と今のファイルの HEAD の版を取りに行く（App が表示の後に呼ぶ）
    pub fn start_git(&mut self) {
        let base = self.buf.path.as_deref().map(abs).or_else(|| Some(self.cwd.clone()));
        self.git.root = base.as_deref().and_then(nagi_workers::git::repo_root).map(|r| abs(&r));
        if self.git.root.is_some() {
            self.watch_git_dir();
            self.refresh_git();
        }
    }

    /// .git のフォルダ（HEAD・index・ORIG_HEAD などの直下のファイル）を見張る。コミット・チェックアウト・リセットで変わる。
    /// ワークツリーの .git はファイル（`gitdir: …`）なので、その先を見張る
    fn watch_git_dir(&mut self) {
        let Some(root) = &self.git.root else { return };
        let dot = root.join(".git");
        let dir = if dot.is_dir() {
            dot
        } else {
            let Some(d) = std::fs::read_to_string(&dot).ok().and_then(|s| s.trim().strip_prefix("gitdir:").map(|d| root.join(d.trim()))) else { return };
            d
        };
        if self.git.git_dir.as_ref() != Some(&dir) {
            self.watch_listing(&dir);
            self.git.git_dir = Some(dir);
        }
    }

    /// 見張っている .git の中身が変わった（`d` がそのフォルダなら読み直して true）
    pub(crate) fn on_git_dir_change(&mut self, d: &Path) -> bool {
        let n = |p: &Path| abs(p).to_string_lossy().to_lowercase();
        if !self.git.git_dir.as_deref().is_some_and(|g| n(g) == n(d)) {
            return false;
        }
        // 取っている途中なら、届いてからもう一度（1 回のコミットで知らせが何度も来る）
        if self.git.status_rx.is_some() {
            self.git.stale = true;
        } else {
            self.refresh_git();
        }
        true
    }

    /// status を取り直し、HEAD の版を捨てて取り直す（コミットやチェックアウトの後）
    pub(crate) fn refresh_git(&mut self) {
        self.git.heads.clear();
        self.git.hunks.clear();
        self.git.pending.clear();
        self.refresh_status();
    }

    /// status を取りに行っている途中か（試験で待つため）
    #[cfg(test)]
    pub(crate) fn git_status_pending(&self) -> bool {
        self.git.status_rx.is_some()
    }

    pub(crate) fn refresh_status(&mut self) {
        let Some(root) = self.git.root.clone() else { return };
        let r2 = root.clone();
        let rx = nagi_core::task::spawn("git-status", self.waker(), move |ctx| {
            ctx.send(nagi_workers::git::status(&root));
        });
        // 管理されているファイルのフォルダ（木の色分け用）
        let trx = nagi_core::task::spawn("git-tracked", self.waker(), move |ctx| {
            let root = r2;
            if let Ok(out) = nagi_workers::git::git(&root, &["ls-files", "-z"]) {
                let mut dirs = HashSet::new();
                for f in String::from_utf8_lossy(&out).split('\0') {
                    let mut p = f;
                    while let Some(i) = p.rfind('/') {
                        p = &p[..i];
                        if !dirs.insert(p.to_string()) {
                            break;
                        }
                    }
                }
                ctx.send(dirs);
            }
        });
        self.git.status_rx = Some(rx);
        self.git.tracked_rx = Some(trx);
    }

    fn request_head(&mut self, path: &Path) {
        let Some(root) = self.git.root.clone() else { return };
        let path = abs(path);
        if self.git.heads.contains_key(&path) || !self.git.pending.insert(path.clone()) {
            return;
        }
        let Ok(rel) = path.strip_prefix(&root) else { return };
        let rel = rel.to_string_lossy().replace('\\', "/");
        let tx = self.git.head_chan.get_or_insert_with(nagi_core::task::manual).0.clone();
        nagi_core::task::spawn_to("git-head", tx, self.waker(), move || (path, nagi_workers::git::head_blob(&root, &rel)));
    }

    /// 裏で取った status と HEAD の版を受け取る。変わったら true
    pub fn poll_git(&mut self) -> bool {
        let mut changed = false;
        if let Some(rx) = &self.git.status_rx {
            if let Ok(r) = rx.try_recv() {
                self.git.status_rx = None;
                if let Ok(s) = r {
                    self.git.status = Some(s);
                    // 状態が変わったので差分も計算し直す（追加したファイルは全体が新しい行）
                    self.git.hunks.clear();
                    self.sync_commit_items();
                }
                if std::mem::take(&mut self.git.stale) {
                    self.refresh_git();
                }
                changed = true;
            }
        }
        if let Some(rx) = &self.git.tracked_rx {
            if let Ok(d) = rx.try_recv() {
                self.git.tracked_dirs = Some(d);
                self.git.tracked_rx = None;
                changed = true;
            }
        }
        if let Some((_, rx)) = &self.git.head_chan {
            while let Ok((p, blob)) = rx.try_recv() {
                self.git.pending.remove(&p);
                self.git.heads.insert(p.clone(), blob.map(Arc::new));
                self.git.hunks.remove(&p);
                changed = true;
            }
        }
        changed
    }

    /// プロジェクトの木の色分け用: パスの絶対パス → 状態。フォルダは、管理されていなければ Untracked（赤）、
    /// 管理されている変更を含めば Modified（青）
    pub(crate) fn tree_git_states(&self) -> HashMap<PathBuf, FileState> {
        let mut out = HashMap::new();
        let (Some(root), Some(st)) = (self.git.root.as_ref(), self.git.status.as_ref()) else { return out };
        let sep = std::path::MAIN_SEPARATOR_STR;
        for c in &st.changes {
            let p = root.join(c.path.replace('/', sep));
            // 親のフォルダ（根の手前まで）
            let rel_dirs: Vec<&str> = c.path.match_indices('/').map(|(i, _)| &c.path[..i]).collect();
            for d in rel_dirs {
                let dp = root.join(d.replace('/', sep));
                let untracked_dir = self.git.tracked_dirs.as_ref().is_some_and(|t| !t.contains(d));
                if untracked_dir {
                    out.insert(dp, FileState::Untracked);
                } else if c.state != FileState::Untracked {
                    out.entry(dp).or_insert(FileState::Modified);
                }
            }
            out.insert(p, c.state);
        }
        out
    }

    fn file_state(&self, path: &Path) -> Option<FileState> {
        let (root, st) = (self.git.root.as_ref()?, self.git.status.as_ref()?);
        let rel = path.strip_prefix(root).ok()?.to_string_lossy().replace('\\', "/");
        st.changes.iter().find(|c| c.path == rel).map(|c| c.state)
    }

    /// バッファの差分（HEAD と比べる）。版が変わったときだけ計算し直す
    fn hunks_of(&mut self, path: &Path, buf: &nagi_core::Buffer) -> Option<Vec<Hunk>> {
        self.git.root.as_ref()?;
        let path = abs(path);
        if let Some((v, h)) = self.git.hunks.get(&path) {
            if *v == buf.version {
                return Some(h.clone());
            }
        }
        if buf.len_bytes() > MAX_DIFF_BYTES {
            return None;
        }
        let head = match self.git.heads.get(&path) {
            Some(h) => h.clone(),
            None => {
                self.request_head(&path);
                return None;
            }
        };
        let text = buf.text();
        let new = split_lines(&text);
        let hunks = match head {
            Some(h) => diff_lines(&split_lines(&h), &new),
            // HEAD に無いファイル: 追加したもの（index にある）は全体が新しい行。未追跡は何も出さない
            None if self.file_state(&path) == Some(FileState::Added) => vec![Hunk { old: 0..0, new: 0..new.len(), kind: HunkKind::Add }],
            None => Vec::new(),
        };
        self.git.hunks.insert(path, (buf.version, hunks.clone()));
        Some(hunks)
    }

    /// 作業中のバッファの差分
    pub(crate) fn current_hunks(&mut self) -> Vec<Hunk> {
        let Some(p) = self.buf.path.clone() else { return Vec::new() };
        let buf = std::mem::replace(&mut self.buf, nagi_core::Buffer::from_str(""));
        let h = self.hunks_of(&p, &buf);
        self.buf = buf;
        h.unwrap_or_default()
    }

    /// 描く前に、見えている窓のバッファの差分を計算しておく
    pub(crate) fn prepare_git(&mut self) {
        if self.git.root.is_none() {
            return;
        }
        self.current_hunks();
        let parked: Vec<usize> = (0..self.wins.len()).filter_map(|w| self.win_state(w)).map(|s| s.doc).filter(|&d| d != self.cur_doc).collect();
        for d in parked {
            let Some(doc) = self.parked_doc(d) else { continue };
            let Some(p) = doc.buf.path.clone() else { continue };
            let buf = doc.buf.snapshot();
            let mut b = buf;
            b.version = doc.buf.version;
            self.hunks_of(&p, &b);
        }
    }

    /// 描く用: `path` のバッファの行頭のマーカー（計算済みのものだけ）
    pub fn git_markers(&self, path: Option<&Path>) -> HashMap<usize, Marker> {
        let mut out = HashMap::new();
        let Some(p) = path.map(abs) else { return out };
        let Some((_, hunks)) = self.git.hunks.get(&p) else { return out };
        for h in hunks {
            match h.kind {
                HunkKind::Add => h.new.clone().for_each(|l| {
                    out.insert(l, Marker::Add);
                }),
                HunkKind::Mod => h.new.clone().for_each(|l| {
                    out.insert(l, Marker::Mod);
                }),
                HunkKind::Del => {
                    out.entry(h.new.start).or_insert(Marker::DelAbove);
                }
            }
        }
        out
    }

    /// 状態行に出すブランチ（`main ↑1 ↓2`）と変更の数
    pub fn git_branch_label(&self) -> Option<String> {
        let st = self.git.status.as_ref()?;
        let mut s = if st.branch == "(detached)" { "（HEAD 切り離し）".to_string() } else { st.branch.clone() };
        if st.ahead > 0 {
            s.push_str(&format!(" ↑{}", st.ahead));
        }
        if st.behind > 0 {
            s.push_str(&format!(" ↓{}", st.behind));
        }
        Some(s)
    }

    /// `]c` / `[c`: 次・前の変更の頭へ
    pub(crate) fn hunk_jump(&mut self, next: bool, count: usize) {
        let hunks = self.current_hunks();
        if hunks.is_empty() {
            self.notify.info("変更はありません");
            return;
        }
        let line = self.cursor.line;
        let mut target = None;
        let starts: Vec<usize> = hunks.iter().map(|h| h.new.start.min(self.buf.line_count().saturating_sub(1))).collect();
        let mut cur = line;
        for _ in 0..count.max(1) {
            let t = if next { starts.iter().copied().find(|&s| s > cur) } else { starts.iter().rev().copied().find(|&s| s < cur) };
            match t {
                Some(t) => {
                    target = Some(t);
                    cur = t;
                }
                None => break,
            }
        }
        match target {
            Some(t) => {
                self.push_jump();
                self.cursor.line = t;
                self.with_motion(|m, c| m.first_non_blank(c));
                // ポップアップを開いていれば、移った先の変更を見せる
                if self.git.popup.is_some() {
                    self.git.popup = self.hunk_at_cursor(&hunks);
                }
            }
            None => self.notify.info(if next { "これより後に変更はありません" } else { "これより前に変更はありません" }),
        }
    }

    fn hunk_at_cursor(&self, hunks: &[Hunk]) -> Option<usize> {
        let l = self.cursor.line;
        hunks.iter().position(|h| (h.kind == HunkKind::Del && h.new.start == l) || h.new.contains(&l)).or_else(|| hunks.iter().position(|h| h.kind == HunkKind::Del && h.new.start == l + 1))
    }

    /// `<leader>hp`: カーソルの変更をポップアップで見せる
    pub(crate) fn open_hunk_popup(&mut self) {
        let hunks = self.current_hunks();
        match self.hunk_at_cursor(&hunks) {
            Some(i) => self.git.popup = Some(i),
            None => self.notify.info("カーソルの行に変更はありません"),
        }
    }

    /// ポップアップを開いている間のキー。扱ったら true（ほかのキーはポップアップを閉じてから普通に処理する）
    pub(crate) fn hunk_popup_key(&mut self, key: nagi_vim::Key) -> bool {
        use nagi_vim::Key;
        if self.git.popup.is_none() || !self.pending_keys.is_empty() {
            return false;
        }
        match key {
            Key::Char('u') => self.revert_hunk(),
            Key::Char('s') => self.stage_hunk(),
            Key::Esc => self.git.popup = None,
            // ]c [c は開いたまま次の変更へ
            Key::Char(']') | Key::Char('[') => return false,
            _ => {
                self.git.popup = None;
                return false;
            }
        }
        true
    }

    /// ポップアップに出す (古い行, 新しい行, まとまりの始まりの行) を作業中のバッファについて
    pub fn popup_lines(&self) -> Option<(Vec<String>, Vec<String>, Hunk)> {
        let i = self.git.popup?;
        let p = abs(self.buf.path.as_deref()?);
        let (_, hunks) = self.git.hunks.get(&p)?;
        let h = hunks.get(i)?.clone();
        let head = self.git.heads.get(&p)?.clone();
        let old: Vec<String> = head.as_deref().map(|t| split_lines(t)[h.old.clone()].iter().map(|s| s.to_string()).collect()).unwrap_or_default();
        let new: Vec<String> = h.new.clone().map(|l| self.buf.line(l)).collect();
        Some((old, new, h))
    }

    /// カーソルの変更を HEAD の版に戻す（バッファを書き換えるので u で戻せる）
    pub(crate) fn revert_hunk(&mut self) {
        let hunks = self.current_hunks();
        let Some(i) = self.git.popup.or_else(|| self.hunk_at_cursor(&hunks)) else {
            self.notify.info("カーソルの行に変更はありません");
            return;
        };
        let Some((old, _, h)) = ({
            self.git.popup = Some(i);
            self.popup_lines()
        }) else {
            return;
        };
        let nl = self.buf.newline().to_string();
        let total = self.buf.line_count();
        let start = if h.new.start < total { self.buf.line_start(h.new.start) } else { self.buf.len_bytes() };
        let end = if h.new.end < total { self.buf.line_start(h.new.end) } else { self.buf.len_bytes() };
        let mut text: String = old.iter().map(|l| format!("{l}{nl}")).collect();
        // ファイルの最後の行（改行が無い）を置き換えるときは、末尾の改行を付けない
        if end == self.buf.len_bytes() && !self.buf.slice_string(start, end).ends_with('\n') && text.ends_with(&nl) && h.new.end >= total {
            text.truncate(text.len() - nl.len());
        }
        self.hist.begin();
        self.edit(start, end, &text);
        self.hist.close();
        self.cursor.line = h.new.start.min(self.buf.line_count().saturating_sub(1));
        self.with_motion(|m, c| m.first_non_blank(c));
        self.git.popup = None;
        self.notify.info("変更を戻しました（u で取り消し）");
    }

    /// カーソルの変更だけをステージする（index にパッチを当てる）
    pub(crate) fn stage_hunk(&mut self) {
        let hunks = self.current_hunks();
        let Some(i) = self.git.popup.or_else(|| self.hunk_at_cursor(&hunks)) else {
            self.notify.info("カーソルの行に変更はありません");
            return;
        };
        self.git.popup = Some(i);
        let (Some((old, new, h)), Some(root), Some(path)) = (self.popup_lines(), self.git.root.clone(), self.buf.path.as_deref().map(abs)) else { return };
        let Ok(rel) = path.strip_prefix(&root) else { return };
        let rel = rel.to_string_lossy().replace('\\', "/");
        let new_file = self.git.heads.get(&path).is_some_and(Option::is_none);
        let o: Vec<&str> = old.iter().map(String::as_str).collect();
        let n: Vec<&str> = new.iter().map(String::as_str).collect();
        let patch = nagi_workers::git::hunk_patch(&rel, h.old.start, &o, h.new.start, &n, new_file);
        match nagi_workers::git::apply_cached(&root, &patch) {
            Ok(()) => self.notify.info(format!("変更をステージしました（{} 行）", h.new.len().max(h.old.len()))),
            Err(e) => self.notify.error(format!("ステージできません: {}", e.first_line())),
        }
        self.git.popup = None;
        self.refresh_status();
    }
}



/// カーソルの変更の差分の小窓（layout.rs の View）
pub struct HunkPopupView;

impl crate::layout::View for HunkPopupView {
    fn rect(&self, ed: &crate::editor::Editor, s: crate::layout::Screen) -> Option<crate::panes::Rect> {
        let _ = s;
        (ed.git.popup.is_some() && ed.idle()).then_some(crate::panes::Rect { x: 0, y: 0, w: s.cols, h: s.rows })
    }

    fn draw(&self, ed: &crate::editor::Editor, p: &mut crate::view::Painter, r: crate::panes::Rect, s: crate::layout::Screen) {
        let _ = (r, s);
        draw::draw_hunk_popup(p, ed, s.rows, s.cols);
    }

    fn context(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, row: usize, col: usize) -> bool {
        use nagi_vim::Key::*;
        let _ = (r, row, col);
        let m = crate::menu::Menu::new(row, col)
            .keys("前の変更", &[Char('['), Char('c')])
            .keys("次の変更", &[Char(']'), Char('c')])
            .sep()
            .item("この変更をステージ", |e| e.stage_hunk())
            .item("この変更を戻す", |e| e.revert_hunk())
            .item("閉じる", |e| e.git.popup = None);
        ed.open_menu(m);
        true
    }
}

/// 描き方（view.rs の Painter で描く）
pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    /// 行頭のマーカーから出る差分のポップアップ（変更のまとまりの下。古い行は赤、新しい行は緑の地、変わった部分を濃く）
    pub(crate) fn draw_hunk_popup(p: &mut Painter, ed: &Editor, rows: usize, cols: usize) {
        const MAX_LINES: usize = 12;
        let Some((old, new, h)) = ed.popup_lines() else { return };
        let cell = p.fonts.cell();
        let rect = ed.active_rect();
        let last = if new.is_empty() { h.new.start.saturating_sub(1) } else { h.new.end - 1 };
        if last < ed.top {
            return;
        }
        let below = rect.y + (last - ed.top) + 1;
        let x0 = rect.x + ed.gutter() - 1;
        let w = rect.w.saturating_sub(ed.gutter()).min(110).max(40).min(cols.saturating_sub(x0));
        let n_old = old.len().min(MAX_LINES);
        let n_new = new.len().min(MAX_LINES);
        let h_rows = 1 + n_old + n_new;
        // 下に入らなければ、まとまりの上に出す
        let top = if below + h_rows <= rows { below } else { (rect.y + h.new.start.saturating_sub(ed.top)).saturating_sub(h_rows) };
        let (x, y0) = (x0 as f32 * cell.w, top as f32 * cell.h);
        popup_frame(p, x, y0, w as f32 * cell.w, h_rows as f32 * cell.h);
        let saved = p.cols;
        p.cols = x0 + w;
        let total = ed.git.popup.map(|i| i + 1).unwrap_or(0);
        let kind = match h.kind {
            nagi_core::linediff::HunkKind::Add => format!("{} 行を足した", new.len()),
            nagi_core::linediff::HunkKind::Del => format!("{} 行を消した", old.len()),
            nagi_core::linediff::HunkKind::Mod => format!("{} 行を変えた", new.len().max(old.len())),
        };
        p.text(&format!(" 変更 {total}   {kind}     [c 前  ]c 次  u 戻す  s ステージ  Esc 閉じる"), x0, y0, STATUS_FG);
        let mut y = y0 + cell.h;
        // 対になる行は変わった部分を濃く塗る
        for (i, l) in old.iter().take(n_old).enumerate() {
            p.out.push(Instance::fill(x, y, w as f32 * cell.w, cell.h, DIFF_DEL_BG));
            let span = new.get(i).map(|n| nagi_core::linediff::changed_span(l, n).0);
            draw_diff_line(p, "−", l, span, DIFF_WORD_DEL_BG, GIT_DEL, x0, y);
            y += cell.h;
        }
        for (i, l) in new.iter().take(n_new).enumerate() {
            p.out.push(Instance::fill(x, y, w as f32 * cell.w, cell.h, DIFF_ADD_BG));
            let span = old.get(i).map(|o| nagi_core::linediff::changed_span(o, l).1);
            draw_diff_line(p, "+", l, span, DIFF_WORD_BG, GIT_ADD, x0, y);
            y += cell.h;
        }
        p.cols = saved;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    fn keys(e: &mut Editor, k: &str) {
        for key in nagi_vim::parse_keys(k, " ") {
            e.key(key);
        }
    }

    fn settle(e: &mut Editor) {
        // git を何度も起こすので、ほかの試験と並ぶと 2 秒を超えることがある（10 秒まで待つ）
        for _ in 0..2000 {
            e.poll_git();
            e.prepare_git();
            if e.git.status.is_some() && e.git.pending.is_empty() && e.git.status_rx.is_none() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("git の結果が届かない");
    }

    #[test]
    fn markers_jump_revert_stage() {
        let dir = std::env::temp_dir().join(format!("nagi-gitui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| nagi_workers::git::git(&dir, args).unwrap();
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "test@example.com"]);
        g(&["config", "user.name", "test"]);
        std::fs::write(dir.join("a.txt"), "1\n2\n3\n4\n5\n6\n").unwrap();
        g(&["add", "a.txt"]);
        g(&["commit", "-q", "-m", "first"]);
        // 2 行目を変え、4 行目の後に足し、6 行目を消す
        std::fs::write(dir.join("a.txt"), "1\nTWO\n3\n4\nnew\n5\n").unwrap();
        let mut buf = Buffer::open(&dir.join("a.txt")).unwrap();
        buf.path = Some(dir.join("a.txt"));
        let mut e = Editor::new(buf, None);
        e.rows = 20;
        e.start_git();
        settle(&mut e);
        assert_eq!(e.git_branch_label().as_deref(), Some("main"));
        let m = e.git_markers(e.buf.path.as_deref());
        assert_eq!((m.get(&1), m.get(&4), m.get(&6)), (Some(&Marker::Mod), Some(&Marker::Add), Some(&Marker::DelAbove)));
        // ]c で次の変更へ
        keys(&mut e, "]c");
        assert_eq!(e.cursor.line, 1);
        keys(&mut e, "]c");
        assert_eq!(e.cursor.line, 4);
        keys(&mut e, "[c");
        assert_eq!(e.cursor.line, 1);
        // ポップアップを開いて、2 行目の変更をステージ
        e.open_hunk_popup();
        let (old, new, _) = e.popup_lines().unwrap();
        assert_eq!((old, new), (vec!["2".to_string()], vec!["TWO".to_string()]));
        keys(&mut e, "s");
        assert!(e.notify.text.starts_with("変更をステージしました"), "{}", e.notify.text);
        assert!(String::from_utf8_lossy(&g(&["diff", "--cached"])).contains("+TWO"));
        // 足した行を戻す（バッファから消える）
        keys(&mut e, "5G");
        e.open_hunk_popup();
        keys(&mut e, "u");
        assert_eq!(e.buf.text(), "1\nTWO\n3\n4\n5\n");
        keys(&mut e, "u");
        assert_eq!(e.buf.text(), "1\nTWO\n3\n4\nnew\n5\n", "u で取り消せる");
    }

    /// 木の色分け: ファイルの状態と、管理されている変更を含むフォルダ（管理対象外だけのフォルダは含めない）
    #[test]
    fn tree_states() {
        use nagi_workers::git::{Change, Status};
        let mut e = Editor::new(nagi_core::Buffer::from_str(""), None);
        let root = std::env::temp_dir().join("nagi-tree-git");
        e.git.root = Some(root.clone());
        let ch = |p: &str, state| Change { path: p.into(), state, staged: false, unstaged: true, orig: None };
        e.git.status = Some(Status { changes: vec![ch("src/a/x.rs", FileState::Modified), ch("src/new.rs", FileState::Added), ch("tmp/junk.txt", FileState::Untracked)], ..Default::default() });
        e.git.tracked_dirs = Some(["src".to_string(), "src/a".to_string(), "docs".to_string()].into());
        let s = e.tree_git_states();
        let j = |p: &str| root.join(p.replace('/', std::path::MAIN_SEPARATOR_STR));
        assert_eq!(s.get(&j("src/a/x.rs")), Some(&FileState::Modified));
        assert_eq!(s.get(&j("src/new.rs")), Some(&FileState::Added));
        assert_eq!(s.get(&j("tmp/junk.txt")), Some(&FileState::Untracked));
        assert_eq!(s.get(&j("src/a")), Some(&FileState::Modified), "変更を含むフォルダは青");
        assert_eq!(s.get(&j("src")), Some(&FileState::Modified));
        assert_eq!(s.get(&j("tmp")), Some(&FileState::Untracked), "管理されていないフォルダは赤");
        assert_eq!(s.get(&j("docs")), None);
        assert_eq!(s.get(&root), None);
    }
}
