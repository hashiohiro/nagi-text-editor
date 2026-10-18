//! 実行の構成の画面（IntelliJ の「実行/デバッグ構成」と同じ並び）。画面の中央に重ねて出す。
//!
//! 左は構成の一覧（j/k で選ぶ。a 追加・c 複製・d 削除）、右は選んだ構成の欄（名前・種類・種類ごとの項目）。
//! 欄は Vim の入力欄（Insert で始まり、Esc でノーマル、もう一度 Esc で一覧へ）。Tab・Enter で次の欄、↑↓ で前後の欄。
//! 種類の欄は h/l（← →・Space）で切り替える。Ctrl+Enter で保存して閉じる。一覧で Esc を押すと閉じる（直したものがあれば確かめる）。
//!
//! 保存先は nagi 独自の `.nagi/run.conf`（runconfig::to_conf）。IntelliJ の構成を直すと、同じ名前の nagi の構成として保存し、
//! IntelliJ のファイルは書き換えない（nagi の構成が先に読まれるので、直したものが使われる）。

use std::cell::RefCell;

use nagi_vim::Key;
use nagi_workers::runconfig::{self, KINDS, Kind, RunConfig};

use crate::editor::{Editor, Mode};
use crate::vim_field::{FieldOut, VimField};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    List,
    Name,
    Kind,
    /// 種類ごとの欄（fields の番号）
    Field(usize),
}

/// 下に並べるボタン
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogButton {
    Run,
    Save,
    Cancel,
    /// 一覧の上の ＋ ⧉ −
    Add,
    Copy,
    Delete,
}

pub struct RunDialog {
    pub configs: Vec<RunConfig>,
    pub selected: usize,
    pub focus: Focus,
    pub name: VimField,
    /// 種類ごとの欄（キー, 名前, 入力欄）
    pub fields: Vec<(&'static str, &'static str, VimField)>,
    /// 直したか（閉じるときに確かめる）
    pub dirty: bool,
    /// 直したものを捨てて閉じるか確かめている
    pub confirm_close: bool,
    /// 描いた位置（マウス用）: 一覧の (画面の行, 番号)、欄の (画面の行, Focus)、ボタンの (行, 列の始め, 終わり, ボタン)
    pub list_rows: RefCell<Vec<(usize, usize)>>,
    pub field_rows: RefCell<Vec<(usize, Focus)>>,
    pub buttons: RefCell<Vec<(usize, usize, usize, DialogButton)>>,
}

impl RunDialog {
    fn new(configs: Vec<RunConfig>, selected: usize) -> RunDialog {
        let mut d = RunDialog {
            configs,
            selected,
            focus: Focus::List,
            name: VimField::new("", false),
            fields: Vec::new(),
            dirty: false,
            confirm_close: false,
            list_rows: RefCell::new(Vec::new()),
            field_rows: RefCell::new(Vec::new()),
            buttons: RefCell::new(Vec::new()),
        };
        d.load_fields();
        d
    }

    pub fn current(&self) -> Option<&RunConfig> {
        self.configs.get(self.selected)
    }

    /// 選んだ構成の値を欄に入れる
    fn load_fields(&mut self) {
        let (name, fields) = match self.configs.get(self.selected) {
            Some(c) => (c.name.clone(), c.kind.fields()),
            None => (String::new(), Vec::new()),
        };
        self.name = VimField::new(&name, false);
        self.fields = fields.into_iter().map(|(k, l, v)| (k, l, VimField::new(&v, false))).collect();
    }

    /// 欄の値を構成へ戻す（変わっていれば nagi の構成にする）
    fn commit(&mut self) {
        let Some(c) = self.configs.get_mut(self.selected) else { return };
        let before = c.clone();
        let name = self.name.text().trim().to_string();
        if !name.is_empty() {
            c.name = name;
        }
        for (k, _, f) in &self.fields {
            c.kind.set(k, &f.text());
        }
        if *c != before {
            c.from = "nagi";
            self.dirty = true;
        }
    }

    /// 移れる欄の並び
    fn focus_order(&self) -> Vec<Focus> {
        let mut v = vec![Focus::Name, Focus::Kind];
        v.extend((0..self.fields.len()).map(Focus::Field));
        v
    }

    fn move_focus(&mut self, d: isize) {
        let order = self.focus_order();
        let i = order.iter().position(|f| *f == self.focus).unwrap_or(0) as isize + d;
        self.focus = order[i.clamp(0, order.len() as isize - 1) as usize];
        if let Some(f) = self.field_mut(self.focus) {
            f.reinsert();
        }
    }

    pub fn field(&self, focus: Focus) -> Option<&VimField> {
        match focus {
            Focus::Name => Some(&self.name),
            Focus::Field(i) => self.fields.get(i).map(|f| &f.2),
            _ => None,
        }
    }

    fn field_mut(&mut self, focus: Focus) -> Option<&mut VimField> {
        match focus {
            Focus::Name => Some(&mut self.name),
            Focus::Field(i) => self.fields.get_mut(i).map(|f| &mut f.2),
            _ => None,
        }
    }

    /// 入力欄で Insert か（IME を使う）
    pub fn typing(&self) -> bool {
        self.field(self.focus).is_some_and(|f| f.is_insert())
    }

    /// 種類を前・次へ（同じキーの欄の値は引き継ぐ）
    fn cycle_kind(&mut self, d: isize) {
        self.commit();
        let Some(c) = self.configs.get_mut(self.selected) else { return };
        let i = c.kind.id().and_then(|id| KINDS.iter().position(|k| k.0 == id)).map_or(0, |i| (i as isize + d).rem_euclid(KINDS.len() as isize) as usize);
        let mut kind = Kind::new(KINDS[i].0);
        for (k, _, v) in c.kind.fields() {
            if !v.is_empty() {
                kind.set(k, &v);
            }
        }
        c.kind = kind;
        c.from = "nagi";
        self.dirty = true;
        self.load_fields();
    }

    /// 重ならない名前（「名前」「名前 (2)」…）
    fn unique(&self, base: &str) -> String {
        let mut name = base.to_string();
        let mut n = 2;
        while self.configs.iter().any(|c| c.name == name) {
            name = format!("{base} ({n})");
            n += 1;
        }
        name
    }

    fn select(&mut self, i: usize) {
        self.commit();
        self.selected = i.min(self.configs.len().saturating_sub(1));
        self.load_fields();
    }
}

impl Editor {
    /// 実行の構成の画面を開く（`select` の名前の構成を選んで）
    pub(crate) fn open_run_dialog(&mut self, select: Option<&str>) {
        let configs = self.load_run_configs();
        let want = select.map(String::from).or_else(|| self.run_selected.clone());
        let i = want.and_then(|n| configs.iter().position(|c| c.name == n)).unwrap_or(0);
        let mut d = RunDialog::new(configs, i);
        if d.configs.is_empty() {
            // 構成が無ければ、すぐ 1 つ作って名前の欄から
            d.configs.push(RunConfig { name: "新しい構成".into(), kind: Kind::new("shell"), from: "nagi" });
            d.load_fields();
            d.focus = Focus::Name;
            d.dirty = true;
        }
        self.run_dialog = Some(d);
        self.mode = Mode::RunDialog;
        self.message.clear();
    }

    fn close_run_dialog(&mut self) {
        self.run_dialog = None;
        self.mode = Mode::Normal;
    }

    /// 保存（nagi の構成を .nagi/run.conf に書き出す）。うまくいけば true
    fn save_run_dialog(&mut self) -> bool {
        let path = runconfig::conf_path(&self.nav_root());
        let Some(d) = &mut self.run_dialog else { return false };
        d.commit();
        // 名前が重なっていれば保存しない
        let names: Vec<&str> = d.configs.iter().map(|c| c.name.as_str()).collect();
        if let Some(dup) = names.iter().enumerate().find(|(i, n)| names[..*i].contains(n)).map(|x| x.1.to_string()) {
            self.message = format!("名前が重なっています: {dup}");
            return false;
        }
        let ours: Vec<RunConfig> = d.configs.iter().filter(|c| c.from == "nagi").cloned().collect();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::write(&path, runconfig::to_conf(&ours)) {
            Ok(()) => {
                let sel = d.current().map(|c| c.name.clone());
                d.dirty = false;
                if sel.is_some() {
                    self.run_selected = sel;
                }
                self.message = format!("実行の構成を保存しました（{} 件、{}）", ours.len(), path.display());
                true
            }
            Err(e) => {
                self.message = format!("保存できません: {}: {e}", path.display());
                false
            }
        }
    }

    /// ボタン・ショートカットの動き
    pub(crate) fn run_dialog_button(&mut self, b: DialogButton) {
        match b {
            DialogButton::Save => {
                if self.save_run_dialog() {
                    self.close_run_dialog();
                }
            }
            DialogButton::Run => {
                if self.save_run_dialog() {
                    let cfg = self.run_dialog.as_ref().and_then(|d| d.current().cloned());
                    let msg = self.message.clone();
                    self.close_run_dialog();
                    if let Some(c) = cfg {
                        self.run_config(c);
                    } else {
                        self.message = msg;
                    }
                }
            }
            DialogButton::Cancel => {
                self.close_run_dialog();
                self.message = "実行の構成の画面を閉じました（保存していない変更は捨てました）".into();
            }
            DialogButton::Add | DialogButton::Copy | DialogButton::Delete => {
                if let Some(d) = &mut self.run_dialog {
                    d.commit();
                    d.focus = Focus::List;
                }
                self.run_dialog_key(Key::Char(match b {
                    DialogButton::Add => 'a',
                    DialogButton::Copy => 'c',
                    _ => 'd',
                }));
            }
        }
    }

    /// IME の確定や貼り付け
    pub(crate) fn run_dialog_insert(&mut self, text: &str) {
        let Some(d) = &mut self.run_dialog else { return };
        let focus = d.focus;
        if let Some(f) = d.field_mut(focus) {
            if f.insert_text(text) == FieldOut::Changed {
                d.dirty = true;
            }
        }
    }

    pub(crate) fn run_dialog_key(&mut self, key: Key) {
        let Some(d) = &mut self.run_dialog else {
            self.mode = Mode::Normal;
            return;
        };
        // どこからでも: Ctrl+Enter で保存して閉じる
        if key == Key::Ctrl('m') {
            self.run_dialog_button(DialogButton::Save);
            return;
        }
        let confirm = std::mem::take(&mut d.confirm_close);
        match d.focus {
            Focus::List => {
                let n = d.configs.len();
                match key {
                    Key::Char('j') | Key::Down => d.select(d.selected + 1),
                    Key::Char('k') | Key::Up => d.select(d.selected.saturating_sub(1)),
                    Key::Char('g') => d.select(0),
                    Key::Char('G') => d.select(n.saturating_sub(1)),
                    Key::Enter | Key::Tab | Key::Char('l') | Key::Char('i') => {
                        d.focus = Focus::Name;
                        d.name.reinsert();
                    }
                    Key::Char('a' | '+') => {
                        d.commit();
                        let name = d.unique("新しい構成");
                        d.configs.push(RunConfig { name, kind: Kind::new("shell"), from: "nagi" });
                        d.selected = d.configs.len() - 1;
                        d.load_fields();
                        // 名前はすぐ打てるように空の欄から（打たなければ「新しい構成」のまま）
                        d.name = VimField::new("", false);
                        d.focus = Focus::Name;
                        d.dirty = true;
                    }
                    Key::Char('c') => {
                        d.commit();
                        if let Some(mut c) = d.current().cloned() {
                            c.name = d.unique(&format!("{} のコピー", c.name));
                            c.from = "nagi";
                            d.configs.insert(d.selected + 1, c);
                            d.selected += 1;
                            d.load_fields();
                            d.dirty = true;
                        }
                    }
                    Key::Char('d' | '-' | 'x') | Key::Delete => match d.current() {
                        Some(c) if c.from == "nagi" => {
                            d.configs.remove(d.selected);
                            d.selected = d.selected.min(d.configs.len().saturating_sub(1));
                            d.load_fields();
                            d.dirty = true;
                        }
                        Some(_) => self.message = "IntelliJ の構成は nagi からは消せません（IntelliJ で消してください）".into(),
                        None => {}
                    },
                    Key::Char('r') => self.run_dialog_button(DialogButton::Run),
                    Key::Esc | Key::Char('q') => {
                        if d.dirty && !confirm {
                            d.confirm_close = true;
                            self.message = "保存していない変更があります。もう一度 Esc で捨てて閉じる、Ctrl+Enter で保存して閉じる".into();
                        } else {
                            self.run_dialog_button(DialogButton::Cancel);
                        }
                    }
                    _ => {}
                }
            }
            Focus::Kind => match key {
                Key::Char('h') | Key::Left => d.cycle_kind(-1),
                Key::Char('l') | Key::Right | Key::Char(' ') => d.cycle_kind(1),
                Key::Tab | Key::Enter | Key::Down | Key::Char('j') => d.move_focus(1),
                Key::Up | Key::Char('k') => d.move_focus(-1),
                Key::Esc => {
                    d.commit();
                    d.focus = Focus::List;
                }
                _ => {}
            },
            focus => {
                let normal = d.field(focus).is_some_and(|f| !f.is_insert());
                match key {
                    Key::Tab | Key::Down => {
                        d.commit();
                        d.move_focus(1);
                        return;
                    }
                    Key::Up => {
                        d.commit();
                        d.move_focus(-1);
                        return;
                    }
                    Key::Char('j') if normal => {
                        d.commit();
                        d.move_focus(1);
                        return;
                    }
                    Key::Char('k') if normal => {
                        d.commit();
                        d.move_focus(-1);
                        return;
                    }
                    _ => {}
                }
                let out = self.feed_field(key, |e| {
                    let d = e.run_dialog.as_mut()?;
                    let f = d.focus;
                    d.field_mut(f)
                });
                let Some(d) = &mut self.run_dialog else { return };
                match out {
                    FieldOut::Changed => d.dirty = true,
                    FieldOut::Submit => {
                        d.commit();
                        d.move_focus(1);
                    }
                    FieldOut::Cancel => {
                        d.commit();
                        d.focus = Focus::List;
                    }
                    FieldOut::None => {}
                }
            }
        }
    }

    /// マウス: 画面の (行, 列) を押した
    pub(crate) fn run_dialog_click(&mut self, row: usize, col: usize) {
        let Some(d) = &mut self.run_dialog else { return };
        let button = d.buttons.borrow().iter().find(|(r, c0, c1, _)| *r == row && col >= *c0 && col < *c1).map(|b| b.3);
        if let Some(b) = button {
            self.run_dialog_button(b);
            return;
        }
        let item = d.list_rows.borrow().iter().find(|(r, _)| *r == row).map(|x| x.1);
        if let Some(i) = item {
            d.select(i);
            d.focus = Focus::List;
            return;
        }
        let field = d.field_rows.borrow().iter().find(|(r, _)| *r == row).map(|x| x.1);
        if let Some(f) = field {
            d.commit();
            if d.focus == Focus::Kind && f == Focus::Kind {
                d.cycle_kind(1);
            }
            d.focus = f;
            if let Some(v) = d.field_mut(f) {
                v.reinsert();
            }
        }
    }

    /// 選んだ構成で動かすコマンド（見本として出す）
    pub fn run_dialog_preview(&self) -> String {
        let Some(d) = &self.run_dialog else { return String::new() };
        let Some(c) = d.current() else { return String::new() };
        // 打っている途中の値も映す
        let mut c = c.clone();
        for (k, _, f) in &d.fields {
            c.kind.set(k, &f.text());
        }
        match runconfig::launch(&c, &self.nav_root()) {
            Ok(runconfig::Launch::Command { cmd, .. }) => cmd,
            Ok(runconfig::Launch::Open(url)) => format!("ブラウザで開く: {url}"),
            Err(e) => e,
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
    fn add_edit_save() {
        let dir = std::env::temp_dir().join(format!("nagi-rundlg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join(".run")).unwrap();
        std::fs::write(dir.join(".run/a.run.xml"), r#"<component name="ProjectRunConfigurationManager"><configuration name="IJ" type="ShConfigurationType"><option name="SCRIPT_TEXT" value="echo ij" /><option name="EXECUTE_SCRIPT_FILE" value="false" /></configuration></component>"#).unwrap();
        std::fs::write(dir.join("a.txt"), "x\n").unwrap();
        let mut e = Editor::new(Buffer::open(&dir.join("a.txt")).unwrap(), None);
        e.buf.path = Some(dir.join("a.txt"));
        e.set_screen(140, 40);
        e.open_run_dialog(None);
        assert_eq!(e.mode, Mode::RunDialog);
        assert_eq!(e.run_dialog.as_ref().unwrap().configs.len(), 1);
        // a で追加 → 名前を打つ → Tab で種類 → l で Maven → Tab でゴール
        keys(&mut e, "a");
        let d = e.run_dialog.as_ref().unwrap();
        assert_eq!((d.focus, d.configs.len()), (Focus::Name, 2));
        keys(&mut e, "テスト<Tab>l<Tab>test -q<CR>");
        let d = e.run_dialog.as_ref().unwrap();
        assert_eq!(d.configs[1].name, "テスト");
        assert_eq!(d.configs[1].kind.id(), Some("maven"));
        assert!(matches!(&d.configs[1].kind, Kind::Maven { goals, .. } if goals == &["test", "-q"]), "{:?}", d.configs[1].kind);
        assert!(e.run_dialog_preview().contains("test -q"), "{}", e.run_dialog_preview());
        // IntelliJ の構成は消せない。直すと nagi の構成になる
        keys(&mut e, "<Esc><Esc>gd");
        assert!(e.message.contains("IntelliJ"), "{}", e.message);
        keys(&mut e, "i<Tab><Tab><Esc>Secho nagi<Esc><Esc>");
        assert_eq!(e.run_dialog.as_ref().unwrap().configs[0].from, "nagi");
        // Ctrl+Enter で保存して閉じる
        keys(&mut e, "<C-m>");
        assert_eq!(e.mode, Mode::Normal, "{}", e.message);
        let text = std::fs::read_to_string(dir.join(".nagi/run.conf")).unwrap();
        assert!(text.contains("[IJ]\ntype = shell\ncommand = echo nagi\n") && text.contains("[テスト]\ntype = maven\ngoals = test -q\n"), "{text}");
        // 読み直すと、直した IJ は nagi の方が使われる
        let v = e.load_run_configs();
        assert_eq!(v.iter().find(|c| c.name == "IJ").unwrap().kind, Kind::Shell { command: "echo nagi".into(), workdir: None });
        // 直してから Esc を 1 回だと確かめ、2 回で閉じる
        e.open_run_dialog(Some("テスト"));
        keys(&mut e, "c<Esc>");
        assert_eq!(e.mode, Mode::RunDialog);
        keys(&mut e, "<Esc>");
        assert_eq!(e.mode, Mode::Normal);
    }
}
