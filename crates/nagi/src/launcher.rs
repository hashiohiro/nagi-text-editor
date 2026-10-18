//! ランチャー（起動時のプロジェクトの選択。IntelliJ の Welcome の画面にあたる）。
//!
//! 引数なしで起動したときと `:Project` で、画面全体に出す。左に操作、中央に検索の欄と一覧（ピン留め → 最近）、右に
//! 選んだプロジェクトのプレビュー（種類・git のブランチと変更・開くと復元するファイル・除外するフォルダ・README の先頭）。
//! 一覧は Nagi・IntelliJ・WebStorm・Eclipse などの最近のプロジェクトを 1 つにしたもの。欄は Vim 風: 打てば絞り込み、
//! Esc でノーマル（j / k で選ぶ、p ピン留め、dd 一覧から外す、o フォルダを開く、g git から取得、r 最近のファイル、
//! n 空で始める）、Enter で開く、ノーマルでもう一度 Esc を押すと閉じる（空のバッファで始める）。

use std::collections::HashMap;
use std::path::PathBuf;

use nagi_core::fuzzy::Query;
use nagi_vim::Key;

use crate::editor::{Editor, Mode};
use crate::project_open::{Entry, recent_entries};
use crate::vim_field::{FieldOut, VimField};

/// 選んだプロジェクトのプレビュー
#[derive(Clone, Debug, Default)]
pub struct Preview {
    /// 「IntelliJ のプロジェクト」など
    pub kind: String,
    /// Maven のモジュールの名前
    pub modules: Vec<String>,
    /// git: ブランチ・先行・遅れ・変更（状態の文字, パス）・未追跡の数（git でなければ None）
    pub git: Option<(String, usize, usize, Vec<(char, String)>, usize)>,
    /// 開くと復元するファイル（根からの相対パス, 行, 前面か）
    pub restore: Vec<(String, usize, bool)>,
    /// 除外するフォルダ（根からの相対パス）
    pub excludes: Vec<String>,
    /// README の先頭
    pub readme: Vec<String>,
}

/// 下の行で打っているもの（git の URL）。フォルダはダイアログで選ぶ（打つなら :Project パス）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    Clone,
}

pub struct Launcher {
    pub all: Vec<Entry>,
    /// 絞り込んだ結果（all の添字。ピン留め → 最近の順のまま）
    pub hits: Vec<usize>,
    /// hits の中の選んでいる行
    pub selected: usize,
    pub scroll: usize,
    pub field: VimField,
    pub loading: Option<nagi_core::task::Task<Vec<Entry>>>,
    pub previews: HashMap<PathBuf, Preview>,
    pub preview_rx: Option<(PathBuf, nagi_core::task::Task<Preview>)>,
    /// o（フォルダを開く）・g（git から取得）の入力
    pub ask: Option<(Ask, VimField)>,
    /// git clone の実行中（URL, 結果）
    pub cloning: Option<(String, nagi_core::task::Task<nagi_core::Result<PathBuf>>)>,
    pending_d: bool,
    pub message: String,
    /// 描いた一覧の (画面の行, hits の添字)。クリックで使う
    pub row_map: std::cell::RefCell<Vec<(usize, usize)>>,
    /// 描いた左の操作の (画面の行, 操作の文字) と左の欄の幅。クリックで使う
    pub act_map: std::cell::RefCell<(Vec<(usize, char)>, usize)>,
    /// フォルダを選ぶダイアログを出している
    pub picking: Option<nagi_core::task::Task<Option<PathBuf>>>,
    /// ダイアログを開いたとき入力中だったか（閉じたら戻す。ダイアログで押した Esc などが漏れてきても変わらないように）
    pick_insert: bool,
}

/// 左の操作（文字, 名前, 入力中のキー）
pub const ACTIONS: [(char, &str, &str); 5] = [
    ('o', "フォルダを開く…", "^O"),
    ('w', "WSL のフォルダ…", "^L"),
    ('g', "git から取得…", "^G"),
    ('r', "最近のファイル", "^R"),
    ('n', "空で始める", "^E"),
];

impl Launcher {
    pub fn selected_entry(&self) -> Option<&Entry> {
        self.hits.get(self.selected).map(|&i| &self.all[i])
    }

    fn refilter(&mut self) {
        let text = self.field.text();
        let q = Query::new(&text);
        let keep = self.selected_entry().map(|e| e.path.clone());
        // 名前で絞る。/ や \ を打ったら場所でも
        let by_path = text.contains(['/', '\\']);
        let key = |e: &Entry| if by_path { format!("{} {}", e.name, e.path.display()).replace('\\', "/") } else { e.name.clone() };
        let q = if by_path { Query::new(&text.replace('\\', "/")) } else { q };
        self.hits = (0..self.all.len()).filter(|&i| q.is_empty() || q.score(&key(&self.all[i])).is_some()).collect();
        // 絞り込んだら、いちばん合うものを選ぶ（空ならピン留め・最近の順のまま）
        if !q.is_empty() {
            let mut scored: Vec<(usize, i32)> = self.hits.iter().map(|&i| (i, q.score(&key(&self.all[i])).unwrap_or(-1000))).collect();
            scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            self.hits = scored.into_iter().map(|x| x.0).collect();
            self.selected = 0;
        } else {
            self.selected = keep.and_then(|k| self.hits.iter().position(|&i| self.all[i].path == k)).unwrap_or(0);
        }
        self.scroll = 0;
    }

    /// 読み込み元ごとの数（左に出す）
    pub fn sources(&self) -> Vec<(String, usize)> {
        let mut v: Vec<(String, usize)> = Vec::new();
        for e in &self.all {
            match v.iter_mut().find(|(n, _)| *n == e.from) {
                Some(x) => x.1 += 1,
                None => v.push((e.from.clone(), 1)),
            }
        }
        v
    }
}

/// 最後に開いた時刻を「3 分前」「昨日」の形に
pub fn ago(when: u64) -> String {
    if when == 0 {
        return String::new();
    }
    let s = crate::project_open::now_ms().saturating_sub(when) / 1000;
    match s {
        0..60 => "今".into(),
        60..3600 => format!("{} 分前", s / 60),
        3600..86400 => format!("{} 時間前", s / 3600),
        86400..172800 => "昨日".into(),
        172800..1209600 => format!("{} 日前", s / 86400),
        1209600..5184000 => format!("{} 週間前", s / 604800),
        _ => format!("{} か月前", s / 2592000),
    }
}

/// プレビューを作る（裏のスレッドで。git status を動かすので）
fn build_preview(dir: PathBuf) -> Preview {
    let p = nagi_workers::project::detect(&dir);
    let kind = match p.kind {
        nagi_workers::project::Kind::Plain => "フォルダ".to_string(),
        k => format!("{} のプロジェクト", k.label()),
    };
    let modules = std::fs::read_to_string(dir.join("pom.xml"))
        .map(|pom| pom.split("<module>").skip(1).filter_map(|s| s.split("</module>").next()).map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();
    let git = dir.join(".git").exists().then(|| nagi_workers::git::status(&dir).ok()).flatten().map(|st| {
        let untracked = st.changes.iter().filter(|c| c.state == nagi_workers::git::FileState::Untracked).count();
        let changes: Vec<(char, String)> = st.changes.iter().filter(|c| c.state != nagi_workers::git::FileState::Untracked).map(|c| (c.state.letter(), c.path.clone())).collect();
        (st.branch, st.ahead, st.behind, changes, untracked)
    });
    let rel = |x: &std::path::Path| x.strip_prefix(&dir).map_or(x.display().to_string(), |r| r.to_string_lossy().replace('\\', "/"));
    let restore = nagi_workers::project::open_files(&dir).into_iter().map(|f| (rel(&f.path), f.line, f.current)).collect();
    let excludes = p.excludes.iter().map(|x| rel(x)).collect();
    let readme = ["README.md", "readme.md", "README.txt", "README"]
        .iter()
        .find_map(|n| std::fs::read_to_string(dir.join(n)).ok())
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).take(4).map(|l| l.chars().take(80).collect()).collect())
        .unwrap_or_default();
    Preview { kind, modules, git, restore, excludes, readme }
}

impl Editor {
    /// ランチャーを出す（一覧は裏で集める）
    pub(crate) fn open_launcher(&mut self) {
        let rx = nagi_core::task::spawn("launcher", self.waker(), move |ctx| {
            ctx.send(recent_entries());
        });
        self.launcher = Some(Launcher {
            all: Vec::new(),
            hits: Vec::new(),
            selected: 0,
            scroll: 0,
            field: VimField::new("", false),
            loading: Some(rx),
            previews: HashMap::new(),
            preview_rx: None,
            ask: None,
            cloning: None,
            pending_d: false,
            message: String::new(),
            row_map: Default::default(),
            act_map: Default::default(),
            picking: None,
            pick_insert: true,
        });
        self.finder = None;
        self.mode = Mode::Launcher;
    }

    /// 裏で集めた一覧・プレビュー・clone の結果を受け取る。変わったら true
    pub(crate) fn poll_launcher(&mut self) -> bool {
        let Some(l) = &mut self.launcher else { return false };
        let mut changed = false;
        if let Some(rx) = &l.loading {
            if let Ok(all) = rx.try_recv() {
                l.all = all;
                l.loading = None;
                l.refilter();
                changed = true;
            }
        }
        if let Some((p, rx)) = &l.preview_rx {
            if let Ok(pv) = rx.try_recv() {
                l.previews.insert(p.clone(), pv);
                l.preview_rx = None;
                changed = true;
            }
        }
        // フォルダを選ぶダイアログの結果
        let mut picked = None;
        if let Some(rx) = &l.picking {
            if let Ok(r) = rx.try_recv() {
                picked = Some(r);
            }
        }
        if let Some(r) = picked {
            l.picking = None;
            match r {
                Some(dir) => {
                    self.close_launcher_to(dir);
                    return true;
                }
                None => {
                    if let Some(l) = &mut self.launcher {
                        if l.pick_insert {
                            l.field.reinsert();
                        }
                    }
                    changed = true;
                }
            }
        }
        let Some(l) = &mut self.launcher else { return changed };
        let mut cloned = None;
        if let Some((_, rx)) = &l.cloning {
            if let Ok(r) = rx.try_recv() {
                cloned = Some(r);
            }
        }
        if let Some(r) = cloned {
            l.cloning = None;
            match r {
                Ok(dir) => {
                    self.close_launcher_to(dir);
                    return true;
                }
                Err(e) => {
                    l.message = format!("git clone に失敗しました: {e}");
                    changed = true;
                }
            }
        }
        changed | self.want_preview()
    }

    /// 選んでいるプロジェクトのプレビューがまだなら作り始める
    fn want_preview(&mut self) -> bool {
        let waker = self.waker();
        let Some(l) = &mut self.launcher else { return false };
        let Some(e) = l.selected_entry() else { return false };
        if e.missing || l.previews.contains_key(&e.path) || l.preview_rx.as_ref().is_some_and(|(p, _)| *p == e.path) {
            return false;
        }
        let dir = e.path.clone();
        let d2 = dir.clone();
        let rx = nagi_core::task::spawn("preview", waker, move |ctx| {
            ctx.send(build_preview(d2));
        });
        l.preview_rx = Some((dir, rx));
        false
    }

    /// エクスプローラーから落としたもの: フォルダはプロジェクトとして開き、ファイルは開く（ランチャーなら閉じて）
    pub(crate) fn open_dropped(&mut self, path: PathBuf) {
        if path.is_dir() {
            if self.launcher.is_some() {
                self.close_launcher_to(path);
            } else {
                self.open_project(path);
            }
            return;
        }
        if self.launcher.is_some() {
            self.launcher = None;
        }
        if !matches!(self.mode, Mode::Normal) {
            self.mode = Mode::Normal;
        }
        self.edit_file(path);
    }

    fn close_launcher_to(&mut self, dir: PathBuf) {
        self.launcher = None;
        self.mode = Mode::Normal;
        self.open_project(dir);
    }

    /// ランチャーを閉じて空のバッファで始める
    fn close_launcher(&mut self) {
        self.launcher = None;
        self.mode = Mode::Normal;
    }

    pub(crate) fn launcher_insert(&mut self, text: &str) {
        let Some(l) = &mut self.launcher else { return };
        if let Some((_, f)) = &mut l.ask {
            f.insert_text(text);
            return;
        }
        if l.field.insert_text(text) == FieldOut::Changed {
            l.refilter();
        }
        self.want_preview();
    }

    pub(crate) fn launcher_key(&mut self, key: Key) {
        let Some(l) = &mut self.launcher else {
            self.mode = Mode::Normal;
            return;
        };
        // フォルダを選ぶダイアログを開いている間は、キーを受けない
        if l.picking.is_some() {
            return;
        }
        l.message.clear();
        // o・g の入力
        if l.ask.is_some() {
            let out = self.feed_field(key, |e| e.launcher.as_mut().and_then(|l| l.ask.as_mut()).map(|a| &mut a.1));
            let Some(l) = &mut self.launcher else { return };
            match out {
                FieldOut::Submit => {
                    let (ask, f) = l.ask.take().unwrap();
                    let text = f.text().trim().trim_matches('"').to_string();
                    match ask {
                        Ask::Clone if !text.is_empty() => self.start_clone(text),
                        _ => {}
                    }
                }
                FieldOut::Cancel => l.ask = None,
                _ => {}
            }
            return;
        }
        let normal = !l.field.is_insert() && l.field.ed.pending_keys.is_empty() && l.field.visual().is_none();
        let last = l.hits.len().saturating_sub(1);
        let pending_d = std::mem::take(&mut l.pending_d);
        let moved = crate::list::step_typing(key, l.selected, last + 1);
        match key {
            Key::Enter | Key::Ctrl('m') => {
                if let Some(e) = l.selected_entry() {
                    if e.missing {
                        l.message = format!("フォルダがありません: {}（dd で一覧から外す）", e.path.display());
                    } else {
                        let p = e.path.clone();
                        self.close_launcher_to(p);
                    }
                }
                return;
            }
            _ if moved.is_some() => l.selected = moved.unwrap_or(l.selected),
            // 左の操作（入力中でも使える Ctrl のキー）
            Key::Ctrl('o') => return self.launcher_action('o'),
            Key::Ctrl('l') => return self.launcher_action('w'),
            Key::Ctrl('g') => return self.launcher_action('g'),
            Key::Ctrl('r') => return self.launcher_action('r'),
            Key::Ctrl('e') => return self.launcher_action('n'),
            Key::Char('j' | 'k') if normal => l.selected = crate::list::step(key, l.selected, last + 1, 10).unwrap_or(l.selected),
            Key::Char(c @ ('g' | 'o' | 'w' | 'r' | 'n')) if normal => return self.launcher_action(c),
            Key::Char('G') if normal => l.selected = crate::list::step(key, l.selected, last + 1, 10).unwrap_or(l.selected),

            Key::Char('p') if normal => {
                if let Some(e) = l.selected_entry().cloned() {
                    let on = crate::project_open::toggle_pin(&e.path);
                    let path = e.path.clone();
                    if let Some(x) = l.all.iter_mut().find(|x| x.path == path) {
                        x.pinned = on;
                    }
                    l.all.sort_by(|a, b| b.pinned.cmp(&a.pinned).then(b.when.cmp(&a.when)));
                    l.refilter();
                    l.selected = l.hits.iter().position(|&i| l.all[i].path == path).unwrap_or(0);
                    l.message = if on { format!("ピン留めしました: {}", e.name) } else { format!("ピン留めを外しました: {}", e.name) };
                }
            }
            Key::Char('d') if normal && pending_d => {
                if let Some(e) = l.selected_entry().cloned() {
                    crate::project_open::hide_project(&e.path);
                    l.all.retain(|x| x.path != e.path);
                    l.refilter();
                    l.selected = l.selected.min(l.hits.len().saturating_sub(1));
                    l.message = format!("一覧から外しました: {}（開き直すと戻ります）", e.name);
                }
            }
            Key::Char('d') if normal => l.pending_d = true,
            Key::Ctrl('c') => {
                self.close_launcher();
                return;
            }
            k => match self.feed_field(k, |e| e.launcher.as_mut().map(|l| &mut l.field)) {
                FieldOut::Changed => {
                    if let Some(l) = &mut self.launcher {
                        l.refilter();
                    }
                }
                FieldOut::Cancel => {
                    self.close_launcher();
                    return;
                }
                _ => {}
            },
        }
        self.want_preview();
    }

    /// git clone して開く（置き場所は %USERPROFILE%\IdeaProjects の下、リポジトリの名前）
    fn start_clone(&mut self, url: String) {
        let name = url.trim_end_matches('/').rsplit(['/', ':']).next().unwrap_or("repo").trim_end_matches(".git").to_string();
        let base = std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join("IdeaProjects")).unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let dest = base.join(&name);
        let waker = self.waker();
        let Some(l) = &mut self.launcher else { return };
        if dest.exists() {
            l.message = format!("もうあります: {}（o で開けます）", dest.display());
            return;
        }
        let u = url.clone();
        let rx = nagi_core::task::spawn("clone", waker, move |ctx| {
            let _ = std::fs::create_dir_all(&base);
            let r = nagi_workers::git::git(&base, &["clone", &u, &name]).map(|_| dest);
            ctx.send(r);
        });
        l.message = format!("git clone {url} …");
        l.cloning = Some((url, rx));
    }

    /// 左の操作: o フォルダ（ダイアログ）、w WSL のフォルダ（ダイアログ）、g git から取得、r 最近のファイル、n 空で始める
    pub(crate) fn launcher_action(&mut self, a: char) {
        let waker = self.waker.clone();
        let Some(l) = &mut self.launcher else { return };
        l.pick_insert = l.field.is_insert();
        match a {
            'o' => {
                l.message = "フォルダを選んでください（ダイアログ）".into();
                let start = std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join("IdeaProjects")).filter(|p| p.is_dir());
                l.picking = Some(crate::dialogs::pick_folder("開くプロジェクトのフォルダ", start, waker));
            }
            'w' => match nagi_workers::wsl::distros().first() {
                Some(d) => {
                    l.message = format!("WSL のフォルダを選んでください（{d}。ほかのディストリビューションは \\\\wsl.localhost から）");
                    let start = nagi_workers::wsl::to_unc(d, "/home");
                    l.picking = Some(crate::dialogs::pick_folder("開く WSL のプロジェクトのフォルダ", Some(start), waker));
                }
                None => l.message = "WSL のディストリビューションがありません（wsl --install で入れられます）".into(),
            },
            'g' => l.ask = Some((Ask::Clone, VimField::new("", false))),
            'r' => {
                self.close_launcher();
                self.open_finder(crate::finder::Source::Recent);
            }
            'n' => self.close_launcher(),
            _ => {}
        }
    }

    /// 左の操作のクリック（画面の行・列）。当たれば true
    pub(crate) fn launcher_left_click(&mut self, row: usize, col: usize) -> bool {
        let Some(l) = &self.launcher else { return false };
        let (acts, w) = l.act_map.borrow().clone();
        if col >= w {
            return false;
        }
        match acts.iter().find(|(r, _)| *r == row) {
            Some(&(_, a)) => {
                self.launcher_action(a);
                true
            }
            None => false,
        }
    }

    /// 一覧の行のクリック（2 回で開く）
    pub(crate) fn launcher_click(&mut self, i: usize, clicks: u32) {
        let Some(l) = &mut self.launcher else { return };
        if i >= l.hits.len() {
            return;
        }
        l.selected = i;
        if clicks >= 2 {
            self.launcher_key(Key::Enter);
        } else {
            self.want_preview();
        }
    }
}


/// 描き方（view.rs の Painter で描く）
pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    /// ランチャー: 左に操作、中央に検索の欄と一覧（ピン留め → 最近）、右に選んだプロジェクトのプレビュー。画面全体を覆う
    pub(crate) fn draw_launcher(p: &mut Painter, l: &crate::launcher::Launcher, cols: usize, rows: usize, preedit: &str) {
        const ORANGE: [f32; 4] = rgb(0xd7af87);
        const GREEN: [f32; 4] = rgb(0x87af87);
        let cell = p.fonts.cell();
        let y = |r: usize| r as f32 * cell.h;
        p.out.push(Instance::fill(0.0, 0.0, cols as f32 * cell.w, rows as f32 * cell.h, BG));
        let saved = p.cols;
        // 左: 名前と版・操作・読み込み元
        let lw = 26.min(cols / 5).max(20);
        let pw = if cols >= 170 { 56 } else if cols >= 110 { 42 } else { 0 };
        p.cols = lw;
        p.text("nagi", 3, y(1), FG);
        p.text(env!("CARGO_PKG_VERSION"), 3, y(2), STATUS_FG);
        // 左の操作（押せる。Ctrl のキーは入力中でもノーマルでも効くので、いつも同じ表示に）
        let acts = crate::launcher::ACTIONS;
        let mut map = Vec::new();
        for (i, (k, label, ctrl)) in acts.iter().enumerate() {
            let r = 4 + i * 2;
            let key = ctrl.to_string();
            let kw = p.width(&key);
            p.out.push(Instance::fill(3.0 * cell.w, y(r), (kw + 2) as f32 * cell.w, cell.h, STATUS_FG));
            p.text(&key, 4, y(r), BG);
            p.text(label, 6 + kw, y(r), FG);
            map.push((r, *k));
        }
        *l.act_map.borrow_mut() = (map, lw);
        let mut r = 4 + acts.len() * 2 + 1;
        p.text("読み込み元", 3, y(r), STATUS_FG);
        r += 1;
        for (name, n) in l.sources() {
            let c = p.text("✓ ", 3, y(r), GREEN);
            let c = p.text(&name, c, y(r), FG);
            p.text(&format!("  {n}"), c, y(r), LINE_NO);
            r += 1;
        }
        p.out.push(Instance::fill(lw as f32 * cell.w, 0.0, 1.0, (rows - 1) as f32 * cell.h, LINE_NO));
        // 右: プレビュー
        let px = cols.saturating_sub(pw);
        if pw > 0 {
            p.out.push(Instance::fill(px as f32 * cell.w - 1.0, 0.0, 1.0, (rows - 1) as f32 * cell.h, LINE_NO));
            p.cols = cols;
            if let Some(e) = l.selected_entry() {
                let x = px + 2;
                let mut r = 1;
                let line = |p: &mut Painter, s: &str, fg: [f32; 4], r: &mut usize| {
                    if *r + 1 < rows {
                        p.text(s, x, y(*r), fg);
                    }
                    *r += 1;
                };
                line(p, &e.name, FG, &mut r);
                line(p, &e.path.display().to_string(), STATUS_FG, &mut r);
                match l.previews.get(&e.path) {
                    _ if e.missing => line(p, "フォルダがありません", DIAG_ERROR, &mut r),
                    None => line(p, "読んでいます…", STATUS_FG, &mut r),
                    Some(pv) => {
                        r += 1;
                        line(p, &pv.kind, ACCENT_FG, &mut r);
                        if !e.kind.is_empty() {
                            let m = if pv.modules.is_empty() { String::new() } else { format!("  {}", pv.modules.join(" · ")) };
                            line(p, &format!("{}{m}", e.kind), FG, &mut r);
                        }
                        r += 1;
                        line(p, "git", STATUS_FG, &mut r);
                        match &pv.git {
                            Some((b, ahead, behind, ch, untracked)) => {
                                let ab = if *ahead + *behind > 0 { format!("  ↑{ahead} ↓{behind}") } else { String::new() };
                                let un = if *untracked > 0 { format!("  未追跡 {untracked}") } else { String::new() };
                                line(p, &format!("⎇ {b}{ab}  変更 {}{un}", ch.len()), if ch.is_empty() { FG } else { ORANGE }, &mut r);
                                for (c, path) in ch.iter().take(5) {
                                    line(p, &format!("  {c} {path}"), STATUS_FG, &mut r);
                                }
                                if ch.len() > 5 {
                                    line(p, &format!("  ほか {} 件", ch.len() - 5), STATUS_FG, &mut r);
                                }
                            }
                            None => line(p, "git なし", STATUS_FG, &mut r),
                        }
                        if !pv.restore.is_empty() {
                            r += 1;
                            line(p, "開くと復元するファイル", STATUS_FG, &mut r);
                            for (f, ln, cur) in pv.restore.iter().take(6) {
                                let name = f.rsplit('/').next().unwrap_or(f);
                                line(p, &format!("  {name}  {} 行目{}", ln + 1, if *cur { "  ← 前面" } else { "" }), FG, &mut r);
                            }
                        }
                        if !pv.excludes.is_empty() {
                            r += 1;
                            line(p, "除外するフォルダ（索引・検索に入れない）", STATUS_FG, &mut r);
                            line(p, &format!("  {}", pv.excludes.join(" · ")), STATUS_FG, &mut r);
                        }
                        if !pv.readme.is_empty() {
                            r += 1;
                            line(p, "README", STATUS_FG, &mut r);
                            for t in &pv.readme {
                                line(p, &format!("  {t}"), STATUS_FG, &mut r);
                            }
                        }
                    }
                }
            }
        }
        // 中央: 検索の欄と一覧
        let cx = lw + 2;
        let cr = if pw > 0 { px.saturating_sub(2) } else { cols };
        p.cols = cr;
        let cw = cr.saturating_sub(cx);
        // 欄の枠（1px）
        let (bx, bw) = (cx as f32 * cell.w - 4.0, cw as f32 * cell.w + 8.0);
        for (yy, h) in [(y(1) - 3.0, 1.0), (y(2) + 2.0, 1.0)] {
            p.out.push(Instance::fill(bx, yy, bw, h, LINE_NO));
        }
        p.out.push(Instance::fill(bx, y(1) - 3.0, 1.0, cell.h + 6.0, LINE_NO));
        p.out.push(Instance::fill(bx + bw, y(1) - 3.0, 1.0, cell.h + 6.0, LINE_NO));
        let c = p.text("> ", cx, y(1), STATUS_FG);
        let ask_open = l.ask.is_some();
        if !ask_open && l.field.is_insert() {
            let t = l.field.text();
            field(p, &t, l.field.cursor().1.min(t.len()), preedit, c, y(1), true);
        } else {
            draw_vim_line(p, &l.field, 0, c, y(1), "", !ask_open);
        }
        let info = format!("{}  {}/{} ", l.field.mode_label(), l.hits.len(), l.all.len());
        p.text(&info, cr.saturating_sub(p.width(&info)), y(1), LINE_NO);
        // 一覧（ピン留め → 最近）。列: 名前・どこで・種類・ブランチ・場所・いつ
        let top = 4;
        let bottom = rows.saturating_sub(2);
        let mut map = Vec::new();
        if l.loading.is_some() {
            p.text("最近のプロジェクトを集めています…", cx, y(top), STATUS_FG);
        } else if l.hits.is_empty() {
            p.text(if l.all.is_empty() { "最近のプロジェクトはありません（o でフォルダを開く）" } else { "当たるものがありません" }, cx, y(top), STATUS_FG);
        }
        // 1 件 2 行（IntelliJ の Welcome と同じ）: 名前・どこで開いたか・いつ / 場所・種類・ブランチ
        let cap = (bottom.saturating_sub(top + 3) / 2).max(1);
        let start = if l.selected >= cap { l.selected + 1 - cap } else { 0 };
        let home = std::env::var("USERPROFILE").unwrap_or_default();
        let mut r = top;
        let mut last_pin: Option<bool> = None;
        for (k, &i) in l.hits.iter().enumerate().skip(start) {
            let e = &l.all[i];
            if last_pin != Some(e.pinned) {
                if last_pin.is_some() {
                    r += 1;
                }
                if r + 2 >= bottom {
                    break;
                }
                p.text(if e.pinned { "ピン留め" } else { "最近" }, cx, y(r), STATUS_FG);
                r += 1;
                last_pin = Some(e.pinned);
            }
            if r + 2 > bottom {
                break;
            }
            let sel = k == l.selected;
            if sel {
                p.out.push(Instance::fill(cx as f32 * cell.w - 4.0, y(r), cw as f32 * cell.w + 8.0, 2.0 * cell.h, FG));
            }
            let col = |c: [f32; 4]| if sel { BG } else { c };
            let when = if e.missing { "見つかりません".to_string() } else { crate::launcher::ago(e.when) };
            let from = e.from.trim_end_matches("Idea").to_string();
            let right = format!("{from}   {when}");
            let rw = p.width(&right);
            let rx = cr.saturating_sub(rw + 1);
            let name = clip_width(p, &e.name, rx.saturating_sub(cx + 4));
            p.text(&format!("{}{name}", if e.pinned { "★ " } else { "  " }), cx, y(r), col(if e.missing { DIAG_ERROR } else { FG }));
            let from_c = match from.as_str() {
                "IntelliJ" => ACCENT_FG,
                "WebStorm" => ORANGE,
                "Eclipse" => GREEN,
                _ => STATUS_FG,
            };
            let c = p.text(&from, rx, y(r), col(from_c));
            p.text(&format!("   {when}"), c, y(r), col(if e.missing { DIAG_ERROR } else { STATUS_FG }));
            // 2 行目: 場所（ホームは ~）・種類・ブランチ
            let full = e.path.display().to_string();
            let path = match full.strip_prefix(&home) {
                Some(rest) if !home.is_empty() => format!("~{rest}"),
                _ => full,
            };
            let mut extra = Vec::new();
            if !e.kind.is_empty() {
                extra.push(e.kind.clone());
            }
            if let Some(b) = &e.branch {
                extra.push(format!("⎇ {b}"));
            }
            // 場所を先に。種類とブランチは残りの幅に入る分だけ
            let room = cr.saturating_sub(cx + 3);
            let path = clip_width(p, &path, room);
            let end = p.text(&path, cx + 2, y(r + 1), col(STATUS_FG));
            let ex = extra.join("   ");
            let left = cr.saturating_sub(end + 4);
            if left >= 8 {
                let ex = clip_width(p, &ex, left);
                let exw = p.width(&ex);
                p.text(&ex, cr.saturating_sub(exw + 1), y(r + 1), col(if sel { BG } else { STATUS_FG }));
            }
            map.push((r, k));
            map.push((r + 1, k));
            r += 2;
        }
        *l.row_map.borrow_mut() = map;
        // 最下行: 入力（o・g）・知らせ・キーの案内
        p.cols = cols;
        let by = y(rows - 1);
        p.out.push(Instance::fill(0.0, by - 1.0, cols as f32 * cell.w, 1.0, LINE_NO));
        if let Some((ask, f)) = &l.ask {
            let lead = match ask {
                crate::launcher::Ask::Clone => "git の URL: ",
            };
            let c = p.text(lead, 1, by, FG);
            draw_vim_field(p, f, c, by, preedit);
        } else if !l.message.is_empty() {
            p.text(&l.message, 1, by, FG);
        } else {
            let mode = if l.field.is_insert() { "-- INSERT --" } else { "-- NORMAL --" };
            let c = p.text(mode, 1, by, FG);
            let help = if l.field.is_insert() { "Enter 開く   ↑↓ 選ぶ   左の操作は ^（Ctrl）のキーかクリック   Esc → j/k・p ピン留め・dd 外す" } else { "Enter 開く   j/k 選ぶ   p ピン留め   dd 外す   左の操作は ^（Ctrl）のキーかクリック   i 入力   Esc 閉じる" };
            p.text(help, c + 3, by, STATUS_FG);
        }
        p.cols = saved;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    fn entry(name: &str, pinned: bool, missing: bool) -> Entry {
        Entry { name: name.into(), path: PathBuf::from(format!("C:\\work\\{name}")), from: "IntelliJIdea".into(), when: 0, pinned, missing, kind: String::new(), branch: None }
    }

    #[test]
    fn filter_select_and_keys() {
        let mut e = Editor::new(Buffer::from_str(""), None);
        e.set_screen(160, 40);
        e.open_launcher();
        // 裏の読み込みは待たずに、一覧を差し替える
        let l = e.launcher.as_mut().unwrap();
        l.loading = None;
        l.all = vec![entry("shop", true, false), entry("shop-front", false, false), entry("billing", false, false), entry("old-notes", false, true)];
        l.refilter();
        assert_eq!(e.mode, Mode::Launcher);
        assert_eq!(e.launcher.as_ref().unwrap().hits.len(), 4);
        // 打てば絞り込み（Insert で始まる）
        for k in nagi_vim::parse_keys("bil", " ") {
            e.launcher_key(k);
        }
        let l = e.launcher.as_ref().unwrap();
        assert_eq!(l.hits.len(), 1);
        assert_eq!(l.selected_entry().unwrap().name, "billing");
        // Esc でノーマル、ciw で消して全部（選んでいたものは選んだまま）、j で次
        for k in nagi_vim::parse_keys("<Esc>ciw<Esc>", " ") {
            e.launcher_key(k);
        }
        let l = e.launcher.as_ref().unwrap();
        assert_eq!(l.hits.len(), 4);
        assert_eq!(l.selected_entry().unwrap().name, "billing");
        e.launcher_key(Key::Char('j'));
        assert_eq!(e.launcher.as_ref().unwrap().selected_entry().unwrap().name, "old-notes");
        // 無いフォルダは開かない
        e.launcher_key(Key::Enter);
        assert_eq!(e.mode, Mode::Launcher);
        assert!(e.launcher.as_ref().unwrap().message.contains("ありません"), "{}", e.launcher.as_ref().unwrap().message);
        // ダイアログ（ここでは本物の代わりの通り道）を開いている間のキーは受けず、閉じたら開いたときの入力の状態に戻る
        e.launcher_key(Key::Char('i'));
        assert!(e.launcher.as_ref().unwrap().field.is_insert());
        let (tx, rx) = nagi_core::task::manual();
        let l = e.launcher.as_mut().unwrap();
        l.pick_insert = l.field.is_insert();
        l.picking = Some(rx);
        e.launcher_key(Key::Esc);
        assert!(e.launcher.as_ref().unwrap().field.is_insert(), "ダイアログ中の Esc は受けない");
        tx.send(None).unwrap();
        e.poll_launcher();
        let l = e.launcher.as_ref().unwrap();
        assert!(l.picking.is_none() && l.field.is_insert());
        e.launcher_key(Key::Esc);
        // g で git の URL の入力、Esc Esc でやめる（o はダイアログを出すので試験では押さない）
        for k in nagi_vim::parse_keys("g", " ") {
            e.launcher_key(k);
        }
        assert!(e.launcher.as_ref().unwrap().ask.is_some());
        for k in nagi_vim::parse_keys("<Esc><Esc>", " ") {
            e.launcher_key(k);
        }
        assert!(e.launcher.as_ref().unwrap().ask.is_none());
        // ノーマルの Esc で閉じて空のバッファ
        e.launcher_key(Key::Esc);
        assert_eq!(e.mode, Mode::Normal);
        assert!(e.launcher.is_none());
        assert_eq!(ago(crate::project_open::now_ms() - 180_000), "3 分前");
    }

    /// エクスプローラーから落とした: ファイルはランチャーを閉じて開く、フォルダはプロジェクトとして開く
    #[test]
    fn dropped_file_and_folder() {
        let dir = std::env::temp_dir().join(format!("nagi-drop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("proj/.git")).unwrap();
        std::fs::write(dir.join("a.txt"), "dropped\n").unwrap();
        let mut e = Editor::new(Buffer::from_str(""), None);
        e.set_screen(120, 40);
        e.open_launcher();
        e.open_dropped(dir.join("a.txt"));
        assert!(e.launcher.is_none());
        assert_eq!(e.mode, Mode::Normal);
        assert_eq!(e.buf.line(0), "dropped");
        e.open_launcher();
        e.open_dropped(dir.join("proj"));
        assert!(e.launcher.is_none());
        assert_ne!(e.mode, Mode::Launcher, "ランチャーから開いたときと同じく、ツリーへ");
        assert!(e.git.root.as_ref().is_some_and(|r| r.ends_with("proj")) || e.notify.text.contains("proj"), "{}", e.notify.text);
    }
}
