//! git の画面の 5 段目: 履歴（グラフ付きのログ。IntelliJ の Git ログと同じ形）。
//!
//! 窓の領域全体に、左にブランチ、中央にグラフ付きのコミット、右に選んだコミットのメッセージと変えたファイルを並べる。
//! ログ・ブランチ・コミットの中身は裏で git から取る。ファイルの上で Enter を押すと、1 つ前の版との差分を開く。

use std::sync::mpsc::{Receiver, channel};

use nagi_vim::Key;

use crate::editor::{Editor, Mode, Prompt};

#[derive(Clone, Debug, Default)]
pub struct Commit {
    pub hash: String,
    pub short: String,
    pub refs: String,
    pub subject: String,
    pub author: String,
    pub date: String,
}

#[derive(Clone, Debug)]
pub struct LogRow {
    pub graph: String,
    pub commit: Option<Commit>,
}

#[derive(Clone, Debug, Default)]
pub struct Detail {
    pub hash: String,
    pub author: String,
    pub date: String,
    pub parents: String,
    pub message: String,
    /// (状態の文字, パス)
    pub files: Vec<(char, String)>,
}

/// 確かめてから行う操作
#[derive(Clone, Debug)]
pub enum Confirm {
    Checkout(String),
    CherryPick(String),
}

type Rx<T> = Option<Receiver<Result<T, String>>>;

pub struct LogView {
    pub rows: Vec<LogRow>,
    /// 見せている行（rows の番号。絞り込み中はコミットの行だけ）
    pub visible: Vec<usize>,
    pub selected: usize,
    pub top: usize,
    /// (見出し, 名前, 今のブランチか)
    pub branches: Vec<(String, String, bool)>,
    pub detail: Option<Detail>,
    pub focus_files: bool,
    pub file_sel: usize,
    pub filter: Prompt,
    pub filtering: bool,
    /// ファイルの履歴なら、そのパス
    pub path: Option<String>,
    pub confirm: Option<Confirm>,
    pub loading: bool,
    rx_log: Rx<Vec<LogRow>>,
    rx_branches: Rx<Vec<(String, String, bool)>>,
    rx_detail: Rx<Detail>,
}

/// UNIX 時刻なら「3 日前」の形に（そうでなければそのまま）
fn when(s: &str) -> String {
    match s.trim().parse::<i64>() {
        Ok(t) => crate::blame::relative_time(t, crate::blame::now_secs()),
        Err(_) => s.to_string(),
    }
}

/// `git log --graph --format=<区切り付き>` を読む（グラフだけの行も残す）
pub fn parse_log(out: &str) -> Vec<LogRow> {
    out.lines()
        .map(|l| match l.split_once('\x1f') {
            Some((graph, rest)) => {
                let f: Vec<&str> = rest.split('\x1f').collect();
                let get = |i: usize| f.get(i).map_or(String::new(), |s| s.to_string());
                LogRow { graph: graph.trim_end().to_string(), commit: Some(Commit { short: get(0), refs: get(1), subject: get(2), author: get(3), date: when(&get(4)), hash: get(5) }) }
            }
            None => LogRow { graph: l.trim_end().to_string(), commit: None },
        })
        .collect()
}

/// `git show --name-status --format=...` を読む
pub fn parse_detail(out: &str) -> Detail {
    let (head, files) = out.split_once('\x1e').unwrap_or((out, ""));
    let f: Vec<&str> = head.split('\x1f').collect();
    let get = |i: usize| f.get(i).map_or(String::new(), |s| s.trim().to_string());
    let files = files
        .lines()
        .filter_map(|l| {
            let mut p = l.split('\t');
            let st = p.next()?.chars().next()?;
            let path = p.last()?.to_string();
            Some((st, path))
        })
        .collect();
    Detail { hash: get(0), author: get(1), date: when(&get(2)), parents: get(3), message: get(4), files }
}

/// グラフの文字を罫線にする
pub fn pretty_graph(g: &str) -> String {
    g.chars()
        .map(|c| match c {
            '*' => '●',
            '|' => '│',
            '/' => '╱',
            '\\' => '╲',
            '_' => '─',
            c => c,
        })
        .collect()
}

fn spawn<T: Send + 'static>(waker: Option<std::sync::Arc<dyn Fn() + Send + Sync>>, f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Rx<T> {
    let (tx, rx) = channel();
    let _ = std::thread::Builder::new().name("git-log".into()).spawn(move || {
        if tx.send(f()).is_ok() {
            if let Some(w) = waker {
                w();
            }
        }
    });
    Some(rx)
}

impl LogView {
    fn current(&self) -> Option<&Commit> {
        self.visible.get(self.selected).and_then(|&i| self.rows[i].commit.as_ref())
    }

    pub fn refilter_pub(&mut self) {
        self.refilter();
        self.detail = None;
    }

    fn refilter(&mut self) {
        let q = self.filter.text.to_lowercase();
        self.visible = if q.is_empty() {
            (0..self.rows.len()).collect()
        } else {
            (0..self.rows.len())
                .filter(|&i| {
                    self.rows[i].commit.as_ref().is_some_and(|c| {
                        c.subject.to_lowercase().contains(&q) || c.author.to_lowercase().contains(&q) || c.short.starts_with(&q) || c.refs.to_lowercase().contains(&q)
                    })
                })
                .collect()
        };
        self.selected = self.visible.iter().position(|&i| self.rows[i].commit.is_some()).unwrap_or(0);
        self.top = 0;
    }
}

impl Editor {
    /// `:Glog`（`file` ならそのファイルの履歴）
    pub(crate) fn open_log(&mut self, file: bool) {
        self.open_log_with(file, None);
    }

    /// 2 つのブランチの差（`main..topic`: topic にだけあるコミット）
    pub(crate) fn open_log_range(&mut self, range: &str) {
        self.open_log_with(false, Some(range.to_string()));
    }

    fn open_log_with(&mut self, file: bool, range: Option<String>) {
        if self.git.root.is_none() {
            self.start_git();
        }
        let Some(root) = self.git.root.clone() else {
            self.message = "git のリポジトリではありません".into();
            return;
        };
        let path = if file {
            let Some(p) = self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok()) else { return };
            p.strip_prefix(&root).ok().map(|r| r.to_string_lossy().replace('\\', "/"))
        } else {
            None
        };
        let r2 = root.clone();
        let p2 = path.clone();
        let range2 = range.clone();
        let rx_log = spawn(self.waker.clone(), move || {
            let fmt = "--format=\x1f%h\x1f%D\x1f%s\x1f%an\x1f%at\x1f%H";
            let out = match &p2 {
                // ファイルの履歴は名前の変更をたどる（--follow はグラフと一緒に使えない）
                Some(p) => nagi_workers::git::git(&r2, &["log", "--follow", "-n", "1000", fmt, "--", p]),
                None => match &range2 {
                    Some(r) => nagi_workers::git::git(&r2, &["log", "--graph", "--date-order", "-n", "1000", fmt, r, "--"]),
                    None => nagi_workers::git::git(&r2, &["log", "--graph", "--all", "--date-order", "-n", "1000", fmt]),
                },
            }?;
            Ok(parse_log(&String::from_utf8_lossy(&out)))
        });
        let r3 = root.clone();
        let rx_branches = spawn(self.waker.clone(), move || {
            let out = nagi_workers::git::git(&r3, &["for-each-ref", "--format=%(HEAD)\x1f%(refname)\x1f%(refname:short)", "refs/heads", "refs/remotes", "refs/tags"])?;
            let mut v = Vec::new();
            for l in String::from_utf8_lossy(&out).lines() {
                let f: Vec<&str> = l.split('\x1f').collect();
                if f.len() < 3 || f[2].ends_with("/HEAD") {
                    continue;
                }
                let sec = if f[1].starts_with("refs/heads") {
                    "ローカル"
                } else if f[1].starts_with("refs/remotes") {
                    "リモート"
                } else {
                    "タグ"
                };
                v.push((sec.to_string(), f[2].to_string(), f[0] == "*"));
            }
            Ok(v)
        });
        self.log = Some(LogView {
            rows: Vec::new(),
            visible: Vec::new(),
            selected: 0,
            top: 0,
            branches: Vec::new(),
            detail: None,
            focus_files: false,
            file_sel: 0,
            filter: Prompt::default(),
            filtering: false,
            path: path.or(range.map(|r| format!("比較 {r}"))),
            confirm: None,
            loading: true,
            rx_log,
            rx_branches,
            rx_detail: None,
        });
        self.mode = Mode::Log;
    }

    /// 裏で取ったログ・ブランチ・コミットの中身を受け取る。変わったら true
    pub fn poll_log(&mut self) -> bool {
        let waker = self.waker.clone();
        let root = self.git.root.clone();
        let Some(l) = &mut self.log else { return false };
        let mut changed = false;
        if let Some(Ok(r)) = l.rx_log.as_ref().map(|rx| rx.try_recv()) {
            l.rx_log = None;
            l.loading = false;
            match r {
                Ok(rows) => {
                    l.rows = rows;
                    l.refilter();
                }
                Err(e) => self.message = format!("git log: {}", e.lines().next().unwrap_or("")),
            }
            changed = true;
        }
        let Some(l) = &mut self.log else { return changed };
        if let Some(Ok(r)) = l.rx_branches.as_ref().map(|rx| rx.try_recv()) {
            l.rx_branches = None;
            if let Ok(b) = r {
                l.branches = b;
            }
            changed = true;
        }
        if let Some(Ok(r)) = l.rx_detail.as_ref().map(|rx| rx.try_recv()) {
            l.rx_detail = None;
            if let Ok(d) = r {
                // 届くまでに選びが変わっていたら捨てる
                if l.current().is_some_and(|c| c.hash == d.hash) {
                    l.detail = Some(d);
                    l.file_sel = 0;
                }
            }
            changed = true;
        }
        // 選んでいるコミットの中身がまだなら取りに行く
        let want = l.current().map(|c| c.hash.clone());
        if let (Some(h), Some(root)) = (want, root) {
            if l.detail.as_ref().is_none_or(|d| d.hash != h) && l.rx_detail.is_none() {
                l.rx_detail = spawn(waker, move || {
                    let out = nagi_workers::git::git(&root, &["show", "--name-status", "--format=%H\x1f%an\x1f%at\x1f%P\x1f%B\x1e", &h])?;
                    Ok(parse_detail(&String::from_utf8_lossy(&out)))
                });
            }
        }
        changed
    }

    /// 履歴の画面のキー
    pub(crate) fn log_key(&mut self, key: Key) {
        let rows = self.windows_rows().saturating_sub(3).max(1);
        let Some(l) = &mut self.log else {
            self.mode = Mode::Normal;
            return;
        };
        if let Some(c) = l.confirm.take() {
            if key == Key::Char('y') {
                let (args, label): (Vec<String>, &str) = match &c {
                    Confirm::Checkout(h) => (vec!["checkout".into(), "-q".into(), h.clone()], "チェックアウトしました"),
                    Confirm::CherryPick(h) => (vec!["cherry-pick".into(), h.clone()], "チェリーピックしました"),
                };
                if let Some(root) = self.git.root.clone() {
                    let a: Vec<&str> = args.iter().map(String::as_str).collect();
                    self.message = match nagi_workers::git::git(&root, &a) {
                        Ok(_) => label.to_string(),
                        Err(e) => format!("git: {}", e.lines().last().unwrap_or("")),
                    };
                    self.refresh_git();
                }
            } else {
                self.message = "やめました".into();
            }
            return;
        }
        if l.filtering {
            match key {
                Key::Enter | Key::Esc => l.filtering = false,
                k => {
                    if l.filter.key(k) {
                        l.refilter();
                        l.detail = None;
                    }
                }
            }
            return;
        }
        if l.focus_files {
            let n = l.detail.as_ref().map_or(0, |d| d.files.len());
            match key {
                Key::Char('j') | Key::Down => l.file_sel = (l.file_sel + 1).min(n.saturating_sub(1)),
                Key::Char('k') | Key::Up => l.file_sel = l.file_sel.saturating_sub(1),
                Key::Tab | Key::Esc | Key::Char('h') => l.focus_files = false,
                Key::Enter => self.log_open_file_diff(),
                Key::Char('q') => self.close_log(),
                _ => {}
            }
            return;
        }
        let last = l.visible.len().saturating_sub(1);
        let step = |l: &mut LogView, d: isize| {
            // グラフだけの行は飛ばす
            let mut i = l.selected as isize;
            loop {
                i += d;
                if i < 0 || i as usize > last {
                    return;
                }
                if l.rows[l.visible[i as usize]].commit.is_some() {
                    l.selected = i as usize;
                    return;
                }
            }
        };
        match key {
            Key::Char('j') | Key::Down => step(l, 1),
            Key::Char('k') | Key::Up => step(l, -1),
            Key::Ctrl('d') => (0..rows / 2).for_each(|_| step(l, 1)),
            Key::Ctrl('u') => (0..rows / 2).for_each(|_| step(l, -1)),
            Key::Char('G') => {
                l.selected = last;
                step(l, -1);
                step(l, 1);
            }
            Key::Char('g') => {
                l.selected = 0;
                if l.visible.first().is_some_and(|&i| l.rows[i].commit.is_none()) {
                    step(l, 1);
                }
            }
            Key::Char('/') => l.filtering = true,
            Key::Tab | Key::Enter | Key::Char('l') => {
                if l.detail.as_ref().is_some_and(|d| !d.files.is_empty()) {
                    l.focus_files = true;
                }
            }
            Key::Char('o') => {
                if let Some(c) = l.current() {
                    let h = c.hash.clone();
                    self.message = format!("{} をチェックアウトしますか（HEAD を切り離す。y で進める）", &h[..7.min(h.len())]);
                    if let Some(l) = &mut self.log {
                        l.confirm = Some(Confirm::Checkout(h));
                    }
                }
            }
            Key::Char('c') => {
                if let Some(c) = l.current() {
                    let h = c.hash.clone();
                    self.message = format!("{} を今のブランチにチェリーピックしますか（y で進める）", &h[..7.min(h.len())]);
                    if let Some(l) = &mut self.log {
                        l.confirm = Some(Confirm::CherryPick(h));
                    }
                }
            }
            Key::Char('b') => {
                // ここからブランチ: :Gbranch 名前 ハッシュ を打たせる
                if let Some(c) = l.current() {
                    let h = c.short.clone();
                    self.mode = Mode::Command;
                    self.cmdline = Default::default();
                    self.cmdline.set(&format!("Gbranch  {h}"));
                    self.cmdline.cursor = "Gbranch ".len();
                    self.update_cmd_complete();
                    return;
                }
            }
            Key::Char('y') => {
                if let Some(c) = l.current() {
                    let h = c.hash.clone();
                    if let Some(cb) = &self.clipboard {
                        (cb.set)(&h);
                    }
                    self.registers.insert('"', (h.clone(), false));
                    self.message = format!("ハッシュをコピーしました: {h}");
                }
            }
            Key::Char('q') | Key::Esc => self.close_log(),
            _ => {}
        }
        if let Some(l) = &mut self.log {
            if l.selected < l.top {
                l.top = l.selected;
            } else if l.selected >= l.top + rows {
                l.top = l.selected + 1 - rows;
            }
        }
    }

    pub(crate) fn close_log(&mut self) {
        self.log = None;
        if self.mode == Mode::Log {
            self.mode = Mode::Normal;
        }
    }

    /// 選んだコミットの、選んだファイルの差分（1 つ前の版 ↔ このコミット）
    fn log_open_file_diff(&mut self) {
        let (Some(root), Some(l)) = (self.git.root.clone(), &self.log) else { return };
        let Some(d) = &l.detail else { return };
        let Some((_, path)) = d.files.get(l.file_sel).cloned() else { return };
        let h = d.hash.clone();
        let parent = d.parents.split(' ').next().filter(|p| !p.is_empty()).map(String::from);
        let old = parent.as_deref().and_then(|p| nagi_workers::git::blob_at(&root, p, &path)).unwrap_or_default();
        let new = nagi_workers::git::blob_at(&root, &h, &path).unwrap_or_default();
        let short = &h[..7.min(h.len())];
        self.show_diff(format!("{short}^  {path}"), format!("{short}  {path}"), &old, &new, None);
        self.diff_return = Some(Mode::Log);
    }

    /// `:Gbranch 名前 [起点]`: ブランチを作ってチェックアウトする
    pub(crate) fn git_new_branch(&mut self, arg: &str) {
        let Some(root) = self.git.root.clone() else { return };
        let mut it = arg.split_whitespace();
        let Some(name) = it.next() else {
            self.message = "ブランチの名前を書いてください".into();
            return;
        };
        let mut args = vec!["checkout", "-q", "-b", name];
        if let Some(start) = it.next() {
            args.push(start);
        }
        self.message = match nagi_workers::git::git(&root, &args) {
            Ok(_) => format!("ブランチ {name} を作ってチェックアウトしました"),
            Err(e) => format!("git: {}", e.lines().last().unwrap_or("")),
        };
        self.refresh_git();
        if self.log.is_some() {
            self.close_log();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse() {
        let out = "*\x1fa1b2c3d\x1fHEAD -> main\x1f注文を直す\x1f自分\x1f3 days ago\x1fa1b2c3d4\n|\\  \n| *\x1f7e6f5a4\x1f\x1fクーポン\x1fB\x1f4 days ago\x1f7e6f5a4b\n";
        let rows = parse_log(out);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].commit.as_ref().unwrap().refs, "HEAD -> main");
        assert!(rows[1].commit.is_none());
        assert_eq!(pretty_graph(&rows[2].graph), "│ ●");
        let d = parse_detail("abc\x1f自分\x1f3 days ago\x1fp1 p2\x1f件名\n\n本文\n\x1e\n\nM\tsrc/a.rs\nR100\told.rs\tnew.rs\n");
        assert_eq!((d.hash.as_str(), d.parents.as_str(), d.message.as_str()), ("abc", "p1 p2", "件名\n\n本文"));
        assert_eq!(d.files, vec![('M', "src/a.rs".to_string()), ('R', "new.rs".to_string())]);
    }
}
