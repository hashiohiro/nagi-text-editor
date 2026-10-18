//! git の画面の 6 段目: ブランチのポップアップ（IntelliJ の Git Branches と同じ形）。
//!
//! 上の欄に打つと絞り込む（文字はそのまま欄に入る）。並びは 新しいブランチ… / フェッチ、最近、ローカル、リモート。
//! Enter でチェックアウト（リモートなら追跡するローカルのブランチを作る）、→ で操作の一覧。
//! フェッチと pull はネットワークを使うので裏で行う。

use std::sync::mpsc::{Receiver, channel};

use nagi_core::fuzzy::Query;
use nagi_vim::Key;

use crate::editor::{Editor, Mode, Prompt};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    pub remote: bool,
    pub current: bool,
    /// 上流との差（`[ahead 1]` など）
    pub track: String,
}

/// 一覧の 1 行
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    NewBranch,
    Fetch,
    Header(&'static str),
    Branch(usize),
}

/// 操作の一覧
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Checkout,
    NewFrom,
    Merge,
    Rebase,
    Compare,
    Pull,
    Rename,
    Delete,
}

impl Op {
    pub fn label(self, current: &str) -> String {
        match self {
            Op::Checkout => "チェックアウト".into(),
            Op::NewFrom => "ここから新しいブランチ…".into(),
            Op::Merge => format!("{current} にマージ"),
            Op::Rebase => format!("{current} をこの上へリベース"),
            Op::Compare => format!("{current} と比べる"),
            Op::Pull => "pull（取り込み）".into(),
            Op::Rename => "名前を変える…".into(),
            Op::Delete => "削除…".into(),
        }
    }
}

/// 名前を打っている途中の操作
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Naming {
    /// 新しいブランチ（起点。None は HEAD）
    New(Option<String>),
    Rename(String),
}

pub struct BranchPopup {
    pub query: Prompt,
    pub branches: Vec<Branch>,
    pub entries: Vec<Entry>,
    /// 選んでいる行（entries の番号。見出しは選ばない）
    pub selected: usize,
    /// 開いている操作の一覧と、その中で選んでいるもの
    pub ops: Option<(Vec<Op>, usize)>,
    pub naming: Option<(Naming, crate::vim_field::VimField)>,
    /// 削除の確認（名前, 強制か）
    pub confirm_delete: Option<(String, bool)>,
    rx: Option<Receiver<Result<Vec<Branch>, String>>>,
}

impl BranchPopup {
    pub fn current_name(&self) -> String {
        self.branches.iter().find(|b| b.current).map_or_else(|| "HEAD".into(), |b| b.name.clone())
    }

    /// 検索語で並べ直す
    fn rebuild(&mut self) {
        let q = Query::new(&self.query.text);
        let ok = |b: &Branch| q.is_empty() || q.score(&b.name).is_some();
        let mut e = Vec::new();
        if q.is_empty() {
            e.push(Entry::NewBranch);
            e.push(Entry::Fetch);
        }
        let local: Vec<usize> = (0..self.branches.len()).filter(|&i| !self.branches[i].remote && ok(&self.branches[i])).collect();
        let remote: Vec<usize> = (0..self.branches.len()).filter(|&i| self.branches[i].remote && ok(&self.branches[i])).collect();
        // 最近: 今のブランチと、最近コミットしたローカルの 3 つ（一覧は新しい順に並んでいる）
        let recent: Vec<usize> = local.iter().copied().filter(|&i| self.branches[i].current).chain(local.iter().copied().filter(|&i| !self.branches[i].current).take(3)).collect();
        if !recent.is_empty() && q.is_empty() {
            e.push(Entry::Header("最近"));
            e.extend(recent.iter().map(|&i| Entry::Branch(i)));
        }
        let rest: Vec<usize> = local.into_iter().filter(|i| q.is_empty().then(|| !recent.contains(i)).unwrap_or(true)).collect();
        if !rest.is_empty() {
            e.push(Entry::Header("ローカル"));
            e.extend(rest.into_iter().map(Entry::Branch));
        }
        if !remote.is_empty() {
            e.push(Entry::Header("リモート"));
            e.extend(remote.into_iter().map(Entry::Branch));
        }
        self.entries = e;
        self.selected = self.entries.iter().position(|x| !matches!(x, Entry::Header(_))).unwrap_or(0);
    }

    fn step(&mut self, d: isize) {
        let n = self.entries.len() as isize;
        let mut i = self.selected as isize;
        loop {
            i += d;
            if i < 0 || i >= n {
                return;
            }
            if !matches!(self.entries[i as usize], Entry::Header(_)) {
                self.selected = i as usize;
                return;
            }
        }
    }

    pub fn selected_branch(&self) -> Option<&Branch> {
        match self.entries.get(self.selected)? {
            Entry::Branch(i) => self.branches.get(*i),
            _ => None,
        }
    }
}

/// `git for-each-ref` を読む（新しくコミットした順）
pub fn parse_refs(out: &str) -> Vec<Branch> {
    out.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\x1f').collect();
            if f.len() < 4 || f[2].ends_with("/HEAD") {
                return None;
            }
            Some(Branch { name: f[2].to_string(), remote: f[1].starts_with("refs/remotes/"), current: f[0] == "*", track: f[3].to_string() })
        })
        .collect()
}

impl Editor {
    /// `:Gbranches`（Git.Branches）
    pub(crate) fn open_branches(&mut self) {
        if self.git.root.is_none() {
            self.start_git();
        }
        let Some(root) = self.git.root.clone() else {
            self.message = "git のリポジトリではありません".into();
            return;
        };
        let (tx, rx) = channel();
        let waker = self.waker.clone();
        let _ = std::thread::Builder::new().name("git-refs".into()).spawn(move || {
            let r = nagi_workers::git::git(&root, &["for-each-ref", "--sort=-committerdate", "--format=%(HEAD)\x1f%(refname)\x1f%(refname:short)\x1f%(upstream:track)", "refs/heads", "refs/remotes"])
                .map(|b| parse_refs(&String::from_utf8_lossy(&b)));
            if tx.send(r).is_ok() {
                if let Some(w) = waker {
                    w();
                }
            }
        });
        let mut p = BranchPopup { query: Prompt::default(), branches: Vec::new(), entries: Vec::new(), selected: 0, ops: None, naming: None, confirm_delete: None, rx: Some(rx) };
        p.rebuild();
        self.branch_popup = Some(p);
        self.mode = Mode::Branches;
    }

    fn close_branches(&mut self) {
        self.branch_popup = None;
        if self.mode == Mode::Branches {
            self.mode = Mode::Normal;
        }
    }

    /// 裏の結果を受け取る。変わったら true
    pub fn poll_branches(&mut self) -> bool {
        let Some(p) = &mut self.branch_popup else {
            return self.poll_git_job();
        };
        let mut changed = false;
        if let Some(Ok(r)) = p.rx.as_ref().map(|rx| rx.try_recv()) {
            p.rx = None;
            match r {
                Ok(b) => {
                    p.branches = b;
                    p.rebuild();
                }
                Err(e) => self.message = format!("git: {}", e.lines().last().unwrap_or("")),
            }
            changed = true;
        }
        changed | self.poll_git_job()
    }

    /// 裏で動かしたフェッチ・pull の結果
    fn poll_git_job(&mut self) -> bool {
        let Some((label, rx)) = &self.git_job else { return false };
        let Ok(r) = rx.try_recv() else { return false };
        self.message = match r {
            Ok(_) => format!("{label}しました"),
            Err(e) => format!("{label}できません: {}", e.lines().last().unwrap_or("")),
        };
        self.git_job = None;
        self.refresh_git();
        true
    }

    /// フェッチ・pull を裏で動かす
    pub(crate) fn run_git_job(&mut self, label: &str, args: &[&str]) {
        let Some(root) = self.git.root.clone() else { return };
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let (tx, rx) = channel();
        let waker = self.waker.clone();
        let _ = std::thread::Builder::new().name("git-net".into()).spawn(move || {
            let a: Vec<&str> = args.iter().map(String::as_str).collect();
            let r = nagi_workers::git::git(&root, &a).map(|b| String::from_utf8_lossy(&b).into_owned());
            if tx.send(r).is_ok() {
                if let Some(w) = waker {
                    w();
                }
            }
        });
        self.message = format!("{label}しています…");
        self.git_job = Some((label.to_string(), rx));
    }

    /// git をその場で動かして結果を知らせる（チェックアウトなど、手元だけで済むもの）
    fn git_now(&mut self, args: &[&str], ok: String) -> bool {
        let Some(root) = self.git.root.clone() else { return false };
        let r = nagi_workers::git::git(&root, args);
        self.message = match &r {
            Ok(_) => ok,
            Err(e) => format!("git: {}", e.lines().last().unwrap_or("")),
        };
        self.refresh_git();
        r.is_ok()
    }

    fn checkout_branch(&mut self, b: &Branch) {
        if b.current {
            self.message = format!("{} はもうチェックアウトしています", b.name);
            return;
        }
        let ok = if b.remote {
            // origin/x → x を作って追跡する（あればそれをチェックアウト）
            let local = b.name.split_once('/').map_or(b.name.as_str(), |(_, n)| n).to_string();
            let exists = self.branch_popup.as_ref().is_some_and(|p| p.branches.iter().any(|x| !x.remote && x.name == local));
            if exists {
                self.git_now(&["checkout", "-q", &local], format!("{local} をチェックアウトしました"))
            } else {
                self.git_now(&["checkout", "-q", "--track", &b.name], format!("{local} を作って {} を追跡します", b.name))
            }
        } else {
            self.git_now(&["checkout", "-q", &b.name], format!("{} をチェックアウトしました", b.name))
        };
        if ok {
            self.close_branches();
        }
    }

    fn run_op(&mut self, op: Op, b: Branch) {
        let cur = self.branch_popup.as_ref().map(|p| p.current_name()).unwrap_or_default();
        match op {
            Op::Checkout => self.checkout_branch(&b),
            Op::NewFrom => {
                if let Some(p) = &mut self.branch_popup {
                    p.naming = Some((Naming::New(Some(b.name.clone())), crate::vim_field::VimField::new("", false)));
                }
            }
            Op::Merge => {
                if self.git_now(&["merge", "--no-edit", &b.name], format!("{} を {cur} にマージしました", b.name)) {
                    self.close_branches();
                }
            }
            Op::Rebase => {
                if self.git_now(&["rebase", &b.name], format!("{cur} を {} の上へリベースしました", b.name)) {
                    self.close_branches();
                }
            }
            Op::Compare => {
                self.close_branches();
                self.open_log_range(&format!("{cur}..{}", b.name));
            }
            Op::Pull => {
                self.close_branches();
                self.run_git_job("pull ", &["pull", "--no-edit"]);
            }
            Op::Rename => {
                if let Some(p) = &mut self.branch_popup {
                    p.naming = Some((Naming::Rename(b.name.clone()), crate::vim_field::VimField::new(&b.name, false)));
                }
            }
            Op::Delete => {
                if let Some(p) = &mut self.branch_popup {
                    p.confirm_delete = Some((b.name.clone(), false));
                }
                self.message = format!("ブランチ {} を消しますか（y で消す）", b.name);
            }
        }
    }

    /// ポップアップのキー
    pub(crate) fn branches_key(&mut self, key: Key) {
        let Some(p) = &mut self.branch_popup else {
            self.mode = Mode::Normal;
            return;
        };
        // 削除の確認
        if let Some((name, force)) = p.confirm_delete.take() {
            if key != Key::Char('y') {
                self.message = "消すのをやめました".into();
                return;
            }
            let flag = if force { "-D" } else { "-d" };
            let root = self.git.root.clone();
            let r = root.map(|r| nagi_workers::git::git(&r, &["branch", flag, &name]));
            match r {
                Some(Ok(_)) => {
                    self.message = format!("ブランチ {name} を消しました");
                    self.open_branches();
                }
                Some(Err(e)) if !force && e.contains("not fully merged") => {
                    self.message = format!("{name} はマージしていません。それでも消しますか（y で強制的に消す）");
                    if let Some(p) = &mut self.branch_popup {
                        p.confirm_delete = Some((name, true));
                    }
                }
                Some(Err(e)) => self.message = format!("git: {}", e.lines().last().unwrap_or("")),
                None => {}
            }
            return;
        }
        // 名前を打っている
        if p.naming.is_some() {
            use crate::vim_field::FieldOut;
            let out = self.feed_field(key, |e| e.branch_popup.as_mut().and_then(|p| p.naming.as_mut()).map(|n| &mut n.1));
            let Some(p) = &mut self.branch_popup else { return };
            match out {
                FieldOut::Cancel => p.naming = None,
                FieldOut::Submit => {
                    let (kind, f) = p.naming.take().unwrap();
                    let name = f.text().trim().to_string();
                    if name.is_empty() {
                        return;
                    }
                    let done = match kind {
                        Naming::New(from) => {
                            let mut args = vec!["checkout", "-q", "-b", name.as_str()];
                            if let Some(f) = from.as_deref() {
                                args.push(f);
                            }
                            self.git_now(&args, format!("ブランチ {name} を作ってチェックアウトしました"))
                        }
                        Naming::Rename(old) => self.git_now(&["branch", "-m", &old, &name], format!("{old} を {name} に変えました")),
                    };
                    if done {
                        self.close_branches();
                    }
                }
                _ => {}
            }
            return;
        }
        // 操作の一覧
        if let Some((ops, sel)) = &mut p.ops {
            let moved = crate::list::step_typing(key, *sel, ops.len());
            match key {
                _ if moved.is_some() => *sel = moved.unwrap_or(*sel),
                Key::Left | Key::Esc | Key::Ctrl('h') => p.ops = None,
                Key::Enter | Key::Right => {
                    let op = ops[*sel];
                    p.ops = None;
                    if let Some(b) = p.selected_branch().cloned() {
                        self.run_op(op, b);
                    }
                }
                _ => {}
            }
            return;
        }
        match key {
            Key::Esc => self.close_branches(),
            Key::Down | Key::Ctrl('n') | Key::Tab => p.step(1),
            Key::Up | Key::Ctrl('p') => p.step(-1),
            Key::Right | Key::Ctrl('l') => {
                if let Some(b) = p.selected_branch().cloned() {
                    let mut ops = vec![Op::Checkout, Op::NewFrom];
                    if !b.current {
                        ops.extend([Op::Merge, Op::Rebase, Op::Compare]);
                    }
                    if b.current {
                        ops.push(Op::Pull);
                    }
                    if !b.remote {
                        ops.push(Op::Rename);
                        if !b.current {
                            ops.push(Op::Delete);
                        }
                    }
                    p.ops = Some((ops, 0));
                }
            }
            Key::Enter => match p.entries.get(p.selected).cloned() {
                Some(Entry::NewBranch) => p.naming = Some((Naming::New(None), crate::vim_field::VimField::new("", false))),
                Some(Entry::Fetch) => {
                    self.close_branches();
                    self.run_git_job("フェッチ", &["fetch", "--all", "--prune"]);
                }
                Some(Entry::Branch(_)) => {
                    if let Some(b) = p.selected_branch().cloned() {
                        self.checkout_branch(&b);
                    }
                }
                _ => {}
            },
            k => {
                if p.query.key(k) {
                    p.rebuild();
                }
            }
        }
    }

    pub(crate) fn branches_insert(&mut self, text: &str) {
        let Some(p) = &mut self.branch_popup else { return };
        match &mut p.naming {
            Some((_, f)) => {
                f.insert_text(text);
            }
            None => {
                p.query.insert(text);
                p.rebuild();
            }
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
        for _ in 0..400 {
            e.poll_branches();
            e.poll_git();
            if e.branch_popup.as_ref().is_none_or(|p| p.rx.is_none()) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("ブランチの一覧が届かない");
    }

    #[test]
    fn create_checkout_delete() {
        let dir = std::env::temp_dir().join(format!("nagi-branches-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| String::from_utf8_lossy(&nagi_workers::git::git(&dir, args).unwrap()).trim().to_string();
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "test@example.com"]);
        g(&["config", "user.name", "test"]);
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        g(&["add", "."]);
        g(&["commit", "-q", "-m", "first"]);
        g(&["branch", "feature/coupon"]);
        let mut buf = Buffer::open(&dir.join("a.txt")).unwrap();
        buf.path = Some(dir.join("a.txt"));
        let mut e = Editor::new(buf, None);
        e.start_git();
        e.open_branches();
        wait(&mut e);
        // 絞り込んで Enter でチェックアウト
        keys(&mut e, "cou");
        let p = e.branch_popup.as_ref().unwrap();
        assert_eq!(p.selected_branch().map(|b| b.name.as_str()), Some("feature/coupon"));
        keys(&mut e, "<CR>");
        assert_eq!(g(&["branch", "--show-current"]), "feature/coupon");
        assert!(e.branch_popup.is_none());
        // 新しいブランチを作る
        e.open_branches();
        wait(&mut e);
        keys(&mut e, "<CR>topic<CR>");
        assert_eq!(g(&["branch", "--show-current"]), "topic");
        // feature/coupon を消す（操作の一覧の最後）
        e.open_branches();
        wait(&mut e);
        keys(&mut e, "coupon<Right>");
        let n = e.branch_popup.as_ref().unwrap().ops.as_ref().unwrap().0.len();
        for _ in 1..n {
            keys(&mut e, "<Down>");
        }
        keys(&mut e, "<CR>y");
        assert!(e.message.contains("消しました"), "{}", e.message);
        assert!(!g(&["branch"]).contains("coupon"));
    }
}
