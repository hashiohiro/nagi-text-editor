//! ランチャー（起動時のプロジェクトの選択。IntelliJ の Welcome の画面にあたる）。
//!
//! 引数なしで起動したときと `:Project` で、画面全体に出す。左に操作、中央に検索の欄と一覧（ピン留め → 最近）、右に
//! 選んだプロジェクトのプレビュー（種類・git のブランチと変更・開くと復元するファイル・除外するフォルダ・README の先頭）。
//! 一覧は Nagi・IntelliJ・WebStorm・Eclipse などの最近のプロジェクトを 1 つにしたもの。欄は Vim 風: 打てば絞り込み、
//! Esc でノーマル（j / k で選ぶ、p ピン留め、dd 一覧から外す、o フォルダを開く、g git から取得、r 最近のファイル、
//! n 空で始める）、Enter で開く、ノーマルでもう一度 Esc を押すと閉じる（空のバッファで始める）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};

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

/// 下の行で打っているもの（フォルダの場所・git の URL）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    Folder,
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
    pub loading: Option<Receiver<Vec<Entry>>>,
    pub previews: HashMap<PathBuf, Preview>,
    pub preview_rx: Option<(PathBuf, Receiver<Preview>)>,
    /// o（フォルダを開く）・g（git から取得）の入力
    pub ask: Option<(Ask, VimField)>,
    /// git clone の実行中（URL, 結果）
    pub cloning: Option<(String, Receiver<Result<PathBuf, String>>)>,
    pending_d: bool,
    pub message: String,
    /// 描いた一覧の (画面の行, hits の添字)。クリックで使う
    pub row_map: std::cell::RefCell<Vec<(usize, usize)>>,
}

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
        let (tx, rx) = channel();
        let waker = self.waker.clone();
        let _ = std::thread::Builder::new().name("launcher".into()).spawn(move || {
            if tx.send(recent_entries()).is_ok() {
                if let Some(w) = waker {
                    w();
                }
            }
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
        let Some(l) = &mut self.launcher else { return false };
        let Some(e) = l.selected_entry() else { return false };
        if e.missing || l.previews.contains_key(&e.path) || l.preview_rx.as_ref().is_some_and(|(p, _)| *p == e.path) {
            return false;
        }
        let dir = e.path.clone();
        let (tx, rx) = channel();
        let waker = self.waker.clone();
        let d2 = dir.clone();
        let _ = std::thread::Builder::new().name("preview".into()).spawn(move || {
            if tx.send(build_preview(d2)).is_ok() {
                if let Some(w) = waker {
                    w();
                }
            }
        });
        l.preview_rx = Some((dir, rx));
        false
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
                        Ask::Folder if !text.is_empty() => {
                            let dir = PathBuf::from(&text);
                            if dir.is_dir() {
                                self.close_launcher_to(dir);
                            } else {
                                l.message = format!("フォルダがありません: {text}");
                            }
                        }
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
            Key::Down | Key::Ctrl('n') | Key::Ctrl('j') | Key::Tab => l.selected = (l.selected + 1).min(last),
            Key::Up | Key::Ctrl('p') | Key::Ctrl('k') => l.selected = l.selected.saturating_sub(1),
            Key::Ctrl('o') => l.ask = Some((Ask::Folder, VimField::new("", false))),
            Key::Ctrl('g') => l.ask = Some((Ask::Clone, VimField::new("", false))),
            Key::Char('j') if normal => l.selected = (l.selected + 1).min(last),
            Key::Char('k') if normal => l.selected = l.selected.saturating_sub(1),
            Key::Char('g') if normal => l.ask = Some((Ask::Clone, VimField::new("", false))),
            Key::Char('G') if normal => l.selected = last,
            Key::Char('o') if normal => l.ask = Some((Ask::Folder, VimField::new("", false))),
            Key::Char('n') if normal => {
                self.close_launcher();
                return;
            }
            Key::Char('r') if normal => {
                self.close_launcher();
                self.open_finder(crate::finder::Source::Recent);
                return;
            }
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
        let Some(l) = &mut self.launcher else { return };
        if dest.exists() {
            l.message = format!("もうあります: {}（o で開けます）", dest.display());
            return;
        }
        let (tx, rx) = channel();
        let waker = self.waker.clone();
        let u = url.clone();
        let _ = std::thread::Builder::new().name("clone".into()).spawn(move || {
            let _ = std::fs::create_dir_all(&base);
            let r = nagi_workers::git::git(&base, &["clone", &u, &name]).map(|_| dest);
            if tx.send(r).is_ok() {
                if let Some(w) = waker {
                    w();
                }
            }
        });
        l.message = format!("git clone {url} …");
        l.cloning = Some((url, rx));
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
        // o でフォルダの入力、Esc Esc でやめる
        for k in nagi_vim::parse_keys("o", " ") {
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
}
