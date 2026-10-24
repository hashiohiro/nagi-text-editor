//! git の画面の 6 段目: ブランチのポップアップ（IntelliJ の Git Branches と同じ形）。
//!
//! 上の欄に打つと絞り込む（文字はそのまま欄に入る）。並びは 新しいブランチ… / フェッチ、最近、ローカル、リモート。
//! Enter でチェックアウト（リモートなら追跡するローカルのブランチを作る）、→ で操作の一覧。
//! フェッチと pull はネットワークを使うので裏で行う。


use nagi_core::fuzzy::Query;
use nagi_vim::Key;

use crate::editor::{Editor, State, Prompt};

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
    rx: Option<nagi_core::task::Task<nagi_core::Result<Vec<Branch>>>>,
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
            self.notify.info("git のリポジトリではありません");
            return;
        };
        let rx = nagi_core::task::spawn("git-refs", self.waker(), move |ctx| {
            let r = nagi_workers::git::git(&root, &["for-each-ref", "--sort=-committerdate", "--format=%(HEAD)\x1f%(refname)\x1f%(refname:short)\x1f%(upstream:track)", "refs/heads", "refs/remotes"])
                .map(|b| parse_refs(&String::from_utf8_lossy(&b)));
            ctx.send(r);
        });
        let mut p = BranchPopup { query: Prompt::default(), branches: Vec::new(), entries: Vec::new(), selected: 0, ops: None, naming: None, rx: Some(rx) };
        p.rebuild();
        self.branch_popup = Some(p);
        self.set_state(State::Branches);
    }

    fn close_branches(&mut self) {
        self.branch_popup = None;
        if self.state() == State::Branches {
            self.set_state(State::Normal);
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
                Err(e) => self.notify.error(format!("git: {}", nagi_workers::git::reason(&e))),
            }
            changed = true;
        }
        changed | self.poll_git_job()
    }

    /// 裏で動かしたフェッチ・pull の結果
    fn poll_git_job(&mut self) -> bool {
        let Some((label, rx)) = &self.git_job else { return false };
        let Ok(r) = rx.try_recv() else { return false };
        match r {
            Ok(_) => self.notify.info(format!("{label}しました")),
            Err(e) => self.notify.error(format!("{label}できません: {}", nagi_workers::git::reason(&e))),
        }
        self.git_job = None;
        self.refresh_git();
        true
    }

    /// フェッチ・pull を裏で動かす
    pub(crate) fn run_git_job(&mut self, label: &str, args: &[&str]) {
        let Some(root) = self.git.root.clone() else { return };
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let rx = nagi_core::task::spawn("git-net", self.waker(), move |ctx| {
            let a: Vec<&str> = args.iter().map(String::as_str).collect();
            let r = nagi_workers::git::git(&root, &a).map(|b| String::from_utf8_lossy(&b).into_owned());
            ctx.send(r);
        });
        self.notify.info(format!("{label}しています…"));
        self.git_job = Some((label.to_string(), rx));
    }

    /// git をその場で動かして結果を知らせる（チェックアウトなど、手元だけで済むもの）
    pub(crate) fn git_now(&mut self, args: &[&str], ok: String) -> bool {
        let Some(root) = self.git.root.clone() else { return false };
        let r = nagi_workers::git::git(&root, args);
        match &r {
            Ok(_) => self.notify.info(ok),
            Err(e) => self.notify.error(format!("git: {}", nagi_workers::git::reason(&e))),
        }
        self.refresh_git();
        r.is_ok()
    }

    fn checkout_branch(&mut self, b: &Branch) {
        if b.current {
            self.notify.info(format!("{} はもうチェックアウトしています", b.name));
            return;
        }
        let local = b.name.split_once('/').map_or(b.name.as_str(), |(_, n)| n).to_string();
        let exists = self.branch_popup.as_ref().is_some_and(|p| p.branches.iter().any(|x| !x.remote && x.name == local));
        if self.checkout_ref(&b.name, b.remote, exists) {
            self.close_branches();
        }
    }

    /// ブランチをチェックアウトする。リモートの origin/x なら x（`local_exists` でなければ作って追跡する）
    pub(crate) fn checkout_ref(&mut self, name: &str, remote: bool, local_exists: bool) -> bool {
        if !remote {
            return self.git_now(&["checkout", "-q", name], format!("{name} をチェックアウトしました"));
        }
        let local = name.split_once('/').map_or(name, |(_, n)| n).to_string();
        if local_exists {
            self.git_now(&["checkout", "-q", &local], format!("{local} をチェックアウトしました"))
        } else {
            self.git_now(&["checkout", "-q", "--track", name], format!("{local} を作って {name} を追跡します"))
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
                let name = b.name.clone();
                self.ask(format!("ブランチ {name} を消しますか"), move |e| e.delete_branch(name, false));
            }
        }
    }

    /// ブランチを消す（マージしていなければ、もう一度確かめてから強制的に）
    fn delete_branch(&mut self, name: String, force: bool) {
        let flag = if force { "-D" } else { "-d" };
        let root = self.git.root.clone();
        let r = root.map(|r| nagi_workers::git::git(&r, &["branch", flag, &name]));
        match r {
            Some(Ok(_)) => {
                self.notify.info(format!("ブランチ {name} を消しました"));
                self.open_branches();
            }
            Some(Err(e)) if !force && e.to_string().contains("not fully merged") => {
                let n = name.clone();
                self.ask(format!("{name} はマージしていません。それでも消しますか"), move |e| e.delete_branch(n, true));
            }
            Some(Err(e)) => self.notify.error(format!("git: {}", nagi_workers::git::reason(&e))),
            None => {}
        }
    }

    /// ポップアップのキー
    pub(crate) fn branches_key(&mut self, key: Key) {
        let Some(p) = &mut self.branch_popup else {
            self.set_state(State::Normal);
            return;
        };
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
            // 移動は一覧の共通の動き（list.rs）。見出しの行は飛ばす
            Key::Down | Key::Ctrl('n') | Key::Tab | Key::Up | Key::Ctrl('p') => {
                if let Some(to) = crate::list::step_typing(key, p.selected, p.entries.len()) {
                    p.selected = crate::list::snap(p.selected, to, p.entries.len(), |i| !matches!(p.entries[i], Entry::Header(_)));
                }
            }
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

    /// ブランチの小窓のクリック（画面の行・列と本文の行数）。行を押すと選び、ダブルクリックで Enter と同じ（チェックアウトなど）。
    /// 小窓の外を押したら閉じる。名前を打っている間・操作の一覧を出している間は、キーで
    pub(crate) fn branches_click(&mut self, row: usize, col: usize, rows: usize, cols: usize, clicks: u32) {
        let Some(p) = &mut self.branch_popup else { return };
        // 描き方（draw_branch_popup）と同じ割り付け: 枠は 2 行目から、1 行目は絞り込み、その下に一覧
        let (top, w) = (2, 56.min(cols.saturating_sub(4)));
        let x0 = cols.saturating_sub(w) / 2;
        let list_h = p.entries.len().min(rows.saturating_sub(8)).max(1);
        if row < top || row > top + list_h || col < x0 || col >= x0 + w {
            self.close_branches();
            return;
        }
        if p.naming.is_some() || p.ops.is_some() || row == top {
            return;
        }
        let scroll = (p.selected + 1).saturating_sub(list_h);
        let hit = Some(scroll + row - top - 1).filter(|&i| i < p.entries.len() && !matches!(p.entries[i], Entry::Header(_)));
        if let Some(crate::list::Pick::Accept(_)) = crate::list::pick(&mut p.selected, hit, clicks, 2) {
            self.branches_key(Key::Enter);
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



/// この部品の入口（component.rs）
pub struct BranchesComponent;

impl crate::component::Component for BranchesComponent {
    fn status(&self, ed: &crate::editor::Editor) -> Option<crate::component::Status> {
        let _ = ed;
        crate::component::Status::new("-- ブランチ --", "打つと絞り込み  ↑↓ 移動  Enter チェックアウト  → 操作  Esc 閉じる ")
    }

    fn key(&self, ed: &mut crate::editor::Editor, key: nagi_vim::Key) -> crate::editor::Effect {
        ed.branches_key(key);
        crate::editor::Effect::None
    }

    fn text(&self, ed: &mut crate::editor::Editor, text: &str) {
        ed.branches_insert(text);
    }
}


/// ブランチの小窓（layout.rs の View）
pub struct BranchPopupView;

impl crate::layout::View for BranchPopupView {
    fn rect(&self, ed: &crate::editor::Editor, s: crate::layout::Screen) -> Option<crate::panes::Rect> {
        let _ = s;
        ed.branch_popup.as_ref()?;
        Some(crate::panes::Rect { x: 0, y: 0, w: s.cols, h: s.rows })
    }

    fn draw(&self, ed: &crate::editor::Editor, p: &mut crate::view::Painter, r: crate::panes::Rect, s: crate::layout::Screen) {
        let _ = (r, s);
        if let Some(b) = &ed.branch_popup {
            draw::draw_branch_popup(p, b, s.rows, s.cols, &ed.preedit);
        }
    }

    fn click(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, row: usize, col: usize, clicks: u32) -> crate::layout::Click {
        // 小窓は画面全体を持つ（r は本文の大きさ）
        ed.branches_click(row, col, r.h, r.w, clicks);
        crate::layout::Click::Done
    }

    fn wheel(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, row: usize, col: usize, dy: isize, dx: isize) -> bool {
        let _ = (r, row, col, dx);
        crate::layout::wheel_keys(ed, dy)
    }

    fn context(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, row: usize, col: usize) -> bool {
        use nagi_vim::Key::*;
        let _ = (r, row, col);
        // 押した行を選んで、その操作の一覧を出す（→ と同じ）
        ed.branches_click(row, col, r.h, r.w, 1);
        ed.panel_keys(&[Right]);
        true
    }
}

/// 描き方（view.rs の Painter で描く）
pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    /// ブランチのポップアップ（画面の上の中央。IntelliJ の Git Branches と同じ並び）
    pub(crate) fn draw_branch_popup(p: &mut Painter, b: &crate::branches::BranchPopup, rows: usize, cols: usize, preedit: &str) {
        use crate::branches::Entry;
        let cell = p.fonts.cell();
        let w = 56.min(cols.saturating_sub(4));
        let list_h = b.entries.len().min(rows.saturating_sub(8)).max(1);
        let h = list_h + 1;
        let x0 = (cols.saturating_sub(w)) / 2;
        let top = 2;
        let (x, y) = (x0 as f32 * cell.w, top as f32 * cell.h);
        p.out.push(Instance::fill(x - 1.0, y - 1.0, w as f32 * cell.w + 2.0, h as f32 * cell.h + 2.0, STATUS_FG));
        p.out.push(Instance::fill(x, y, w as f32 * cell.w, h as f32 * cell.h, rgb(0x141414)));
        let saved = p.cols;
        p.cols = x0 + w;
        // 上の欄: 名前を打っているときはその欄、そうでなければ絞り込み
        let (label, text) = match &b.naming {
            Some((crate::branches::Naming::New(from), f)) => (format!(" 新しいブランチ{}: ", from.as_ref().map_or(String::new(), |f| format!("（{f} から）"))), Some(f)),
            Some((crate::branches::Naming::Rename(old), f)) => (format!(" {old} の新しい名前: "), Some(f)),
            None => (" / ".to_string(), None),
        };
        let c = p.text(&label, x0, y, STATUS_FG);
        match text {
            Some(f) => {
                draw_vim_field(p, f, c, y, preedit);
            }
            None => {
                field(p, &b.query.text, b.query.cursor, preedit, c, y, true);
            }
        }
        p.out.push(Instance::fill(x, y + cell.h - 1.0, w as f32 * cell.w, 1.0, LINE_NO));
        // 一覧（選んだ行が見えるように流す）
        let scroll = (b.selected + 1).saturating_sub(list_h);
        for (k, e) in b.entries.iter().enumerate().skip(scroll).take(list_h) {
            let yy = y + (1 + k - scroll) as f32 * cell.h;
            let sel = k == b.selected && b.naming.is_none();
            if sel {
                p.out.push(Instance::fill(x, yy, w as f32 * cell.w, cell.h, rgb(0x2c4a70)));
            }
            match e {
                Entry::NewBranch => {
                    p.text(" ＋ 新しいブランチ…", x0, yy, FG);
                }
                Entry::Fetch => {
                    p.text(" ⟳ フェッチ", x0, yy, FG);
                }
                Entry::Header(t) => {
                    p.text(&format!(" {t}"), x0, yy, STATUS_FG);
                }
                Entry::Branch(i) => {
                    let br = &b.branches[*i];
                    let mark = if br.current { "●" } else { " " };
                    let c = p.text(&format!("   {mark} "), x0, yy, GIT_ADD);
                    let c = p.text(&br.name, c, yy, if br.current { rgb(0xe8e8e8) } else { FG });
                    if !br.track.is_empty() {
                        let t = br.track.replace("ahead", "↑").replace("behind", "↓").replace(['[', ']'], "");
                        p.text(&format!("  {t}"), c, yy, STATUS_FG);
                    }
                    if sel && b.ops.is_none() {
                        p.text("›", x0 + w - 2, yy, FG);
                    }
                }
            }
        }
        p.cols = saved;
        // 操作の一覧（選んだ行の右に）
        if let Some((ops, osel)) = &b.ops {
            let cur = b.current_name();
            let labels: Vec<String> = ops.iter().map(|o| format!(" {} ", o.label(&cur))).collect();
            let ow = labels.iter().map(|l| p.width(l)).max().unwrap_or(10);
            let ox = (x0 + w + 1).min(cols.saturating_sub(ow + 1));
            let oy = top + 1 + b.selected.saturating_sub(scroll);
            let (fx, fy) = (ox as f32 * cell.w, oy as f32 * cell.h);
            p.out.push(Instance::fill(fx - 1.0, fy - 1.0, ow as f32 * cell.w + 2.0, labels.len() as f32 * cell.h + 2.0, STATUS_FG));
            p.out.push(Instance::fill(fx, fy, ow as f32 * cell.w, labels.len() as f32 * cell.h, rgb(0x1c1c1c)));
            let saved = p.cols;
            p.cols = ox + ow;
            for (k, l) in labels.iter().enumerate() {
                let yy = fy + k as f32 * cell.h;
                if k == *osel {
                    p.out.push(Instance::fill(fx, yy, ow as f32 * cell.w, cell.h, rgb(0x2c4a70)));
                }
                p.text(l, ox, yy, FG);
            }
            p.cols = saved;
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
        // ほかの試験と並ぶと 2 秒を超えることがある（10 秒まで待つ）
        for _ in 0..2000 {
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
        assert!(e.notify.text.contains("消しました"), "{}", e.notify.text);
        assert!(!g(&["branch"]).contains("coupon"));
    }

    /// マウスで: 行を押すと選び（見出しは選ばない）、ダブルクリックでチェックアウト（Enter と同じ）。小窓の外を押すと閉じる
    #[test]
    fn click_select_and_checkout() {
        let dir = std::env::temp_dir().join(format!("nagi-branches-click-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| String::from_utf8_lossy(&nagi_workers::git::git(&dir, args).unwrap()).trim().to_string();
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "test@example.com"]);
        g(&["config", "user.name", "test"]);
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        g(&["add", "."]);
        g(&["commit", "-q", "-m", "first"]);
        g(&["branch", "topic"]);
        let mut buf = Buffer::open(&dir.join("a.txt")).unwrap();
        buf.path = Some(dir.join("a.txt"));
        let mut e = Editor::new(buf, None);
        e.set_screen(120, 40);
        e.start_git();
        e.open_branches();
        wait(&mut e);
        let s = crate::layout::Screen { rows: 40, cols: 120 };
        // 小窓は 2 行目から（1 行目は絞り込み）、幅 56 で真ん中
        let x = (120 - 56) / 2 + 5;
        let p = e.branch_popup.as_ref().unwrap();
        let k = p.entries.iter().position(|en| matches!(en, Entry::Branch(i) if p.branches[*i].name == "topic")).unwrap();
        let header = p.entries.iter().position(|en| matches!(en, Entry::Header(_))).unwrap();
        crate::layout::press(&mut e, s, 3 + header, x, |_| 1);
        assert_ne!(e.branch_popup.as_ref().unwrap().selected, header, "見出しは選ばない");
        crate::layout::press(&mut e, s, 3 + k, x, |_| 1);
        assert_eq!(e.branch_popup.as_ref().unwrap().selected, k);
        assert_eq!(g(&["branch", "--show-current"]), "main", "1 回では何もしない");
        crate::layout::press(&mut e, s, 3 + k, x, |_| 2);
        assert_eq!(g(&["branch", "--show-current"]), "topic", "{}", e.notify.text);
        // 外を押すと閉じる
        e.open_branches();
        wait(&mut e);
        crate::layout::press(&mut e, s, 0, 0, |_| 1);
        assert!(e.branch_popup.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
