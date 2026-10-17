//! ファジーファインダー（ファイル・バッファ・最近のファイル）。見た目はターミナルの fzf と同じで、
//! 最下行に `> 検索語`、その上に候補を良い順に下から並べる。
//!
//! ファイルの一覧は裏のスレッドで集め、集まった分から候補に足す。一度集めたプロジェクトの一覧は覚えておき、
//! 次に開くときはすぐに出す。検索語を打ち足したときは、前の候補だけを絞る。

use std::path::{Path, PathBuf};

use nagi_core::fuzzy::{Query, rank_ids, sort_hits};
use nagi_vim::Key;
use nagi_workers::files::FileWalk;

use crate::editor::{Editor, Effect, Mode, Prompt};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Files,
    Buffers,
    Recent,
    /// コマンドパレット
    Commands,
    /// 定義の候補（gd で複数見つかったとき）
    Definitions,
    /// シンボル検索（GotoSymbol）と型だけ（GotoClass）
    Symbols,
    Types,
    /// ファイルの構造（FileStructurePopup）
    Outline,
}

/// コマンドパレットの項目（説明, Ex コマンド）
const COMMANDS: &[(&str, &str)] = &[
    ("ファイルを開く", "Files"),
    ("バッファを切り替える", "Buffers"),
    ("最近のファイル", "Recent"),
    ("全文検索", "Grep"),
    ("ファイルツリーの表示切替", "Tree"),
    ("保存", "w"),
    ("すべて保存", "wa"),
    ("閉じる", "q"),
    ("すべて閉じて終了", "qa"),
    ("保存せずに終了", "qa!"),
    ("次のバッファ", "bn"),
    ("前のバッファ", "bp"),
    ("バッファを閉じる", "bd"),
    ("開いているバッファの一覧", "ls"),
    ("上下に分割", "split"),
    ("左右に分割", "vsplit"),
    ("新しいバッファを上下に分割して開く", "new"),
    ("ほかの窓を閉じる", "only"),
    ("窓を閉じる", "close"),
    ("次の検索結果", "cn"),
    ("前の検索結果", "cp"),
    ("検索結果のパネルへ", "copen"),
    ("検索結果のパネルを閉じる", "cclose"),
    ("検索の強調を消す", "noh"),
    ("ファイルを読み直す（変更を捨てる）", "e!"),
    ("設定（ideavimrc）を読み直す", "source"),
    ("入力の遅延の計測を表示", "NagiStats"),
    ("入力の遅延の計測を消す", "NagiStats!"),
];

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Files => "ファイル",
            Source::Buffers => "バッファ",
            Source::Recent => "最近のファイル",
            Source::Commands => "コマンド",
            Source::Definitions => "定義",
            Source::Symbols => "シンボル",
            Source::Types => "型",
            Source::Outline => "構造",
        }
    }
}

pub struct Finder {
    pub source: Source,
    pub query: Prompt,
    /// ファイルの候補の根（候補はここからの相対パス）
    pub root: PathBuf,
    /// 候補（画面に出す文字列）
    pub items: Vec<String>,
    /// バッファの候補なら、その番号（items と同じ並び）
    docs: Vec<usize>,
    /// 場所の候補なら (パス, 行, 列)（items と同じ並び）
    locs: Vec<(PathBuf, usize, usize)>,
    /// 当たった候補 (items の添字, 点数)。良い順
    pub hits: Vec<(u32, i32)>,
    /// 選んでいる候補（hits の添字。0 が一番良いもの）
    pub selected: usize,
    /// 画面に出している範囲の先頭（hits の添字）
    pub scroll: usize,
    walk: Option<FileWalk>,
    /// 覚えた一覧を出しながら集め直している間の新しい一覧（集め終えたら差し替える）
    fresh: Option<Vec<String>>,
    recent_rx: Option<std::sync::mpsc::Receiver<Vec<PathBuf>>>,
    /// hits を数えたときの検索語と、そのときの候補の数
    scored_query: String,
    scored_upto: usize,
}

impl Finder {
    /// 場所の一覧を入れる（定義の候補・シンボル・ファイルの構造）
    pub fn set_locations(&mut self, items: Vec<String>, locs: Vec<(PathBuf, usize, usize)>) {
        self.items = items;
        self.locs = locs;
        self.scored_query.clear();
        self.scored_upto = 0;
        self.hits.clear();
        self.refilter();
        self.selected = 0;
    }

    pub fn walking(&self) -> bool {
        self.walk.is_some() || self.recent_rx.is_some()
    }

    /// 検索語や候補が変わったら絞り直す
    fn refilter(&mut self) {
        let q = Query::new(&self.query.text);
        let text = self.query.text.clone();
        let narrowing = !self.scored_query.is_empty() && text.starts_with(&self.scored_query) && text.split_whitespace().count() == self.scored_query.split_whitespace().count();
        // 打ち足したときは前の候補だけを、そうでなければ全部を採点し直す。後から届いた候補はどちらでも採点する
        let mut ids: Vec<u32> = if narrowing { self.hits.iter().map(|&(i, _)| i).collect() } else { (0..self.scored_upto as u32).collect() };
        ids.extend(self.scored_upto as u32..self.items.len() as u32);
        let mut hits = rank_ids(&q, &self.items, &ids);
        sort_hits(&mut hits);
        self.hits = hits;
        self.scored_query = text;
        self.scored_upto = self.items.len();
        self.selected = self.selected.min(self.hits.len().saturating_sub(1));
    }
}

/// 最近開いたファイルの記録（`%LOCALAPPDATA%\nagi\recent.txt`、新しい順に 1 行 1 つ）
fn recent_file() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("nagi").join("recent.txt"))
}

const RECENT_MAX: usize = 300;

/// 開いたファイルを記録の先頭へ（裏のスレッドで書く）
pub fn remember_recent(path: &Path) {
    // 試験で開いた一時ファイルを、使っている人の記録に残さない
    if cfg!(test) {
        return;
    }
    let Some(file) = recent_file() else { return };
    let Ok(abs) = std::path::absolute(path) else { return };
    let _ = std::thread::Builder::new().name("recent".into()).spawn(move || {
        let old = std::fs::read_to_string(&file).unwrap_or_default();
        let me = abs.to_string_lossy().into_owned();
        let mut lines = vec![me.clone()];
        // もう無いファイルは落とす
        lines.extend(old.lines().filter(|l| !l.eq_ignore_ascii_case(&me) && !l.is_empty() && Path::new(l).exists()).take(RECENT_MAX - 1).map(String::from));
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&file, lines.join("\n") + "\n");
    });
}

impl Editor {
    /// ファインダーを開く
    pub(crate) fn open_finder(&mut self, source: Source) {
        let root = self
            .buf
            .path
            .as_deref()
            .and_then(|p| std::path::absolute(p).ok())
            .and_then(|p| nagi_workers::files::project_root(&p))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let root = std::path::absolute(&root).unwrap_or(root);
        let mut f = Finder {
            source,
            query: Prompt::default(),
            root: root.clone(),
            items: Vec::new(),
            docs: Vec::new(),
            locs: Vec::new(),
            hits: Vec::new(),
            selected: 0,
            scroll: 0,
            walk: None,
            fresh: None,
            recent_rx: None,
            scored_query: String::new(),
            scored_upto: 0,
        };
        match source {
            Source::Files => {
                // 覚えた一覧があればすぐ出し、裏で集め直して（作った・消したファイルを反映して）差し替える
                if let Some(list) = self.file_cache.get(&root) {
                    f.items = list.clone();
                    f.fresh = Some(Vec::new());
                }
                let waker = self.waker.clone().unwrap_or_else(|| std::sync::Arc::new(|| {}));
                f.walk = Some(nagi_workers::files::walk(&root, waker));
            }
            Source::Buffers => {
                // 直前のバッファを先頭に、作業中のバッファは最後に（すぐ戻れるように）
                let mut list = self.buffer_list();
                list.sort_by_key(|(i, _, _, active)| (*active, Some(*i) != self.alt_doc, *i));
                for (i, p, dirty, _) in list {
                    let name = p.map_or("[No Name]".to_string(), |p| display_path(&p, &root));
                    f.items.push(format!("{}{} {name}", i + 1, if dirty { "+" } else { "" }));
                    f.docs.push(i);
                }
            }
            // 場所の一覧は呼んだ側が set_locations で入れる
            Source::Definitions | Source::Symbols | Source::Types | Source::Outline => {}
            Source::Commands => {
                for (label, cmd) in COMMANDS {
                    f.items.push(format!("{label}  :{cmd}"));
                }
            }
            Source::Recent => {
                let (tx, rx) = std::sync::mpsc::channel();
                let waker = self.waker.clone();
                let _ = std::thread::Builder::new().name("recent".into()).spawn(move || {
                    let list: Vec<PathBuf> = recent_file()
                        .and_then(|f| std::fs::read_to_string(f).ok())
                        .map(|s| s.lines().filter(|l| !l.is_empty()).map(PathBuf::from).filter(|p| p.exists()).collect())
                        .unwrap_or_default();
                    if tx.send(list).is_ok() {
                        if let Some(w) = waker {
                            w();
                        }
                    }
                });
                f.recent_rx = Some(rx);
            }
        }
        f.refilter();
        self.finder = Some(f);
        self.mode = Mode::Finder;
        self.message.clear();
    }

    /// 裏で集めている候補を受け取る。変わったら true（描き直す）
    pub fn poll_finder(&mut self) -> bool {
        let Some(f) = &mut self.finder else { return false };
        let mut changed = false;
        if let Some(w) = &f.walk {
            loop {
                match w.rx.try_recv() {
                    Ok(batch) => match &mut f.fresh {
                        Some(fresh) => fresh.extend(batch),
                        None => {
                            f.items.extend(batch);
                            changed = true;
                        }
                    },
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        // 集め終えた: 短い順に並べ直して覚えておく（検索語が空のときに短いパスが上に来る）
                        f.walk = None;
                        if let Some(fresh) = f.fresh.take() {
                            // 選んでいた候補は、差し替えた後も同じものを選ぶ
                            let sel = f.hits.get(f.selected).map(|&(i, _)| f.items[i as usize].clone());
                            f.items = fresh;
                            f.hits.clear();
                            f.items.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
                            f.scored_upto = 0;
                            f.scored_query.clear();
                            f.refilter();
                            f.selected = sel.and_then(|s| f.hits.iter().position(|&(i, _)| f.items[i as usize] == s)).unwrap_or(0);
                        }
                        f.items.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
                        f.scored_upto = 0;
                        f.scored_query.clear();
                        self.file_cache.insert(f.root.clone(), f.items.clone());
                        changed = true;
                        break;
                    }
                }
            }
        }
        if let Some(rx) = &f.recent_rx {
            if let Ok(list) = rx.try_recv() {
                let root = f.root.clone();
                f.items = list.iter().map(|p| display_path(p, &root)).collect();
                f.recent_rx = None;
                changed = true;
            }
        }
        if changed {
            f.refilter();
        }
        changed
    }

    pub(crate) fn finder_key(&mut self, key: Key) -> Effect {
        let Some(f) = &mut self.finder else {
            self.mode = Mode::Normal;
            return Effect::None;
        };
        match key {
            Key::Esc | Key::Ctrl('c') | Key::Ctrl('g') | Key::Ctrl('[') => self.close_finder(),
            Key::Enter => return self.accept_finder(None),
            Key::Ctrl('v') => return self.accept_finder(Some(true)),
            Key::Ctrl('x') | Key::Ctrl('s') => return self.accept_finder(Some(false)),
            // 良い候補は下にあるので、上へ = 次の候補
            Key::Up | Key::Ctrl('p') | Key::Ctrl('k') => f.selected = (f.selected + 1).min(f.hits.len().saturating_sub(1)),
            Key::Down | Key::Ctrl('n') | Key::Ctrl('j') | Key::Tab => f.selected = f.selected.saturating_sub(1),
            k => {
                if f.query.key(k) {
                    f.refilter();
                    f.selected = 0;
                    f.scroll = 0;
                }
            }
        }
        Effect::None
    }

    pub(crate) fn finder_insert(&mut self, text: &str) {
        if let Some(f) = &mut self.finder {
            f.query.insert(text);
            f.refilter();
            f.selected = 0;
            f.scroll = 0;
        }
    }

    fn close_finder(&mut self) {
        self.finder = None;
        self.mode = Mode::Normal;
    }

    /// 選んだ候補を開く（コマンドなら実行する）。`split` は分割して開く（true: 左右、false: 上下）
    fn accept_finder(&mut self, split: Option<bool>) -> Effect {
        let Some(f) = self.finder.take() else { return Effect::None };
        self.mode = Mode::Normal;
        let Some(&(i, _)) = f.hits.get(f.selected) else { return Effect::None };
        let i = i as usize;
        if let (Some(v), false) = (split, matches!(f.source, Source::Commands | Source::Outline)) {
            self.split(v);
        }
        match f.source {
            Source::Files => self.edit_file(f.root.join(&f.items[i])),
            Source::Recent => {
                let p = PathBuf::from(&f.items[i]);
                self.edit_file(if p.is_absolute() { p } else { f.root.join(p) });
            }
            Source::Buffers => self.switch_doc(f.docs[i]),
            Source::Commands => {
                let cmd = f.items[i].rsplit_once("  :").map_or("", |(_, c)| c).to_string();
                return self.run_ex(&cmd);
            }
            Source::Definitions | Source::Symbols | Source::Types | Source::Outline => {
                let (p, l, c) = f.locs[i].clone();
                self.jump_to(&p, l, c);
            }
        }
        Effect::None
    }
}

/// `root` の下なら相対パス（区切りは `/`）、そうでなければそのまま
fn display_path(p: &Path, root: &Path) -> String {
    match p.strip_prefix(root) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => p.display().to_string(),
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

    fn project(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nagi-finder-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("src/view")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "main\n").unwrap();
        std::fs::write(dir.join("src/view/panes.rs"), "panes\n").unwrap();
        std::fs::write(dir.join("README.md"), "readme\n").unwrap();
        dir
    }

    fn wait_walk(e: &mut Editor) {
        for _ in 0..500 {
            e.poll_finder();
            if e.finder.as_ref().is_some_and(|f| !f.walking()) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("ファイルを集め終わらない");
    }

    #[test]
    fn files_open_and_split() {
        let dir = project("files");
        let mut e = Editor::new(Buffer::open(&dir.join("README.md")).unwrap(), None);
        e.buf.path = Some(dir.join("README.md"));
        e.set_screen(100, 40);
        e.open_finder(Source::Files);
        assert_eq!(e.mode, Mode::Finder);
        wait_walk(&mut e);
        let f = e.finder.as_ref().unwrap();
        assert_eq!(f.items.len(), 3);
        // 検索語が空なら短い順
        assert_eq!(f.items[f.hits[0].0 as usize], "README.md");
        keys(&mut e, "pan");
        let f = e.finder.as_ref().unwrap();
        assert_eq!(f.hits.len(), 1);
        keys(&mut e, "<CR>");
        assert_eq!(e.mode, Mode::Normal);
        assert_eq!(e.buf.line(0), "panes");
        // 2 回目は覚えた一覧をすぐ出す（裏で集め直している間も）。Ctrl-v で左右に分けて開く
        e.open_finder(Source::Files);
        assert_eq!(e.finder.as_ref().unwrap().items.len(), 3);
        keys(&mut e, "main<C-v>");
        assert!(e.is_split());
        assert_eq!(e.buf.line(0), "main");
        // 作ったファイルは、開き直すと（裏で集め直した後に）一覧に入る
        std::fs::write(dir.join("src/new_file.rs"), "").unwrap();
        e.open_finder(Source::Files);
        wait_walk(&mut e);
        assert_eq!(e.finder.as_ref().unwrap().items.len(), 4);
        keys(&mut e, "<Esc>");
        // Esc で閉じても何も変えない
        e.open_finder(Source::Files);
        keys(&mut e, "read<Esc>");
        assert_eq!(e.mode, Mode::Normal);
        assert_eq!(e.buf.line(0), "main");
    }

    #[test]
    fn buffers_and_selection() {
        let dir = project("buffers");
        let mut e = Editor::new(Buffer::open(&dir.join("README.md")).unwrap(), None);
        e.buf.path = Some(dir.join("README.md"));
        keys(&mut e, &format!(":e {}<CR>", dir.join("src/main.rs").display()));
        keys(&mut e, &format!(":e {}<CR>", dir.join("src/view/panes.rs").display()));
        e.open_finder(Source::Buffers);
        let f = e.finder.as_ref().unwrap();
        // 直前のバッファが一番上（選んだ状態）、作業中のバッファは最後
        assert!(f.items[f.hits[0].0 as usize].ends_with("src/main.rs"), "{:?}", f.items);
        assert!(f.items[f.hits[2].0 as usize].ends_with("panes.rs"));
        // 上へ 1 つ（次の候補）選んで開く
        keys(&mut e, "<C-p><CR>");
        assert_eq!(e.buf.line(0), "readme");
        // IME で確定した文字も検索語に入る
        e.open_finder(Source::Buffers);
        e.ime_commit("pan");
        assert_eq!(e.finder.as_ref().unwrap().hits.len(), 1);
    }

    #[test]
    fn palette_runs_commands() {
        let mut e = Editor::new(Buffer::from_str("x\n"), None);
        e.set_screen(100, 40);
        e.open_finder(Source::Commands);
        // 日本語の説明で絞って実行する（IME で打った語）
        e.ime_commit("左右");
        assert_eq!(e.finder.as_ref().unwrap().hits.len(), 1);
        keys(&mut e, "<CR>");
        assert!(e.is_split());
        // コマンド名でも当たる。終了の知らせは App へ返す
        e.open_finder(Source::Commands);
        keys(&mut e, "qa!");
        let mut last = Effect::None;
        for key in nagi_vim::parse_keys("<CR>", " ") {
            last = e.key(key);
        }
        assert!(matches!(last, Effect::Quit));
    }
}
