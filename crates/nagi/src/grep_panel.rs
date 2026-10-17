//! 全文検索のパネル（下にドッキング）。最下行で検索語を打つたびに探し直し、結果を `path:行: 本文` の形で並べる。
//!
//! パネルに移ると共通の「リストモード」で操作する（architecture.md §7.1）: `j/k` `gg/G` `Ctrl-d/u` で選び、
//! Enter で開いて編集画面へ、`o` はパネルに残ったまま開く、`/` で検索語を直す、Esc で編集画面へ、`q` で閉じる。

use std::path::PathBuf;

use nagi_vim::Key;
use nagi_workers::grep::{GrepJob, Hit};

use crate::editor::{Editor, Mode, Prompt};
use crate::panes::Rect;

pub struct GrepPanel {
    pub query: Prompt,
    pub regex: bool,
    pub root: PathBuf,
    /// 一致（パス, 行の順）
    pub hits: Vec<Hit>,
    pub selected: usize,
    /// 画面に出している範囲の先頭
    pub scroll: usize,
    job: Option<GrepJob>,
    /// 正規表現の誤り
    pub error: Option<String>,
    /// 全文検索以外の結果（診断など）の見出し。None なら全文検索
    pub title: Option<String>,
    /// 結果を出しているコマンドが動いている（診断）
    pub running: bool,
    /// 選びを手で動かしたか（動かしていなければ、結果が届いても先頭を選んだまま）
    moved: bool,
    /// リストモードで `g` を 1 回押した（`gg` の途中）
    pending_g: bool,
    /// リストモードで `Ctrl-w` を押した（窓の操作の途中）
    pending_w: bool,
}

/// 最下行の入力欄の前の印
pub const GREP_LEAD: &str = "検索> ";

impl GrepPanel {
    pub fn searching(&self) -> bool {
        self.job.is_some() || self.running
    }

    /// 結果のファイルの数
    pub fn files(&self) -> usize {
        let mut n = 0;
        let mut last: Option<&str> = None;
        for h in &self.hits {
            if last != Some(h.path.as_str()) {
                n += 1;
                last = Some(&h.path);
            }
        }
        n
    }

    fn select(&mut self, i: usize) {
        self.selected = i.min(self.hits.len().saturating_sub(1));
        self.moved = true;
    }
}

/// パネルの高さ（画面の本文の 35%、6〜画面 - 4 行）
pub fn panel_height(rows: usize) -> usize {
    (rows * 35 / 100).max(6).min(rows.saturating_sub(4)).max(1)
}

impl Editor {
    /// パネルの矩形（開いていなければ None）。1 行目は見出し
    pub fn panel_rect(&self) -> Option<Rect> {
        if !self.bottom_visible() {
            return None;
        }
        let (cols, rows) = self.screen;
        let h = self.bottom_height();
        Some(Rect { x: 0, y: rows - h, w: cols, h })
    }

    /// コミットのパネルの矩形（左の欄。1 行目はタブ）
    pub fn commit_rect(&self) -> Option<Rect> {
        (self.left_visible()? == crate::tree_panel::LeftTab::Commit).then(|| self.left_rect()).flatten()
    }

    /// 窓に使える行数（下のパネルの分を除く）
    pub(crate) fn windows_rows(&self) -> usize {
        let rows = self.screen.1;
        if self.bottom_visible() {
            rows - self.bottom_height()
        } else {
            rows
        }
    }

    /// `:Grep [語]` / FindInPath。最下行で検索語を打つ（前の検索語を残しておく）
    pub(crate) fn open_grep(&mut self, initial: &str) {
        let root = self
            .buf
            .path
            .as_deref()
            .and_then(|p| std::path::absolute(p).ok())
            .and_then(|p| nagi_workers::files::project_root(&p))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let root = std::path::absolute(&root).unwrap_or(root);
        let g = self.grep.get_or_insert_with(|| GrepPanel {
            query: Prompt::default(),
            regex: false,
            root: root.clone(),
            hits: Vec::new(),
            selected: 0,
            scroll: 0,
            job: None,
            error: None,
            title: None,
            running: false,
            moved: false,
            pending_g: false,
            pending_w: false,
        });
        self.bottom_hidden = false;
        g.root = root;
        // 語を付けて呼んだら（`:Grep 語`）探し始めて編集画面へ戻る（Vim の :grep と同じ）。付けなければ最下行で打つ
        self.mode = if initial.is_empty() { Mode::GrepPrompt } else { Mode::Normal };
        if !initial.is_empty() {
            g.query.set(initial);
        }
        self.message.clear();
        self.restart_grep();
        self.rows = self.text_rows(self.active_rect());
    }

    /// 正規表現で探し始める（参照の検索 gr）。パネルに結果を出し、編集画面に戻る
    pub(crate) fn open_grep_regex(&mut self, pattern: &str) {
        self.open_grep("");
        if let Some(g) = &mut self.grep {
            g.regex = true;
            g.query.set(pattern);
        }
        self.mode = Mode::Normal;
        self.restart_grep();
    }

    fn restart_grep(&mut self) {
        let waker = self.waker.clone().unwrap_or_else(|| std::sync::Arc::new(|| {}));
        let Some(g) = &mut self.grep else { return };
        g.job = None;
        g.hits.clear();
        g.selected = 0;
        g.scroll = 0;
        g.moved = false;
        g.error = None;
        g.title = None;
        g.running = false;
        if g.query.text.is_empty() {
            return;
        }
        match nagi_workers::grep::search(&g.root, &g.query.text, g.regex, waker) {
            Ok(job) => g.job = Some(job),
            Err(e) => g.error = Some(e),
        }
    }

    /// 届いた結果を受け取る。変わったら true（描き直す）
    pub fn poll_grep(&mut self) -> bool {
        let Some(g) = &mut self.grep else { return false };
        let Some(job) = &g.job else { return false };
        let mut changed = false;
        loop {
            match job.rx.try_recv() {
                Ok(batch) => {
                    // 1 ファイル分ずつ届くので、パスの順の位置へまとめて入れる（届く順は決まらないが、並びは毎回同じになる）
                    let at = g.hits.partition_point(|h| h.path < batch[0].path);
                    if g.moved && at <= g.selected && !g.hits.is_empty() {
                        g.selected += batch.len();
                    }
                    g.hits.splice(at..at, batch);
                    changed = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    g.job = None;
                    changed = true;
                    break;
                }
            }
        }
        changed
    }

    /// 最下行で検索語を打っている間のキー
    pub(crate) fn grep_prompt_key(&mut self, key: Key) {
        let Some(g) = &mut self.grep else {
            self.mode = Mode::Normal;
            return;
        };
        match key {
            // 結果があればパネルは残す（リストモードへは Ctrl-w j かクリックで）
            Key::Esc | Key::Ctrl('c') | Key::Ctrl('[') => {
                self.mode = Mode::Normal;
                if g.hits.is_empty() && g.job.is_none() {
                    self.close_grep();
                }
            }
            Key::Enter => {
                self.mode = Mode::Normal;
                self.open_hit(false);
            }
            Key::Down | Key::Ctrl('n') | Key::Ctrl('j') | Key::Tab => {
                let i = g.selected + 1;
                g.select(i);
            }
            Key::Up | Key::Ctrl('p') | Key::Ctrl('k') => {
                let i = g.selected.saturating_sub(1);
                g.select(i);
            }
            Key::Alt('r') => {
                g.regex = !g.regex;
                self.restart_grep();
            }
            k => {
                if g.query.key(k) {
                    self.restart_grep();
                }
            }
        }
    }

    pub(crate) fn grep_insert(&mut self, text: &str) {
        if let Some(g) = &mut self.grep {
            g.query.insert(text);
            self.restart_grep();
        }
    }

    /// パネルにいる間（リストモード）のキー
    pub(crate) fn list_key(&mut self, key: Key) -> crate::editor::Effect {
        let half = (self.panel_rect().map_or(10, |r| r.h) / 2).max(1);
        let Some(g) = &mut self.grep else {
            self.mode = Mode::Normal;
            return crate::editor::Effect::None;
        };
        let g_pending = std::mem::take(&mut g.pending_g);
        if std::mem::take(&mut g.pending_w) {
            // Ctrl-w の続き: 上・左右・直前の窓へは編集画面へ戻る。c / q はパネルを閉じる
            match key {
                Key::Char('c' | 'q') | Key::Ctrl('c' | 'q') => self.close_grep(),
                Key::Char('j') | Key::Ctrl('j') => {}
                Key::Char('h') | Key::Ctrl('h') if self.tree.is_some() => self.open_tree(),
                _ => self.mode = Mode::Normal,
            }
            return crate::editor::Effect::None;
        }
        match key {
            Key::Ctrl('w') => g.pending_w = true,
            Key::Char('j') | Key::Down => {
                let i = g.selected + 1;
                g.select(i);
            }
            Key::Char('k') | Key::Up => {
                let i = g.selected.saturating_sub(1);
                g.select(i);
            }
            Key::Char('g') if g_pending => g.select(0),
            Key::Char('g') => g.pending_g = true,
            Key::Char('G') => {
                let i = g.hits.len();
                g.select(i);
            }
            Key::Ctrl('d') => {
                let i = g.selected + half;
                g.select(i);
            }
            Key::Ctrl('u') => {
                let i = g.selected.saturating_sub(half);
                g.select(i);
            }
            Key::Enter => {
                self.mode = Mode::Normal;
                self.open_hit(false);
            }
            Key::Char('o') => self.open_hit(true),
            Key::Char('/') => self.mode = Mode::GrepPrompt,
            Key::Char('q') => self.close_grep(),
            Key::Esc => self.mode = Mode::Normal,
            Key::Char(':') => {
                self.mode = Mode::Command;
                self.cmdline = Default::default();
            }
            _ => {}
        }
        crate::editor::Effect::None
    }

    /// パネルへ移る（リストモード）
    pub(crate) fn focus_panel(&mut self) {
        if self.grep.is_some() {
            self.bottom_hidden = false;
            self.list_side = crate::editor::Side::Bottom;
            self.mode = Mode::List;
            self.pending_keys.clear();
        }
    }

    pub(crate) fn close_grep(&mut self) {
        self.grep = None;
        self.bottom_hidden = false;
        if self.mode == Mode::GrepPrompt || (self.mode == Mode::List && self.list_side == crate::editor::Side::Bottom) {
            self.mode = Mode::Normal;
        }
        self.rows = self.text_rows(self.active_rect());
    }

    /// 選んだ一致を作業中の窓で開く。`stay` ならリストモードのまま
    pub(crate) fn open_hit(&mut self, stay: bool) {
        let Some(g) = &self.grep else { return };
        let Some(h) = g.hits.get(g.selected).cloned() else { return };
        let path = g.root.join(&h.path);
        self.edit_file(path);
        let line = (h.line - 1).min(self.buf.line_count().saturating_sub(1));
        let text = self.buf.line(line);
        let lead = text.len() - text.trim_start().len();
        self.cursor.line = line;
        self.cursor.byte = (lead + h.range.0).min(text.len());
        self.cursor.want_col = nagi_core::display_col(&text, self.cursor.byte, false);
        self.clamp_normal();
        // 画面の中ほどに出す
        self.top = line.saturating_sub(self.rows / 2);
        if stay {
            self.mode = Mode::List;
        }
    }

    /// `:cn` / `:cp`
    pub(crate) fn next_hit(&mut self, forward: bool, count: usize) {
        let Some(g) = &mut self.grep else {
            self.message = "E42: 検索の結果がありません".into();
            return;
        };
        if g.hits.is_empty() {
            self.message = "E42: 検索の結果がありません".into();
            return;
        }
        let i = if forward { g.selected + count } else { g.selected.saturating_sub(count) };
        if (forward && g.selected + 1 >= g.hits.len()) || (!forward && g.selected == 0) {
            self.message = "E553: これ以上の結果はありません".into();
            return;
        }
        g.select(i);
        let mode = self.mode;
        self.open_hit(false);
        self.mode = if mode == Mode::List { Mode::List } else { Mode::Normal };
        let g = self.grep.as_ref().unwrap();
        self.message = format!("({} / {}) {}:{}", g.selected + 1, g.hits.len(), g.hits[g.selected].path, g.hits[g.selected].line);
    }

    /// パネルの中のクリック。`row` はパネルの中の行（0 は見出し）
    pub fn panel_click(&mut self, row: usize, clicks: u32) {
        self.list_side = crate::editor::Side::Bottom;
        let Some(g) = &mut self.grep else { return };
        if row == 0 {
            self.focus_panel();
            return;
        }
        let i = g.scroll + row - 1;
        if i < g.hits.len() {
            g.select(i);
        }
        if clicks >= 2 {
            self.mode = Mode::Normal;
            self.open_hit(false);
        } else {
            self.focus_panel();
        }
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

    fn wait(e: &mut Editor) {
        for _ in 0..1000 {
            e.poll_grep();
            if e.grep.as_ref().is_some_and(|g| !g.searching()) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("検索が終わらない");
    }

    #[test]
    fn grep_open_and_list_mode() {
        let dir = std::env::temp_dir().join(format!("nagi-grep-panel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn a() {}\n    let needle = 1;\n").unwrap();
        std::fs::write(dir.join("src/b.rs"), "// Needle here\nx\nneedle again\n").unwrap();
        let mut e = Editor::new(Buffer::open(&dir.join("src/a.rs")).unwrap(), None);
        e.buf.path = Some(dir.join("src/a.rs"));
        e.set_screen(100, 40);
        e.open_grep("");
        assert_eq!(e.mode, Mode::GrepPrompt);
        assert!(e.windows_rows() < 40, "パネルの分だけ窓が縮む");
        keys(&mut e, "needle");
        wait(&mut e);
        let g = e.grep.as_ref().unwrap();
        assert_eq!(g.hits.iter().map(|h| (h.path.as_str(), h.line)).collect::<Vec<_>>(), vec![("src/a.rs", 2), ("src/b.rs", 1), ("src/b.rs", 3)]);
        // Enter で先頭を開く。行頭の空白の後の一致の位置へ
        keys(&mut e, "<CR>");
        assert_eq!(e.mode, Mode::Normal);
        assert_eq!((e.cursor.line, e.cursor.byte), (1, 8));
        assert!(e.grep.is_some(), "パネルは残る");
        // :cn で次へ（別のファイル）
        keys(&mut e, ":cn<CR>");
        assert_eq!(e.buf.line(0), "// Needle here");
        assert_eq!((e.cursor.line, e.cursor.byte), (0, 3));
        // Ctrl-w j でパネルへ、G で最後、Enter で開いて編集画面へ
        keys(&mut e, "<C-w>j");
        assert_eq!(e.mode, Mode::List);
        keys(&mut e, "G<CR>");
        assert_eq!(e.mode, Mode::Normal);
        assert_eq!(e.cursor.line, 2);
        // パネルで q を押すと閉じ、窓が元の大きさに戻る
        keys(&mut e, "<C-w>jq");
        assert!(e.grep.is_none());
        assert_eq!(e.windows_rows(), 40);
    }
}
