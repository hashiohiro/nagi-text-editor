//! 設定の画面（IntelliJ の Settings と同じ並び）。画面の中央に重ねて出す。
//!
//! 左は分類、右はその分類の項目（上の絞り込みの欄で、名前・説明・キーから探せる）。下に選んだ項目の説明とボタン。
//! - オン・オフは Space・Enter で切り替え、選択肢は h/l（Enter で次）、数・文字・キーは Enter で Vim の入力欄にして直す
//! - r で既定値に戻す、p で保存先（全体 ⇔ このプロジェクト）を切り替える（プロジェクトで上書きできる項目だけ）
//! - Tab で分類 ⇔ 項目、/ で絞り込み、Ctrl+Enter で適用して閉じる、Esc で閉じる（直したものがあれば確かめる）
//!
//! 適用すると Editor::apply_settings で動いている nagi にもすぐ効かせ、ファイルに保存する。

use std::cell::RefCell;

use nagi_vim::Key;

use crate::editor::{Editor, State};
use crate::settings::{CATEGORIES, Item, SCHEMA, SKind, Settings};
use crate::vim_field::{FieldOut, VimField};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SFocus {
    Cats,
    Items,
    /// 値を直している（入力欄）
    Edit,
    /// 絞り込みの欄
    Filter,
    /// 一覧から選んでいる（フォント）
    Combo,
}

/// コンボボックスの 1 行（出す名前, 決める値, 右に出す印）
#[derive(Clone, Debug)]
pub struct ComboItem {
    pub label: String,
    pub value: String,
    pub tag: String,
}

/// コンボボックス（フォント・JDK の一覧）。打つと絞り込み、↑↓ で選び、Enter で決める
pub struct Combo {
    pub items: Vec<ComboItem>,
    pub filter: VimField,
    pub sel: usize,
    pub top: usize,
}

impl Combo {
    /// 絞り込んだ一覧（元の番号）
    pub fn shown(&self) -> Vec<usize> {
        let q = self.filter.text().to_lowercase();
        (0..self.items.len()).filter(|&i| q.is_empty() || self.items[i].label.to_lowercase().contains(&q)).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SButton {
    Apply,
    Ok,
    Cancel,
    /// 保存先（全体 ⇔ このプロジェクト）
    Scope,
}

pub struct SettingsDialog {
    /// 直している写し（適用で Editor.settings へ）
    pub work: Settings,
    pub cat: usize,
    pub selected: usize,
    pub top: usize,
    pub focus: SFocus,
    pub filter: VimField,
    pub field: Option<VimField>,
    pub combo: Option<Combo>,
    /// フォントの一覧（初めて開いたときに読む）
    fonts: Option<Vec<(String, bool)>>,
    /// コンボボックスの描いた行（行, 列の始め, 終わり, 一覧の番号）
    pub combo_rows: crate::ui::Hits<usize>,
    /// 保存先をプロジェクトにしているか
    pub project_scope: bool,
    pub dirty: bool,
    pub confirm_close: bool,
    /// 描いた位置（マウス用）: 分類の (行, 番号, 列の始め, 終わり)、項目の (行, 番号, 値の列)、ボタン
    pub cat_rows: crate::ui::Hits<usize>,
    pub item_rows: RefCell<Vec<(usize, usize, usize)>>,
    pub buttons: crate::ui::Hits<SButton>,
    /// 言語のツールの見つかったもの（種類 → 説明の行。開いたときに裏で調べる。WSL のプロジェクトなら WSL の中も）
    pub tool_info: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
    /// ダウンロードできる版（種類 → 一覧。コンボボックスを初めて開いたときに裏で調べる）
    pub releases: Releases,
    waker: nagi_workers::Waker,
}

pub type Releases = std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, nagi_core::Result<Vec<nagi_workers::toolchains::Release>>>>>;

/// 版を調べている間の一覧の行の値
const RELEASES_WAIT: &str = "download:wait";

impl SettingsDialog {
    /// 出している項目（分類と絞り込みで）
    pub fn items(&self) -> Vec<&'static Item> {
        let q = self.filter.text().to_lowercase();
        let cat = CATEGORIES[self.cat];
        SCHEMA
            .iter()
            .filter(|i| {
                if !q.is_empty() {
                    return i.label.to_lowercase().contains(&q) || i.help.to_lowercase().contains(&q) || i.key.contains(&q) || i.cat.contains(&q);
                }
                cat == "（すべて）" || i.cat == cat
            })
            .collect()
    }

    pub fn current(&self) -> Option<&'static Item> {
        self.items().get(self.selected).copied()
    }

    /// 項目の今の値（画面の保存先から見た値）
    pub fn value(&self, it: &Item) -> String {
        self.work.get(it.key)
    }

    /// 画面に出す値の文字
    pub fn display(&self, it: &Item) -> String {
        let v = self.value(it);
        match it.kind {
            SKind::Bool => if self.work.bool(it.key) { "✓ オン".into() } else { "  オフ".into() },
            SKind::Choice(c) => c.iter().find(|x| x.0 == v).map_or(v.clone(), |x| x.1.to_string()),
            SKind::Tool(_) if v.is_empty() => "（自動: PATH から）".into(),
            _ if v.is_empty() => "（既定）".into(),
            _ => v,
        }
    }

    fn set(&mut self, it: &Item, value: &str) {
        let project = self.project_scope && it.project;
        self.work.set(it.key, value, project);
        self.dirty = true;
    }

    /// オン・オフを切り替える／選択肢を前後へ
    fn cycle(&mut self, d: isize) {
        let Some(it) = self.current() else { return };
        match it.kind {
            SKind::Bool => {
                let v = if self.work.bool(it.key) { "false" } else { "true" };
                self.set(it, v);
            }
            SKind::Choice(c) => {
                let v = self.value(it);
                let i = c.iter().position(|x| x.0 == v).unwrap_or(0) as isize;
                let n = (i + d).rem_euclid(c.len() as isize) as usize;
                self.set(it, c[n].0);
            }
            _ => {}
        }
    }

    /// 値を直し始める（数・文字・キー）
    fn edit(&mut self) {
        let Some(it) = self.current() else { return };
        match it.kind {
            SKind::Bool | SKind::Choice(_) => self.cycle(1),
            SKind::Font | SKind::Jdk | SKind::Tool(_) => {
                let items: Vec<ComboItem> = if let SKind::Tool(id) = it.kind {
                    self.fetch_releases(id);
                    tool_items(id, &self.releases)
                } else if it.kind == SKind::Font {
                    self.fonts
                        .get_or_insert_with(nagi_text::font_families)
                        .iter()
                        .map(|(n, mono)| ComboItem { label: n.clone(), value: n.clone(), tag: if *mono { "等幅".into() } else { String::new() } })
                        .collect()
                } else {
                    jdk_items()
                };
                let cur = self.value(it);
                let sel = items.iter().position(|f| f.value.eq_ignore_ascii_case(&cur)).unwrap_or(0);
                self.combo = Some(Combo { items, filter: VimField::new("", false), sel, top: sel.saturating_sub(5) });
                self.focus = SFocus::Combo;
            }
            _ => {
                self.field = Some(VimField::new(&self.value(it), false));
                self.focus = SFocus::Edit;
            }
        }
    }

    /// コンボボックスで選んだものに決める。ダウンロードを選んだら、何をかを返す（Editor が始める。JDK は版、ツールは tool:種類）
    fn choose_combo(&mut self, index: Option<usize>) -> Option<String> {
        let (Some(it), Some(c)) = (self.current(), self.combo.take()) else { return None };
        self.focus = SFocus::Items;
        let shown = c.shown();
        let pick = index.or_else(|| shown.get(c.sel).copied());
        let value = pick.and_then(|i| c.items.get(i)).map(|f| f.value.clone())?;
        if value == RELEASES_WAIT {
            self.focus = SFocus::Combo;
            self.combo = Some(c);
            return None;
        }
        if let Some(v) = value.strip_prefix("download:") {
            return Some(v.to_string());
        }
        self.set(it, &value);
        None
    }

    /// 直した値を決める（数は範囲を確かめる）。うまくいけば None、だめなら理由
    fn commit_edit(&mut self) -> Option<String> {
        let (Some(it), Some(f)) = (self.current(), self.field.take()) else { return None };
        let v = f.text().trim().to_string();
        self.focus = SFocus::Items;
        match it.kind {
            SKind::Int(lo, hi) => match v.parse::<i64>() {
                Ok(n) if n >= lo && n <= hi => self.set(it, &n.to_string()),
                _ => return Some(format!("{} は {lo}〜{hi} の数です", it.label)),
            },
            SKind::Key if !v.is_empty() => match crate::keymap::parse_chord(&v) {
                // 書き方をそろえて入れる（ctrl+alt+s → Ctrl+Alt+S）
                Some(c) => self.set(it, &c.label()),
                None => return Some(format!("キーの書き方が分かりません: {v}（例: Ctrl+Alt+S・Shift+F10）")),
            },
            _ => self.set(it, &v),
        }
        None
    }

    fn reset(&mut self) {
        let Some(it) = self.current() else { return };
        if self.project_scope && it.project {
            self.work.unset_project(it.key);
        } else {
            self.work.global.remove(it.key);
        }
        self.dirty = true;
    }
}

/// 足りないツールを入れ終わったら動かすもの（Editor::fetch_missing_tools）
#[derive(Clone, Debug)]
pub(crate) enum AfterTools {
    Job { cmd: String, root: std::path::PathBuf, title: String, output: bool },
    Config(nagi_workers::runconfig::RunConfig),
}

/// JDK のコンボボックスの一覧: 自動・入っているもの・ダウンロード（Eclipse Temurin）
fn jdk_items() -> Vec<ComboItem> {
    let mut v = vec![ComboItem { label: "自動（JAVA_HOME か nagi が入れたものから探す）".into(), value: String::new(), tag: String::new() }];
    for j in nagi_workers::jdk::installed() {
        let name = j.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        v.push(ComboItem { label: format!("{}  {}", j.version, name), value: j.path.display().to_string(), tag: if j.vendor.is_empty() { "入っている".into() } else { j.vendor.clone() } });
    }
    for ver in nagi_workers::jdk::DOWNLOADS {
        v.push(ComboItem { label: format!("＋ ダウンロード: Eclipse Temurin {ver}"), value: format!("download:{ver}"), tag: "LTS".into() });
    }
    v
}

impl SettingsDialog {
    /// ダウンロードできる版をまだ調べていなければ、裏で調べ始める
    fn fetch_releases(&self, id: &str) {
        let Ok(mut m) = self.releases.lock() else { return };
        if m.contains_key(id) || cfg!(test) {
            return;
        }
        m.insert(id.to_string(), Err(String::new().into()));
        let (rs, id) = (self.releases.clone(), id.to_string());
        let waker = self.waker.clone();
        nagi_core::task::detach("releases", move || {
            let r = nagi_workers::toolchains::releases(&id);
            if let Ok(mut m) = rs.lock() {
                m.insert(id, r.map_err(|e| if e.is_empty() { "調べられません".into() } else { e }));
            }
            waker();
        });
    }

    /// 版を調べ終えたら、開いているコンボボックスの一覧を作り直す（選んでいる行は保つ）。変えたら true
    pub fn refresh_tool_combo(&mut self) -> bool {
        let Some(SKind::Tool(id)) = self.current().map(|i| i.kind) else { return false };
        let Some(c) = &mut self.combo else { return false };
        if !c.items.iter().any(|i| i.value == RELEASES_WAIT) {
            return false;
        }
        let ready = self.releases.lock().ok().is_some_and(|m| matches!(m.get(id), Some(r) if !matches!(r, Err(e) if e.is_empty())));
        if !ready {
            return false;
        }
        // 調べる前の一覧（入っているもの）はそのまま、末尾の「調べています」だけ差し替える
        c.items.retain(|i| i.value != RELEASES_WAIT);
        c.items.extend(release_items(id, &self.releases));
        true
    }
}

/// ダウンロードの行（調べている間は 1 行の「調べています」）
fn release_items(id: &str, releases: &Releases) -> Vec<ComboItem> {
    let Some(t) = nagi_workers::toolchains::def(id) else { return Vec::new() };
    if t.download.is_none() {
        return Vec::new();
    }
    match releases.lock().ok().and_then(|m| m.get(id).cloned()) {
        Some(Ok(v)) => v
            .iter()
            .enumerate()
            .map(|(i, r)| ComboItem { label: format!("＋ ダウンロード: {} {}", t.label, r.version), value: format!("download:tool:{id}:{i}"), tag: if i == 0 && r.note.is_empty() { "最新".into() } else { r.note.clone() } })
            .collect(),
        Some(Err(e)) if !e.is_empty() => vec![ComboItem { label: format!("＋ ダウンロード: {}（いちばん新しい版。一覧は {e}）", t.label), value: format!("download:tool:{id}"), tag: String::new() }],
        _ => vec![ComboItem { label: format!("＋ ダウンロード: {}（版を調べています…）", t.label), value: RELEASES_WAIT.into(), tag: String::new() }],
    }
}

/// 言語のツールのコンボボックスの一覧: 自動・入っているもの・ダウンロード（版ごと）
fn tool_items(id: &str, releases: &Releases) -> Vec<ComboItem> {
    use nagi_workers::toolchains;
    let mut v = vec![ComboItem { label: "自動（PATH から探す）".into(), value: String::new(), tag: String::new() }];
    let Some(t) = toolchains::def(id) else { return v };
    if !cfg!(test) {
        for f in toolchains::installed(id) {
            v.push(ComboItem { label: format!("{}  {}", if f.version.is_empty() { "?" } else { &f.version }, f.bin.display()), value: f.bin.display().to_string(), tag: f.source.into() });
        }
    }
    if t.download.is_some() {
        v.extend(release_items(id, releases));
    }
    v
}

/// 言語のツールの説明の行を裏で調べる（Windows で見つかったもの、WSL のプロジェクトなら WSL の中のもの）
fn probe_tools(root: &std::path::Path, info: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>, waker: nagi_workers::Waker) {
    use nagi_workers::toolchains::{self, TOOLS};
    let wsl = nagi_workers::exec::Exec::in_dir(root).distro().map(String::from);
    nagi_core::task::detach("probe-tools", move || {
        if let Some(d) = &wsl {
            for (id, path, ver) in toolchains::wsl_report(d) {
                let cmd = toolchains::def(id).map_or("", |t| t.wsl);
                let s = if path.is_empty() { format!("WSL（{d}）: {cmd} は入っていません。このプロジェクトでは WSL の中のものを使う") } else { format!("WSL（{d}）: {cmd} {ver}（{path}）を使う") };
                if let Ok(mut m) = info.lock() {
                    m.insert(id.to_string(), s);
                }
            }
            waker();
            return;
        }
        for t in &TOOLS {
            let s = match toolchains::installed(t.id).into_iter().next() {
                Some(f) => format!("PATH など: {} {}（{}）", t.label, f.version, f.bin.join(t.exe).display()),
                None => format!("{} は見つかりません（Enter でダウンロード）", t.label),
            };
            if let Ok(mut m) = info.lock() {
                m.insert(t.id.to_string(), s);
            }
            waker();
        }
    });
}

impl Editor {
    /// コンボボックスで選んだダウンロードを始める（数なら JDK の版、tool:種類 なら言語のツール）
    pub(crate) fn start_download(&mut self, what: &str) {
        match what.strip_prefix("tool:") {
            Some(rest) => {
                // tool:種類:一覧の番号（番号が無ければいちばん新しい版）
                let (id, idx) = match rest.split_once(':') {
                    Some((id, i)) => (id, i.parse::<usize>().ok()),
                    None => (rest, None),
                };
                let release = idx.and_then(|i| self.settings_dialog.as_ref()?.releases.lock().ok()?.get(id)?.as_ref().ok()?.get(i).cloned());
                self.start_tool_download(id, release);
            }
            None => {
                if let Ok(v) = what.parse::<u32>() {
                    self.start_jdk_download(v);
                }
            }
        }
    }

    /// 動かす行に足りないもの（Maven, JDK。Exec::missing_tools）があれば、ダウンロードし始めて `then` を覚え、false を返す
    /// （入れ終わったら then を動かす）。足りていれば true（そのまま動かしてよい）。IDE が無くても動くように、IDE のものは借りない
    pub(crate) fn fetch_missing_tools(&mut self, (maven, jdk): (bool, bool), then: AfterTools) -> bool {
        if !maven && !jdk {
            return true;
        }
        let what = [(maven, "Maven"), (jdk, "JDK")].iter().filter(|(m, _)| *m).map(|(_, n)| *n).collect::<Vec<_>>().join("・");
        // 入れた後でもまだ足りなければ、もう入れに行かない（同じダウンロードを繰り返さない）
        if self.tools_fetched {
            self.notify.error(format!("{what} が見つかりません（設定の「ビルド・実行」で選ぶか、ダウンロードしてください）"));
            return false;
        }
        if maven && self.tool_download.as_ref().is_none_or(|(id, _)| id != "maven") {
            self.start_tool_download("maven", None);
        }
        if jdk && self.jdk_download.is_none() {
            self.start_jdk_download(nagi_workers::jdk::AUTO);
        }
        self.after_tools = Some(then);
        self.notify.info(format!("{what} が無いので、nagi 用にダウンロードして入れます。入れ終わったら続けて動かします"));
        false
    }

    /// ダウンロードが全部終わっていれば、待たせていたものを動かす
    fn run_after_tools(&mut self) {
        if self.tool_download.is_some() || self.jdk_download.is_some() {
            return;
        }
        let Some(then) = self.after_tools.take() else { return };
        self.tools_fetched = true;
        match then {
            AfterTools::Job { cmd, root, title, output } => self.run_job(&cmd, &root, &title, output),
            AfterTools::Config(cfg) => self.run_config(cfg),
        }
        self.tools_fetched = false;
    }

    /// 言語のツールをダウンロードし始める。進み具合は状態の行に、終わったら設定に入れる
    pub(crate) fn start_tool_download(&mut self, id: &str, release: Option<nagi_workers::toolchains::Release>) {
        let label = nagi_workers::toolchains::def(id).map_or(id, |t| t.label);
        if self.tool_download.is_some() {
            self.notify.info("ほかのツールをダウンロードしています");
            return;
        }
        let what = release.as_ref().map_or("最新版".to_string(), |r| r.version.clone());
        if cfg!(test) {
            self.notify.info(format!("（試験）{label} {what} のダウンロード"));
            return;
        }
        let waker = self.waker();
        self.tool_download = Some((id.to_string(), nagi_workers::toolchains::download(id, release, waker)));
        self.notify.info(format!("{label} {what} をダウンロードします…"));
    }

    /// JDK（Eclipse Temurin の `version`）をダウンロードし始める。進み具合は状態の行に、終わったら設定に入れる
    pub(crate) fn start_jdk_download(&mut self, version: u32) {
        if self.jdk_download.is_some() {
            self.notify.info("JDK をダウンロードしています。終わるまでお待ちください");
            return;
        }
        if cfg!(test) {
            self.notify.info(format!("（試験）JDK {version} のダウンロード"));
            return;
        }
        let waker = self.waker();
        self.jdk_download = Some((version, nagi_workers::jdk::download_temurin(version, waker)));
        self.notify.info(format!("Eclipse Temurin {version} をダウンロードしています（約 200MB。{} に置く）", nagi_workers::jdk::jdks_dir().display()));
    }

    /// ダウンロードの進み具合を受け取る。変わったら true
    pub fn poll_jdk_download(&mut self) -> bool {
        let combo = self.settings_dialog.as_mut().is_some_and(|d| d.refresh_tool_combo());
        let changed = combo | self.poll_tool_download() | self.poll_jdk_only();
        if changed {
            self.run_after_tools();
        }
        changed
    }

    fn poll_tool_download(&mut self) -> bool {
        let Some((id, d)) = &self.tool_download else { return false };
        let (id, polled) = (id.clone(), d.poll());
        let label = nagi_workers::toolchains::def(&id).map_or(id.clone(), |t| t.label.to_string());
        let changed = polled.status.is_some() || polled.done.is_some();
        if let Some(st) = polled.status {
            self.notify.progress(format!("{label}: {st}"));
        }
        if let Some(r) = polled.done {
            self.tool_download = None;
            match r {
                Ok(path) => {
                    // 設定の画面でプロジェクトを保存先にしていれば、そのプロジェクトの版に
                    let project = self.settings_dialog.as_ref().is_some_and(|d| d.project_scope);
                    let p = self.set_downloaded(&format!("toolchain.{id}"), &path, project);
                    let scope = if project { "このプロジェクトの設定" } else { "設定" };
                    self.notify.info(format!("{label} を入れました: {p}（{scope}にしました。ターミナル・実行の構成・:Make で使う）"));
                }
                Err(e) => {
                    self.after_tools = None;
                    self.notify.error(format!("{label} をダウンロードできません: {e}"));
                }
            }
        }
        changed
    }

    fn poll_jdk_only(&mut self) -> bool {
        let Some((ver, d)) = &self.jdk_download else { return false };
        let (ver, polled) = (*ver, d.poll());
        let changed = polled.status.is_some() || polled.done.is_some();
        if let Some(st) = polled.status {
            self.notify.progress(format!("Eclipse Temurin {ver}: {st}"));
        }
        if let Some(r) = polled.done {
            self.jdk_download = None;
            match r {
                Ok(path) => {
                    let p = self.set_downloaded("maven.jdk", &path, false);
                    self.notify.info(format!("JDK を入れました: {p}（設定の JDK にしました）"));
                }
                Err(e) => {
                    self.after_tools = None;
                    self.notify.error(format!("JDK をダウンロードできません: {e}"));
                }
            }
        }
        changed
    }

    /// ダウンロードして入れたものの場所を設定に入れて保存し、すぐ使う（設定の画面を開いていればそちらにも）
    fn set_downloaded(&mut self, key: &str, path: &std::path::Path, project: bool) -> String {
        let p = path.display().to_string();
        let old = self.settings.clone();
        self.settings.set(key, &p, project);
        let _ = self.settings.save();
        self.apply_settings(Some(&old));
        if let Some(d) = &mut self.settings_dialog {
            d.work.set(key, &p, project);
        }
        p
    }

    /// 設定の画面を開く（Ctrl+Alt+S・:Settings）。`query` があれば絞り込んで
    pub(crate) fn open_settings(&mut self, query: &str) {
        let mut work = self.settings.clone();
        work.set_root(Some(&self.nav_root()));
        let mut filter = VimField::new(query, false);
        filter.ed.set_state(State::Normal);
        let d = SettingsDialog {
            work,
            cat: 0,
            selected: 0,
            top: 0,
            focus: if query.is_empty() { SFocus::Cats } else { SFocus::Items },
            filter,
            field: None,
            combo: None,
            fonts: None,
            combo_rows: Default::default(),
            project_scope: false,
            dirty: false,
            confirm_close: false,
            cat_rows: Default::default(),
            item_rows: RefCell::new(Vec::new()),
            buttons: Default::default(),
            tool_info: Default::default(),
            releases: Default::default(),
            waker: self.waker(),
        };
        if !cfg!(test) {
            let waker = self.waker();
            probe_tools(&self.nav_root(), d.tool_info.clone(), waker);
        }
        self.settings_dialog = Some(d);
        self.set_state(State::Settings);
        self.notify.clear();
    }

    /// 適用（保存して、動いている nagi に効かせる）
    fn apply_settings_dialog(&mut self) -> bool {
        let Some(d) = &mut self.settings_dialog else { return false };
        if d.focus == SFocus::Edit {
            if let Some(e) = d.commit_edit() {
                self.notify.error(e);
                return false;
            }
        }
        let work = d.work.clone();
        d.dirty = false;
        match work.save() {
            Ok(()) => {
                let old = std::mem::replace(&mut self.settings, work);
                self.apply_settings(Some(&old));
                self.notify.info("設定を保存しました");
                true
            }
            Err(e) => {
                self.notify.error(format!("設定を保存できません: {e}"));
                false
            }
        }
    }

    pub(crate) fn settings_button(&mut self, b: SButton) {
        match b {
            SButton::Apply => {
                self.apply_settings_dialog();
            }
            SButton::Ok => {
                if self.apply_settings_dialog() {
                    self.settings_dialog = None;
                    self.set_state(State::Normal);
                }
            }
            SButton::Cancel => {
                self.settings_dialog = None;
                self.set_state(State::Normal);
                self.notify.info("設定の画面を閉じました（適用していない変更は捨てました）");
            }
            SButton::Scope => {
                if let Some(d) = &mut self.settings_dialog {
                    d.project_scope = !d.project_scope;
                }
            }
        }
    }

    pub(crate) fn settings_insert(&mut self, text: &str) {
        let Some(d) = &mut self.settings_dialog else { return };
        match d.focus {
            SFocus::Edit => {
                if let Some(f) = &mut d.field {
                    f.insert_text(text);
                }
            }
            SFocus::Filter => {
                d.filter.insert_text(text);
                d.selected = 0;
                d.top = 0;
            }
            SFocus::Combo => {
                if let Some(c) = &mut d.combo {
                    c.filter.insert_text(text);
                    c.sel = 0;
                    c.top = 0;
                }
            }
            _ => {}
        }
    }

    /// 入力欄にいて Insert か（IME を使う）
    pub fn settings_typing(&self) -> bool {
        self.settings_dialog.as_ref().is_some_and(|d| match d.focus {
            SFocus::Edit => d.field.as_ref().is_some_and(|f| f.is_insert()),
            SFocus::Combo => d.combo.as_ref().is_some_and(|c| c.filter.is_insert()),
            SFocus::Filter => d.filter.is_insert(),
            _ => false,
        })
    }

    pub(crate) fn settings_key(&mut self, key: Key) {
        let Some(d) = &mut self.settings_dialog else {
            self.set_state(State::Normal);
            return;
        };
        if key == Key::Ctrl('m') {
            self.settings_button(SButton::Ok);
            return;
        }
        let confirm = std::mem::take(&mut d.confirm_close);
        match d.focus {
            SFocus::Combo => {
                let n = d.combo.as_ref().map_or(0, |c| c.shown().len());
                let normal = d.combo.as_ref().is_some_and(|c| !c.filter.is_insert());
                // 打って絞り込む一覧: ↑↓ Ctrl+n/p など。ノーマルでは j・k も
                let sel = d.combo.as_ref().map_or(0, |c| c.sel);
                let moved = crate::list::step_typing(key, sel, n).or_else(|| if normal { crate::list::step(key, sel, n, 10) } else { None });
                match key {
                    _ if moved.is_some() => {
                        if let Some(c) = &mut d.combo {
                            c.sel = moved.unwrap_or(c.sel);
                        }
                    }
                    Key::Enter => {
                        if let Some(v) = d.choose_combo(None) {
                            self.start_download(&v);
                        }
                    }
                    Key::Esc if normal || d.combo.as_ref().is_some_and(|c| c.filter.text().is_empty()) => {
                        d.combo = None;
                        d.focus = SFocus::Items;
                    }
                    _ => {
                        self.feed_field(key, |e| Some(&mut e.settings_dialog.as_mut()?.combo.as_mut()?.filter));
                        if let Some(c) = self.settings_dialog.as_mut().and_then(|d| d.combo.as_mut()) {
                            c.sel = 0;
                            c.top = 0;
                        }
                    }
                }
            }
            SFocus::Edit => {
                let out = self.feed_field(key, |e| e.settings_dialog.as_mut()?.field.as_mut());
                let Some(d) = &mut self.settings_dialog else { return };
                match out {
                    FieldOut::Submit => {
                        if let Some(e) = d.commit_edit() {
                            self.notify.error(e);
                        }
                    }
                    FieldOut::Cancel => {
                        d.field = None;
                        d.focus = SFocus::Items;
                    }
                    _ => {}
                }
            }
            SFocus::Filter => {
                let out = self.feed_field(key, |e| Some(&mut e.settings_dialog.as_mut()?.filter));
                let Some(d) = &mut self.settings_dialog else { return };
                d.selected = 0;
                d.top = 0;
                match out {
                    FieldOut::Submit | FieldOut::Cancel => d.focus = SFocus::Items,
                    _ => {}
                }
                if matches!(key, Key::Down | Key::Tab) {
                    d.focus = SFocus::Items;
                }
            }
            SFocus::Cats => match key {
                Key::Char('j') | Key::Down => {
                    d.cat = (d.cat + 1).min(CATEGORIES.len() - 1);
                    d.selected = 0;
                    d.top = 0;
                }
                Key::Char('k') | Key::Up => {
                    d.cat = d.cat.saturating_sub(1);
                    d.selected = 0;
                    d.top = 0;
                }
                Key::Char('l') | Key::Enter | Key::Tab | Key::Right => d.focus = SFocus::Items,
                Key::Char('/') => {
                    d.focus = SFocus::Filter;
                    d.filter.reinsert();
                }
                Key::Char('p') => d.project_scope = !d.project_scope,
                Key::Esc | Key::Char('q') => self.close_settings(confirm),
                _ => {}
            },
            SFocus::Items => {
                let n = d.items().len();
                let moved = crate::list::step(key, d.selected, n, 10);
                match key {
                    _ if moved.is_some() => d.selected = moved.unwrap_or(d.selected),
                    Key::Char('g') => d.selected = 0,
                    Key::Tab => d.focus = SFocus::Cats,
                    Key::Char('h') | Key::Left => {
                        if d.current().is_some_and(|i| matches!(i.kind, SKind::Choice(_))) {
                            d.cycle(-1);
                        } else {
                            d.focus = SFocus::Cats;
                        }
                    }
                    Key::Char('l') | Key::Right => d.cycle(1),
                    Key::Char(' ') | Key::Enter | Key::Char('i') => d.edit(),
                    Key::Char('r') => d.reset(),
                    Key::Char('p') => d.project_scope = !d.project_scope,
                    Key::Char('/') => {
                        d.focus = SFocus::Filter;
                        d.filter.reinsert();
                    }
                    Key::Esc | Key::Char('q') => self.close_settings(confirm),
                    _ => {}
                }
            }
        }
    }

    fn close_settings(&mut self, confirm: bool) {
        let Some(d) = &mut self.settings_dialog else { return };
        if d.dirty && !confirm {
            d.confirm_close = true;
            self.notify.info("適用していない変更があります。もう一度 Esc で捨てて閉じる、Ctrl+Enter で適用して閉じる");
        } else {
            self.settings_button(SButton::Cancel);
        }
    }

    /// マウス: 画面の (行, 列) を押した
    pub(crate) fn settings_click(&mut self, row: usize, col: usize, clicks: u32) {
        let Some(d) = &mut self.settings_dialog else { return };
        // コンボボックスを開いていれば、その行を押すと決める（外を押すと閉じる）
        if d.combo.is_some() {
            let hit = d.combo_rows.at(row, col);
            let mut sel = hit.unwrap_or(0);
            // 選択肢は 1 回で決める（メニューと同じ）
            match crate::list::pick(&mut sel, hit, clicks, 1) {
                Some(crate::list::Pick::Accept(i) | crate::list::Pick::Select(i)) => {
                    if let Some(v) = d.choose_combo(Some(i)) {
                        self.start_download(&v);
                    }
                }
                None => {
                    d.combo = None;
                    d.focus = SFocus::Items;
                }
            }
            return;
        }
        let button = d.buttons.at(row, col);
        if let Some(b) = button {
            self.settings_button(b);
            return;
        }
        let cat = d.cat_rows.at(row, col);
        let mut c = d.cat;
        if let Some(pick) = crate::list::pick(&mut c, cat, clicks, 2) {
            if c != d.cat {
                d.selected = 0;
                d.top = 0;
            }
            d.cat = c;
            // ダブルクリックは Enter と同じ（項目へ）
            d.focus = if matches!(pick, crate::list::Pick::Accept(_)) { SFocus::Items } else { SFocus::Cats };
            return;
        }
        let item = d.item_rows.borrow().iter().find(|(r, _, _)| *r == row).map(|x| (x.1, x.2));
        if let Some((i, vx)) = item {
            let again = d.selected == i && d.focus == SFocus::Items;
            let pick = crate::list::pick(&mut d.selected, Some(i), clicks, 2);
            d.focus = SFocus::Items;
            // 値の上を押すか、選んでいる項目をもう一度押すか、ダブルクリックで、切り替え・直す
            if col >= vx || again || matches!(pick, Some(crate::list::Pick::Accept(_))) {
                d.edit();
            }
        }
    }
}



/// この部品の入口（component.rs）
pub struct SettingsComponent;

impl crate::component::Component for SettingsComponent {
    fn key(&self, ed: &mut crate::editor::Editor, key: nagi_vim::Key) -> crate::editor::Effect {
        ed.settings_key(key);
        crate::editor::Effect::None
    }

    fn text(&self, ed: &mut crate::editor::Editor, text: &str) {
        ed.settings_insert(text);
    }

    fn click(&self, ed: &mut crate::editor::Editor, row: usize, col: usize, clicks: u32) -> bool {
        ed.settings_click(row, col, clicks);
        true
    }
}


/// 設定の画面（layout.rs の View）
pub struct SettingsView;

impl crate::layout::View for SettingsView {
    fn rect(&self, ed: &crate::editor::Editor, s: crate::layout::Screen) -> Option<crate::panes::Rect> {
        let _ = s;
        ed.settings_dialog.as_ref()?;
        Some(crate::panes::Rect { x: 0, y: 0, w: s.cols, h: s.rows })
    }

    fn draw(&self, ed: &crate::editor::Editor, p: &mut crate::view::Painter, r: crate::panes::Rect, s: crate::layout::Screen) {
        let _ = (r, s);
        if let Some(d) = &ed.settings_dialog {
            draw::draw_settings_dialog(p, ed, d, s.cols, s.rows);
        }
    }

    fn wheel(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, row: usize, col: usize, dy: isize, dx: isize) -> bool {
        let _ = (r, row, col, dx);
        crate::layout::wheel_keys(ed, dy)
    }

    fn context(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, row: usize, col: usize) -> bool {
        use nagi_vim::Key::*;
        let _ = r;
        // 押した項目を選んでから（1 回のクリックは選ぶだけ）
        ed.settings_click(row, col, 1);
        let m = crate::menu::Menu::new(row, col)
            .keys("値を直す", &[Enter])
            .keys("既定に戻す", &[Char('r')])
            .keys("全体 / このプロジェクトだけ", &[Char('p')])
            .keys("絞り込み", &[Char('/')])
            .sep()
            .keys("閉じる", &[Esc]);
        ed.open_menu(m);
        true
    }
}

/// 描き方（view.rs の Painter で描く）
pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    /// 設定の画面（画面の中央に重ねる）。上に絞り込み、左に分類、右に項目（名前と値）、下に説明とボタン
    pub(crate) fn draw_settings_dialog(p: &mut Painter, ed: &Editor, d: &crate::settings_dialog::SettingsDialog, cols: usize, rows: usize) {
        use crate::settings::{CATEGORIES, SKind};
        use crate::settings_dialog::{SButton, SFocus};
        let cell = p.fonts.cell();
        let w = cols.saturating_sub(6).clamp(40, 130).min(cols);
        let h = rows.saturating_sub(2).clamp(12, 34).min(rows);
        let (x0, y0) = dialog_frame(p, cols, rows, w, h);
        let y = |r: usize| (y0 + r) as f32 * cell.h;
        let saved = p.cols;
        p.cols = x0 + w;
        let mut buttons = Vec::new();
        // 1 行目: 題と絞り込みの欄、保存先
        let end = p.text(" 設定   ", x0, y(0), FG);
        let fend = if d.focus == SFocus::Filter {
            let e = p.text("🔍 ", end, y(0), FG);
            draw_vim_field(p, &d.filter, e, y(0), &ed.preedit)
        } else {
            let t = d.filter.text();
            p.text(&format!("🔍 {}", if t.is_empty() { "/ で絞り込み".to_string() } else { t }), end, y(0), STATUS_FG)
        };
        let scope = if d.project_scope { " 保存先: このプロジェクト（p で切り替え） " } else { " 保存先: 全体（p で切り替え） " };
        let sw = p.width(scope);
        let sx = (x0 + w).saturating_sub(sw + 1).max(fend + 2);
        p.out.push(Instance::fill(sx as f32 * cell.w, y(0), sw as f32 * cell.w, cell.h, if d.project_scope { rgb(0x2c4a70) } else { MATCH_BG }));
        p.text(scope, sx, y(0), FG);
        buttons.push((y0, sx, sx + sw, SButton::Scope));
        // 左: 分類
        let lw = 16;
        let mut cat_rows = Vec::new();
        p.cols = x0 + lw;
        let filtering = !d.filter.text().is_empty();
        for (i, c) in CATEGORIES.iter().enumerate() {
            let r = 2 + i;
            let sel = i == d.cat && !filtering;
            if sel {
                p.out.push(Instance::fill(x0 as f32 * cell.w, y(r), lw as f32 * cell.w, cell.h, if d.focus == SFocus::Cats { FG } else { MATCH_BG }));
            }
            p.text(&format!(" {c}"), x0, y(r), if sel && d.focus == SFocus::Cats { BG } else if filtering { STATUS_FG } else { FG });
            cat_rows.push((y0 + r, x0, x0 + lw, i));
        }
        p.out.push(Instance::fill((x0 + lw) as f32 * cell.w + cell.w * 0.5, y(1), 1.0, (h - 3) as f32 * cell.h, LINE_NO));
        // 右: 項目
        let rx = x0 + lw + 2;
        let items = d.items();
        let body = h.saturating_sub(6);
        let top = d.selected.saturating_sub(body.saturating_sub(1));
        let label_w = items.iter().map(|i| p.width(i.label)).max().unwrap_or(10).min(34) + 2;
        let vx = rx + label_w;
        let mut item_rows = Vec::new();
        p.cols = x0 + w;
        for (k, it) in items.iter().enumerate().skip(top).take(body) {
            let r = 2 + k - top;
            let sel = k == d.selected;
            let focused = sel && matches!(d.focus, SFocus::Items | SFocus::Edit);
            if focused {
                p.out.push(Instance::fill(rx as f32 * cell.w - cell.w * 0.5, y(r), 2.0, cell.h, ACCENT_FG));
            }
            if sel {
                p.out.push(Instance::fill(rx as f32 * cell.w, y(r), (x0 + w - rx - 1) as f32 * cell.w, cell.h, MATCH_BG));
            }
            let saved2 = p.cols;
            p.cols = vx.saturating_sub(1);
            p.text(it.label, rx, y(r), if focused { FG } else { rgb(0xb0b0b0) });
            p.cols = saved2;
            // 値（直している項目は入力欄）
            let overridden = d.work.project.contains_key(it.key) && it.project;
            let changed = d.work.global.contains_key(it.key) || overridden;
            if sel && d.focus == SFocus::Edit {
                if let Some(f) = &d.field {
                    draw_vim_field(p, f, vx, y(r), &ed.preedit);
                }
            } else {
                let v = d.display(it);
                let color = match it.kind {
                    SKind::Bool if d.work.bool(it.key) => GIT_ADD,
                    _ if changed => FG,
                    _ => STATUS_FG,
                };
                let shown = match it.kind {
                    SKind::Choice(_) if focused => format!("◀ {v} ▶"),
                    _ => v,
                };
                let end = p.text(&shown, vx, y(r), color);
                if overridden {
                    p.text("  P", end, y(r), ACCENT_FG);
                }
            }
            item_rows.push((y0 + r, k, vx));
        }
        if items.is_empty() {
            p.text("合う項目がありません", rx, y(2), STATUS_FG);
        }
        // 説明（選んだ項目）
        if let Some(it) = d.current() {
            let mut desc = it.help.to_string();
            if it.project {
                desc = format!("{desc}{}プロジェクトごとに変えられる（p）", if desc.is_empty() { "" } else { "。" });
            }
            let def = match it.kind {
                SKind::Choice(c) => c.iter().find(|x| x.0 == it.default).map_or(it.default, |x| x.1),
                _ if it.default.is_empty() => "空",
                _ => it.default,
            };
            // JDK は、使う java の場所も出す
            let desc = if it.kind == SKind::Jdk {
                let jdk = d.work.text(it.key).map(std::path::PathBuf::from).or_else(nagi_workers::jobs::java_home_fix).or_else(|| std::env::var_os("JAVA_HOME").map(std::path::PathBuf::from));
                match jdk {
                    Some(j) => format!("java: {}", j.join("bin").join("java.exe").display()),
                    None => "java: 見つかりません（Enter でダウンロード）".into(),
                }
            } else if let SKind::Tool(id) = it.kind {
                // 使うもの: 設定したもの、無ければ裏で調べた結果（WSL のプロジェクトなら WSL の中のもの）
                let info = d.tool_info.lock().ok().and_then(|m| m.get(id).cloned());
                let exe = nagi_workers::toolchains::def(id).map_or("", |t| t.exe);
                match (d.work.text(it.key), info) {
                    (_, Some(i)) if i.starts_with("WSL") => i,
                    (Some(b), _) => format!("使う: {}", std::path::Path::new(&b).join(exe).display()),
                    (None, Some(i)) => i,
                    (None, None) => "調べています…".into(),
                }
            } else {
                desc
            };
            p.text(&format!("{desc}{}既定: {def}   r で既定に戻す", if desc.is_empty() { "" } else { "   " }), rx, y(h - 3), STATUS_FG);
        }
        // 下: 案内とボタン
        let help = match d.focus {
            SFocus::Cats => "j/k 分類  Tab 項目へ  / 絞り込み  Ctrl+Enter 適用して閉じる  Esc 閉じる",
            SFocus::Items => "j/k 選ぶ  Space・Enter 切り替え・直す  h/l 選択肢  Tab 分類へ  Ctrl+Enter OK",
            SFocus::Edit => "Enter 決める  Esc ノーマル→やめる",
            SFocus::Filter => "打って絞り込み  Enter・↓ 項目へ",
            SFocus::Combo => "打って絞り込み  ↑↓ 選ぶ  Enter・クリック 決める  Esc やめる",
        };
        let by = h - 1;
        buttons.extend(dialog_buttons(p, x0 + w, y0 + by, &[(" 取り消し ", SButton::Cancel), (" 適用 ", SButton::Apply), (" OK ", SButton::Ok)], SButton::Ok));
        p.cols = x0 + w.saturating_sub(30);
        p.text(help, x0 + 1, y(by), STATUS_FG);
        // コンボボックス（フォントの一覧）: 選んでいる項目の下（入らなければ上）に重ねる
        let mut combo_rows = Vec::new();
        if let (Some(c), Some(&(irow, _, ivx))) = (&d.combo, item_rows.iter().find(|r| r.1 == d.selected)) {
            let shown = c.shown();
            let cw = 64.min((x0 + w).saturating_sub(ivx + 1)).max(20);
            let list_h = shown.len().clamp(1, 12);
            let total = list_h + 1;
            let below = irow + 1 + total <= y0 + h - 2;
            let top_row = if below { irow + 1 } else { irow.saturating_sub(total) };
            let cy = |r: usize| (top_row + r) as f32 * cell.h;
            p.out.push(Instance::fill(ivx as f32 * cell.w - 1.0, cy(0) - 1.0, cw as f32 * cell.w + 2.0, total as f32 * cell.h + 2.0, ACCENT_FG));
            p.out.push(Instance::fill(ivx as f32 * cell.w, cy(0), cw as f32 * cell.w, total as f32 * cell.h, rgb(0x1c1c1c)));
            p.cols = ivx + cw;
            let e = p.text("🔍 ", ivx, cy(0), FG);
            if c.filter.text().is_empty() && c.filter.is_insert() {
                p.caret(e, cy(0));
                p.text(&format!("打って絞り込み（{} 件）", shown.len()), e + 1, cy(0), LINE_NO);
            } else {
                draw_vim_field(p, &c.filter, e, cy(0), &ed.preedit);
            }
            let sel = c.sel.min(shown.len().saturating_sub(1));
            let start = sel.saturating_sub(list_h.saturating_sub(1)).max(c.top.min(sel));
            for (k, &i) in shown.iter().enumerate().skip(start).take(list_h) {
                let r = 1 + k - start;
                let on = k == sel;
                if on {
                    p.out.push(Instance::fill(ivx as f32 * cell.w, cy(r), cw as f32 * cell.w, cell.h, ACCENT_FG));
                }
                let item = &c.items[i];
                let end = p.text(&format!(" {}", item.label), ivx, cy(r), if on { BG } else { FG });
                if !item.tag.is_empty() {
                    let tag = format!("{} ", item.tag);
                    let tag = tag.as_str();
                    let tx = (ivx + cw).saturating_sub(p.width(tag));
                    if tx > end {
                        p.text(tag, tx, cy(r), if on { BG } else { STATUS_FG });
                    }
                }
                combo_rows.push((top_row + r, ivx, ivx + cw, i));
            }
            if shown.is_empty() {
                p.text(" 合うフォントがありません", ivx, cy(1), STATUS_FG);
            }
        }
        d.combo_rows.set(combo_rows);
        p.cols = saved;
        d.cat_rows.set(cat_rows);
        *d.item_rows.borrow_mut() = item_rows;
        d.buttons.set(buttons);
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

    #[test]
    fn edit_and_apply() {
        let dir = std::env::temp_dir().join(format!("nagi-setdlg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join("a.txt"), "x\n").unwrap();
        let mut e = Editor::new(Buffer::open(&dir.join("a.txt")).unwrap(), None);
        e.buf.path = Some(dir.join("a.txt"));
        e.set_screen(140, 40);
        e.open_settings("");
        assert_eq!(e.state(), State::Settings);
        // エディタの分類 → 行番号（選択肢）を l で相対に
        keys(&mut e, "j<Tab>");
        let d = e.settings_dialog.as_ref().unwrap();
        assert_eq!(d.current().unwrap().key, "editor.line_numbers");
        keys(&mut e, "l");
        assert_eq!(e.settings_dialog.as_ref().unwrap().work.get("editor.line_numbers"), "relative");
        // 絞り込み → 数の項目を直す（範囲の外は断る）
        keys(&mut e, "/インデントの幅<CR>");
        assert_eq!(e.settings_dialog.as_ref().unwrap().current().unwrap().key, "editor.indent_width");
        keys(&mut e, "<CR><Esc>S99<CR>");
        assert!(e.notify.text.contains("1〜16"), "{}", e.notify.text);
        keys(&mut e, "<CR><Esc>S2<CR>");
        assert_eq!(e.settings_dialog.as_ref().unwrap().work.int("editor.indent_width"), 2);
        // p でこのプロジェクトに。オン・オフは Space
        keys(&mut e, "p");
        assert!(e.settings_dialog.as_ref().unwrap().project_scope);
        // Ctrl+Enter で適用して閉じる → Editor.settings に入り、プロジェクトのファイルは書かない（全体に入れたので）
        keys(&mut e, "<C-m>");
        assert_eq!(e.state(), State::Normal, "{}", e.notify.text);
        assert_eq!(e.settings.get("editor.line_numbers"), "relative");
        assert_eq!(e.settings.int("editor.indent_width"), 2);
        // 直してから Esc を 1 回だと確かめる
        e.open_settings("波線");
        keys(&mut e, " <Esc>");
        assert_eq!(e.state(), State::Settings);
        keys(&mut e, "<Esc>");
        assert_eq!(e.state(), State::Normal);
        assert!(e.settings.bool("editor.squiggles"), "捨てたので変わらない");
    }

    /// フォントはコンボボックス: 開いて絞り込み、選んで決める（一覧は Windows に入っているもの）
    #[test]
    fn font_combo() {
        let mut e = Editor::new(Buffer::from_str(""), None);
        e.set_screen(140, 40);
        e.open_settings("フォント（英数字）");
        keys(&mut e, "<CR>");
        let d = e.settings_dialog.as_ref().unwrap();
        assert_eq!(d.focus, SFocus::Combo);
        let c = d.combo.as_ref().unwrap();
        assert!(c.items.iter().any(|f| f.label == "Consolas" && f.tag == "等幅"), "Consolas は等幅として一覧にある");
        // 打って絞り込み、Enter で決める
        keys(&mut e, "consol<CR>");
        let d = e.settings_dialog.as_ref().unwrap();
        assert_eq!(d.focus, SFocus::Items);
        assert_eq!(d.work.get("appearance.font"), "Consolas");
        // Esc でやめると変わらない
        keys(&mut e, "<CR>cour<Esc><Esc>");
        assert_eq!(e.settings_dialog.as_ref().unwrap().work.get("appearance.font"), "Consolas");
    }

    /// JDK はコンボボックス: 自動・入っているもの・ダウンロード。ダウンロードを選ぶと始める（試験では始めない）
    #[test]
    fn jdk_combo() {
        let mut e = Editor::new(Buffer::from_str(""), None);
        e.set_screen(140, 40);
        e.open_settings("JDK（コンパイル");
        keys(&mut e, "<CR>");
        let c = e.settings_dialog.as_ref().unwrap().combo.as_ref().unwrap();
        assert!(c.items[0].label.starts_with("自動"));
        assert!(c.items.iter().any(|i| i.value == "download:21"));
        keys(&mut e, "temurin 21<CR>");
        assert!(e.notify.text.contains("JDK 21"), "{}", e.notify.text);
        assert_eq!(e.settings_dialog.as_ref().unwrap().work.get("maven.jdk"), "", "ダウンロードが終わるまでは変えない");
    }

    /// 言語のツール: 版を調べている間は 1 行、届いたら版ごとの行に。選ぶとその版をダウンロード
    #[test]
    fn tool_versions() {
        use nagi_workers::toolchains::Release;
        let mut e = Editor::new(Buffer::from_str(""), None);
        e.set_screen(140, 40);
        e.open_settings("Node.js");
        keys(&mut e, "<CR>");
        let d = e.settings_dialog.as_ref().unwrap();
        assert!(d.combo.as_ref().unwrap().items.iter().any(|i| i.value == RELEASES_WAIT), "調べている行");
        // 調べ終えた（試験ではネットワークを使わないので、ここで入れる）
        let rs = vec![Release { version: "24.22.0".into(), url: "u1".into(), note: "LTS".into() }, Release { version: "22.30.0".into(), url: "u2".into(), note: "LTS".into() }];
        d.releases.lock().unwrap().insert("node".into(), Ok(rs));
        assert!(e.poll_jdk_download(), "一覧を作り直す");
        let c = e.settings_dialog.as_ref().unwrap().combo.as_ref().unwrap();
        let labels: Vec<&str> = c.items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"＋ ダウンロード: Node.js 22.30.0") && !c.items.iter().any(|i| i.value == RELEASES_WAIT), "{labels:?}");
        keys(&mut e, "22.30<CR>");
        assert!(e.notify.text.contains("Node.js 22.30.0"), "{}", e.notify.text);
    }
}
