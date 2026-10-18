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

use crate::editor::{Editor, Mode};
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
    pub combo_rows: RefCell<Vec<(usize, usize, usize, usize)>>,
    /// 保存先をプロジェクトにしているか
    pub project_scope: bool,
    pub dirty: bool,
    pub confirm_close: bool,
    /// 描いた位置（マウス用）: 分類の (行, 番号, 列の始め, 終わり)、項目の (行, 番号, 値の列)、ボタン
    pub cat_rows: RefCell<Vec<(usize, usize, usize, usize)>>,
    pub item_rows: RefCell<Vec<(usize, usize, usize)>>,
    pub buttons: RefCell<Vec<(usize, usize, usize, SButton)>>,
}

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
            SKind::Font | SKind::Jdk => {
                let items: Vec<ComboItem> = if it.kind == SKind::Font {
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

    /// コンボボックスで選んだものに決める。ダウンロードを選んだら、その版を返す（Editor が始める）
    fn choose_combo(&mut self, index: Option<usize>) -> Option<u32> {
        let (Some(it), Some(c)) = (self.current(), self.combo.take()) else { return None };
        self.focus = SFocus::Items;
        let shown = c.shown();
        let pick = index.or_else(|| shown.get(c.sel).copied());
        let value = pick.and_then(|i| c.items.get(i)).map(|f| f.value.clone())?;
        if let Some(v) = value.strip_prefix("download:") {
            return v.parse().ok();
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

/// JDK のコンボボックスの一覧: 自動・入っているもの・ダウンロード（Eclipse Temurin）
fn jdk_items() -> Vec<ComboItem> {
    let mut v = vec![ComboItem { label: "自動（JAVA_HOME か ~\\.jdks から探す）".into(), value: String::new(), tag: String::new() }];
    for j in nagi_workers::jdk::installed() {
        let name = j.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        v.push(ComboItem { label: format!("{}  {}", j.version, name), value: j.path.display().to_string(), tag: if j.vendor.is_empty() { "入っている".into() } else { j.vendor.clone() } });
    }
    for ver in nagi_workers::jdk::DOWNLOADS {
        v.push(ComboItem { label: format!("＋ ダウンロード: Eclipse Temurin {ver}"), value: format!("download:{ver}"), tag: "LTS".into() });
    }
    v
}

impl Editor {
    /// JDK（Eclipse Temurin の `version`）をダウンロードし始める。進み具合は状態の行に、終わったら設定に入れる
    pub(crate) fn start_jdk_download(&mut self, version: u32) {
        if self.jdk_download.is_some() {
            self.message = "JDK をダウンロードしています。終わるまでお待ちください".into();
            return;
        }
        if cfg!(test) {
            self.message = format!("（試験）JDK {version} のダウンロード");
            return;
        }
        let waker = self.waker.clone().unwrap_or_else(|| std::sync::Arc::new(|| {}));
        self.jdk_download = Some((version, nagi_workers::jdk::download_temurin(version, waker)));
        self.message = format!("Eclipse Temurin {version} をダウンロードしています（約 200MB。~\\.jdks に置く）");
    }

    /// ダウンロードの進み具合を受け取る。変わったら true
    pub fn poll_jdk_download(&mut self) -> bool {
        use nagi_workers::drivers::DlMsg;
        let Some((ver, d)) = &self.jdk_download else { return false };
        let ver = *ver;
        let mut changed = false;
        let mut done = None;
        while let Ok(m) = d.rx.try_recv() {
            changed = true;
            match m {
                DlMsg::Progress(got, _) => self.message = format!("Eclipse Temurin {ver} をダウンロードしています… {} MB", got / 1024 / 1024),
                DlMsg::Extracting => self.message = format!("Eclipse Temurin {ver} を展開しています…"),
                DlMsg::Done(r) => done = Some(r),
            }
        }
        if let Some(r) = done {
            self.jdk_download = None;
            match r {
                Ok(path) => {
                    let p = path.display().to_string();
                    let old = self.settings.clone();
                    self.settings.set("maven.jdk", &p, false);
                    let _ = self.settings.save();
                    self.apply_settings(Some(&old));
                    if let Some(d) = &mut self.settings_dialog {
                        d.work.set("maven.jdk", &p, false);
                    }
                    self.message = format!("JDK を入れました: {p}（設定の JDK にしました）");
                }
                Err(e) => self.message = format!("JDK をダウンロードできません: {e}"),
            }
        }
        changed
    }

    /// 設定の画面を開く（Ctrl+Alt+S・:Settings）。`query` があれば絞り込んで
    pub(crate) fn open_settings(&mut self, query: &str) {
        let mut work = self.settings.clone();
        work.set_root(Some(&self.nav_root()));
        let mut filter = VimField::new(query, false);
        filter.ed.mode = Mode::Normal;
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
            combo_rows: RefCell::new(Vec::new()),
            project_scope: false,
            dirty: false,
            confirm_close: false,
            cat_rows: RefCell::new(Vec::new()),
            item_rows: RefCell::new(Vec::new()),
            buttons: RefCell::new(Vec::new()),
        };
        self.settings_dialog = Some(d);
        self.mode = Mode::Settings;
        self.message.clear();
    }

    /// 適用（保存して、動いている nagi に効かせる）
    fn apply_settings_dialog(&mut self) -> bool {
        let Some(d) = &mut self.settings_dialog else { return false };
        if d.focus == SFocus::Edit {
            if let Some(e) = d.commit_edit() {
                self.message = e;
                return false;
            }
        }
        let work = d.work.clone();
        d.dirty = false;
        match work.save() {
            Ok(()) => {
                let old = std::mem::replace(&mut self.settings, work);
                self.apply_settings(Some(&old));
                self.message = "設定を保存しました".into();
                true
            }
            Err(e) => {
                self.message = format!("設定を保存できません: {e}");
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
                    self.mode = Mode::Normal;
                }
            }
            SButton::Cancel => {
                self.settings_dialog = None;
                self.mode = Mode::Normal;
                self.message = "設定の画面を閉じました（適用していない変更は捨てました）".into();
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
            self.mode = Mode::Normal;
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
                match key {
                    Key::Down | Key::Tab | Key::Ctrl('n') | Key::Ctrl('j') => {
                        if let Some(c) = &mut d.combo {
                            c.sel = (c.sel + 1).min(n.saturating_sub(1));
                        }
                    }
                    Key::Char('j') if normal => {
                        if let Some(c) = &mut d.combo {
                            c.sel = (c.sel + 1).min(n.saturating_sub(1));
                        }
                    }
                    Key::Up | Key::Ctrl('p') | Key::Ctrl('k') => {
                        if let Some(c) = &mut d.combo {
                            c.sel = c.sel.saturating_sub(1);
                        }
                    }
                    Key::Char('k') if normal => {
                        if let Some(c) = &mut d.combo {
                            c.sel = c.sel.saturating_sub(1);
                        }
                    }
                    Key::Enter => {
                        if let Some(v) = d.choose_combo(None) {
                            self.start_jdk_download(v);
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
                            self.message = e;
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
                match key {
                    Key::Char('j') | Key::Down => d.selected = (d.selected + 1).min(n.saturating_sub(1)),
                    Key::Char('k') | Key::Up => d.selected = d.selected.saturating_sub(1),
                    Key::Char('g') => d.selected = 0,
                    Key::Char('G') => d.selected = n.saturating_sub(1),
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
            self.message = "適用していない変更があります。もう一度 Esc で捨てて閉じる、Ctrl+Enter で適用して閉じる".into();
        } else {
            self.settings_button(SButton::Cancel);
        }
    }

    /// マウス: 画面の (行, 列) を押した
    pub(crate) fn settings_click(&mut self, row: usize, col: usize) {
        let Some(d) = &mut self.settings_dialog else { return };
        // コンボボックスを開いていれば、その行を押すと決める（外を押すと閉じる）
        if d.combo.is_some() {
            let hit = d.combo_rows.borrow().iter().find(|(r, c0, c1, _)| *r == row && col >= *c0 && col < *c1).map(|x| x.3);
            match hit {
                Some(i) => {
                    if let Some(v) = d.choose_combo(Some(i)) {
                        self.start_jdk_download(v);
                    }
                }
                None => {
                    d.combo = None;
                    d.focus = SFocus::Items;
                }
            }
            return;
        }
        let button = d.buttons.borrow().iter().find(|(r, c0, c1, _)| *r == row && col >= *c0 && col < *c1).map(|b| b.3);
        if let Some(b) = button {
            self.settings_button(b);
            return;
        }
        let cat = d.cat_rows.borrow().iter().find(|(r, _, c0, c1)| *r == row && col >= *c0 && col < *c1).map(|x| x.1);
        if let Some(i) = cat {
            d.cat = i;
            d.selected = 0;
            d.top = 0;
            d.focus = SFocus::Cats;
            return;
        }
        let item = d.item_rows.borrow().iter().find(|(r, _, _)| *r == row).map(|x| (x.1, x.2));
        if let Some((i, vx)) = item {
            let again = d.selected == i && d.focus == SFocus::Items;
            d.selected = i;
            d.focus = SFocus::Items;
            // 値の上を押すか、選んでいる項目をもう一度押すと、切り替え・直す
            if col >= vx || again {
                d.edit();
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
        assert_eq!(e.mode, Mode::Settings);
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
        assert!(e.message.contains("1〜16"), "{}", e.message);
        keys(&mut e, "<CR><Esc>S2<CR>");
        assert_eq!(e.settings_dialog.as_ref().unwrap().work.int("editor.indent_width"), 2);
        // p でこのプロジェクトに。オン・オフは Space
        keys(&mut e, "p");
        assert!(e.settings_dialog.as_ref().unwrap().project_scope);
        // Ctrl+Enter で適用して閉じる → Editor.settings に入り、プロジェクトのファイルは書かない（全体に入れたので）
        keys(&mut e, "<C-m>");
        assert_eq!(e.mode, Mode::Normal, "{}", e.message);
        assert_eq!(e.settings.get("editor.line_numbers"), "relative");
        assert_eq!(e.settings.int("editor.indent_width"), 2);
        // 直してから Esc を 1 回だと確かめる
        e.open_settings("波線");
        keys(&mut e, " <Esc>");
        assert_eq!(e.mode, Mode::Settings);
        keys(&mut e, "<Esc>");
        assert_eq!(e.mode, Mode::Normal);
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
        assert!(e.message.contains("JDK 21"), "{}", e.message);
        assert_eq!(e.settings_dialog.as_ref().unwrap().work.get("maven.jdk"), "", "ダウンロードが終わるまでは変えない");
    }
}
