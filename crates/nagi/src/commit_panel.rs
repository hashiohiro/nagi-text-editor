//! git の画面の 2 段目: コミットのパネル（左の欄の「コミット」タブ。IntelliJ のコミットの画面と同じ形）。
//!
//! 上に変えたファイルと未追跡のファイルをチェックボックス付きで並べ（リストモード）、下にコミットのメッセージを
//! 複数行で書き、その下に amend とボタン。コミットは選んだファイルだけ（ステージの状態によらない。IntelliJ と同じ）。
//! 「コミットしてプッシュ」は、コミットの後に送るコミットの一覧を出して確かめてから送る。
//! git は裏のスレッドで動かし、終わったら UI を起こす。

use std::path::PathBuf;

use nagi_vim::Key;
use nagi_workers::git::{Change, FileState};

use crate::editor::{Editor, Effect, State, Side};

pub struct Item {
    pub change: Change,
    pub checked: bool,
}

/// 裏で動いている git の結果
enum Job {
    Commit { push: bool },
    Push,
    Revert,
}

pub struct CommitPanel {
    pub items: Vec<Item>,
    pub selected: usize,
    pub scroll: usize,
    /// コミットのメッセージ（複数行の Vim 風の入力欄。Ctrl+Enter でコミット）
    pub msg: crate::vim_field::VimField,
    pub amend: bool,
    /// 送るコミットの確認（ブランチの行き先, コミットの一覧）
    pub confirm_push: Option<(String, Vec<String>)>,
    /// 戻す前の確認（項目の番号）
    pub confirm_revert: Option<usize>,
    running: Option<(Job, nagi_core::task::Task<nagi_core::Result<String>>)>,
    pub busy_label: Option<String>,
}

impl CommitPanel {
    pub fn tracked(&self) -> usize {
        self.items.iter().filter(|i| i.change.state != FileState::Untracked).count()
    }

    pub fn checked(&self) -> Vec<&Change> {
        self.items.iter().filter(|i| i.checked).map(|i| &i.change).collect()
    }

    /// メッセージのカーソルの (行, 行の中の byte)
    pub fn cursor_pos(&self) -> (usize, usize) {
        self.msg.cursor()
    }
}

/// 左の欄の中の並び（行は矩形の上から数える）
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    /// ブランチと件数の行
    pub info: usize,
    /// ファイルの一覧の先頭と行数
    pub list: usize,
    pub list_h: usize,
    /// 「コミットメッセージ」の見出し
    pub title: usize,
    /// メッセージの欄の先頭と行数
    pub msg: usize,
    pub msg_h: usize,
    pub amend: usize,
    pub buttons: usize,
}

pub fn layout(h: usize) -> Layout {
    // 1 行目はタブ。下から ボタン・amend・欄・見出しの順に取り、残りを一覧に
    let msg_h = (h / 4).clamp(3, 8);
    let buttons = h.saturating_sub(1);
    let amend = buttons.saturating_sub(1);
    // 欄の下の枠線のぶん 1 行あける
    let msg = amend.saturating_sub(msg_h + 1);
    let title = msg.saturating_sub(1);
    let list = 2;
    let list_h = title.saturating_sub(list + 1).max(1);
    Layout { info: 1, list, list_h, title, msg, msg_h, amend, buttons }
}

/// ボタン（見出し, 横の位置, 幅, プッシュもするか）。幅 `w` の欄の右寄せ
pub fn buttons(w: usize) -> [(&'static str, usize, usize, bool); 2] {
    let main = " コミット ";
    let push = " プッシュも… ";
    let mw = nagi_core::display_col(main, main.len(), false);
    let pw = nagi_core::display_col(push, push.len(), false);
    let mx = w.saturating_sub(mw + 1);
    let px = mx.saturating_sub(pw + 1);
    [(push, px, pw, true), (main, mx, mw, false)]
}

fn run_git<F: FnOnce() -> nagi_core::Result<String> + Send + 'static>(waker: Option<std::sync::Arc<dyn Fn() + Send + Sync>>, f: F) -> nagi_core::task::Task<nagi_core::Result<String>> {
    let rx = nagi_core::task::spawn("git-job", waker.unwrap_or_else(nagi_core::task::no_waker), move |ctx| {
        ctx.send(f());
    });
    rx
}

impl Editor {
    /// `:Git`（CheckinProject / ChangesView）: コミットのパネルを開いて移る
    pub(crate) fn open_commit_panel(&mut self) {
        if self.git.root.is_none() {
            self.start_git();
        }
        if self.git.root.is_none() {
            self.notify.info("git のリポジトリではありません");
            return;
        }
        self.left_tab = crate::tree_panel::LeftTab::Commit;
        self.left_hidden = false;
        if self.commit.is_none() {
            self.commit = Some(CommitPanel {
                items: Vec::new(),
                selected: 0,
                scroll: 0,
                msg: crate::vim_field::VimField::new("", true),
                amend: false,
                confirm_push: None,
                confirm_revert: None,
                running: None,
                busy_label: None,
            });
        }
        self.sync_commit_items();
        self.refresh_status();
        self.list_side = Side::Commit;
        self.set_state(State::List);
        self.rows = self.text_rows(self.active_rect());
    }

    pub(crate) fn close_commit_panel(&mut self) {
        self.commit = None;
        if self.state() == State::CommitMsg || (self.state() == State::List && self.list_side == Side::Commit) {
            self.set_state(State::Normal);
        }
        self.rows = self.text_rows(self.active_rect());
    }

    /// status の変更を一覧に入れる（チェックは前の状態を引き継ぐ。新しいものは、追跡しているファイルならチェック）
    pub(crate) fn sync_commit_items(&mut self) {
        let (Some(p), Some(st)) = (&mut self.commit, &self.git.status) else { return };
        let old: std::collections::HashMap<String, bool> = p.items.iter().map(|i| (i.change.path.clone(), i.checked)).collect();
        let mut tracked: Vec<&Change> = st.changes.iter().filter(|c| c.state != FileState::Untracked).collect();
        let mut untracked: Vec<&Change> = st.changes.iter().filter(|c| c.state == FileState::Untracked).collect();
        tracked.sort_by(|a, b| a.path.cmp(&b.path));
        untracked.sort_by(|a, b| a.path.cmp(&b.path));
        p.items = tracked
            .into_iter()
            .chain(untracked)
            .map(|c| Item { checked: *old.get(&c.path).unwrap_or(&(c.state != FileState::Untracked)), change: c.clone() })
            .collect();
        p.selected = p.selected.min(p.items.len().saturating_sub(1));
    }

    /// 裏の git の結果を受け取る。変わったら true
    pub fn poll_commit(&mut self) -> bool {
        let Some(p) = &mut self.commit else { return false };
        let Some((_, rx)) = &p.running else { return false };
        let Ok(r) = rx.try_recv() else { return false };
        let (job, _) = p.running.take().unwrap();
        p.busy_label = None;
        match (job, r) {
            (Job::Commit { push }, Ok(out)) => {
                p.msg = crate::vim_field::VimField::new("", true);
                p.amend = false;
                self.notify.info(format!("コミットしました: {}", out.trim()));
                self.refresh_git();
                if push {
                    self.prepare_push();
                }
            }
            (Job::Push, Ok(out)) => {
                self.notify.info(format!("プッシュしました{}", if out.trim().is_empty() { String::new() } else { format!(": {}", out.trim().lines().last().unwrap_or("")) }));
                self.refresh_status();
            }
            (Job::Revert, Ok(_)) => {
                self.notify.info("変更を戻しました");
                self.refresh_git();
            }
            (_, Err(e)) => self.notify.error(format!("git: {}", e.last_line())),
        }
        true
    }

    /// コミット（`push` ならその後に送る前の確認を出す）
    pub(crate) fn commit(&mut self, push: bool) {
        let Some(root) = self.git.root.clone() else { return };
        let Some(p) = &mut self.commit else { return };
        if p.running.is_some() {
            return;
        }
        let msg = p.msg.text().trim().to_string();
        if msg.is_empty() && !p.amend {
            self.notify.info("コミットのメッセージを書いてください（c で欄へ）");
            return;
        }
        let paths: Vec<String> = p.checked().iter().flat_map(|c| [Some(c.path.clone()), c.orig.clone()]).flatten().collect();
        if paths.is_empty() && !p.amend {
            self.notify.info("コミットするファイルを選んでください（Space）");
            return;
        }
        let amend = p.amend;
        p.busy_label = Some("コミットしています…".into());
        let rx = run_git(self.waker.clone(), move || {
            use nagi_workers::git::{git, git_input};
            if !paths.is_empty() {
                let mut args = vec!["add", "-A", "--"];
                args.extend(paths.iter().map(String::as_str));
                git(&root, &args)?;
            }
            let mut args = vec!["commit", "-q", "-F", "-"];
            if amend {
                args.push("--amend");
                if msg.is_empty() {
                    args.retain(|a| *a != "-F" && *a != "-");
                    args.push("--no-edit");
                }
            }
            if !paths.is_empty() {
                args.push("--only");
                args.push("--");
                args.extend(paths.iter().map(String::as_str));
            }
            git_input(&root, &args, Some(msg.as_bytes()))?;
            let out = git(&root, &["log", "-1", "--format=%h %s"])?;
            Ok(String::from_utf8_lossy(&out).into_owned())
        });
        p.running = Some((Job::Commit { push }, rx));
    }

    /// Vcs.Push / :Gpush: コミットのパネルを開いて、送る前の確認を出す
    pub(crate) fn push_from_anywhere(&mut self) {
        self.open_commit_panel();
        if self.commit.is_some() {
            self.prepare_push();
        }
    }

    /// 送るコミットを調べて確認を出す（上流が無ければ origin に同じ名前で作る）
    fn prepare_push(&mut self) {
        let Some(root) = self.git.root.clone() else { return };
        let branch = self.git.status.as_ref().map(|s| s.branch.clone()).unwrap_or_default();
        let up = nagi_workers::git::git(&root, &["rev-parse", "--abbrev-ref", "@{u}"]).ok().map(|b| String::from_utf8_lossy(&b).trim().to_string());
        let range = if up.is_some() { "@{u}..HEAD".to_string() } else { "HEAD".to_string() };
        let log = nagi_workers::git::git(&root, &["log", "--format=%h %s", "-n", "50", &range]).map(|b| String::from_utf8_lossy(&b).lines().map(String::from).collect::<Vec<_>>()).unwrap_or_default();
        let dest = match &up {
            Some(u) => format!("{branch} → {u}"),
            None => format!("{branch} → origin/{branch}（新しく作る）"),
        };
        if let Some(p) = &mut self.commit {
            p.confirm_push = Some((dest, log));
        }
    }

    fn push(&mut self) {
        let Some(root) = self.git.root.clone() else { return };
        let Some(p) = &mut self.commit else { return };
        let new_upstream = p.confirm_push.as_ref().is_some_and(|(d, _)| d.contains("新しく作る"));
        p.confirm_push = None;
        let branch = self.git.status.as_ref().map(|s| s.branch.clone()).unwrap_or_default();
        p.busy_label = Some("プッシュしています…".into());
        let rx = run_git(self.waker.clone(), move || {
            let out = if new_upstream { nagi_workers::git::git(&root, &["push", "-u", "origin", &branch]) } else { nagi_workers::git::git(&root, &["push"]) }?;
            Ok(String::from_utf8_lossy(&out).into_owned())
        });
        p.running = Some((Job::Push, rx));
    }

    fn revert_item(&mut self, i: usize) {
        let Some(root) = self.git.root.clone() else { return };
        let Some(p) = &mut self.commit else { return };
        let Some(it) = p.items.get(i) else { return };
        let c = it.change.clone();
        if c.state == FileState::Untracked {
            self.notify.warn("未追跡のファイルは戻せません（消すならファイルツリーなどから）");
            return;
        }
        let rx = run_git(self.waker.clone(), move || {
            use nagi_workers::git::git;
            match c.state {
                // 追加したファイルは index から外す（作業ツリーのファイルは残す）
                FileState::Added => git(&root, &["rm", "-q", "--cached", "--", &c.path]),
                _ => {
                    let mut args = vec!["restore", "--staged", "--worktree", "--source=HEAD", "--"];
                    args.push(&c.path);
                    if let Some(o) = &c.orig {
                        args.push(o);
                    }
                    git(&root, &args)
                }
            }
            .map(|_| String::new())
        });
        p.running = Some((Job::Revert, rx));
    }

    /// マウス: 左の欄の中の (行, 列) を押した（行・列は矩形の中の位置）
    pub(crate) fn commit_click(&mut self, row: usize, col: usize, clicks: u32) {
        let Some(r) = self.commit_rect() else { return };
        let l = layout(r.h);
        let Some(p) = &mut self.commit else { return };
        if row >= l.list && row < l.list + l.list_h {
            // 見出しの行を含めた並びから項目を探す
            let tracked = p.tracked();
            let at = p.scroll + row - l.list;
            let item = if at == 0 {
                None
            } else if at <= tracked {
                Some(at - 1)
            } else if at == tracked + 1 {
                None
            } else {
                Some(at - 2)
            };
            self.list_side = Side::Commit;
            (self.vim, self.focus) = crate::editor::split_state(State::List);
            let Some(i) = item.filter(|&i| i < p.items.len()) else { return };
            p.selected = i;
            // チェックボックスの上なら切り替え、ダブルクリックで差分
            if (3..6).contains(&col) {
                p.items[i].checked = !p.items[i].checked;
            } else if clicks >= 2 {
                self.commit_list_key(Key::Char('d'));
            }
        } else if row >= l.msg && row < l.msg + l.msg_h {
            p.msg.reinsert();
            self.set_state(State::CommitMsg);
        } else if row == l.amend {
            p.amend = !p.amend;
        } else if row == l.buttons {
            for (_, x, w, push) in buttons(r.w) {
                if col >= x && col < x + w {
                    self.commit(push);
                }
            }
        }
    }

    /// パネルにいる間（リストモード）のキー
    pub(crate) fn commit_list_key(&mut self, key: Key) -> Effect {
        let Some(p) = &mut self.commit else {
            self.set_state(State::Normal);
            return Effect::None;
        };
        // 送る前の確認
        if p.confirm_push.is_some() {
            match key {
                Key::Enter | Key::Char('y') => self.push(),
                _ => {
                    p.confirm_push = None;
                    self.notify.info("プッシュをやめました");
                }
            }
            return Effect::None;
        }
        if let Some(i) = p.confirm_revert.take() {
            if key == Key::Char('y') {
                self.revert_item(i);
            } else {
                self.notify.info("戻すのをやめました");
            }
            return Effect::None;
        }
        let last = p.items.len().saturating_sub(1);
        let moved = crate::list::step(key, p.selected, p.items.len(), 10);
        match key {
            _ if moved.is_some() => p.selected = moved.unwrap_or(p.selected),
            Key::Char('g') => p.selected = 0,
            Key::Char(' ') => {
                if let Some(it) = p.items.get_mut(p.selected) {
                    it.checked = !it.checked;
                }
                p.selected = (p.selected + 1).min(last);
            }
            // すべての選択を切り替える
            Key::Char('a') => {
                let all = p.items.iter().all(|i| i.checked);
                p.items.iter_mut().for_each(|i| i.checked = !all);
            }
            Key::Char('A') => p.amend = !p.amend,
            Key::Char('c') | Key::Char('i') => {
                p.msg.reinsert();
                self.set_state(State::CommitMsg);
            }
            Key::Ctrl('m') => self.commit(false),
            Key::Char('P') => self.commit(true),
            Key::Char('u') => {
                if !p.items.is_empty() {
                    p.confirm_revert = Some(p.selected);
                    self.notify.info(format!("{} の変更を戻しますか（y で戻す）", p.items[p.selected].change.path));
                }
            }
            Key::Char('r') => {
                self.refresh_git();
            }
            Key::Enter | Key::Char('o') => {
                let (Some(root), Some(it)) = (self.git.root.clone(), p.items.get(p.selected)) else { return Effect::None };
                if it.change.state == FileState::Deleted {
                    self.notify.warn("消したファイルは開けません");
                    return Effect::None;
                }
                let path: PathBuf = root.join(&it.change.path);
                let stay = key == Key::Char('o');
                self.edit_file(path);
                self.set_state(if stay { State::List } else { State::Normal });
            }
            Key::Char('d') => {
                let (Some(root), Some(it)) = (self.git.root.clone(), p.items.get(p.selected)) else { return Effect::None };
                let rel = it.change.path.clone();
                self.open_file_diff(&root, &rel);
            }
            Key::Char('q') => self.close_commit_panel(),
            Key::Esc => self.set_state(State::Normal),
            Key::Ctrl('w') => self.set_state(State::Normal),
            Key::Char(':') => {
                self.enter_command("");
            }
            _ => {}
        }
        Effect::None
    }

    /// メッセージの欄にいる間のキー
    pub(crate) fn commit_msg_key(&mut self, key: Key) {
        use crate::vim_field::FieldOut;
        if self.commit.is_none() {
            self.set_state(State::Normal);
            return;
        }
        // Tab は一覧へ。ほかは Vim 風の欄（Ctrl+Enter でコミット、ノーマルの Esc で一覧へ）
        if key == Key::Tab {
            self.set_state(State::List);
            return;
        }
        match self.feed_field(key, |e| e.commit.as_mut().map(|c| &mut c.msg)) {
            FieldOut::Submit => {
                self.set_state(State::List);
                self.commit(false);
            }
            FieldOut::Cancel => self.set_state(State::List),
            _ => {}
        }
    }

    pub(crate) fn commit_msg_insert(&mut self, text: &str) {
        if let Some(p) = &mut self.commit {
            p.msg.insert_text(text);
        }
    }
}



/// この部品の入口（component.rs）
pub struct CommitListComponent;

impl crate::component::Component for CommitListComponent {
    fn status(&self, ed: &crate::editor::Editor) -> Option<crate::component::Status> {
        let _ = ed;
        crate::component::Status::new("-- コミット --", "Space 選択  d 差分  u 戻す  c メッセージ  Ctrl-Enter コミット  P プッシュも  Alt-1 プロジェクト  q 閉じる ")
    }

    fn key(&self, ed: &mut crate::editor::Editor, key: nagi_vim::Key) -> crate::editor::Effect {
        ed.commit_list_key(key)
    }
}


/// この部品の入口（component.rs）
pub struct CommitMsgComponent;

impl crate::component::Component for CommitMsgComponent {
    fn status(&self, ed: &crate::editor::Editor) -> Option<crate::component::Status> {
        let _ = ed;
        crate::component::Status::new("-- コミットメッセージ --   Esc 一覧へ   Ctrl-Enter コミット", "")
    }

    fn key(&self, ed: &mut crate::editor::Editor, key: nagi_vim::Key) -> crate::editor::Effect {
        ed.commit_msg_key(key);
        crate::editor::Effect::None
    }

    fn text(&self, ed: &mut crate::editor::Editor, text: &str) {
        ed.commit_msg_insert(text);
    }
}


/// コミットの欄（layout.rs の View）
pub struct CommitView;

impl crate::layout::View for CommitView {
    fn rect(&self, ed: &crate::editor::Editor, s: crate::layout::Screen) -> Option<crate::panes::Rect> {
        let _ = s;
        ed.commit.as_ref()?;
        ed.commit_rect()
    }

    fn draw(&self, ed: &crate::editor::Editor, p: &mut crate::view::Painter, r: crate::panes::Rect, s: crate::layout::Screen) {
        let _ = (r, s);
        if let Some(c) = &ed.commit {
            draw::draw_commit_panel(p, ed, c, r);
        }
    }

    fn click(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, row: usize, col: usize, clicks: u32) -> crate::layout::Click {
        let _ = (r, row, col, clicks);
        // 左の欄の矩形で受け取る（1 行目はタブなので LeftTabsView が先に受け取る）
        let Some(l) = ed.left_rect() else { return crate::layout::Click::Pass };
        ed.commit_click(row - l.y, col - l.x, clicks);
        crate::layout::Click::Done
    }
}

/// 描き方（view.rs の Painter で描く）
pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    /// コミットのパネル（左の欄の「コミット」タブ）。上から ブランチ、変えたファイル、メッセージの欄、amend、ボタン
    pub(crate) fn draw_commit_panel(p: &mut Painter, ed: &Editor, c: &crate::commit_panel::CommitPanel, r: Rect) {
        use nagi_workers::git::FileState;
        let cell = p.fonts.cell();
        let (x0, y0, wpx) = (r.x as f32 * cell.w, r.y as f32 * cell.h, r.w as f32 * cell.w);
        p.out.push(Instance::fill(x0, y0, wpx, r.h as f32 * cell.h, BG));
        p.out.push(Instance::fill(((r.x + r.w) as f32 + 0.5) * cell.w, y0, 1.0, r.h as f32 * cell.h, LINE_NO));
        let focused = ed.state() == State::List && ed.list_side == crate::editor::Side::Commit;
        let editing = ed.state() == State::CommitMsg;
        let l = crate::commit_panel::layout(r.h);
        let row_y = |k: usize| y0 + k as f32 * cell.h;
        let saved = p.cols;
        p.cols = r.x + r.w;
        draw_left_tabs(p, r, crate::tree_panel::LeftTab::Commit, focused || editing, "");
        // ブランチと件数（動いている git があればその名前）
        let info = format!(
            " {}   変更 {}{}",
            ed.git_branch_label().unwrap_or_default(),
            c.items.len(),
            c.busy_label.as_ref().map_or(String::new(), |b| format!("   {b}"))
        );
        p.text(&info, r.x, row_y(l.info), STATUS_FG);
        if let Some((dest, list)) = &c.confirm_push {
            // 送る前の確認（一覧の場所に出す）
            p.text(&format!(" プッシュ: {dest}"), r.x, row_y(l.list), FG);
            p.text(&format!(" 送るコミット {} 件", list.len()), r.x, row_y(l.list + 1), STATUS_FG);
            for (k, s) in list.iter().take(l.list_h.saturating_sub(3)).enumerate() {
                p.text(&format!("   {s}"), r.x, row_y(l.list + 2 + k), FG);
            }
            p.text(" Enter / y で送る   Esc でやめる", r.x, row_y(l.list + l.list_h - 1), FG);
        } else {
            // 一覧（見出しの行を含めて並べる）
            let tracked = c.tracked();
            let mut lines: Vec<Option<usize>> = vec![None];
            lines.extend((0..tracked).map(Some));
            if tracked < c.items.len() {
                lines.push(None);
                lines.extend((tracked..c.items.len()).map(Some));
            }
            for (k, li) in lines.iter().enumerate().skip(c.scroll).take(l.list_h) {
                let y = row_y(l.list + k - c.scroll);
                match li {
                    None => {
                        let label = if k == 0 { format!(" ▾ 変更 ({tracked})") } else { format!(" ▾ 未追跡 ({})", c.items.len() - tracked) };
                        p.text(&label, r.x, y, STATUS_FG);
                    }
                    Some(i) => {
                        let it = &c.items[*i];
                        let sel = *i == c.selected;
                        let reversed = sel && focused;
                        if reversed {
                            p.out.push(Instance::fill(x0, y, wpx, cell.h, FG));
                        } else if sel {
                            p.out.push(Instance::fill(x0, y, wpx, cell.h, MATCH_BG));
                        }
                        let (fg, dim) = if reversed { (BG, BG) } else { (FG, STATUS_FG) };
                        let state_color = if reversed {
                            BG
                        } else {
                            match it.change.state {
                                FileState::Added => GIT_ADD,
                                FileState::Deleted | FileState::Conflicted => GIT_DEL,
                                FileState::Untracked => STATUS_FG,
                                FileState::Renamed => rgb(0xd7af5f),
                                FileState::Modified => GIT_MOD,
                            }
                        };
                        let (dir, name) = it.change.path.rsplit_once('/').map_or(("", it.change.path.as_str()), |(d, n)| (d, n));
                        let mut c2 = p.text(&format!("   [{}] ", if it.checked { 'x' } else { ' ' }), r.x, y, fg);
                        c2 = p.text(&it.change.state.letter().to_string(), c2, y, state_color);
                        c2 = p.text(&format!(" {name}"), c2, y, fg);
                        p.text(&format!("  {dir}"), c2, y, dim);
                    }
                }
            }
            if c.items.is_empty() {
                p.text("   変更はありません", r.x, row_y(l.list + 1), STATUS_FG);
            }
        }
        // メッセージの欄
        p.out.push(Instance::fill(x0, row_y(l.title) - cell.h * 0.5, wpx, 1.0, LINE_NO));
        let checked = c.items.iter().filter(|i| i.checked).count();
        p.text(&format!(" コミットメッセージ（{checked} ファイル）"), r.x, row_y(l.title), STATUS_FG);
        let mx = r.x + 1;
        let mw = r.w.saturating_sub(2);
        let box_top = row_y(l.msg);
        let (bx, bw, bh) = (mx as f32 * cell.w - 2.0, mw as f32 * cell.w + 4.0, l.msg_h as f32 * cell.h);
        let border = if editing { FG } else { LINE_NO };
        p.out.push(Instance::fill(bx, box_top - 1.0, bw, 1.0, border));
        p.out.push(Instance::fill(bx, box_top + bh, bw, 1.0, border));
        p.out.push(Instance::fill(bx, box_top - 1.0, 1.0, bh + 2.0, border));
        p.out.push(Instance::fill(bx + bw - 1.0, box_top - 1.0, 1.0, bh + 2.0, border));
        p.cols = mx + mw;
        if c.msg.text().is_empty() && !editing {
            p.text("c で書く（1 行目が件名）", mx, box_top, STATUS_FG);
        }
        let (cl, _) = c.cursor_pos();
        let first = cl.saturating_sub(l.msg_h.saturating_sub(1));
        let n = c.msg.ed.buf.line_count();
        for k in first..(first + l.msg_h).min(n) {
            let y = box_top + (k - first) as f32 * cell.h;
            draw_vim_line(p, &c.msg, k, mx, y, &ed.preedit, editing);
        }
        p.cols = r.x + r.w;
        // amend とボタン（右寄せ）
        let ay = row_y(l.amend);
        p.text(&format!(" [{}] 直前のコミットを直す（A）", if c.amend { 'x' } else { ' ' }), r.x, ay, STATUS_FG);
        let by = row_y(l.buttons);
        for (label, x, w, push) in crate::commit_panel::buttons(r.w) {
            let bg = if push { MATCH_BG } else { FG };
            p.out.push(Instance::fill((r.x + x) as f32 * cell.w, by, w as f32 * cell.w, cell.h, bg));
            p.text(label, r.x + x, by, if push { FG } else { BG });
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

    fn wait(e: &mut Editor) {
        for _ in 0..600 {
            e.poll_git();
            e.poll_commit();
            let busy = e.commit.as_ref().is_some_and(|p| p.running.is_some()) || e.git_status_pending();
            if !busy && e.git.status.is_some() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("git が終わらない");
    }

    #[test]
    fn commit_selected_files() {
        let dir = std::env::temp_dir().join(format!("nagi-commit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| String::from_utf8_lossy(&nagi_workers::git::git(&dir, args).unwrap()).into_owned();
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "test@example.com"]);
        g(&["config", "user.name", "test"]);
        // 改行を変えない（Windows の git の既定の core.autocrlf=true だと、戻したファイルが CRLF になる）
        g(&["config", "core.autocrlf", "false"]);
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        std::fs::write(dir.join("b.txt"), "b\n").unwrap();
        g(&["add", "."]);
        g(&["commit", "-q", "-m", "first"]);
        std::fs::write(dir.join("a.txt"), "a2\n").unwrap();
        std::fs::write(dir.join("b.txt"), "b2\n").unwrap();
        std::fs::write(dir.join("new.txt"), "n\n").unwrap();
        let mut buf = Buffer::open(&dir.join("a.txt")).unwrap();
        buf.path = Some(dir.join("a.txt"));
        let mut e = Editor::new(buf, None);
        e.start_git();
        e.open_commit_panel();
        wait(&mut e);
        let p = e.commit.as_ref().unwrap();
        let v: Vec<(&str, bool)> = p.items.iter().map(|i| (i.change.path.as_str(), i.checked)).collect();
        assert_eq!(v, vec![("a.txt", true), ("b.txt", true), ("new.txt", false)], "未追跡は初めはチェックしない");
        // b を外し、new を入れて、メッセージを書いてコミット
        keys(&mut e, "j<Space><Space>");
        keys(&mut e, "cfix a<CR><CR>body<Esc>");
        assert_eq!(e.commit.as_ref().unwrap().msg.text(), "fix a\n\nbody");
        keys(&mut e, "<C-m>");
        wait(&mut e);
        wait(&mut e);
        assert!(e.notify.text.starts_with("コミットしました"), "{}", e.notify.text);
        assert_eq!(g(&["log", "-1", "--format=%B"]).trim(), "fix a\n\nbody");
        let files = g(&["show", "--name-only", "--format=", "HEAD"]);
        assert_eq!(files.trim(), "a.txt\nnew.txt");
        // b はコミットしていないので残る。一覧も作り直される
        let p = e.commit.as_ref().unwrap();
        assert_eq!(p.items.iter().map(|i| i.change.path.as_str()).collect::<Vec<_>>(), vec!["b.txt"]);
        assert!(p.msg.text().is_empty());
        // b の変更を戻す（確認してから）
        keys(&mut e, "u");
        assert!(e.notify.text.contains("y で戻す"));
        keys(&mut e, "y");
        wait(&mut e);
        wait(&mut e);
        assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "b\n");
        assert!(e.commit.as_ref().unwrap().items.is_empty());
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use crate::tree_panel::{LeftTab, stripe_at, tab_at};

    #[test]
    fn left_panel_layout() {
        // 上から タブ・ブランチ・一覧・見出し・欄・amend・ボタンの順で、重ならない
        for h in [12, 20, 40, 60] {
            let l = layout(h);
            assert!(l.list + l.list_h < l.title, "{h}: {l:?}");
            assert!(l.title < l.msg && l.msg + l.msg_h < l.amend && l.amend < l.buttons && l.buttons < h, "{h}: {l:?}");
        }
        // タブ「 プロジェクト 」は 14 セル、その右が「 コミット 」
        assert_eq!(tab_at(1), Some(LeftTab::Project));
        assert_eq!(tab_at(5), Some(LeftTab::Commit));
        assert_eq!(tab_at(8), None);
        assert_eq!((stripe_at(0), stripe_at(8), stripe_at(20)), (Some(LeftTab::Project), Some(LeftTab::Commit), None));
        // ボタンは右寄せで重ならない
        let [(_, px, pw, true), (_, mx, _, false)] = buttons(36) else { panic!() };
        assert!(px + pw < mx);
    }
}
