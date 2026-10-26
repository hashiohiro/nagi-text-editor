//! git の画面の 5 段目: 履歴（グラフ付きのログ。IntelliJ の Git ログと同じ形）。
//!
//! 窓の領域全体に、左にブランチ、中央にグラフ付きのコミット、右に選んだコミットのメッセージと変えたファイルを並べる。
//! ログ・ブランチ・コミットの中身は裏で git から取る。ファイルの上で Enter を押すと、1 つ前の版との差分を開く。


use nagi_vim::Key;

use crate::editor::{Editor, State};

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

impl LogRow {
    /// 真ん中の列で横に送る部分（グラフ・ref・題名）の表示の幅（描き方と同じ並び）
    pub fn text_width(&self) -> usize {
        let w = |s: &str| nagi_core::display_col(s, s.len(), false);
        let graph = w(&pretty_graph(&self.graph));
        match &self.commit {
            Some(c) => graph + 1 + if c.refs.is_empty() { 0 } else { w(&c.refs) + 2 } + w(&c.subject),
            None => graph,
        }
    }
}

/// 履歴（幅 `w`）の左のブランチの列と右の中身の列の幅（ドラッグで変えていればその幅。真ん中に 20 列は残す）
pub fn log_cols(w: usize, branch_w: Option<usize>, detail_w: Option<usize>) -> (usize, usize) {
    let bmin = 6.min(w);
    let bw = branch_w.unwrap_or(20.min(w / 6)).clamp(bmin, (w / 2).max(bmin));
    let dmin = 10.min(w);
    let dw = detail_w.unwrap_or(42.min(w / 3)).clamp(dmin, w.saturating_sub(bw + 2 + 20).max(dmin));
    (bw, dw)
}

/// 履歴の真ん中の列で、横に送る部分の見える幅（右の作者・日時・ハッシュの分を除いた、だいたいの幅）
pub fn log_text_w(w: usize, (bw, dw): (usize, usize)) -> usize {
    w.saturating_sub(bw + dw + 2).saturating_sub(32).max(1)
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
    /// 2 つのコミットを比べているときの古い方（hash が新しい方。files はその間で変わったファイル）
    pub base: Option<String>,
}

/// 確かめてから行う操作（確認の窓で「はい」なら log_do）
#[derive(Clone, Debug)]
pub enum Confirm {
    Checkout(String),
    CherryPick(String),
    Revert(String),
    /// 複数のチェリーピック（古い順）
    CherryPickMany(Vec<String>),
    /// 複数の取り消し（新しい順）
    RevertMany(Vec<String>),
    /// HEAD のコミットを取り消して、変更は作業ツリーに残す（IntelliJ の Undo Commit）
    Undo(String),
    /// 親にまとめる（true ならメッセージをつなぐスカッシュ、false ならフィックスアップ）
    Squash(String, bool),
    Drop(String),
    /// このコミットまでリセット（--soft・--mixed・--hard・--keep）
    Reset(String, &'static str),
    /// 今のブランチをこの上へ載せ直す
    Rebase(String),
    /// ここまで push（ハッシュ, 上流のブランチ）
    PushTo(String, String),
    /// ブランチを消す（名前, 強制）
    DeleteBranch(String, bool),
    /// このブランチをチェックアウトして、さっきまでのブランチの上へ載せ直す
    CheckoutRebase(String, String),
}

type Rx<T> = Option<nagi_core::task::Task<nagi_core::Result<T>>>;

pub struct LogView {
    /// 下の欄の「Git」タブに収めている（全画面でない）
    pub docked: bool,
    pub rows: Vec<LogRow>,
    /// 見せている行（rows の番号。絞り込み中はコミットの行だけ）
    pub visible: Vec<usize>,
    pub selected: usize,
    /// 複数選んだコミット（ハッシュ。Ctrl・Shift+クリック、Space）。空なら、選んでいるのは selected の 1 つだけ
    pub marked: Vec<String>,
    /// Shift+クリックで範囲を選ぶ起点（visible の番号）
    mark_anchor: Option<usize>,
    pub top: usize,
    /// 真ん中の列（グラフ・ref・題名）の左端の列。作者・日時・ハッシュは右に留めたまま
    pub left: usize,
    /// (見出し, 名前, 今のブランチか)
    pub branches: Vec<(String, String, bool)>,
    pub detail: Option<Detail>,
    pub focus_files: bool,
    pub file_sel: usize,
    /// 絞り込み（決めた文字）と、打っている間の Vim 風の入力欄
    pub filter_text: String,
    pub filter: Option<crate::vim_field::VimField>,
    /// ファイルの履歴なら、そのパス
    pub path: Option<String>,
    pub loading: bool,
    /// ブランチの列で選んでいるもの（branches の番号）
    pub branch_sel: Option<usize>,
    /// ブランチの列の先頭に見せている行（見出しの行も数える）
    pub branch_top: usize,
    /// 読み直すための元（ファイルの履歴のパス・比べる範囲）
    src_path: Option<String>,
    src_range: Option<String>,
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
    Detail { hash: get(0), author: get(1), date: when(&get(2)), parents: get(3), message: get(4), files, base: None }
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

fn spawn<T: Send + 'static>(waker: Option<std::sync::Arc<dyn Fn() + Send + Sync>>, f: impl FnOnce() -> nagi_core::Result<T> + Send + 'static) -> Rx<T> {
    let rx = nagi_core::task::spawn("git-log", waker.unwrap_or_else(nagi_core::task::no_waker), move |ctx| {
        ctx.send(f());
    });
    Some(rx)
}

impl LogView {
    /// ブランチの列の行（見出しの下を 0 とする）→ ブランチの番号（区分の見出しの行は None）
    pub fn branch_at(&self, row: usize) -> Option<usize> {
        self.branch_rows().get(self.branch_top + row).copied().flatten()
    }

    /// ブランチの列の行（見出しは None、ブランチは branches の番号）
    pub fn branch_rows(&self) -> Vec<Option<usize>> {
        let mut out = Vec::new();
        let mut last = "";
        for (i, (sec, _, _)) in self.branches.iter().enumerate() {
            if sec != last {
                out.push(None);
                last = sec;
            }
            out.push(Some(i));
        }
        out
    }

    /// 名前（ブランチ・タグ）の指すコミットの行（見せている行の番号）
    pub fn row_of_ref(&self, name: &str) -> Option<usize> {
        self.visible.iter().position(|&i| {
            self.rows[i].commit.as_ref().is_some_and(|c| c.refs.split(", ").any(|r| r.trim_start_matches("HEAD -> ").trim_start_matches("tag: ") == name))
        })
    }

    fn current(&self) -> Option<&Commit> {
        self.visible.get(self.selected).and_then(|&i| self.rows[i].commit.as_ref())
    }

    fn commit_at(&self, k: usize) -> Option<&Commit> {
        self.visible.get(k).and_then(|&i| self.rows[i].commit.as_ref())
    }

    /// 選んでいるコミット（複数選んでいればそのすべて、でなければ今の 1 つ）。並びは履歴の順（新しい方が先）
    pub fn picked(&self) -> Vec<Commit> {
        if self.marked.is_empty() {
            return self.current().cloned().into_iter().collect();
        }
        self.visible.iter().filter_map(|&i| self.rows[i].commit.as_ref()).filter(|c| self.marked.contains(&c.hash)).cloned().collect()
    }

    /// k 番目（visible の番号）を選びに足す・外す（Ctrl+クリック）。`range` なら起点から k までを選ぶ（Shift+クリック）
    fn mark_click(&mut self, k: usize, range: bool) {
        if range {
            let a = self.mark_anchor.unwrap_or(self.selected);
            let (lo, hi) = (a.min(k), a.max(k));
            self.marked = (lo..=hi).filter_map(|i| self.commit_at(i).map(|c| c.hash.clone())).collect();
            self.mark_anchor = Some(a);
        } else {
            // 初めの Ctrl+クリックでは、今選んでいる 1 つも選びに入れる（IntelliJ と同じ）
            if self.marked.is_empty() {
                self.marked.extend(self.current().map(|c| c.hash.clone()));
            }
            self.toggle_mark(k);
            self.mark_anchor = Some(k);
        }
        self.selected = k;
    }

    /// k 番目を選びに足す・外す
    fn toggle_mark(&mut self, k: usize) {
        let Some(h) = self.commit_at(k).map(|c| c.hash.clone()) else { return };
        match self.marked.iter().position(|m| *m == h) {
            Some(i) => {
                self.marked.remove(i);
            }
            None => self.marked.push(h),
        }
    }

    pub fn refilter_pub(&mut self) {
        self.refilter();
        self.detail = None;
    }

    fn refilter(&mut self) {
        let q = self.filter_text.to_lowercase();
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
        // 絞り込みで見えなくなったコミットに操作が及ばないよう、選びは外す
        self.marked.clear();
        self.mark_anchor = None;
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
            self.notify.info("git のリポジトリではありません");
            return;
        };
        let path = if file {
            let Some(p) = self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok()) else { return };
            p.strip_prefix(&root).ok().map(|r| r.to_string_lossy().replace('\\', "/"))
        } else {
            None
        };
        // 新しく開く（下の欄に収めるのは select_bottom_tab が後で決める）
        self.log = None;
        self.load_log(root, path, range);
    }

    /// 履歴を読み直す（操作の後。下の欄に収めているかは保つ）
    pub(crate) fn reload_log(&mut self) {
        let (Some(root), Some(l)) = (self.git.root.clone(), &self.log) else { return };
        let (path, range) = (l.src_path.clone(), l.src_range.clone());
        let state = self.state();
        self.load_log(root, path, range);
        self.set_state(state);
    }

    /// 履歴を（読み直すときも）裏で取る
    fn load_log(&mut self, root: std::path::PathBuf, path: Option<String>, range: Option<String>) {
        let docked = self.log.as_ref().is_some_and(|l| l.docked);
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
                // origin/HEAD は除く（短い名前だと「origin」になるので、元の名前で見る）
                if f.len() < 3 || f[1].ends_with("/HEAD") {
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
            left: 0,
            docked,
            branch_sel: None,
            branch_top: 0,
            src_path: path.clone(),
            src_range: range.clone(),
            rows: Vec::new(),
            visible: Vec::new(),
            selected: 0,
            marked: Vec::new(),
            mark_anchor: None,
            top: 0,
            branches: Vec::new(),
            detail: None,
            focus_files: false,
            file_sel: 0,
            filter_text: String::new(),
            filter: None,
            path: path.or(range.map(|r| if r.contains("..") { format!("比較 {r}") } else { format!("ブランチ {r}") })),
            loading: true,
            rx_log,
            rx_branches,
            rx_detail: None,
        });
        self.set_state(State::Log);
    }

    /// 裏で取ったログ・ブランチ・コミットの中身を受け取る。変わったら true
    pub fn poll_log(&mut self) -> bool {
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
                Err(e) => self.notify.error(format!("git log: {}", e.first_line())),
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
        self.log_request_detail();
        changed
    }

    /// 選んでいるコミットの中身がまだなら、裏で取りに行く。描く前にも呼ぶ（選びはキー・マウスで変わり、
    /// 裏の仕事が終わったときだけ見ていると、ほかの仕事が無い間は前のコミットの中身のままになる）
    pub(crate) fn log_request_detail(&mut self) {
        let waker = self.waker.clone();
        let root = self.git.root.clone();
        let Some(l) = &mut self.log else { return };
        let want = l.current().map(|c| c.hash.clone());
        if let (Some(h), Some(root)) = (want, root) {
            if l.detail.as_ref().is_none_or(|d| d.hash != h) && l.rx_detail.is_none() {
                l.rx_detail = spawn(waker, move || {
                    let out = nagi_workers::git::git(&root, &["show", "--name-status", "--format=%H\x1f%an\x1f%at\x1f%P\x1f%B\x1e", &h])?;
                    Ok(parse_detail(&String::from_utf8_lossy(&out)))
                });
            }
        }
    }

    /// 履歴の画面のキー
    pub(crate) fn log_key(&mut self, key: Key) {
        let docked = self.log.as_ref().is_some_and(|l| l.docked);
        let rows = self.log_rows();
        // 複数選んでいれば、Esc はまず選びを外す
        if key == Key::Esc {
            if let Some(l) = self.log.as_mut().filter(|l| l.filter.is_none() && !l.focus_files && !l.marked.is_empty()) {
                l.marked.clear();
                l.mark_anchor = None;
                return;
            }
        }
        // 下の欄に収めているとき: Esc は本文へ戻るだけ、q は欄を隠す（どちらも履歴は残す）。Ctrl-w k も本文へ
        if docked && self.log.as_ref().is_some_and(|l| l.filter.is_none() && !l.focus_files) {
            match key {
                Key::Esc => {
                    self.set_state(State::Normal);
                    return;
                }
                Key::Char('q') => {
                    self.hide_bottom();
                    return;
                }
                _ => {}
            }
        }
        let Some(l) = &mut self.log else {
            self.set_state(State::Normal);
            return;
        };
        if l.filter.is_some() {
            use crate::vim_field::FieldOut;
            let out = self.feed_field(key, |e| e.log.as_mut().and_then(|l| l.filter.as_mut()));
            let Some(l) = &mut self.log else { return };
            match out {
                FieldOut::Changed => {
                    l.filter_text = l.filter.as_ref().map(|f| f.text()).unwrap_or_default();
                    l.refilter();
                    l.detail = None;
                }
                FieldOut::Submit | FieldOut::Cancel => l.filter = None,
                FieldOut::None => {}
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
        // 移動は一覧の共通の動き（list.rs）。グラフだけの行は飛ばす
        let n = l.visible.len();
        let to = if key == Key::Char('g') { Some(0) } else { crate::list::step(key, l.selected, n, rows) };
        if let Some(to) = to {
            l.selected = crate::list::snap(l.selected, to, n, |i| l.rows[l.visible[i]].commit.is_some());
            return;
        }
        match key {
            Key::Char('/') => l.filter = Some(crate::vim_field::VimField::new(&l.filter_text, false)),
            Key::Tab | Key::Enter | Key::Char('l') => {
                if l.detail.as_ref().is_some_and(|d| !d.files.is_empty()) {
                    l.focus_files = true;
                }
            }
            Key::Char('o') => {
                if let Some(h) = l.current().map(|c| c.hash.clone()) {
                    self.log_ask(Confirm::Checkout(h));
                }
            }
            Key::Char(' ') => {
                let k = l.selected;
                l.toggle_mark(k);
                l.mark_anchor = Some(k);
            }
            Key::Char('c') => self.log_cherry_pick_picked(),
            Key::Char('b') => {
                if let Some(h) = l.current().map(|c| c.short.clone()) {
                    self.prompt_command("Gbranch ", &format!(" {h}"));
                    return;
                }
            }
            Key::Char('y') => self.log_copy_picked(false),
            Key::Char('q') | Key::Esc => self.close_log(),
            _ => {}
        }
        if let Some(l) = &mut self.log {
            crate::list::fit(l.selected, &mut l.top, rows);
        }
    }

    /// 下の欄の Git を押した（欄の中身の行・列・幅。0 行目は絞り込みの見出し）。
    /// 真ん中はコミットを選ぶ（ダブルクリックで変えたファイルへ）、右は変えたファイルを選ぶ（ダブルクリックで差分）
    pub(crate) fn log_click(&mut self, row: usize, col: usize, w: usize, clicks: u32) {
        self.set_state(State::Log);
        let (bw, dw) = self.log_cols_for(w);
        let mods = self.click_mods;
        let Some(l) = &mut self.log else { return };
        // 0 行目（「/ で絞り込み」の見出し）を押したら絞り込みの欄を開く
        if row == 0 {
            if l.filter.is_none() {
                l.filter = Some(crate::vim_field::VimField::new(&l.filter_text, false));
            }
            return;
        }
        l.filter = None;
        // 描き方（draw_log_view）と同じ割り付け
        let cx = bw + 1;
        let cw = w.saturating_sub(bw + dw + 2);
        let dx = cx + cw + 1;
        if col >= cx && col < cx + cw {
            let hit = crate::list::row_at(l.top, row, 1, l.visible.len()).filter(|&k| l.rows[l.visible[k]].commit.is_some());
            // Ctrl+クリックで 1 つずつ足す・外す、Shift+クリックで範囲を選ぶ
            if let Some(k) = hit.filter(|_| mods.ctrl || mods.shift) {
                l.mark_click(k, mods.shift);
                l.focus_files = false;
                return;
            }
            let pick = crate::list::pick(&mut l.selected, hit, clicks, 2);
            if pick.is_some() {
                l.focus_files = false;
                l.marked.clear();
                l.mark_anchor = Some(l.selected);
            }
            if matches!(pick, Some(crate::list::Pick::Accept(_))) && l.detail.as_ref().is_some_and(|d| !d.files.is_empty()) {
                l.focus_files = true;
            }
        } else if col >= dx {
            // 右: 見出し 2 行・本文（8 行まで）・「変えたファイル」の後にファイル
            let Some(d) = &l.detail else { return };
            let first = 5 + d.message.lines().count().min(8);
            let n = d.files.len();
            let pick = crate::list::pick(&mut l.file_sel, row.checked_sub(first).filter(|&k| k < n), clicks, 2);
            if pick.is_some() {
                l.focus_files = true;
            }
            if matches!(pick, Some(crate::list::Pick::Accept(_))) {
                self.log_open_file_diff();
            }
        } else if col < bw {
            // 左: ブランチ。押すとそのブランチの指すコミットへ、ダブルクリックでそのブランチの履歴だけを出す
            // （IntelliJ と同じ。チェックアウトは右クリックのメニューから）
            l.focus_files = false;
            let mut i = l.branch_sel.unwrap_or(0);
            let Some(pick) = crate::list::pick(&mut i, l.branch_at(row - 1), clicks, 2) else { return };
            l.branch_sel = Some(i);
            if let Some(k) = l.row_of_ref(&l.branches[i].1) {
                l.selected = k;
                let rows = self.log_rows();
                if let Some(l) = &mut self.log {
                    crate::list::fit(k, &mut l.top, rows);
                }
            }
            if matches!(pick, crate::list::Pick::Accept(_)) {
                self.log_show_branch(i);
            }
        } else {
            l.focus_files = false;
        }
    }

    /// そのブランチの履歴だけを出す。もう出していれば、全部のブランチに戻す（下の欄か窓の領域かは保つ）
    pub(crate) fn log_show_branch(&mut self, i: usize) {
        let Some(l) = &mut self.log else { return };
        let Some(name) = l.branches.get(i).map(|b| b.1.clone()) else { return };
        let all = l.src_range.as_deref() == Some(name.as_str());
        l.src_range = if all { None } else { Some(name.clone()) };
        l.src_path = None;
        self.reload_log();
        if let Some(l) = &mut self.log {
            l.branch_sel = Some(i);
        }
        self.notify.info(if all { "全部のブランチの履歴を出しました".to_string() } else { format!("{name} の履歴だけを出しました（もう一度ダブルクリックで全部に戻す）") });
    }

    /// `:` の行に `head` と `tail` を入れ、その間にカーソルを置く（名前などを打たせる）
    fn prompt_command(&mut self, head: &str, tail: &str) {
        self.enter_command("");
        self.cmdline.set(&format!("{head}{tail}"));
        self.cmdline.cursor = head.len();
        self.update_cmd_complete();
    }

    /// 確かめてから行う（確認の窓で「はい」なら log_do）
    pub(crate) fn log_ask(&mut self, c: Confirm) {
        let s = |h: &str| h[..7.min(h.len())].to_string();
        let cur = self.git.status.as_ref().map(|s| s.branch.clone()).unwrap_or_default();
        let q = match &c {
            Confirm::Checkout(h) => format!("{} をチェックアウトしますか（HEAD を切り離す）", s(h)),
            Confirm::CherryPick(h) => format!("{} を {cur} にチェリーピックしますか", s(h)),
            Confirm::Revert(h) => format!("{} を取り消すコミットを足しますか（revert）", s(h)),
            Confirm::CherryPickMany(hs) => format!("{} 件のコミットを {cur} にチェリーピックしますか（古い順に。衝突したら全部やめて元に戻す）", hs.len()),
            Confirm::RevertMany(hs) => format!("{} 件のコミットを取り消すコミットを足しますか（revert。新しい順に。衝突したら全部やめて元に戻す）", hs.len()),
            Confirm::Undo(h) => format!("{} を取り消して、変更は作業ツリーに残しますか", s(h)),
            Confirm::Squash(h, true) => format!("{} を 1 つ前のコミットにまとめますか（メッセージはつなぐ）", s(h)),
            Confirm::Squash(h, false) => format!("{} を 1 つ前のコミットにまとめますか（このメッセージは捨てる）", s(h)),
            Confirm::Drop(h) => format!("{} を消しますか（その後のコミットは載せ直す）", s(h)),
            Confirm::Reset(h, "--hard") => format!("{cur} を {} までリセットしますか（--hard: 作業中の変更も消える）", s(h)),
            Confirm::Reset(h, m) => format!("{cur} を {} までリセットしますか（{m}）", s(h)),
            Confirm::Rebase(h) => format!("{cur} を {} の上へリベースしますか", s(h)),
            Confirm::PushTo(h, up) => format!("{} までを {up} へ push しますか", s(h)),
            Confirm::DeleteBranch(b, false) => format!("ブランチ {b} を消しますか"),
            Confirm::DeleteBranch(b, true) => format!("ブランチ {b} はマージされていません。それでも消しますか"),
            Confirm::CheckoutRebase(b, onto) => format!("{b} をチェックアウトして {onto} の上へリベースしますか"),
        };
        self.ask(q, move |e| e.log_do(c));
    }

    /// 確かめた操作を行い、git の状態と履歴を読み直す
    fn log_do(&mut self, c: Confirm) {
        use nagi_workers::git;
        let Some(root) = self.git.root.clone() else { return };
        let r: nagi_core::Result<String> = match &c {
            Confirm::Checkout(h) => git::git(&root, &["checkout", "-q", h]).map(|_| "チェックアウトしました".into()),
            Confirm::CherryPick(h) => git::cherry_pick(&root, h).map(|_| "チェリーピックしました".into()),
            Confirm::Revert(h) => git::revert(&root, h).map(|_| "取り消すコミットを足しました".into()),
            Confirm::CherryPickMany(hs) => git::cherry_pick_many(&root, hs).map(|_| format!("{} 件をチェリーピックしました", hs.len())),
            Confirm::RevertMany(hs) => git::revert_many(&root, hs).map(|_| format!("{} 件を取り消すコミットを足しました", hs.len())),
            Confirm::Undo(_) => git::git(&root, &["reset", "--soft", "HEAD~1"]).map(|_| "コミットを取り消しました（変更は残っています）".into()),
            Confirm::Squash(h, keep) => git::squash_into_parent(&root, h, *keep).map(|_| "1 つ前のコミットにまとめました".into()),
            Confirm::Drop(h) => git::drop_commit(&root, h).map(|_| "コミットを消しました".into()),
            Confirm::Reset(h, m) => git::git(&root, &["reset", m, h]).map(|_| format!("リセットしました（{m}）")),
            Confirm::Rebase(h) => git::rebase_onto(&root, h).map(|_| "リベースしました".into()),
            Confirm::PushTo(h, up) => {
                // 送るのはネットワークを使うので裏で
                let (remote, branch) = up.split_once('/').unwrap_or(("origin", up));
                let refspec = format!("{h}:refs/heads/{branch}");
                self.run_git_job("push ", &["push", remote, &refspec]);
                return;
            }
            Confirm::DeleteBranch(b, force) => match git::git(&root, &["branch", if *force { "-D" } else { "-d" }, b]) {
                // マージされていなければ、もう一度確かめてから -D
                Err(e) if !force && e.to_string().contains("not fully merged") => {
                    self.log_ask(Confirm::DeleteBranch(b.clone(), true));
                    return;
                }
                r => r.map(|_| format!("ブランチ {b} を消しました")),
            },
            Confirm::CheckoutRebase(b, onto) => git::git(&root, &["checkout", "-q", b]).and_then(|_| git::rebase_onto(&root, onto)).map(|_| format!("{b} を {onto} の上へリベースしました")),
        };
        match r {
            Ok(m) => self.notify.info(m),
            Err(e) => self.notify.error(format!("git: {}", nagi_workers::git::reason(&e))),
        }
        self.refresh_git();
        self.reload_log();
    }

    /// ブランチの列の i 番目をチェックアウトする（右クリックのメニュー）
    pub(crate) fn log_checkout_branch(&mut self, i: usize) {
        let Some((sec, name, cur)) = self.log.as_ref().and_then(|l| l.branches.get(i).cloned()) else { return };
        if cur {
            self.notify.info(format!("{name} はもうチェックアウトしています"));
            return;
        }
        if sec == "タグ" {
            self.log_ask(Confirm::Checkout(name));
            return;
        }
        let remote = sec == "リモート";
        let local = name.split_once('/').map_or(name.as_str(), |(_, n)| n).to_string();
        let exists = self.log.as_ref().is_some_and(|l| l.branches.iter().any(|(s, n, _)| s == "ローカル" && *n == local));
        self.checkout_ref(&name, remote, exists);
        self.reload_log();
    }

    /// ホイール: 表示だけを送る（選んでいるコミットは動かさない。キーで動かすと見える範囲へ戻る）。
    /// 横は真ん中の列（グラフ・ref・題名）を送る。`w` は履歴の幅、`col` は履歴の中のマウスの列（左のブランチの列ならそこを送る）
    pub(crate) fn log_wheel(&mut self, dy: isize, dx: isize, w: usize, col: usize) {
        let rows = self.log_rows();
        let cols = self.log_cols_for(w);
        let Some(l) = &mut self.log else { return };
        if col < cols.0 {
            let n = l.branch_rows().len();
            l.branch_top = (l.branch_top as isize + dy).clamp(0, n.saturating_sub(rows) as isize) as usize;
            return;
        }
        l.top = (l.top as isize + dy).clamp(0, l.visible.len().saturating_sub(rows) as isize) as usize;
        if dx != 0 {
            let widest = l.visible.iter().skip(l.top).take(rows).map(|&i| l.rows[i].text_width()).max().unwrap_or(0);
            crate::list::hwheel(&mut l.left, widest + 1, log_text_w(w, cols), dx);
        }
    }

    /// 履歴（幅 `w`）の左のブランチの列と右の中身の列の幅
    pub(crate) fn log_cols_for(&self, w: usize) -> (usize, usize) {
        log_cols(w, self.log_branch_w, self.log_detail_w)
    }

    /// 履歴を出している矩形（窓の領域いっぱいか、下の欄の中身）。区切りをつかむのに使う
    pub(crate) fn log_rect(&self) -> Option<crate::panes::Rect> {
        let l = self.log.as_ref()?;
        if self.diff.is_some() {
            return None;
        }
        if !l.docked {
            let (x, w) = self.windows_x();
            return Some(crate::panes::Rect { x, y: 0, w, h: self.windows_rows() });
        }
        (self.bottom_kind == crate::result_grid::BottomKind::Git).then(|| self.bottom_content_rect()).flatten()
    }

    /// 履歴の行の高さ（下の欄か窓の領域か）
    fn log_rows(&self) -> usize {
        if self.log.as_ref().is_some_and(|l| l.docked) {
            self.bottom_content_rect().map_or(5, |r| r.h).saturating_sub(2).max(1)
        } else {
            self.windows_rows().saturating_sub(3).max(1)
        }
    }

    /// 右クリック（欄の中の行・列・幅と、メニューを出す画面の位置）。押した行を選んでから、その行のメニューを出す
    pub(crate) fn log_context(&mut self, row: usize, col: usize, w: usize, at: (usize, usize)) -> bool {
        // 複数選んだコミットの上なら、選びを保ったまま、選んだもの全部へのメニュー
        if self.log_marked_at(row, col, w) {
            return self.log_context_picked(at);
        }
        self.log_click(row, col, w, 1);
        let (bw, dw) = self.log_cols_for(w);
        let Some(l) = &self.log else { return false };
        let cur = self.git.status.as_ref().map(|s| s.branch.clone()).unwrap_or_default();
        let upstream = self.git.status.as_ref().and_then(|s| s.upstream.clone());
        // 右の列（選んだコミットの変えたファイル）は w - dw から（割り付けは log_click と同じ）
        let menu = if l.focus_files && col >= w.saturating_sub(dw) {
            let Some(d) = &l.detail else { return false };
            let Some((_, path)) = d.files.get(l.file_sel).cloned() else { return false };
            let (h, short) = (d.hash.clone(), d.hash[..7.min(d.hash.len())].to_string());
            let mut m = crate::menu::Menu::new(at.0, at.1);
            m = m.item("差分を見る（このコミットでの変更）", |e| e.log_open_file_diff());
            let (x, p) = (h.clone(), path.clone());
            m = m.item("作業中のファイルと比べる", move |e| e.log_compare_worktree(&x, &p));
            let p = path.clone();
            m = m.item("ファイルを開く", move |e| e.log_open_worktree_file(&p));
            let p = path.clone();
            m = m.item("このファイルの履歴", move |e| e.open_log_file(&p));
            m = m.sep();
            let (x, p, s) = (h.clone(), path.clone(), short.clone());
            m = m.item("この版に戻す…", move |e| {
                let q = format!("{p} を {s} の版に戻しますか（作業中の変更は消える）");
                e.ask(q, move |e| {
                    if e.git_now(&["checkout", &x, "--", &p], format!("{p} を {s} の版に戻しました")) {
                        e.reload_log();
                    }
                })
            });
            let p = path.clone();
            m = m.item("パスをコピー", move |e| e.copy_text(p, "パス"));
            m
        } else if col < bw {
            let Some(i) = l.branch_sel.filter(|_| row >= 1 && l.branch_at(row - 1).is_some()) else { return false };
            let (sec, name, is_cur) = l.branches[i].clone();
            let local = sec == "ローカル";
            let mut m = crate::menu::Menu::new(at.0, at.1);
            if !is_cur {
                m = m.item("チェックアウト", move |e| e.log_checkout_branch(i));
            }
            let n = name.clone();
            m = m.item(format!("{name} から新しいブランチ…"), move |e| e.prompt_command("Gbranch ", &format!(" {n}")));
            if !is_cur && sec != "タグ" {
                let (n, c) = (name.clone(), cur.clone());
                m = m.item(format!("チェックアウトして {cur} の上へリベース"), move |e| e.log_ask(Confirm::CheckoutRebase(n, c)));
                let n = name.clone();
                let c = cur.clone();
                m = m.item(format!("{cur} と比べる"), move |e| e.open_log_range(&format!("{c}..{n}")));
                let n = name.clone();
                m = m.item(format!("{cur} を {name} の上へリベース"), move |e| e.log_ask(Confirm::Rebase(n)));
                let n = name.clone();
                m = m.item(format!("{name} を {cur} へマージ"), move |e| {
                    if e.git_now(&["merge", "--no-edit", &n], format!("{n} をマージしました")) {
                        e.reload_log();
                    }
                });
            }
            if local {
                m = m.sep();
                let n = name.clone();
                m = m.item("名前を変える…", move |e| e.prompt_command(&format!("Grename {n} "), ""));
                let n = name.clone();
                m = m.item("プッシュ", move |e| e.run_git_job("push ", &["push", "-u", "origin", &n]));
                if !is_cur {
                    let n = name.clone();
                    m = m.item("削除", move |e| e.log_ask(Confirm::DeleteBranch(n, false)));
                }
            }
            m
        } else {
            let Some(c) = l.current().filter(|_| !l.focus_files).cloned() else { return false };
            let head = l.rows.iter().filter_map(|r| r.commit.as_ref()).find(|c| c.refs.starts_with("HEAD")).map(|c| c.hash.clone());
            let is_head = head.as_deref() == Some(c.hash.as_str());
            let (h, short) = (c.hash.clone(), c.short.clone());
            let mut m = crate::menu::Menu::new(at.0, at.1);
            let x = h.clone();
            m = m.item("ハッシュをコピー", move |e| e.copy_text(x, "ハッシュ"));
            m = m.item("ハッシュと題名をコピー", |e| e.log_copy_picked(true));
            let x = h.clone();
            m = m.item("チェリーピック", move |e| e.log_ask(Confirm::CherryPick(x)));
            let x = h.clone();
            m = m.item("このリビジョンをチェックアウト", move |e| e.log_ask(Confirm::Checkout(x)));
            let x = short.clone();
            m = m.item("新しいブランチ…", move |e| e.prompt_command("Gbranch ", &format!(" {x}")));
            let x = short.clone();
            m = m.item("新しいタグ…", move |e| e.prompt_command("Gtag ", &format!(" {x}")));
            m = m.sep();
            let x = h.clone();
            m = m.item(format!("{cur} をここまでリセット（--soft）"), move |e| e.log_ask(Confirm::Reset(x, "--soft")));
            let x = h.clone();
            m = m.item(format!("{cur} をここまでリセット（--mixed）"), move |e| e.log_ask(Confirm::Reset(x, "--mixed")));
            let x = h.clone();
            m = m.item(format!("{cur} をここまでリセット（--hard）"), move |e| e.log_ask(Confirm::Reset(x, "--hard")));
            let x = h.clone();
            m = m.item(format!("{cur} をここまでリセット（--keep）"), move |e| e.log_ask(Confirm::Reset(x, "--keep")));
            let x = h.clone();
            m = m.item("コミットを取り消す（revert）", move |e| e.log_ask(Confirm::Revert(x)));
            if is_head {
                let x = h.clone();
                m = m.item("コミットを元に戻す（undo）", move |e| e.log_ask(Confirm::Undo(x)));
            }
            m = m.sep();
            let (x, subject) = (short.clone(), c.subject.clone());
            m = m.item("メッセージを編集…", move |e| e.prompt_command(&format!("Greword {x} "), &subject));
            let x = h.clone();
            m = m.item("1 つ前にまとめる（フィックスアップ）", move |e| e.log_ask(Confirm::Squash(x, false)));
            let x = h.clone();
            m = m.item("1 つ前にまとめる（スカッシュ）", move |e| e.log_ask(Confirm::Squash(x, true)));
            let x = h.clone();
            m = m.item("コミットを消す", move |e| e.log_ask(Confirm::Drop(x)));
            let x = h.clone();
            m = m.item(format!("{cur} をここへリベース"), move |e| e.log_ask(Confirm::Rebase(x)));
            if let Some(up) = upstream {
                let x = h.clone();
                m = m.item(format!("ここまでプッシュ（{up}）"), move |e| e.log_ask(Confirm::PushTo(x, up)));
            }
            m = m.sep();
            m = m.item("親のコミットへ", |e| e.log_goto_parent());
            m
        };
        self.open_menu(menu);
        true
    }

    /// 真ん中の列の (row, col) が、複数選んだコミットの行か（そうなら、その行を今の行にする）
    fn log_marked_at(&mut self, row: usize, col: usize, w: usize) -> bool {
        let (bw, dw) = self.log_cols_for(w);
        let Some(l) = &mut self.log else { return false };
        let cx = bw + 1;
        let cw = w.saturating_sub(bw + dw + 2);
        if l.marked.len() < 2 || row == 0 || col < cx || col >= cx + cw {
            return false;
        }
        let hit = crate::list::row_at(l.top, row, 1, l.visible.len()).filter(|&k| l.commit_at(k).is_some_and(|c| l.marked.contains(&c.hash)));
        if crate::list::pick(&mut l.selected, hit, 1, 2).is_none() {
            return false;
        }
        l.focus_files = false;
        l.filter = None;
        self.set_state(State::Log);
        true
    }

    /// 複数選んだコミットへのメニュー
    fn log_context_picked(&mut self, at: (usize, usize)) -> bool {
        let Some(l) = &self.log else { return false };
        let picked = l.picked();
        let n = picked.len();
        let cur = self.git.status.as_ref().map(|s| s.branch.clone()).unwrap_or_default();
        let mut m = crate::menu::Menu::new(at.0, at.1);
        m = m.item(format!("{n} 件のハッシュをコピー"), |e| e.log_copy_picked(false));
        m = m.item(format!("{n} 件のハッシュと題名をコピー"), |e| e.log_copy_picked(true));
        if n == 2 {
            let (new, old) = (picked[0].hash.clone(), picked[1].hash.clone());
            m = m.item("2 つのコミットを比べる", move |e| e.log_compare_two(&old, &new));
        }
        m = m.sep();
        m = m.item(format!("{n} 件を {cur} にチェリーピック"), |e| e.log_cherry_pick_picked());
        let hs: Vec<String> = picked.iter().map(|c| c.hash.clone()).collect();
        m = m.item(format!("{n} 件を取り消す（revert）"), move |e| e.log_ask(Confirm::RevertMany(hs)));
        m = m.sep();
        m = m.item("選びを外す", |e| {
            if let Some(l) = &mut e.log {
                l.marked.clear();
                l.mark_anchor = None;
            }
        });
        self.open_menu(m);
        true
    }

    /// 選んでいるコミットをチェリーピックする（複数なら古い順に）
    fn log_cherry_pick_picked(&mut self) {
        let Some(l) = &self.log else { return };
        let mut hs: Vec<String> = l.picked().into_iter().map(|c| c.hash).collect();
        hs.reverse();
        match hs.len() {
            0 => {}
            1 => self.log_ask(Confirm::CherryPick(hs.remove(0))),
            _ => self.log_ask(Confirm::CherryPickMany(hs)),
        }
    }

    /// 選んでいるコミットのハッシュ（`subject` なら短いハッシュと題名）を、1 行に 1 つずつコピーする
    fn log_copy_picked(&mut self, subject: bool) {
        let Some(l) = &self.log else { return };
        let picked = l.picked();
        if picked.is_empty() {
            return;
        }
        let text: Vec<String> = picked.iter().map(|c| if subject { format!("{} {}", c.short, c.subject) } else { c.hash.clone() }).collect();
        let what = match (subject, picked.len()) {
            (false, 1) => "ハッシュ".to_string(),
            (true, 1) => "ハッシュと題名".to_string(),
            (false, n) => format!("{n} 件のハッシュ"),
            (true, n) => format!("{n} 件のハッシュと題名"),
        };
        self.copy_text(text.join("\n"), &what);
    }

    /// 2 つのコミットを比べる: 右の列に、`old` から `new` までに変わったファイルを出す（Enter・ダブルクリックでその差分）
    fn log_compare_two(&mut self, old: &str, new: &str) {
        let waker = self.waker.clone();
        let Some(root) = self.git.root.clone() else { return };
        let Some(l) = &mut self.log else { return };
        // 右の列は今の行のコミットの中身なので、新しい方を今の行に
        if let Some(k) = l.visible.iter().position(|&i| l.rows[i].commit.as_ref().is_some_and(|c| c.hash == new)) {
            l.selected = k;
        }
        l.detail = None;
        l.focus_files = false;
        let (old, new) = (old.to_string(), new.to_string());
        l.rx_detail = spawn(waker, move || {
            let out = nagi_workers::git::git(&root, &["diff", "--name-status", &old, &new])?;
            let s = |h: &str| h[..7.min(h.len())].to_string();
            let head = format!("{new}\x1f\x1f\x1f{old}\x1f{} から {} までに変わったファイル\x1e\n", s(&old), s(&new));
            Ok(Detail { base: Some(old.clone()), ..parse_detail(&format!("{head}{}", String::from_utf8_lossy(&out))) })
        });
    }

    /// 選んでいるコミットの（1 つ目の）親の行へ
    fn log_goto_parent(&mut self) {
        let rows = self.log_rows();
        let Some(l) = &mut self.log else { return };
        let Some(p) = l.detail.as_ref().and_then(|d| d.parents.split(' ').next().map(String::from)).filter(|p| !p.is_empty()) else { return };
        if let Some(k) = l.visible.iter().position(|&i| l.rows[i].commit.as_ref().is_some_and(|c| c.hash == p)) {
            l.selected = k;
            crate::list::fit(k, &mut l.top, rows);
        }
    }

    /// `:Greword ハッシュ 新しいメッセージ`
    pub(crate) fn git_reword(&mut self, arg: &str) {
        let Some(root) = self.git.root.clone() else { return };
        let Some((h, msg)) = arg.trim().split_once(' ') else {
            self.notify.info("ハッシュと新しいメッセージを書いてください");
            return;
        };
        match nagi_workers::git::reword(&root, h, msg.trim()) {
            Ok(()) => self.notify.info("メッセージを直しました"),
            Err(e) => self.notify.error(format!("git: {}", nagi_workers::git::reason(&e))),
        }
        self.refresh_git();
        self.reload_log();
    }

    /// `:Gtag 名前 [ハッシュ]`
    pub(crate) fn git_tag(&mut self, arg: &str) {
        let mut it = arg.split_whitespace();
        let Some(name) = it.next() else {
            self.notify.info("タグの名前を書いてください");
            return;
        };
        let mut args = vec!["tag", name];
        args.extend(it.next());
        if self.git_now(&args, format!("タグ {name} を付けました")) {
            self.reload_log();
        }
    }

    /// `:Grename 今の名前 新しい名前`
    pub(crate) fn git_rename_branch(&mut self, arg: &str) {
        let v: Vec<&str> = arg.split_whitespace().collect();
        let [old, new] = v[..] else {
            self.notify.info("今の名前と新しい名前を書いてください");
            return;
        };
        if self.git_now(&["branch", "-m", old, new], format!("{old} を {new} に変えました")) {
            self.reload_log();
        }
    }

    pub(crate) fn close_log(&mut self) {
        self.log = None;
        if self.state() == State::Log {
            self.set_state(State::Normal);
        }
    }

    /// 選んだコミットの、選んだファイルの差分（1 つ前の版 ↔ このコミット）
    fn log_open_file_diff(&mut self) {
        let (Some(root), Some(l)) = (self.git.root.clone(), &self.log) else { return };
        let Some(d) = &l.detail else { return };
        let Some((_, path)) = d.files.get(l.file_sel).cloned() else { return };
        let h = d.hash.clone();
        // 2 つのコミットを比べているときは古い方と、でなければ（1 つ目の）親と
        let parent = d.base.clone().or_else(|| d.parents.split(' ').next().filter(|p| !p.is_empty()).map(String::from));
        let old = parent.as_deref().and_then(|p| nagi_workers::git::blob_at(&root, p, &path)).unwrap_or_default();
        let new = nagi_workers::git::blob_at(&root, &h, &path).unwrap_or_default();
        let short = &h[..7.min(h.len())];
        let left = match &d.base {
            Some(b) => b[..7.min(b.len())].to_string(),
            None => format!("{short}^"),
        };
        self.show_diff(format!("{left}  {path}"), format!("{short}  {path}"), &old, &new, None);
        self.diff_return = Some(State::Log);
    }

    /// コミット `h` の版の `path` と、作業中のファイルを比べる（右が作業中）
    fn log_compare_worktree(&mut self, h: &str, path: &str) {
        let Some(root) = self.git.root.clone() else { return };
        let old = nagi_workers::git::blob_at(&root, h, path).unwrap_or_default();
        let new = std::fs::read_to_string(root.join(path)).unwrap_or_default();
        let short = &h[..7.min(h.len())];
        self.show_diff(format!("{short}  {path}"), format!("作業中  {path}"), &old, &new, None);
        self.diff_return = Some(State::Log);
    }

    /// 作業ツリーの `path` を開く（無ければ知らせる）
    fn log_open_worktree_file(&mut self, path: &str) {
        let Some(root) = self.git.root.clone() else { return };
        let p = root.join(path);
        if !p.exists() {
            self.notify.warn(format!("{path} は作業ツリーにありません"));
            return;
        }
        self.edit_file(p);
        self.set_state(State::Normal);
    }

    /// `path`（根からの / 区切り）の履歴を開く
    fn open_log_file(&mut self, path: &str) {
        let Some(root) = self.git.root.clone() else { return };
        self.log = None;
        self.load_log(root, Some(path.to_string()), None);
    }

    /// `:Gbranch 名前 [起点]`: ブランチを作ってチェックアウトする
    pub(crate) fn git_new_branch(&mut self, arg: &str) {
        let Some(root) = self.git.root.clone() else { return };
        let mut it = arg.split_whitespace();
        let Some(name) = it.next() else {
            self.notify.info("ブランチの名前を書いてください");
            return;
        };
        let mut args = vec!["checkout", "-q", "-b", name];
        if let Some(start) = it.next() {
            args.push(start);
        }
        match nagi_workers::git::git(&root, &args) {
            Ok(_) => self.notify.info(format!("ブランチ {name} を作ってチェックアウトしました")),
            Err(e) => self.notify.error(format!("git: {}", nagi_workers::git::reason(&e))),
        }
        self.refresh_git();
        // 履歴を出していれば読み直す（新しいブランチが見える）
        self.reload_log();
    }
}



/// この部品の入口（component.rs）
pub struct LogComponent;

impl crate::component::Component for LogComponent {
    fn status(&self, ed: &crate::editor::Editor) -> Option<crate::component::Status> {
        let name = match ed.log.as_ref().filter(|l| l.filter.is_some()) {
            Some(l) => format!("/{}", l.filter_text),
            None => "-- 履歴 --".into(),
        };
        crate::component::Status::keys(name, "j/k 移動  / 絞り込み  Tab ファイル  Enter 差分  o チェックアウト  b ブランチ  c チェリーピック  y ハッシュ  q 閉じる ", "↑↓ 選ぶ  ダブルクリック ファイル  右クリック 操作  Esc 窓へ ")
    }

    fn key(&self, ed: &mut crate::editor::Editor, key: nagi_vim::Key) -> crate::editor::Effect {
        ed.log_key(key);
        crate::editor::Effect::None
    }

    /// 絞り込みの欄に打っている間の文字
    fn text(&self, ed: &mut crate::editor::Editor, text: &str) {
        if let Some(l) = &mut ed.log {
            if let Some(f) = &mut l.filter {
                f.insert_text(text);
                l.filter_text = f.text();
                l.refilter_pub();
            }
        }
    }
}


/// 履歴の画面（layout.rs の View）
pub struct LogScreenView;

impl crate::layout::View for LogScreenView {
    fn rect(&self, ed: &crate::editor::Editor, s: crate::layout::Screen) -> Option<crate::panes::Rect> {
        let _ = s;
        if ed.diff.is_some() || !ed.log.as_ref().is_some_and(|l| !l.docked) {
            return None;
        }
        let (x, w) = ed.windows_x();
        Some(crate::panes::Rect { x, y: 0, w, h: ed.windows_rows() })
    }

    fn draw(&self, ed: &crate::editor::Editor, p: &mut crate::view::Painter, r: crate::panes::Rect, s: crate::layout::Screen) {
        let _ = (r, s);
        if let Some(l) = &ed.log {
            draw::draw_log_view(p, l, r, ed.log_cols_for(r.w), &ed.preedit, ed.git.status.as_ref().map(|s| (s.branch.as_str(), s.ahead as usize, s.behind as usize)));
        }
    }

    fn click(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, row: usize, col: usize, clicks: u32) -> crate::layout::Click {
        let _ = (col, clicks);
        // 窓を覆って出ている履歴: 下の欄と同じ割り付けで選ぶ
        ed.log_click(row - r.y, col - r.x, r.w, clicks);
        crate::layout::Click::Done
    }

    fn context(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, row: usize, col: usize) -> bool {
        ed.log_context(row - r.y, col - r.x, r.w, (row, col))
    }

    fn wheel(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, row: usize, col: usize, dy: isize, dx: isize) -> bool {
        let _ = row;
        ed.log_wheel(dy, dx, r.w, col - r.x);
        true
    }
}

/// 描き方（view.rs の Painter で描く）
pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    /// 履歴（窓の領域全体）。左にブランチ、中央にグラフ付きのコミット、右に選んだコミットの中身
    /// `ab` は今のブランチの (名前, 送るコミット数, 受け取るコミット数)
    pub(crate) fn draw_log_view(p: &mut Painter, l: &crate::log_view::LogView, r: Rect, (bw, dw): (usize, usize), preedit: &str, ab: Option<(&str, usize, usize)>) {
        const LANES: [[f32; 4]; 5] = [rgb(0x87afd7), rgb(0xd7af87), rgb(0x87af87), rgb(0xd787af), rgb(0xafafd7)];
        // 複数選んだコミットの行
        const MARKED_BG: [f32; 4] = rgb(0x264f78);
        let cell = p.fonts.cell();
        let (x0, y0) = (r.x as f32 * cell.w, r.y as f32 * cell.h);
        p.out.push(Instance::fill(x0, y0, r.w as f32 * cell.w, r.h as f32 * cell.h, BG));
        let cx = r.x + bw + 1;
        let cw = r.w.saturating_sub(bw + dw + 2);
        let dx = cx + cw + 1;
        // 上の行: 絞り込みと件数
        let n = l.visible.iter().filter(|&&i| l.rows[i].commit.is_some()).count();
        let head = format!(
            " {}{}   {} 件{}{}",
            if l.filter_text.is_empty() { "絞り込み（/）".to_string() } else { format!("/{}", l.filter_text) },
            l.path.as_ref().map_or(String::new(), |p| format!("   ファイル: {p}")),
            n,
            if l.marked.is_empty() { String::new() } else { format!("   {} 件を選択（右クリックで操作・Esc で外す）", l.marked.len()) },
            if l.loading { " …" } else { "" }
        );
        if let Some(f) = &l.filter {
            let c = p.text(" /", r.x, y0, FG);
            let end = draw_vim_field(p, f, c, y0, preedit);
            p.text(&format!("   {n} 件"), end, y0, STATUS_FG);
        } else {
            p.text(&head, r.x, y0, STATUS_FG);
        }
        // 右上: 今のブランチと、送る（↑）・受け取る（↓）コミットの数（受け取る数は最後にフェッチした時点）
        if let Some((name, ahead, behind)) = ab {
            let s = format!("{name}  ↑{ahead} 送る  ↓{behind} 受け取る ");
            let x = (r.x + r.w).saturating_sub(p.width(&s));
            let c = p.text(&format!("{name}  "), x, y0, FG);
            let c = p.text(&format!("↑{ahead}"), c, y0, if ahead > 0 { GIT_ADD } else { STATUS_FG });
            let c = p.text(" 送る  ", c, y0, STATUS_FG);
            let c = p.text(&format!("↓{behind}"), c, y0, if behind > 0 { DIAG_WARN } else { STATUS_FG });
            p.text(" 受け取る ", c, y0, STATUS_FG);
        }
        p.out.push(Instance::fill(x0, y0 + cell.h - 1.0, r.w as f32 * cell.w, 1.0, LINE_NO));
        let body = r.h.saturating_sub(1);
        // 左: ブランチ
        let saved = p.cols;
        p.cols = r.x + bw;
        // 見出し（ローカル・リモート・タグ）とブランチを、先頭の行（branch_top）から
        let rows = l.branch_rows();
        for (k, item) in rows.iter().skip(l.branch_top).take(body).enumerate() {
            let y = y0 + (1 + k) as f32 * cell.h;
            match item {
                None => {
                    // 見出し: 次のブランチの節
                    let sec = rows[l.branch_top + k + 1..].iter().flatten().next().map_or("", |&i| l.branches[i].0.as_str());
                    p.text(&format!(" {sec}"), r.x, y, STATUS_FG);
                }
                Some(i) => {
                    let (_, name, cur) = &l.branches[*i];
                    if l.branch_sel == Some(*i) {
                        p.out.push(Instance::fill(x0, y, bw as f32 * cell.w, cell.h, MATCH_BG));
                    }
                    p.text(&format!(" {} {name}", if *cur { '*' } else { ' ' }), r.x, y, if *cur { FG } else { STATUS_FG });
                }
            }
        }
        p.cols = saved;
        p.out.push(Instance::fill((r.x + bw) as f32 * cell.w + cell.w * 0.5, y0 + cell.h, 1.0, body as f32 * cell.h, LINE_NO));
        // 中央: コミット
        let saved = p.cols;
        p.cols = cx + cw;
        for (k, &i) in l.visible.iter().enumerate().skip(l.top).take(body) {
            let row = &l.rows[i];
            let y = y0 + (1 + k - l.top) as f32 * cell.h;
            let sel = k == l.selected;
            let focused = sel && !l.focus_files;
            let marked = row.commit.as_ref().is_some_and(|c| l.marked.contains(&c.hash));
            if focused {
                p.out.push(Instance::fill(cx as f32 * cell.w, y, cw as f32 * cell.w, cell.h, LIST_SEL_BG));
            } else if marked {
                p.out.push(Instance::fill(cx as f32 * cell.w, y, cw as f32 * cell.w, cell.h, MARKED_BG));
            } else if sel {
                p.out.push(Instance::fill(cx as f32 * cell.w, y, cw as f32 * cell.w, cell.h, MATCH_BG));
            }
            // グラフ・ref・題名を 1 つに並べ、横に送った分を落としてから描く
            let graph = crate::log_view::pretty_graph(&row.graph);
            let mut items: Vec<Item> = Vec::new();
            for (j, (_, g)) in graph.grapheme_indices(true).enumerate() {
                let color = if focused { FG_BRIGHT } else { LANES[(j / 2) % LANES.len()] };
                items.push(Item { g: Cow::Owned(g.to_string()), width: p.cell_width(g), fg: color, bg: None, underline: false });
            }
            let Some(cm) = &row.commit else {
                skip_cols(&mut items, l.left, &mut None);
                p.draw(&items, cx + 1, y);
                continue;
            };
            {
                let (fg, dim, refc) = if focused { (FG_BRIGHT, FG_BRIGHT, FG_BRIGHT) } else { (FG, STATUS_FG, rgb(0xd7af87)) };
                // 作者と日時は表示の幅で揃える（全角は 2 セル）
                let pad = |p: &Painter, s: &str, w: usize| {
                    let t: String = s.chars().scan(0, |acc, ch| {
                        *acc += p.width(ch.encode_utf8(&mut [0u8; 4]));
                        (*acc <= w).then_some(ch)
                    }).collect();
                    let tw = p.width(&t);
                    format!("{t}{}", " ".repeat(w.saturating_sub(tw)))
                };
                let right = format!("{} {} {}", pad(p, &cm.author, 10), pad(p, &cm.date, 9), cm.short);
                let rw = p.width(&right);
                items.extend(p.items(" ", fg).into_iter().map(|i| Item { g: Cow::Owned(i.g.into_owned()), ..i }));
                if !cm.refs.is_empty() {
                    items.extend(p.items(&format!("{}  ", cm.refs), refc).into_iter().map(|i| Item { g: Cow::Owned(i.g.into_owned()), ..i }));
                }
                items.extend(p.items(&cm.subject, fg));
                let limit = (cx + cw).saturating_sub(rw + 1);
                skip_cols(&mut items, l.left, &mut None);
                let saved2 = p.cols;
                p.cols = limit.saturating_sub(1);
                p.draw(&items, cx + 1, y);
                p.cols = saved2;
                p.text(&right, limit, y, dim);
            }
        }
        p.cols = saved;
        p.out.push(Instance::fill((cx + cw) as f32 * cell.w + cell.w * 0.5, y0 + cell.h, 1.0, body as f32 * cell.h, LINE_NO));
        // 右: 選んだコミットの中身
        let saved = p.cols;
        p.cols = r.x + r.w;
        if let Some(d) = &l.detail {
            let mut y = y0 + cell.h;
            let line = |p: &mut Painter, y: &mut f32, s: &str, color: [f32; 4]| {
                p.text(s, dx + 1, *y, color);
                *y += cell.h;
            };
            let short = |h: &str| h[..7.min(h.len())].to_string();
            if let Some(b) = &d.base {
                // 2 つのコミットの比べ（行の数は 1 つのコミットのときと同じ。押した行の数え方を変えない）
                line(p, &mut y, &format!("比較  {} → {}", short(b), short(&d.hash)), STATUS_FG);
                line(p, &mut y, "左が古い方、右が新しい方", STATUS_FG);
            } else {
                line(p, &mut y, &format!("{}   {}   {}", short(&d.hash), d.author, d.date), STATUS_FG);
                line(p, &mut y, &format!("親 {}", d.parents.split(' ').map(short).collect::<Vec<_>>().join(" ")), STATUS_FG);
            }
            y += cell.h * 0.5;
            for (k, m) in d.message.lines().enumerate().take(8) {
                line(p, &mut y, m, if k == 0 { rgb(0xe8e8e8) } else { STATUS_FG });
            }
            y += cell.h * 0.5;
            line(p, &mut y, &format!("変えたファイル（{}）", d.files.len()), STATUS_FG);
            let limit = y0 + r.h as f32 * cell.h;
            for (k, (st, path)) in d.files.iter().enumerate() {
                if y + cell.h > limit {
                    break;
                }
                let sel = l.focus_files && k == l.file_sel;
                if sel {
                    p.out.push(Instance::fill(dx as f32 * cell.w, y, (r.x + r.w - dx) as f32 * cell.w, cell.h, LIST_SEL_BG));
                }
                let sc = if sel {
                    BG
                } else {
                    match st {
                        'A' => GIT_ADD,
                        'D' => GIT_DEL,
                        'M' => GIT_MOD,
                        _ => rgb(0xd7af5f),
                    }
                };
                let c = p.text(&format!("{st}"), dx + 1, y, sc);
                p.text(&format!("  {path}"), c, y, if sel { FG_BRIGHT } else { FG });
                y += cell.h;
            }
        } else if !l.rows.is_empty() {
            p.text("…", dx + 1, y0 + cell.h, STATUS_FG);
        }
        p.cols = saved;
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

    /// 本文へ移った後でも、Git の欄の行を押せば選び直せる
    #[test]
    fn click_selects_commit_and_file() {
        let mut e = Editor::new(nagi_core::Buffer::from_str("x"), None);
        e.set_screen(120, 40);
        let rows = parse_log("*\x1fa1\x1f\x1f一\x1fA\x1f1\x1fa1\n|\n*\x1fb2\x1f\x1f二\x1fA\x1f2\x1fb2\n");
        e.log = Some(LogView {
            left: 0,
            branch_sel: None,
            branch_top: 0,
            src_path: None,
            src_range: None,
            docked: true,
            visible: (0..rows.len()).collect(),
            rows,
            selected: 0,
            marked: Vec::new(),
            mark_anchor: None,
            top: 0,
            branches: Vec::new(),
            detail: Some(parse_detail("a1\x1fA\x1f1\x1fp\x1f件名\x1e\n\nM\ta.rs\nA\tb.rs\n")),
            focus_files: false,
            file_sel: 0,
            filter_text: String::new(),
            filter: None,
            path: None,
            loading: false,
            rx_log: None,
            rx_branches: None,
            rx_detail: None,
        });
        e.select_bottom_tab(crate::result_grid::BottomKind::Git);
        e.set_state(State::Normal);
        // 真ん中の 3 行目（グラフだけの行の次）を押す。欄の 0 行目はタブ、1 行目は絞り込みの見出し
        e.bottom_click(4, 60, 1);
        assert_eq!(e.state(), State::Log);
        assert_eq!(e.log.as_ref().unwrap().selected, 2);
        // グラフだけの行は選ばない
        e.bottom_click(3, 60, 1);
        assert_eq!(e.log.as_ref().unwrap().selected, 2);
        // 本文へ戻ってから、右の変えたファイル（見出し 2 行・件名 1 行・「変えたファイル」の次）を押す
        e.set_state(State::Normal);
        e.bottom_click(1 + 5 + 1 + 1, 110, 1);
        let l = e.log.as_ref().unwrap();
        assert_eq!((e.state(), l.focus_files, l.file_sel), (State::Log, true, 1));
        // 真ん中を押せばコミットの選びへ戻る
        e.bottom_click(2, 60, 1);
        let l = e.log.as_ref().unwrap();
        assert_eq!((l.focus_files, l.selected), (false, 0));
    }

    /// 本物のリポジトリで: コミットを選ぶと右の中身がそのコミットになる（裏の仕事が無くても、描く前に取りに行く）・
    /// ブランチの列はホイールで送れる・列の間の線をドラッグで動かせる
    #[test]
    fn detail_follows_and_columns() {
        let dir = std::env::temp_dir().join(format!("nagi-logcols-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| String::from_utf8_lossy(&nagi_workers::git::git(&dir, args).unwrap()).trim().to_string();
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "test@example.com"]);
        g(&["config", "user.name", "test"]);
        for f in ["a", "b", "c"] {
            std::fs::write(dir.join(f), format!("{f}\n")).unwrap();
            g(&["add", "."]);
            g(&["commit", "-q", "-m", &format!("add {f}")]);
        }
        for k in 0..40 {
            g(&["branch", &format!("b{k:02}")]);
        }
        let mut buf = nagi_core::Buffer::open(&dir.join("a")).unwrap();
        buf.path = Some(dir.join("a"));
        let mut e = Editor::new(buf, None);
        e.set_screen(160, 40);
        e.start_git();
        e.open_log(false);
        let wait = |e: &mut Editor, what: &dyn Fn(&Editor) -> bool| {
            for _ in 0..500 {
                e.poll_background();
                if what(e) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            panic!("届かない");
        };
        wait(&mut e, &|e| e.log.as_ref().is_some_and(|l| !l.loading && !l.branches.is_empty() && l.detail.is_some()));
        // 2 つ目のコミットを押す → 描く前の取りに行き（view::build が呼ぶ）で、右の中身がそのコミットになる
        let r = e.log_rect().unwrap();
        let (bw, _) = e.log_cols_for(r.w);
        e.log_click(2, bw + 5, r.w, 1);
        let want = e.log.as_ref().unwrap().current().unwrap().hash.clone();
        assert_ne!(e.log.as_ref().unwrap().detail.as_ref().unwrap().hash, want);
        e.log_request_detail();
        assert!(e.log.as_ref().unwrap().rx_detail.is_some(), "選んだらすぐ取りに行く");
        wait(&mut e, &|e| e.log.as_ref().unwrap().detail.as_ref().is_some_and(|d| d.hash == want));
        // ブランチの列はホイールで送る（41 本で画面に入らない）。送った分だけ、押した行のブランチもずれる
        let first = e.log.as_ref().unwrap().branch_at(0);
        e.log_wheel(5, 0, r.w, 1);
        let l = e.log.as_ref().unwrap();
        assert_eq!(l.branch_top, 5);
        assert_ne!(l.branch_at(0), first);
        assert_eq!(l.branch_at(0), l.branch_rows()[5]);
        e.log_wheel(-100, 0, r.w, 1);
        assert_eq!(e.log.as_ref().unwrap().branch_top, 0);
        // 列の間の線をつかんで動かす（ダブルクリックで元に戻す）
        let (bw, dw) = e.log_cols_for(r.w);
        let sep = e.sep_at(r.y + 3, r.x + bw).expect("ブランチの列の右の線");
        assert!(sep.horizontal_drag());
        e.drag_sep(&sep, r.y + 3, r.x + 40);
        assert_eq!(e.log_cols_for(r.w).0, 40);
        let sep2 = e.sep_at(r.y + 3, r.x + r.w - dw - 1).expect("中身の列の左の線");
        e.drag_sep(&sep2, r.y + 3, r.x + r.w - 61);
        assert_eq!(e.log_cols_for(r.w).1, 60);
        assert!(e.sep_at(r.y, r.x + 40).is_none(), "見出しの行ではつかまない");
        e.reset_sep(&sep);
        e.reset_sep(&sep2);
        assert_eq!(e.log_cols_for(r.w), (bw, dw));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 本物のリポジトリで: ブランチの列を押して選ぶ・ダブルクリックでそのブランチの履歴・右クリックでチェックアウト・本文を押すと戻る・右クリックのメニューで消す
    #[test]
    fn branches_and_commit_menu() {
        let dir = std::env::temp_dir().join(format!("nagi-logops-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| String::from_utf8_lossy(&nagi_workers::git::git(&dir, args).unwrap()).trim().to_string();
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "test@example.com"]);
        g(&["config", "user.name", "test"]);
        g(&["config", "core.autocrlf", "false"]);
        for f in ["a", "b", "c"] {
            std::fs::write(dir.join(f), format!("{f}\n")).unwrap();
            g(&["add", "."]);
            g(&["commit", "-q", "-m", &format!("add {f}")]);
            if f == "a" {
                g(&["branch", "topic"]);
            }
        }
        let mut buf = nagi_core::Buffer::open(&dir.join("a")).unwrap();
        buf.path = Some(dir.join("a"));
        let mut e = Editor::new(buf, None);
        e.set_screen(120, 40);
        e.start_git();
        let wait = |e: &mut Editor| {
            for _ in 0..500 {
                e.poll_background();
                if e.log.as_ref().is_some_and(|l| !l.loading && !l.branches.is_empty() && l.rx_detail.is_none()) && e.git.status.is_some() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            panic!("履歴が届かない");
        };
        e.select_bottom_tab(crate::result_grid::BottomKind::Git);
        wait(&mut e);
        let l = e.log.as_ref().unwrap();
        // ブランチの列: 0 行目は「ローカル」の見出し、1 行目 main、2 行目 topic
        assert_eq!((l.branch_at(0), l.branch_at(1), l.branch_at(2)), (None, Some(0), Some(1)));
        // 欄の 0 行目はタブ、1 行目は見出し、2 行目から中身。topic を押すと、その指すコミットへ
        e.bottom_click(4, 2, 1);
        let l = e.log.as_ref().unwrap();
        assert_eq!(l.branch_sel, Some(1));
        assert_eq!(l.current().map(|c| c.subject.as_str()), Some("add a"));
        // ダブルクリックで、そのブランチの履歴だけ（チェックアウトはしない）
        e.bottom_click(4, 2, 2);
        wait(&mut e);
        assert_eq!(g(&["rev-parse", "--abbrev-ref", "HEAD"]), "main", "チェックアウトしない");
        let subjects = |e: &Editor| e.log.as_ref().unwrap().rows.iter().filter_map(|r| r.commit.as_ref().map(|c| c.subject.clone())).collect::<Vec<_>>();
        assert_eq!(subjects(&e), vec!["add a"], "topic の履歴だけ");
        assert_eq!(e.log.as_ref().unwrap().branch_sel, Some(1));
        // もう一度ダブルクリックで全部に戻る（下の欄に収めたまま）
        e.bottom_click(4, 2, 2);
        wait(&mut e);
        assert_eq!(subjects(&e).len(), 3);
        assert!(e.log.as_ref().unwrap().docked);
        // チェックアウトは右クリックのメニューから
        let w = e.bottom_content_rect().unwrap().w;
        assert!(e.log_context(3, 2, w, (5, 2)));
        let i = e.menu.as_ref().unwrap().items.iter().position(|it| it.label == "チェックアウト").unwrap();
        e.menu_run(i);
        assert_eq!(g(&["rev-parse", "--abbrev-ref", "HEAD"]), "topic", "{}", e.notify.text);
        wait(&mut e);
        // 本文を押すとエディタへ（どのペインからでも）
        assert_eq!(e.state(), State::Log);
        let r = e.active_rect();
        e.click(0, r.x + e.gutter());
        assert_eq!(e.state(), State::Normal);
        // main に戻って、真ん中の 2 つ目のコミット（add b）を右クリック →「コミットを消す」→ y
        g(&["checkout", "-q", "main"]);
        e.refresh_git();
        e.reload_log();
        wait(&mut e);
        let k = e.log.as_ref().unwrap().visible.iter().position(|&i| e.log.as_ref().unwrap().rows[i].commit.as_ref().is_some_and(|c| c.subject == "add b")).unwrap();
        let w = e.bottom_content_rect().unwrap().w;
        assert!(e.log_context(1 + k, 60, w, (10, 60)));
        assert_eq!(e.state(), State::Menu);
        let i = e.menu.as_ref().unwrap().items.iter().position(|it| it.label == "コミットを消す").unwrap();
        e.menu_run(i);
        // 確認の窓（はい・いいえ）。y で答える
        assert_eq!(e.state(), State::Menu);
        assert!(e.menu.as_ref().unwrap().title.as_deref().is_some_and(|t| t.contains("消しますか")));
        e.key(Key::Char('y'));
        assert_eq!(e.state(), State::Log, "答えたら履歴へ戻る");
        assert_eq!(g(&["log", "--format=%s"]), "add c\nadd a", "{}", e.notify.text);
        // メッセージを編集（:Greword）
        let head = g(&["rev-parse", "--short", "HEAD"]);
        e.git_reword(&format!("{head} add c!"));
        assert_eq!(g(&["log", "-1", "--format=%s"]), "add c!");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 本物のリポジトリで: Ctrl+クリックで足す・Shift+クリックで範囲・Esc で外す・右クリックで選んだもの全部へ
    /// （2 つを比べる・まとめてチェリーピック）
    #[test]
    fn multi_select_commits() {
        let dir = std::env::temp_dir().join(format!("nagi-logmulti-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| String::from_utf8_lossy(&nagi_workers::git::git(&dir, args).unwrap()).trim().to_string();
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "test@example.com"]);
        g(&["config", "user.name", "test"]);
        g(&["config", "core.autocrlf", "false"]);
        for f in ["a", "b", "c"] {
            std::fs::write(dir.join(f), format!("{f}\n")).unwrap();
            g(&["add", "."]);
            g(&["commit", "-q", "-m", &format!("add {f}")]);
        }
        g(&["checkout", "-q", "-b", "side", "HEAD~2"]);
        std::fs::write(dir.join("d"), "d\n").unwrap();
        g(&["add", "."]);
        g(&["commit", "-q", "-m", "add d"]);
        let mut buf = nagi_core::Buffer::open(&dir.join("a")).unwrap();
        buf.path = Some(dir.join("a"));
        let mut e = Editor::new(buf, None);
        e.set_screen(120, 40);
        e.start_git();
        let wait = |e: &mut Editor, what: &dyn Fn(&Editor) -> bool| {
            for _ in 0..500 {
                e.poll_background();
                if what(e) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            panic!("届かない");
        };
        let ready = |e: &Editor| e.log.as_ref().is_some_and(|l| !l.loading && !l.branches.is_empty() && l.rx_detail.is_none()) && e.git.status.is_some();
        e.select_bottom_tab(crate::result_grid::BottomKind::Git);
        wait(&mut e, &ready);
        // 題名の行（見せている行の番号）。欄の 0 行目はタブ、1 行目は見出しなので、押す行は 2 + k
        let at = |e: &Editor, s: &str| e.log.as_ref().unwrap().visible.iter().position(|&i| e.log.as_ref().unwrap().rows[i].commit.as_ref().is_some_and(|c| c.subject == s)).unwrap();
        let click = |e: &mut Editor, k: usize, mods: nagi_vim::Mods| {
            e.click_mods = mods;
            e.bottom_click(2 + k, 60, 1);
            e.click_mods = Default::default();
        };
        let ctrl = nagi_vim::Mods { ctrl: true, ..Default::default() };
        let shift = nagi_vim::Mods { shift: true, ..Default::default() };
        let subjects = |e: &Editor| e.log.as_ref().unwrap().picked().into_iter().map(|c| c.subject).collect::<Vec<_>>();
        let (ka, kb, kc) = (at(&e, "add a"), at(&e, "add b"), at(&e, "add c"));
        // 押すと 1 つ、Ctrl+クリックで今のものに足す（並びは履歴の順）
        click(&mut e, kc, Default::default());
        assert_eq!(subjects(&e), vec!["add c"]);
        click(&mut e, kb, ctrl);
        assert_eq!(subjects(&e), vec!["add c", "add b"]);
        // Ctrl+クリックをもう一度で外す
        click(&mut e, kb, ctrl);
        assert_eq!(subjects(&e), vec!["add c"]);
        // Shift+クリックで、起点から押したところまで
        click(&mut e, kc, Default::default());
        click(&mut e, ka, shift);
        let l = e.log.as_ref().unwrap();
        let want: Vec<String> = (kc.min(ka)..=kc.max(ka)).filter_map(|k| l.commit_at(k).map(|c| c.subject.clone())).collect();
        assert_eq!(subjects(&e), want);
        assert!(want.len() >= 3);
        // Esc で選びを外す（履歴は閉じない）
        e.key(Key::Esc);
        assert!(e.log.as_ref().unwrap().marked.is_empty());
        assert_eq!(e.state(), State::Log);
        // 2 つを選んで右クリック → 比べる: 右の列に、a から c までに変わったファイル
        click(&mut e, ka, Default::default());
        click(&mut e, kc, ctrl);
        let w = e.bottom_content_rect().unwrap().w;
        assert!(e.log_context(1 + ka, 60, w, (10, 60)));
        let labels: Vec<String> = e.menu.as_ref().unwrap().items.iter().map(|it| it.label.clone()).collect();
        assert!(labels.contains(&"2 件を side にチェリーピック".to_string()), "{labels:?}");
        assert_eq!(e.log.as_ref().unwrap().marked.len(), 2, "右クリックで選びを外さない");
        let i = labels.iter().position(|s| s == "2 つのコミットを比べる").unwrap();
        e.menu_run(i);
        wait(&mut e, &|e| e.log.as_ref().unwrap().detail.as_ref().is_some_and(|d| d.base.is_some()));
        let l = e.log.as_ref().unwrap();
        let d = l.detail.as_ref().unwrap();
        assert_eq!(l.current().unwrap().subject, "add c", "新しい方を今の行に");
        assert_eq!(d.files, vec![('A', "b".to_string()), ('A', "c".to_string())]);
        // b と c を選んで、まとめてチェリーピック（古い順に）
        let (kb, kc) = (at(&e, "add b"), at(&e, "add c"));
        click(&mut e, kb, Default::default());
        click(&mut e, kc, ctrl);
        assert!(e.log_context(1 + kb, 60, w, (10, 60)));
        let i = e.menu.as_ref().unwrap().items.iter().position(|it| it.label == "2 件を side にチェリーピック").unwrap();
        e.menu_run(i);
        assert!(e.menu.as_ref().unwrap().title.as_deref().is_some_and(|t| t.contains("2 件のコミット")));
        e.key(Key::Char('y'));
        assert_eq!(g(&["log", "--format=%s", "side"]), "add c\nadd b\nadd d\nadd a", "{}", e.notify.text);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
