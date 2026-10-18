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

type Rx<T> = Option<nagi_core::task::Task<nagi_core::Result<T>>>;

pub struct LogView {
    /// 下の欄の「Git」タブに収めている（全画面でない）
    pub docked: bool,
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
    /// 絞り込み（決めた文字）と、打っている間の Vim 風の入力欄
    pub filter_text: String,
    pub filter: Option<crate::vim_field::VimField>,
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

fn spawn<T: Send + 'static>(waker: Option<std::sync::Arc<dyn Fn() + Send + Sync>>, f: impl FnOnce() -> nagi_core::Result<T> + Send + 'static) -> Rx<T> {
    let rx = nagi_core::task::spawn("git-log", waker.unwrap_or_else(nagi_core::task::no_waker), move |ctx| {
        ctx.send(f());
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
            docked: false,
            rows: Vec::new(),
            visible: Vec::new(),
            selected: 0,
            top: 0,
            branches: Vec::new(),
            detail: None,
            focus_files: false,
            file_sel: 0,
            filter_text: String::new(),
            filter: None,
            path: path.or(range.map(|r| format!("比較 {r}"))),
            confirm: None,
            loading: true,
            rx_log,
            rx_branches,
            rx_detail: None,
        });
        self.set_state(State::Log);
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
        let docked = self.log.as_ref().is_some_and(|l| l.docked);
        let rows = if docked { self.bottom_content_rect().map_or(5, |r| r.h).saturating_sub(2).max(1) } else { self.windows_rows().saturating_sub(3).max(1) };
        // 下の欄に収めているとき: Esc は本文へ戻るだけ、q は欄を隠す（どちらも履歴は残す）。Ctrl-w k も本文へ
        if docked && self.log.as_ref().is_some_and(|l| l.filter.is_none() && l.confirm.is_none() && !l.focus_files) {
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
        if let Some(c) = l.confirm.take() {
            if key == Key::Char('y') {
                let (args, label): (Vec<String>, &str) = match &c {
                    Confirm::Checkout(h) => (vec!["checkout".into(), "-q".into(), h.clone()], "チェックアウトしました"),
                    Confirm::CherryPick(h) => (vec!["cherry-pick".into(), h.clone()], "チェリーピックしました"),
                };
                if let Some(root) = self.git.root.clone() {
                    let a: Vec<&str> = args.iter().map(String::as_str).collect();
                    match nagi_workers::git::git(&root, &a) {
                        Ok(_) => self.notify.info(label.to_string()),
                        Err(e) => self.notify.error(format!("git: {}", e.last_line())),
                    }
                    self.refresh_git();
                }
            } else {
                self.notify.info("やめました");
            }
            return;
        }
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
            Key::Char('/') => l.filter = Some(crate::vim_field::VimField::new(&l.filter_text, false)),
            Key::Tab | Key::Enter | Key::Char('l') => {
                if l.detail.as_ref().is_some_and(|d| !d.files.is_empty()) {
                    l.focus_files = true;
                }
            }
            Key::Char('o') => {
                if let Some(c) = l.current() {
                    let h = c.hash.clone();
                    self.notify.info(format!("{} をチェックアウトしますか（HEAD を切り離す。y で進める）", &h[..7.min(h.len())]));
                    if let Some(l) = &mut self.log {
                        l.confirm = Some(Confirm::Checkout(h));
                    }
                }
            }
            Key::Char('c') => {
                if let Some(c) = l.current() {
                    let h = c.hash.clone();
                    self.notify.info(format!("{} を今のブランチにチェリーピックしますか（y で進める）", &h[..7.min(h.len())]));
                    if let Some(l) = &mut self.log {
                        l.confirm = Some(Confirm::CherryPick(h));
                    }
                }
            }
            Key::Char('b') => {
                // ここからブランチ: :Gbranch 名前 ハッシュ を打たせる
                if let Some(c) = l.current() {
                    let h = c.short.clone();
                    self.enter_command("");
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
                    self.notify.info(format!("ハッシュをコピーしました: {h}"));
                }
            }
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
        let Some(l) = &mut self.log else { return };
        l.confirm = None;
        if row == 0 {
            return;
        }
        l.filter = None;
        // 描き方（view.rs の draw_log_view）と同じ割り付け
        let bw = 20.min(w / 6);
        let dw = 42.min(w / 3);
        let cx = bw + 1;
        let cw = w.saturating_sub(bw + dw + 2);
        let dx = cx + cw + 1;
        if col >= cx && col < cx + cw {
            let k = l.top + row - 1;
            if k < l.visible.len() && l.rows[l.visible[k]].commit.is_some() {
                l.selected = k;
                l.focus_files = false;
                if clicks >= 2 && l.detail.as_ref().is_some_and(|d| !d.files.is_empty()) {
                    l.focus_files = true;
                }
            }
        } else if col >= dx {
            // 右: 見出し 2 行・本文（8 行まで）・「変えたファイル」の後にファイル
            let Some(d) = &l.detail else { return };
            let first = 5 + d.message.lines().count().min(8);
            if let Some(k) = row.checked_sub(first).filter(|&k| k < d.files.len()) {
                l.focus_files = true;
                l.file_sel = k;
                if clicks >= 2 {
                    self.log_open_file_diff();
                }
            }
        } else {
            l.focus_files = false;
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
        let parent = d.parents.split(' ').next().filter(|p| !p.is_empty()).map(String::from);
        let old = parent.as_deref().and_then(|p| nagi_workers::git::blob_at(&root, p, &path)).unwrap_or_default();
        let new = nagi_workers::git::blob_at(&root, &h, &path).unwrap_or_default();
        let short = &h[..7.min(h.len())];
        self.show_diff(format!("{short}^  {path}"), format!("{short}  {path}"), &old, &new, None);
        self.diff_return = Some(State::Log);
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
            Err(e) => self.notify.error(format!("git: {}", e.last_line())),
        }
        self.refresh_git();
        if self.log.is_some() {
            self.close_log();
        }
    }
}



/// この部品の入口（component.rs）
pub struct LogComponent;

impl crate::component::Component for LogComponent {
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
            draw::draw_log_view(p, l, r, &ed.preedit);
        }
    }
}

/// 描き方（view.rs の Painter で描く）
pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    /// 履歴（窓の領域全体）。左にブランチ、中央にグラフ付きのコミット、右に選んだコミットの中身
    pub(crate) fn draw_log_view(p: &mut Painter, l: &crate::log_view::LogView, r: Rect, preedit: &str) {
        const LANES: [[f32; 4]; 5] = [rgb(0x87afd7), rgb(0xd7af87), rgb(0x87af87), rgb(0xd787af), rgb(0xafafd7)];
        let cell = p.fonts.cell();
        let (x0, y0) = (r.x as f32 * cell.w, r.y as f32 * cell.h);
        p.out.push(Instance::fill(x0, y0, r.w as f32 * cell.w, r.h as f32 * cell.h, BG));
        let bw = 20.min(r.w / 6);
        let dw = 42.min(r.w / 3);
        let cx = r.x + bw + 1;
        let cw = r.w.saturating_sub(bw + dw + 2);
        let dx = cx + cw + 1;
        // 上の行: 絞り込みと件数
        let n = l.visible.iter().filter(|&&i| l.rows[i].commit.is_some()).count();
        let head = format!(
            " {}{}   {} 件{}",
            if l.filter_text.is_empty() { "/ で絞り込み".to_string() } else { format!("/{}", l.filter_text) },
            l.path.as_ref().map_or(String::new(), |p| format!("   ファイル: {p}")),
            n,
            if l.loading { " …" } else { "" }
        );
        if let Some(f) = &l.filter {
            let c = p.text(" /", r.x, y0, FG);
            let end = draw_vim_field(p, f, c, y0, preedit);
            p.text(&format!("   {n} 件"), end, y0, STATUS_FG);
        } else {
            p.text(&head, r.x, y0, STATUS_FG);
        }
        p.out.push(Instance::fill(x0, y0 + cell.h - 1.0, r.w as f32 * cell.w, 1.0, LINE_NO));
        let body = r.h.saturating_sub(1);
        // 左: ブランチ
        let saved = p.cols;
        p.cols = r.x + bw;
        let mut y = y0 + cell.h;
        let mut last_sec = "";
        for (sec, name, cur) in &l.branches {
            if y >= y0 + body as f32 * cell.h {
                break;
            }
            if sec != last_sec {
                p.text(&format!(" {sec}"), r.x, y, STATUS_FG);
                y += cell.h;
                last_sec = sec;
            }
            p.text(&format!(" {} {name}", if *cur { '*' } else { ' ' }), r.x, y, if *cur { FG } else { STATUS_FG });
            y += cell.h;
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
            if focused {
                p.out.push(Instance::fill(cx as f32 * cell.w, y, cw as f32 * cell.w, cell.h, FG));
            } else if sel {
                p.out.push(Instance::fill(cx as f32 * cell.w, y, cw as f32 * cell.w, cell.h, MATCH_BG));
            }
            let mut c = cx + 1;
            for (j, ch) in crate::log_view::pretty_graph(&row.graph).chars().enumerate() {
                let mut b = [0u8; 4];
                let color = if focused { BG } else { LANES[(j / 2) % LANES.len()] };
                c = p.text(ch.encode_utf8(&mut b), c, y, color);
            }
            if let Some(cm) = &row.commit {
                let (fg, dim, refc) = if focused { (BG, BG, BG) } else { (FG, STATUS_FG, rgb(0xd7af87)) };
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
                let mut c2 = c + 1;
                if !cm.refs.is_empty() {
                    c2 = p.text(&format!("{}  ", cm.refs), c2, y, refc);
                }
                let limit = (cx + cw).saturating_sub(rw + 1);
                let saved2 = p.cols;
                p.cols = limit.saturating_sub(1);
                p.text(&cm.subject, c2, y, fg);
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
            line(p, &mut y, &format!("{}   {}   {}", &d.hash[..7.min(d.hash.len())], d.author, d.date), STATUS_FG);
            line(p, &mut y, &format!("親 {}", d.parents.split(' ').map(|h| &h[..7.min(h.len())]).collect::<Vec<_>>().join(" ")), STATUS_FG);
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
                    p.out.push(Instance::fill(dx as f32 * cell.w, y, (r.x + r.w - dx) as f32 * cell.w, cell.h, FG));
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
                p.text(&format!("  {path}"), c, y, if sel { BG } else { FG });
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
            docked: true,
            visible: (0..rows.len()).collect(),
            rows,
            selected: 0,
            top: 0,
            branches: Vec::new(),
            detail: Some(parse_detail("a1\x1fA\x1f1\x1fp\x1f件名\x1e\n\nM\ta.rs\nA\tb.rs\n")),
            focus_files: false,
            file_sel: 0,
            filter_text: String::new(),
            filter: None,
            path: None,
            confirm: None,
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
}
