//! ファジーファインダー（ファイル・バッファ・最近のファイル）。見た目はターミナルの fzf と同じで、
//! 最下行に `> 検索語`、その上に候補を良い順に下から並べる。
//!
//! ファイルの一覧は裏のスレッドで集め、集まった分から候補に足す。一度集めたプロジェクトの一覧は覚えておき、
//! 次に開くときはすぐに出す。検索語を打ち足したときは、前の候補だけを絞る。

use std::path::{Path, PathBuf};

use nagi_core::fuzzy::{Query, rank_ids, sort_hits};
use nagi_vim::Key;
use nagi_workers::files::FileWalk;

use crate::editor::{Editor, Effect, State, Prompt};
use crate::vim_field::FieldOut;

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
    /// 最近のプロジェクト（Nagi・IntelliJ・WebStorm・Eclipse）と、Eclipse のワークスペースの中のプロジェクト
    Projects,
    /// 実行の構成（選ぶと動かす）
    RunConfigs,
    /// 最近の編集箇所（RecentLocations）
    Locations,
    /// どこでも検索（SearchEverywhere。Shift 2 回）: 型・シンボル・ファイル・コマンドをまとめて
    Everywhere,
}

/// どこでも検索の候補を選んだときにすること（items と同じ並び）
#[derive(Clone, Debug)]
enum Every {
    /// プロジェクトのファイル（items は根からの相対パス）
    File,
    /// 型・シンボルの定義
    Loc(PathBuf, usize, usize),
    /// コマンド（Ex の名前）
    Cmd(String),
}

/// コマンドパレットの項目（説明, Ex コマンド）
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
            Source::Projects => "プロジェクト",
            Source::RunConfigs => "実行の構成",
            Source::Locations => "最近の編集箇所",
            Source::Everywhere => "どこでも",
        }
    }
}

impl Source {
    /// どこでも検索のタブの名前
    pub fn tab_label(self) -> &'static str {
        match self {
            Source::Types => "クラス",
            Source::Commands => "アクション",
            s => s.label(),
        }
    }
}

impl Finder {
    /// プロジェクトの候補を入れる（`member` なら Eclipse のワークスペースの中から選ぶ）
    pub fn set_projects(&mut self, items: Vec<String>, locs: Vec<(PathBuf, usize, usize)>, member: bool) {
        self.projects_rx = None;
        self.items = items;
        self.locs = locs;
        self.pick_member = member;
        self.refilter();
    }

    /// 欄の中身を絞り込みの語に写して絞り直す
    pub fn sync_query(&mut self) {
        self.query.text = self.field.text();
        self.query.cursor = self.field.cursor().1.min(self.query.text.len());
        self.refilter();
        self.selected = 0;
        self.scroll = 0;
    }
}

pub struct Finder {
    pub source: Source,
    /// 上の帯の部品から開いたときの列（その下に出す）
    pub anchor: Option<usize>,
    /// 絞り込みの語（field の中身の写し。IME の位置と絞り込みに使う）
    pub query: Prompt,
    /// 絞り込みの欄（Vim 風。Insert で始め、Esc でノーマル: j / k で候補を選ぶ、w や ciw で直す。もう一度 Esc で閉じる）
    pub field: crate::vim_field::VimField,
    /// ファイルの候補の根（候補はここからの相対パス）
    pub root: PathBuf,
    /// 候補（画面に出す文字列）
    pub items: Vec<String>,
    /// バッファの候補なら、その番号（items と同じ並び）
    docs: Vec<usize>,
    /// 場所の候補なら (パス, 行, 列)（items と同じ並び）
    locs: Vec<(PathBuf, usize, usize)>,
    /// どこでも検索の候補の種類（items と同じ並び）
    every: Vec<Every>,
    /// 当たった候補 (items の添字, 点数)。良い順
    pub hits: Vec<(u32, i32)>,
    /// 選んでいる候補（hits の添字。0 が一番良いもの）
    pub selected: usize,
    /// 画面に出している範囲の先頭（hits の添字）
    pub scroll: usize,
    walk: Option<FileWalk>,
    /// 覚えた一覧を出しながら集め直している間の新しい一覧（集め終えたら差し替える）
    fresh: Option<Vec<String>>,
    recent_rx: Option<nagi_core::task::Task<Vec<PathBuf>>>,
    /// 最近のプロジェクト（表示, 場所）を裏で集めている
    projects_rx: Option<nagi_core::task::Task<Vec<(String, PathBuf)>>>,
    /// Eclipse のワークスペースの中から選んでいる（選んだらそのまま開く）
    pick_member: bool,
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
        self.walk.is_some() || self.recent_rx.is_some() || self.projects_rx.is_some()
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
        // どこでも検索は IntelliJ と同じく、型・ファイル・シンボル・アクションの順にまとめる（それぞれの中は良い順）
        if self.source == Source::Everywhere {
            let types = ["クラス", "インターフェース", "型", "enum"];
            let group = |i: u32| match self.every.get(i as usize) {
                Some(Every::Loc(..)) if self.items[i as usize].split("  ").nth(1).is_some_and(|k| types.contains(&k)) => 0,
                Some(Every::File) => 1,
                Some(Every::Loc(..)) => 2,
                _ => 3,
            };
            hits.sort_by_key(|&(i, _)| group(i));
        }
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
    nagi_core::task::detach("recent", move || {
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
            .or_else(|| Some(self.cwd.clone()))
            .unwrap_or_default();
        let root = std::path::absolute(&root).unwrap_or(root);
        let mut f = Finder {
            source,
            anchor: None,
            query: Prompt::default(),
            field: crate::vim_field::VimField::new("", false),
            root: root.clone(),
            items: Vec::new(),
            docs: Vec::new(),
            locs: Vec::new(),
            every: Vec::new(),
            hits: Vec::new(),
            selected: 0,
            scroll: 0,
            walk: None,
            fresh: None,
            recent_rx: None,
            projects_rx: None,
            pick_member: false,
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
                let waker = self.waker();
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
            Source::Definitions | Source::Symbols | Source::Types | Source::Outline | Source::Locations => {}
            Source::Commands => {
                // コマンドの表（commands.rs）から
                for (cmd, help) in crate::commands::listed() {
                    f.items.push(format!("{help}  :{cmd}"));
                }
                for (help, cmd) in crate::commands::PALETTE_EXTRA {
                    f.items.push(format!("{help}  :{cmd}"));
                }
            }
            Source::RunConfigs => f.items = self.run_list_items(),
            Source::Everywhere => {
                // 型を先に、次にシンボル（索引ができていれば）。同じ点数なら前のものが上に来る
                if let Some(idx) = &self.nav.index {
                    let iroot = idx.root.clone();
                    let mut all: Vec<(String, PathBuf, usize, usize, nagi_syntax::defs::DefKind)> = idx.all().map(|(n, l)| (n.to_string(), iroot.join(&*l.path), l.line as usize, l.col as usize, l.kind)).collect();
                    all.sort_by(|a, b| (!a.4.is_type(), a.0.len()).cmp(&(!b.4.is_type(), b.0.len())).then_with(|| a.0.cmp(&b.0)));
                    for (n, p, l, c, k) in all {
                        f.items.push(format!("{n}  {}  {}:{}", k.label(), display_path(&p, &root), l + 1));
                        f.every.push(Every::Loc(p, l, c));
                    }
                }
                // ファイル（覚えた一覧があればそれ、無ければ裏で集める）
                match self.file_cache.get(&root) {
                    Some(list) => {
                        f.every.extend(list.iter().map(|_| Every::File));
                        f.items.extend(list.iter().cloned());
                    }
                    None => f.walk = Some(nagi_workers::files::walk(&root, self.waker())),
                }
                for (cmd, help) in crate::commands::listed() {
                    f.items.push(format!("{help}  :{cmd}"));
                    f.every.push(Every::Cmd(cmd.to_string()));
                }
            }
            Source::Projects => {
                let rx = nagi_core::task::spawn("projects", self.waker(), move |ctx| {
                    ctx.send(crate::project_open::recent_list());
                });
                f.projects_rx = Some(rx);
            }
            Source::Recent => {
                let rx = nagi_core::task::spawn("recent", self.waker(), move |ctx| {
                    let list: Vec<PathBuf> = recent_file()
                        .and_then(|f| std::fs::read_to_string(f).ok())
                        .map(|s| s.lines().filter(|l| !l.is_empty()).map(PathBuf::from).filter(|p| p.exists()).collect())
                        .unwrap_or_default();
                    ctx.send(list);
                });
                f.recent_rx = Some(rx);
            }
        }
        f.refilter();
        self.finder = Some(f);
        self.set_state(State::Finder);
        self.notify.clear();
    }

    /// 裏で集めている候補を受け取る。変わったら true（描き直す）
    pub fn poll_finder(&mut self) -> bool {
        let Some(f) = &mut self.finder else { return false };
        let mut changed = false;
        // どこでも検索: 集まったファイルを足すだけ（並べ替えると種類の並びとずれる）
        if let (Some(w), Source::Everywhere) = (&f.walk, f.source) {
            loop {
                match w.rx.try_recv() {
                    Ok(batch) => {
                        f.every.extend(batch.iter().map(|_| Every::File));
                        f.items.extend(batch);
                        changed = true;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        f.walk = None;
                        changed = true;
                        break;
                    }
                }
            }
        }
        if let (Some(w), false) = (&f.walk, f.source == Source::Everywhere) {
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
        if let Some(rx) = &f.projects_rx {
            if let Ok(list) = rx.try_recv() {
                f.items = list.iter().map(|(l, _)| l.clone()).collect();
                f.locs = list.into_iter().map(|(_, p)| (p, 0, 0)).collect();
                f.projects_rx = None;
                changed = true;
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
            self.set_state(State::Normal);
            return Effect::None;
        };
        let normal = !f.field.is_insert() && f.field.ed.pending_keys.is_empty() && f.field.visual().is_none();
        // 上が一番良い候補（IntelliJ と同じ）。↓ で次の候補へ
        let up = |f: &mut Finder| f.selected = f.selected.saturating_sub(1);
        let down = |f: &mut Finder| f.selected = (f.selected + 1).min(f.hits.len().saturating_sub(1));
        match key {
            Key::Ctrl('c') | Key::Ctrl('g') => self.close_finder(),
            Key::Enter => return self.accept_finder(None),
            Key::Ctrl('v') => return self.accept_finder(Some(true)),
            Key::Ctrl('x') | Key::Ctrl('s') => return self.accept_finder(Some(false)),
            // 良い候補は下にあるので、上へ = 次の候補
            Key::Up | Key::Ctrl('p') | Key::Ctrl('k') => up(f),
            Key::Down | Key::Ctrl('n') | Key::Ctrl('j') | Key::Tab => down(f),
            // ノーマルでは j / k で候補を選ぶ
            Key::Char('j') if normal => down(f),
            Key::Char('k') if normal => up(f),
            k => match self.feed_field(k, |e| e.finder.as_mut().map(|f| &mut f.field)) {
                FieldOut::Changed => {
                    if let Some(f) = &mut self.finder {
                        f.sync_query();
                    }
                }
                FieldOut::Submit => return self.accept_finder(None),
                FieldOut::Cancel => self.close_finder(),
                FieldOut::None => {
                    if let Some(f) = &mut self.finder {
                        f.query.cursor = f.field.cursor().1.min(f.query.text.len());
                    }
                }
            },
        }
        Effect::None
    }

    pub(crate) fn finder_insert(&mut self, text: &str) {
        if let Some(f) = &mut self.finder {
            if f.field.insert_text(text) == FieldOut::Changed {
                f.sync_query();
            }
        }
    }

    /// ファインダーのクリック（画面の行・列と画面の大きさ）。候補を押すと選び、ダブルクリックで開く（Enter と同じ）。
    /// 上のタブを押すとその種類に替え（打った語は残す）、小窓の外を押したら閉じる（ほかの小窓と同じ）
    pub(crate) fn finder_click(&mut self, row: usize, col: usize, cols: usize, rows: usize, clicks: u32) {
        let Some(f) = &mut self.finder else { return };
        let b = draw::finder_box(cols, rows, f.anchor);
        if !b.contains(row, col) {
            self.close_finder();
            return;
        }
        if row == b.y {
            if let Some(src) = draw::finder_tabs(f.source).into_iter().find(|t| col >= b.x + t.1 && col < b.x + t.2).map(|t| t.0) {
                self.finder_switch(src);
            }
            return;
        }
        let top = b.y + draw::LIST_TOP;
        let hit = (row >= top).then(|| f.scroll + (row - top)).filter(|&i| i < f.hits.len() && row < top + draw::finder_height(rows));
        if let Some(crate::list::Pick::Accept(_)) = crate::list::pick(&mut f.selected, hit, clicks, 2) {
            let eff = self.accept_finder(None);
            if !matches!(eff, Effect::None) {
                self.click_effect = Some(eff);
            }
        }
    }

    /// どこでも検索のタブ: 種類を替える（打った語は残す）
    pub(crate) fn finder_switch(&mut self, src: Source) {
        let Some(q) = self.finder.as_ref().map(|f| f.query.text.clone()) else { return };
        if self.finder.as_ref().is_some_and(|f| f.source == src) {
            return;
        }
        self.open_finder(src);
        if !q.is_empty() {
            self.finder_insert(&q);
        }
    }

    fn close_finder(&mut self) {
        self.finder = None;
        self.set_state(State::Normal);
    }

    /// 選んだ候補を開く（コマンドなら実行する）。`split` は分割して開く（true: 左右、false: 上下）
    fn accept_finder(&mut self, split: Option<bool>) -> Effect {
        let Some(f) = self.finder.take() else { return Effect::None };
        self.set_state(State::Normal);
        let Some(&(i, _)) = f.hits.get(f.selected) else { return Effect::None };
        let i = i as usize;
        let cmd_pick = matches!(f.every.get(i), Some(Every::Cmd(_)));
        if let (Some(v), false) = (split, cmd_pick || matches!(f.source, Source::Commands | Source::Outline | Source::Projects | Source::RunConfigs)) {
            self.split(v);
        }
        match f.source {
            Source::Files => self.edit_file(f.root.join(&f.items[i])),
            Source::Recent => {
                let p = PathBuf::from(&f.items[i]);
                self.edit_file(if p.is_absolute() { p } else { f.root.join(p) });
            }
            Source::Buffers => self.switch_doc(f.docs[i]),
            Source::RunConfigs => self.run_list_pick(i),
            Source::Commands => {
                let cmd = f.items[i].rsplit_once("  :").map_or("", |(_, c)| c).to_string();
                return self.run_ex(&cmd);
            }
            Source::Projects => {
                let p = f.locs[i].0.clone();
                if f.pick_member {
                    self.open_project_direct(p);
                } else {
                    self.open_project(p);
                }
            }
            Source::Definitions | Source::Symbols | Source::Types | Source::Outline | Source::Locations => {
                let (p, l, c) = f.locs[i].clone();
                self.jump_to(&p, l, c);
            }
            Source::Everywhere => match f.every.get(i).cloned() {
                Some(Every::File) => self.edit_file(f.root.join(&f.items[i])),
                Some(Every::Loc(p, l, c)) => self.jump_to(&p, l, c),
                Some(Every::Cmd(c)) => return self.run_ex(&c),
                None => {}
            },
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



/// この部品の入口（component.rs）
pub struct FinderComponent;

impl crate::component::Component for FinderComponent {
    fn key(&self, ed: &mut crate::editor::Editor, key: nagi_vim::Key) -> crate::editor::Effect {
        ed.finder_key(key)
    }

    fn text(&self, ed: &mut crate::editor::Editor, text: &str) {
        ed.finder_insert(text);
    }
}


/// ファインダー（layout.rs の View）
pub struct FinderView;

impl crate::layout::View for FinderView {
    fn rect(&self, ed: &crate::editor::Editor, s: crate::layout::Screen) -> Option<crate::panes::Rect> {
        let _ = s;
        ed.finder.as_ref()?;
        Some(crate::panes::Rect { x: 0, y: 0, w: s.cols, h: s.rows })
    }

    fn draw(&self, ed: &crate::editor::Editor, p: &mut crate::view::Painter, r: crate::panes::Rect, s: crate::layout::Screen) {
        let _ = (r, s);
        if let Some(f) = &ed.finder {
            draw::draw_finder(p, f, s.rows, s.cols, &ed.preedit);
        }
    }

    fn click(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, row: usize, col: usize, clicks: u32) -> crate::layout::Click {
        // 小窓は画面全体を持つ（r.h は本文の行数）
        ed.finder_click(row, col, r.w, r.h, clicks);
        crate::layout::Click::Done
    }

    fn wheel(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, row: usize, col: usize, dy: isize, dx: isize) -> bool {
        let _ = (r, row, col, dx);
        crate::layout::wheel_keys(ed, dy)
    }
}

/// 描き方（view.rs の Painter で描く）
pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    /// 小窓の中で候補の一覧が始まる行（タブ・入力欄・区切り）
    pub(crate) const LIST_TOP: usize = 3;

    /// 小窓（IntelliJ の Search Everywhere と同じく、画面の上の方の真ん中）
    pub(crate) fn finder_box(cols: usize, rows: usize, anchor: Option<usize>) -> Rect {
        let h = (rows * 3 / 5).max(12).min(rows.saturating_sub(2));
        // 上の帯の部品から開いたときは、その下に（IntelliJ のプロジェクトの選び方と同じ）
        if let Some(c) = anchor {
            let w = 64.min(cols.saturating_sub(4));
            return Rect { x: c.min(cols.saturating_sub(w + 1)), y: 1, w, h };
        }
        let w = (cols * 3 / 5).max(70).min(cols.saturating_sub(4));
        let y = (rows / 8).max(1).min(rows.saturating_sub(h));
        Rect { x: cols.saturating_sub(w) / 2, y, w, h }
    }

    /// ファインダーの候補を出す高さ（小窓からタブ・入力欄・区切り・下の案内を除いた行）
    pub(crate) fn finder_height(rows: usize) -> usize {
        let h = (rows * 3 / 5).max(12).min(rows.saturating_sub(2));
        h.saturating_sub(LIST_TOP + 1).max(1)
    }

    /// 上のタブ（種類, 小窓の左端からの列の始め, 終わり）。どこでも検索の仲間のときだけ並べる（IntelliJ の All・Classes・Files・Symbols・Actions）
    pub(crate) fn finder_tabs(src: Source) -> Vec<(Source, usize, usize)> {
        const TABS: [(Source, &str); 5] = [(Source::Everywhere, "すべて"), (Source::Types, "クラス"), (Source::Files, "ファイル"), (Source::Symbols, "シンボル"), (Source::Commands, "アクション")];
        if !TABS.iter().any(|t| t.0 == src) {
            return Vec::new();
        }
        let mut x = 1;
        TABS.iter()
            .map(|(s, l)| {
                let w = nagi_core::display_col(l, l.len(), false) + 2;
                let t = (*s, x, x + w);
                x += w + 1;
                t
            })
            .collect()
    }

    /// ファインダー: 枠のある小窓に、上にタブ（か種類の名前）、入力欄、上から良い順の候補、下に件数と案内
    pub(crate) fn draw_finder(p: &mut Painter, f: &crate::finder::Finder, rows: usize, cols: usize, preedit: &str) {
        let cell = p.fonts.cell();
        let b = finder_box(cols, rows, f.anchor);
        let (x0, y0, wpx, hpx) = (b.x as f32 * cell.w, b.y as f32 * cell.h, b.w as f32 * cell.w, b.h as f32 * cell.h);
        // 影の代わりの枠と地
        p.out.push(Instance::fill(x0 - 1.0, y0 - 1.0, wpx + 2.0, hpx + 2.0, SEPARATOR));
        p.out.push(Instance::fill(x0, y0, wpx, hpx, POPUP_BG));
        let saved = p.cols;
        p.cols = b.x + b.w;
        // タブ（選んでいるものは青い下線）
        let tabs = finder_tabs(f.source);
        if tabs.is_empty() {
            p.text(&format!(" {}", f.source.label()), b.x, y0, FG_BRIGHT);
        }
        for (s, c0, c1) in &tabs {
            let on = *s == f.source;
            p.text(&format!(" {} ", if *s == Source::Everywhere { "すべて" } else { s.tab_label() }), b.x + c0, y0, if on { FG_BRIGHT } else { STATUS_FG });
            if on {
                p.out.push(Instance::fill((b.x + c0) as f32 * cell.w, y0 + cell.h - 3.0, (c1 - c0) as f32 * cell.w, 3.0, ACCENT_FG));
            }
        }
        // 入力欄（虫眼鏡のアイコンと、打った語）
        let fy = y0 + cell.h;
        { let px = crate::icons::small(cell.h); crate::icons::draw(p, "g_search", (b.x as f32 + 1.5) * cell.w - px / 2.0, crate::icons::centered(fy, cell.h, px), px, None); }
        draw_vim_line(p, &f.field, 0, b.x + 3, fy, preedit, true);
        p.out.push(Instance::fill(x0, fy + cell.h + cell.h * 0.5, wpx, 1.0, SEPARATOR));
        // 候補（上が一番良い）
        let q = nagi_core::fuzzy::Query::new(&f.query.text);
        let h = finder_height(rows);
        for k in 0..h {
            let Some(&(i, _)) = f.hits.get(f.scroll + k) else { break };
            let text = &f.items[i as usize];
            let y = (b.y + LIST_TOP + k) as f32 * cell.h;
            let selected = f.scroll + k == f.selected;
            let hit: std::collections::HashSet<usize> = if q.is_empty() { Default::default() } else { q.positions(text).into_iter().collect() };
            if selected {
                p.out.push(Instance::fill(x0 + 4.0, y, wpx - 8.0, cell.h, LIST_SEL_BG));
            }
            // 「名前  種類  場所」の 2 つ目からは薄く（IntelliJ の場所の灰色）
            let name_end = text.find("  ").unwrap_or(text.len());
            let mut items = p.items(" ", FG);
            let mut ci = 0;
            let mut bi = 0;
            for g in text.graphemes(true) {
                let m = hit.contains(&ci);
                let width = p.cell_width(g);
                let base = if selected { FG_BRIGHT } else if bi < name_end { FG } else { STATUS_FG };
                items.push(Item { g: Cow::Borrowed(g), width, fg: if m { FINDER_MATCH } else { base }, bg: None, underline: false });
                ci += g.chars().count();
                bi += g.len();
            }
            p.draw(&items, b.x + 1, y);
        }
        // 下の案内と件数
        let by = (b.y + b.h - 1) as f32 * cell.h;
        p.out.push(Instance::fill(x0, by, wpx, 1.0, SEPARATOR));
        let mode = if f.field.is_insert() { String::new() } else { format!("-- {} --  ", f.field.mode_label()) };
        let left = format!(" {mode}Enter 開く  Ctrl+V 左右に分割  ↑↓ 選ぶ  Esc 閉じる");
        p.text(&left, b.x, by, STATUS_FG);
        let count = format!("{}/{}{} ", f.hits.len(), f.items.len(), if f.walking() { " …" } else { "" });
        let cw = p.width(&count);
        p.text(&count, (b.x + b.w).saturating_sub(cw), by, STATUS_FG);
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

    /// マウスで: 候補を押すと選び、ダブルクリックで開く。小窓の外を押すと閉じる。上のタブで種類を替える（Space f a の「すべてを検索」もこのファインダー）
    #[test]
    fn click_select_and_double_click_open() {
        let dir = project("click");
        let mut e = Editor::new(Buffer::open(&dir.join("README.md")).unwrap(), None);
        e.buf.path = Some(dir.join("README.md"));
        e.set_screen(100, 40);
        e.open_finder(Source::Files);
        wait_walk(&mut e);
        let s = crate::layout::Screen { rows: 40, cols: 100 };
        let f = e.finder.as_ref().unwrap();
        let second = f.items[f.hits[1].0 as usize].clone();
        // 小窓の一覧の 1 行目が一番良い候補、その下が 2 番目
        let b = draw::finder_box(100, 40, None);
        let row = b.y + draw::LIST_TOP + 1;
        crate::layout::press(&mut e, s, row, b.x + 5, |_| 1);
        assert_eq!(e.finder.as_ref().unwrap().selected, 1, "押した行の候補を選ぶ");
        assert_eq!(e.state(), State::Finder, "1 回では開かない");
        crate::layout::press(&mut e, s, row, b.x + 5, |_| 2);
        assert_eq!(e.state(), State::Normal);
        assert!(e.buf.path.as_ref().unwrap().ends_with(second.replace('/', std::path::MAIN_SEPARATOR_STR)), "{:?} {second}", e.buf.path);
        // どこでも検索のタブ: 「ファイル」を押すと、打った語を残してファイルだけに
        e.open_finder(Source::Everywhere);
        e.finder_insert("main");
        let (_, c0, _) = draw::finder_tabs(Source::Everywhere)[2];
        crate::layout::press(&mut e, s, b.y, b.x + c0 + 1, |_| 1);
        let f = e.finder.as_ref().unwrap();
        assert_eq!((f.source, f.query.text.as_str()), (Source::Files, "main"));
        // 小窓の外を押すと閉じる
        crate::layout::press(&mut e, s, 0, 5, |_| 1);
        assert!(e.finder.is_none());
        assert_eq!(e.state(), State::Normal);
    }

    /// どこでも検索（Shift 2 回）: ファイルとコマンドが 1 つの候補に並び、選んだものの種類どおりに開く・動かす
    #[test]
    fn search_everywhere() {
        let dir = project("every");
        let mut e = Editor::new(Buffer::open(&dir.join("README.md")).unwrap(), None);
        e.buf.path = Some(dir.join("README.md"));
        e.set_screen(100, 40);
        keys(&mut e, ":SearchEverywhere<CR>");
        assert_eq!(e.state(), State::Finder);
        wait_walk(&mut e);
        let f = e.finder.as_ref().unwrap();
        assert_eq!(f.items.len(), f.every.len());
        assert!(f.items.iter().any(|i| i == "src/view/panes.rs") && f.items.iter().any(|i| i.ends_with(":HideAll")));
        keys(&mut e, "view/pan<CR>");
        assert!(e.buf.path.as_ref().unwrap().ends_with("panes.rs"), "{:?}", e.buf.path);
        // コマンドを選べば動かす
        e.open_finder(Source::Everywhere);
        wait_walk(&mut e);
        keys(&mut e, ":vnew<CR>");
        assert_eq!(e.state(), State::Normal);
        assert_eq!(e.window_rects().len(), 2);
    }

    #[test]
    fn files_open_and_split() {
        let dir = project("files");
        let mut e = Editor::new(Buffer::open(&dir.join("README.md")).unwrap(), None);
        e.buf.path = Some(dir.join("README.md"));
        e.set_screen(100, 40);
        e.open_finder(Source::Files);
        assert_eq!(e.state(), State::Finder);
        wait_walk(&mut e);
        let f = e.finder.as_ref().unwrap();
        assert_eq!(f.items.len(), 3);
        // 検索語が空なら短い順
        assert_eq!(f.items[f.hits[0].0 as usize], "README.md");
        keys(&mut e, "pan");
        let f = e.finder.as_ref().unwrap();
        assert_eq!(f.hits.len(), 1);
        keys(&mut e, "<CR>");
        assert_eq!(e.state(), State::Normal);
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
        keys(&mut e, "<Esc><Esc>");
        // 欄は Vim 風: Esc でノーマル（j / k で選ぶ、ciw で直す）、もう一度 Esc で閉じる。閉じても何も変えない
        e.open_finder(Source::Files);
        keys(&mut e, "read<Esc>");
        assert_eq!(e.state(), State::Finder);
        assert!(!e.finder.as_ref().unwrap().field.is_insert());
        keys(&mut e, "ciwmain<Esc>");
        assert_eq!(e.finder.as_ref().unwrap().query.text, "main");
        let hits = e.finder.as_ref().unwrap().hits.len();
        keys(&mut e, "k");
        assert_eq!(e.finder.as_ref().unwrap().selected, 1.min(hits - 1), "k で次の候補");
        keys(&mut e, "j");
        assert_eq!(e.finder.as_ref().unwrap().selected, 0);
        keys(&mut e, "<Esc>");
        assert_eq!(e.state(), State::Normal);
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
        // 下へ 1 つ（次の候補）選んで開く
        keys(&mut e, "<C-n><CR>");
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
        e.ime_commit("左右に分割");
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
