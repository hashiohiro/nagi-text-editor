//! git の画面の 2 段目: コミットのパネル（下にドッキング。IntelliJ のコミットの画面と同じ形）。
//!
//! 左に変えたファイルと未追跡のファイルをチェックボックス付きで並べ（リストモード）、右にコミットのメッセージを
//! 複数行で書く。コミットは選んだファイルだけ（ステージの状態によらない。IntelliJ と同じ）。
//! 「コミットしてプッシュ」は、コミットの後に送るコミットの一覧を出して確かめてから送る。
//! git は裏のスレッドで動かし、終わったら UI を起こす。

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};

use nagi_vim::Key;
use nagi_workers::git::{Change, FileState};

use crate::editor::{Editor, Effect, Mode, Side};

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
    /// コミットのメッセージ（複数行）と、その中のカーソル（byte）
    pub msg: String,
    pub cursor: usize,
    pub amend: bool,
    /// 送るコミットの確認（ブランチの行き先, コミットの一覧）
    pub confirm_push: Option<(String, Vec<String>)>,
    /// 戻す前の確認（項目の番号）
    pub confirm_revert: Option<usize>,
    running: Option<(Job, Receiver<Result<String, String>>)>,
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
        let before = &self.msg[..self.cursor];
        let line = before.matches('\n').count();
        let col = before.rfind('\n').map_or(before.len(), |i| before.len() - i - 1);
        (line, col)
    }
}

fn run_git<F: FnOnce() -> Result<String, String> + Send + 'static>(waker: Option<std::sync::Arc<dyn Fn() + Send + Sync>>, f: F) -> Receiver<Result<String, String>> {
    let (tx, rx) = channel();
    let _ = std::thread::Builder::new().name("git-job".into()).spawn(move || {
        if tx.send(f()).is_ok() {
            if let Some(w) = waker {
                w();
            }
        }
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
            self.message = "git のリポジトリではありません".into();
            return;
        }
        // 下のパネルは 1 つだけ
        self.grep = None;
        if self.commit.is_none() {
            self.commit = Some(CommitPanel {
                items: Vec::new(),
                selected: 0,
                scroll: 0,
                msg: String::new(),
                cursor: 0,
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
        self.mode = Mode::List;
        self.rows = self.text_rows(self.active_rect());
    }

    pub(crate) fn close_commit_panel(&mut self) {
        self.commit = None;
        if self.mode == Mode::CommitMsg || (self.mode == Mode::List && self.list_side == Side::Commit) {
            self.mode = Mode::Normal;
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
                p.msg.clear();
                p.cursor = 0;
                p.amend = false;
                self.message = format!("コミットしました: {}", out.trim());
                self.refresh_git();
                if push {
                    self.prepare_push();
                }
            }
            (Job::Push, Ok(out)) => {
                self.message = format!("プッシュしました{}", if out.trim().is_empty() { String::new() } else { format!(": {}", out.trim().lines().last().unwrap_or("")) });
                self.refresh_status();
            }
            (Job::Revert, Ok(_)) => {
                self.message = "変更を戻しました".into();
                self.refresh_git();
            }
            (_, Err(e)) => self.message = format!("git: {}", e.lines().last().unwrap_or("")),
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
        let msg = p.msg.trim().to_string();
        if msg.is_empty() && !p.amend {
            self.message = "コミットのメッセージを書いてください（c で欄へ）".into();
            return;
        }
        let paths: Vec<String> = p.checked().iter().flat_map(|c| [Some(c.path.clone()), c.orig.clone()]).flatten().collect();
        if paths.is_empty() && !p.amend {
            self.message = "コミットするファイルを選んでください（Space）".into();
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
            self.message = "未追跡のファイルは戻せません（消すならファイルツリーなどから）".into();
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

    /// パネルにいる間（リストモード）のキー
    pub(crate) fn commit_list_key(&mut self, key: Key) -> Effect {
        let Some(p) = &mut self.commit else {
            self.mode = Mode::Normal;
            return Effect::None;
        };
        // 送る前の確認
        if p.confirm_push.is_some() {
            match key {
                Key::Enter | Key::Char('y') => self.push(),
                _ => {
                    p.confirm_push = None;
                    self.message = "プッシュをやめました".into();
                }
            }
            return Effect::None;
        }
        if let Some(i) = p.confirm_revert.take() {
            if key == Key::Char('y') {
                self.revert_item(i);
            } else {
                self.message = "戻すのをやめました".into();
            }
            return Effect::None;
        }
        let last = p.items.len().saturating_sub(1);
        match key {
            Key::Char('j') | Key::Down => p.selected = (p.selected + 1).min(last),
            Key::Char('k') | Key::Up => p.selected = p.selected.saturating_sub(1),
            Key::Char('G') => p.selected = last,
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
            Key::Char('c') | Key::Char('i') => self.mode = Mode::CommitMsg,
            Key::Ctrl('m') => self.commit(false),
            Key::Char('P') => self.commit(true),
            Key::Char('u') => {
                if !p.items.is_empty() {
                    p.confirm_revert = Some(p.selected);
                    self.message = format!("{} の変更を戻しますか（y で戻す）", p.items[p.selected].change.path);
                }
            }
            Key::Char('r') => {
                self.refresh_git();
            }
            Key::Enter | Key::Char('o') => {
                let (Some(root), Some(it)) = (self.git.root.clone(), p.items.get(p.selected)) else { return Effect::None };
                if it.change.state == FileState::Deleted {
                    self.message = "消したファイルは開けません".into();
                    return Effect::None;
                }
                let path: PathBuf = root.join(&it.change.path);
                let stay = key == Key::Char('o');
                self.edit_file(path);
                self.mode = if stay { Mode::List } else { Mode::Normal };
            }
            Key::Char('d') => {
                let (Some(root), Some(it)) = (self.git.root.clone(), p.items.get(p.selected)) else { return Effect::None };
                let rel = it.change.path.clone();
                self.open_file_diff(&root, &rel);
            }
            Key::Char('q') => self.close_commit_panel(),
            Key::Esc => self.mode = Mode::Normal,
            Key::Ctrl('w') => self.mode = Mode::Normal,
            Key::Char(':') => {
                self.mode = Mode::Command;
                self.cmdline = Default::default();
            }
            _ => {}
        }
        Effect::None
    }

    /// メッセージの欄にいる間のキー
    pub(crate) fn commit_msg_key(&mut self, key: Key) {
        let Some(p) = &mut self.commit else {
            self.mode = Mode::Normal;
            return;
        };
        let prev = |t: &str, i: usize| t[..i].chars().next_back().map_or(0, char::len_utf8);
        match key {
            Key::Esc | Key::Tab => self.mode = Mode::List,
            Key::Ctrl('m') => {
                self.mode = Mode::List;
                self.commit(false);
            }
            Key::Enter => {
                p.msg.insert(p.cursor, '\n');
                p.cursor += 1;
            }
            Key::Char(c) => {
                p.msg.insert(p.cursor, c);
                p.cursor += c.len_utf8();
            }
            Key::Backspace if p.cursor > 0 => {
                let n = prev(&p.msg, p.cursor);
                p.cursor -= n;
                p.msg.replace_range(p.cursor..p.cursor + n, "");
            }
            Key::Left => p.cursor -= prev(&p.msg, p.cursor),
            Key::Right => p.cursor += p.msg[p.cursor..].chars().next().map_or(0, char::len_utf8),
            Key::Up | Key::Down => {
                // 前・次の行の同じ列へ
                let (line, col) = p.cursor_pos();
                let lines: Vec<&str> = p.msg.split('\n').collect();
                let to = if key == Key::Up { line.checked_sub(1) } else { Some(line + 1).filter(|&l| l < lines.len()) };
                if let Some(t) = to {
                    let start: usize = lines[..t].iter().map(|l| l.len() + 1).sum();
                    let mut c = col.min(lines[t].len());
                    while !lines[t].is_char_boundary(c) {
                        c -= 1;
                    }
                    p.cursor = start + c;
                }
            }
            Key::Home => p.cursor = p.msg[..p.cursor].rfind('\n').map_or(0, |i| i + 1),
            Key::End => p.cursor += p.msg[p.cursor..].find('\n').unwrap_or(p.msg.len() - p.cursor),
            _ => {}
        }
    }

    pub(crate) fn commit_msg_insert(&mut self, text: &str) {
        if let Some(p) = &mut self.commit {
            p.msg.insert_str(p.cursor, text);
            p.cursor += text.len();
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
        assert_eq!(e.commit.as_ref().unwrap().msg, "fix a\n\nbody");
        keys(&mut e, "<C-m>");
        wait(&mut e);
        wait(&mut e);
        assert!(e.message.starts_with("コミットしました"), "{}", e.message);
        assert_eq!(g(&["log", "-1", "--format=%B"]).trim(), "fix a\n\nbody");
        let files = g(&["show", "--name-only", "--format=", "HEAD"]);
        assert_eq!(files.trim(), "a.txt\nnew.txt");
        // b はコミットしていないので残る。一覧も作り直される
        let p = e.commit.as_ref().unwrap();
        assert_eq!(p.items.iter().map(|i| i.change.path.as_str()).collect::<Vec<_>>(), vec!["b.txt"]);
        assert!(p.msg.is_empty());
        // b の変更を戻す（確認してから）
        keys(&mut e, "u");
        assert!(e.message.contains("y で戻す"));
        keys(&mut e, "y");
        wait(&mut e);
        wait(&mut e);
        assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "b\n");
        assert!(e.commit.as_ref().unwrap().items.is_empty());
    }
}
