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

use crate::editor::{Editor, State};
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
    /// 一覧の (画面の行, 構成の番号, 列の始め, 終わり)
    pub list_rows: crate::ui::Hits<usize>,
    /// 欄の (画面の行, Focus, 値の始まりの列)
    pub field_rows: RefCell<Vec<(usize, Focus, usize)>>,
    pub buttons: crate::ui::Hits<DialogButton>,
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
            list_rows: Default::default(),
            field_rows: RefCell::new(Vec::new()),
            buttons: Default::default(),
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
        self.set_state(State::RunDialog);
        self.notify.clear();
    }

    fn close_run_dialog(&mut self) {
        self.run_dialog = None;
        self.set_state(State::Normal);
    }

    /// 保存（nagi の構成を .nagi/run.conf に書き出す）。うまくいけば true
    fn save_run_dialog(&mut self) -> bool {
        let path = runconfig::conf_path(&self.nav_root());
        let Some(d) = &mut self.run_dialog else { return false };
        d.commit();
        // 名前が重なっていれば保存しない
        let names: Vec<&str> = d.configs.iter().map(|c| c.name.as_str()).collect();
        if let Some(dup) = names.iter().enumerate().find(|(i, n)| names[..*i].contains(n)).map(|x| x.1.to_string()) {
            self.notify.info(format!("名前が重なっています: {dup}"));
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
                self.notify.info(format!("実行の構成を保存しました（{} 件、{}）", ours.len(), path.display()));
                true
            }
            Err(e) => {
                self.notify.error(format!("保存できません: {}: {e}", path.display()));
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
                    let msg = self.notify.text.clone();
                    self.close_run_dialog();
                    if let Some(c) = cfg {
                        self.run_config(c);
                    } else {
                        self.notify.info(msg);
                    }
                }
            }
            DialogButton::Cancel => {
                self.close_run_dialog();
                self.notify.info("実行の構成の画面を閉じました（保存していない変更は捨てました）");
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
            self.set_state(State::Normal);
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
                let moved = crate::list::step(key, d.selected, n, 10);
                match key {
                    _ if moved.is_some() => d.select(moved.unwrap_or(d.selected)),
                    Key::Char('g') => d.select(0),
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
                        Some(_) => self.notify.warn("IntelliJ の構成は nagi からは消せません（IntelliJ で消してください）"),
                        None => {}
                    },
                    Key::Char('r') => self.run_dialog_button(DialogButton::Run),
                    Key::Esc | Key::Char('q') => {
                        if d.dirty && !confirm {
                            d.confirm_close = true;
                            self.notify.info("保存していない変更があります。もう一度 Esc で捨てて閉じる、Ctrl+Enter で保存して閉じる");
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
    pub(crate) fn run_dialog_click(&mut self, row: usize, col: usize, clicks: u32) {
        let Some(d) = &mut self.run_dialog else { return };
        let button = d.buttons.at(row, col);
        if let Some(b) = button {
            self.run_dialog_button(b);
            return;
        }
        let item = d.list_rows.at(row, col);
        let mut sel = d.selected;
        if let Some(pick) = crate::list::pick(&mut sel, item, clicks, 2) {
            d.select(sel);
            d.focus = Focus::List;
            // ダブルクリックは Enter と同じ（その構成を直す）
            if matches!(pick, crate::list::Pick::Accept(_)) {
                self.run_dialog_key(Key::Enter);
            }
            return;
        }
        // 欄は名前の列から右（一覧より右）
        let field = d.field_rows.borrow().iter().find(|(r, _, x)| *r == row && col + 16 >= *x).map(|x| (x.1, x.2));
        if let Some((f, x)) = field {
            d.commit();
            // 種類は押すたびに次の種類へ
            if f == Focus::Kind {
                if d.focus == Focus::Kind {
                    d.cycle_kind(1);
                }
                d.focus = f;
                return;
            }
            d.focus = f;
            // 押した文字の位置へカーソルを置いて打てるように（値より右なら行末）
            if let Some(v) = d.field_mut(f) {
                v.reinsert();
                let text = v.ed.buf.line(0);
                let byte = nagi_core::byte_at_col(&text, col.saturating_sub(x), false).min(text.len());
                v.ed.cursor.line = 0;
                v.ed.cursor.byte = byte;
                v.ed.cursor.want_col = nagi_core::display_col(&text, byte, false);
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
            Err(e) => e.to_string(),
        }
    }
}



/// この部品の入口（component.rs）
pub struct RunDialogComponent;

impl crate::component::Component for RunDialogComponent {
    fn key(&self, ed: &mut crate::editor::Editor, key: nagi_vim::Key) -> crate::editor::Effect {
        ed.run_dialog_key(key);
        crate::editor::Effect::None
    }

    fn text(&self, ed: &mut crate::editor::Editor, text: &str) {
        ed.run_dialog_insert(text);
    }

    fn click(&self, ed: &mut crate::editor::Editor, row: usize, col: usize, clicks: u32) -> bool {
        ed.run_dialog_click(row, col, clicks);
        true
    }
}


/// 実行の構成の画面（layout.rs の View）
pub struct RunDialogView;

impl crate::layout::View for RunDialogView {
    fn rect(&self, ed: &crate::editor::Editor, s: crate::layout::Screen) -> Option<crate::panes::Rect> {
        let _ = s;
        ed.run_dialog.as_ref()?;
        Some(crate::panes::Rect { x: 0, y: 0, w: s.cols, h: s.rows })
    }

    fn draw(&self, ed: &crate::editor::Editor, p: &mut crate::view::Painter, r: crate::panes::Rect, s: crate::layout::Screen) {
        let _ = (r, s);
        if let Some(d) = &ed.run_dialog {
            draw::draw_run_dialog(p, ed, d, s.cols, s.rows);
        }
    }

    fn wheel(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, row: usize, col: usize, dy: isize, dx: isize) -> bool {
        let _ = (r, row, col, dx);
        crate::layout::wheel_keys(ed, dy)
    }

    fn context(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, row: usize, col: usize) -> bool {
        use nagi_vim::Key::*;
        let _ = r;
        // 押した構成を選んでから（1 回のクリックは選ぶだけ）
        ed.run_dialog_click(row, col, 1);
        let m = crate::menu::Menu::new(row, col).keys("実行", &[Char('r')]).keys("この構成を複製", &[Char('c')]).sep().keys("閉じる", &[Esc]);
        ed.open_menu(m);
        true
    }
}

/// 描き方（view.rs の Painter で描く）
pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    /// 実行の構成の画面（画面の中央に重ねる）。左に一覧（上に ＋ ⧉ −）、右に欄、下にコマンドの見本とボタン
    pub(crate) fn draw_run_dialog(p: &mut Painter, ed: &Editor, d: &crate::run_dialog::RunDialog, cols: usize, rows: usize) {
        use crate::run_dialog::{DialogButton, Focus};
        let cell = p.fonts.cell();
        let w = cols.saturating_sub(6).clamp(40, 120).min(cols);
        // 高さは中身に合わせる（一覧の数・欄の数とコマンドの見本の分）
        let need = (d.configs.len() + 6).max(d.fields.len() + 13).max(16);
        let h = need.min(30).min(rows.saturating_sub(2)).max(12.min(rows));
        let (x0, y0) = dialog_frame(p, cols, rows, w, h);
        let y = |r: usize| (y0 + r) as f32 * cell.h;
        let saved = p.cols;
        p.cols = x0 + w;
        p.text(" 実行の構成", x0, y(0), FG);
        let lw = (w / 3).clamp(22, 36);
        let rx = x0 + lw + 2;
        // 一覧と欄の間の線
        p.out.push(Instance::fill((x0 + lw) as f32 * cell.w + cell.w * 0.5, y(1), 1.0, (h - 3) as f32 * cell.h, LINE_NO));
        let mut buttons = Vec::new();
        // 一覧の上: ＋ ⧉ −
        let mut bx = x0 + 1;
        for (label, b) in [("＋", DialogButton::Add), ("⧉", DialogButton::Copy), ("−", DialogButton::Delete)] {
            let end = p.text(label, bx, y(1), STATUS_FG);
            buttons.push((y0 + 1, bx, end, b));
            bx = end + 2;
        }
        // 一覧
        let list_focused = d.focus == Focus::List;
        let body = h.saturating_sub(5);
        let top = d.selected.saturating_sub(body.saturating_sub(1));
        let mut list_rows = Vec::new();
        p.cols = x0 + lw;
        for (k, c) in d.configs.iter().enumerate().skip(top).take(body) {
            let r = 2 + k - top;
            let sel = k == d.selected;
            if sel {
                p.out.push(Instance::fill(x0 as f32 * cell.w, y(r), lw as f32 * cell.w, cell.h, if list_focused { LIST_SEL_BG } else { MATCH_BG }));
            }
            let (fg, dim) = if sel && list_focused { (FG_BRIGHT, FG_BRIGHT) } else { (FG, STATUS_FG) };
            let mark = if Some(&c.name) == ed.run_selected.as_ref() { "● " } else { "  " };
            let end = p.text(&format!(" {mark}{}", c.name), x0, y(r), fg);
            let tag = if c.from == "IntelliJ" { format!("{}・IJ", c.kind_label()) } else { c.kind_label() };
            let tw = p.width(&tag);
            let tx = (x0 + lw).saturating_sub(tw + 1);
            if tx > end {
                p.text(&tag, tx, y(r), dim);
            }
            list_rows.push((y0 + r, x0, x0 + lw, k));
        }
        // 右: 欄
        p.cols = x0 + w;
        let mut field_rows = Vec::new();
        let label_w = 14;
        let fx = rx + label_w;
        if let Some(c) = d.current() {
            let mut r = 1;
            let row = |p: &mut Painter, r: usize, label: &str, focused: bool| {
                if focused {
                    p.out.push(Instance::fill(rx as f32 * cell.w - cell.w * 0.5, y(r), crate::metrics::accent_line(), cell.h, ACCENT_FG));
                }
                p.text(label, rx, y(r), if focused { FG } else { STATUS_FG });
            };
            // 名前
            row(p, r, "名前", d.focus == Focus::Name);
            if d.focus == Focus::Name {
                draw_vim_field(p, &d.name, fx, y(r), &ed.preedit);
            } else {
                p.text(&d.name.text(), fx, y(r), FG);
            }
            field_rows.push((y0 + r, Focus::Name, fx));
            r += 1;
            // 種類（h/l で切り替え）
            row(p, r, "種類", d.focus == Focus::Kind);
            let kind = c.kind_label();
            if d.focus == Focus::Kind {
                let end = p.text(&format!("◀ {kind} ▶"), fx, y(r), FG);
                p.text("   h/l で切り替え", end, y(r), STATUS_FG);
            } else {
                p.text(&kind, fx, y(r), FG);
            }
            field_rows.push((y0 + r, Focus::Kind, fx));
            r += 1;
            for (i, (_, label, f)) in d.fields.iter().enumerate() {
                let focused = d.focus == Focus::Field(i);
                row(p, r, label, focused);
                if focused {
                    draw_vim_field(p, f, fx, y(r), &ed.preedit);
                } else {
                    let t = f.text();
                    p.text(if t.is_empty() { "—" } else { &t }, fx, y(r), if t.is_empty() { LINE_NO } else { FG });
                }
                field_rows.push((y0 + r, Focus::Field(i), fx));
                r += 1;
            }
            r += 1;
            let place = match (c.from, &c.kind) {
                (_, nagi_workers::runconfig::Kind::Unsupported { .. }) => "この種類は nagi では動かせません（種類を選び直せば nagi の構成になる）",
                ("IntelliJ", _) => "保存先: IntelliJ の構成（直すと nagi の構成として .nagi/run.conf に保存）",
                _ => "保存先: nagi（.nagi/run.conf）",
            };
            p.text(place, rx, y(r), STATUS_FG);
            r += 1;
            // コマンドの見本（長ければ折り返す。ボタンの行の手前まで）
            p.text("コマンド: ", rx, y(r), STATUS_FG);
            let cx = rx + 10;
            let width = (x0 + w).saturating_sub(cx + 1).max(10);
            let mut line = String::new();
            let mut lw = 0;
            for g in ed.run_dialog_preview().graphemes(true) {
                let gw = p.width(g);
                if lw + gw > width {
                    if r + 2 >= h {
                        line.push('…');
                        break;
                    }
                    p.text(&line, cx, y(r), rgb(0xa8a8a8));
                    r += 1;
                    line.clear();
                    lw = 0;
                }
                line.push_str(g);
                lw += gw;
            }
            p.text(&line, cx, y(r), rgb(0xa8a8a8));
        } else {
            p.text("構成がありません。＋（a）で追加", rx, y(1), STATUS_FG);
        }
        // 下: 案内とボタン
        let help = match d.focus {
            Focus::List => "j/k 選ぶ  Enter 直す  a 追加  c 複製  d 削除  r 実行  Ctrl+Enter 保存  Esc 閉じる",
            Focus::Kind => "h/l 種類を切り替え  Tab 次の欄  Esc 一覧へ  Ctrl+Enter 保存",
            _ => "Tab・Enter 次の欄  ↑↓ 前後  Esc ノーマル→一覧  Ctrl+Enter 保存",
        };
        let by = h - 1;
        buttons.extend(dialog_buttons(p, x0 + w, y0 + by, &[(" 取り消し ", DialogButton::Cancel), (" 保存 ", DialogButton::Save), (" 実行 ", DialogButton::Run)], DialogButton::Save));
        p.cols = x0 + w.saturating_sub(30);
        p.text(help, x0 + 1, y(by), STATUS_FG);
        p.cols = saved;
        d.list_rows.set(list_rows);
        *d.field_rows.borrow_mut() = field_rows;
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
        assert_eq!(e.state(), State::RunDialog);
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
        assert!(e.notify.text.contains("IntelliJ"), "{}", e.notify.text);
        keys(&mut e, "i<Tab><Tab><Esc>Secho nagi<Esc><Esc>");
        assert_eq!(e.run_dialog.as_ref().unwrap().configs[0].from, "nagi");
        // Ctrl+Enter で保存して閉じる
        keys(&mut e, "<C-m>");
        assert_eq!(e.state(), State::Normal, "{}", e.notify.text);
        let text = std::fs::read_to_string(dir.join(".nagi/run.conf")).unwrap();
        assert!(text.contains("[IJ]\ntype = shell\ncommand = echo nagi\n") && text.contains("[テスト]\ntype = maven\ngoals = test -q\n"), "{text}");
        // 読み直すと、直した IJ は nagi の方が使われる
        let v = e.load_run_configs();
        assert_eq!(v.iter().find(|c| c.name == "IJ").unwrap().kind, Kind::Shell { command: "echo nagi".into(), workdir: None });
        // 直してから Esc を 1 回だと確かめ、2 回で閉じる
        e.open_run_dialog(Some("テスト"));
        keys(&mut e, "c<Esc>");
        assert_eq!(e.state(), State::RunDialog);
        keys(&mut e, "<Esc>");
        assert_eq!(e.state(), State::Normal);
    }

    /// マウス: 欄を押すとその文字の位置で打てる。種類は押すたびに次へ。一覧の行・ボタンも押せる
    #[test]
    fn mouse() {
        let dir = std::env::temp_dir().join(format!("nagi-rundlg-m-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join(".nagi")).unwrap();
        std::fs::write(dir.join(".nagi/run.conf"), "a = echo abc\nb = echo b\n").unwrap();
        std::fs::write(dir.join("a.txt"), "x\n").unwrap();
        let mut e = Editor::new(Buffer::open(&dir.join("a.txt")).unwrap(), None);
        e.buf.path = Some(dir.join("a.txt"));
        e.set_screen(140, 40);
        e.open_run_dialog(Some("a"));
        // 描いた位置の代わりに入れる（行 10: 名前、行 11: 種類、行 12: コマンド。値は 50 列から）
        let d = e.run_dialog.as_ref().unwrap();
        *d.field_rows.borrow_mut() = vec![(10, Focus::Name, 50), (11, Focus::Kind, 50), (12, Focus::Field(0), 50)];
        d.list_rows.set(vec![(5, 0, 30, 0), (6, 0, 30, 1), (12, 0, 30, 0)]);
        // コマンドの「abc」の b の前を押して打つ
        e.run_dialog_click(12, 56, 1);
        assert_eq!(e.run_dialog.as_ref().unwrap().focus, Focus::Field(0));
        keys(&mut e, "X");
        assert_eq!(e.run_dialog.as_ref().unwrap().fields[0].2.text(), "echo aXbc");
        // 種類を 2 回押すと、2 回目で次の種類へ
        e.run_dialog_click(11, 50, 1);
        e.run_dialog_click(11, 50, 1);
        assert_eq!(e.run_dialog.as_ref().unwrap().current().unwrap().kind.id(), Some("maven"));
        // 一覧の 2 行目
        e.run_dialog_click(6, 1, 1);
        let d = e.run_dialog.as_ref().unwrap();
        assert_eq!((d.selected, d.focus), (1, Focus::List));
    }
}
