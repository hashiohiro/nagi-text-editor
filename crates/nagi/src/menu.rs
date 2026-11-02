//! 右クリックのメニュー（どのペインからも使う部品）。
//!
//! ペインは layout.rs の View の `context` で、押した所の項目を並べた Menu を作って `open_menu` に渡す。
//! 項目は名前と、選んだときに Editor へ行う処理（閉じてから呼ぶ。元の部品へ向き直ってから）。
//! j/k・↑↓ で選び、Enter かクリックで行う。Esc か外を押すと閉じる。

use nagi_vim::Key;

use crate::editor::{Editor, Effect, State};
use crate::panes::Rect;

/// 選んだときの処理
pub type Run = Box<dyn FnOnce(&mut Editor)>;

pub struct MenuItem {
    pub label: String,
    /// None なら区切りの線
    pub run: Option<Run>,
    /// 文字のキーで選ぶ近道（確認の y・n）
    pub accel: Option<char>,
    /// 右に出すショートカット（部品のアクションに付けたキー）
    pub shortcut: Option<String>,
    /// 選べるか（できない操作は薄く出して選べない）
    pub enabled: bool,
}

pub struct Menu {
    /// 上に出す題（確認の問い）
    pub title: Option<String>,
    pub items: Vec<MenuItem>,
    pub sel: usize,
    /// 押した所（画面の行・列）。箱はその下に出す（入らなければ上・左へずらす）
    pub row: usize,
    pub col: usize,
    /// 閉じたら戻る状態
    back: State,
    /// Esc・外を押して閉じたときに行う項目（確認の「いいえ」）
    cancel: usize,
    /// 確かめるダイアログ（問いと、下にボタンを横に並べる。IntelliJ の確認の窓と同じ形）
    pub dialog: bool,
}

/// 確かめるダイアログの幅の上限（等幅の文字で 72 列ぶん。部品の文字の大きさに合わせる）
fn dialog_w() -> usize {
    crate::metrics::cols(72)
}

/// 確かめるダイアログの問いの左の余白（？のアイコンの分）
fn dialog_indent() -> usize {
    2 + crate::metrics::icon_cols(crate::metrics::Icon::Dialog)
}

impl Menu {
    pub fn new(row: usize, col: usize) -> Menu {
        Menu { title: None, items: Vec::new(), sel: 0, row, col, back: State::Normal, cancel: usize::MAX, dialog: false }
    }

    pub fn item(mut self, label: impl Into<String>, run: impl FnOnce(&mut Editor) + 'static) -> Menu {
        self.items.push(MenuItem { label: label.into(), run: Some(Box::new(run)), accel: None, shortcut: None, enabled: true });
        self
    }

    /// 部品のアクション（ui_action.rs）の項目: 名前・付けたショートカット・できるかを表から出す
    pub fn action(self, ed: &Editor, id: &'static str) -> Menu {
        let label = crate::ui_action::find(id).map_or("", |a| a.label);
        self.action_as(ed, id, label)
    }

    /// 名前を今の状態に合わせて差し替えたアクションの項目（「直前のコミットを直す: やめる」など）
    pub fn action_as(mut self, ed: &Editor, id: &'static str, label: &str) -> Menu {
        let Some(a) = crate::ui_action::find(id) else { return self };
        self.items.push(MenuItem {
            label: label.to_string(),
            run: Some(Box::new(move |e: &mut Editor| {
                e.run_ui_action(id);
            })),
            accel: None,
            shortcut: ed.ui_shortcut(id),
            enabled: (a.enabled)(ed),
        });
        self
    }

    /// 標準のキー（付け替えない。Ctrl+Enter など）を右に出す項目
    pub fn item_shortcut(mut self, label: impl Into<String>, shortcut: &str, run: impl FnOnce(&mut Editor) + 'static) -> Menu {
        self.items.push(MenuItem { label: label.into(), run: Some(Box::new(run)), accel: None, shortcut: Some(shortcut.to_string()), enabled: true });
        self
    }

    /// 文字のキーでも選べる項目
    pub fn item_key(mut self, label: impl Into<String>, accel: char, run: impl FnOnce(&mut Editor) + 'static) -> Menu {
        self.items.push(MenuItem { label: label.into(), run: Some(Box::new(run)), accel: Some(accel), shortcut: None, enabled: true });
        self
    }

    /// 区切りの線。先頭・続けての線は引かない（項目が条件で抜けても線が重ならないように）
    pub fn sep(mut self) -> Menu {
        if self.items.last().is_some_and(|it| it.run.is_some() || !it.label.is_empty()) {
            self.items.push(MenuItem { label: String::new(), run: None, accel: None, shortcut: None, enabled: true });
        }
        self
    }

    /// 上の題の行数
    fn head(&self) -> usize {
        usize::from(self.title.is_some())
    }

    fn selectable(&self, i: usize) -> bool {
        self.items.get(i).is_some_and(|it| it.run.is_some() && it.enabled)
    }

    /// 確かめるダイアログの問いの行（幅で折り返す）
    pub fn dialog_lines(&self, w: usize) -> Vec<String> {
        // 部品の文字（等幅でない）は等幅の幅の 9 割ほどなので、その分多く入れる
        let w = crate::metrics::fit(w);
        let mut lines = vec![String::new()];
        let mut cur = 0;
        for ch in self.title.as_deref().unwrap_or("").chars() {
            let cw = nagi_core::display_col(ch.encode_utf8(&mut [0; 4]), ch.len_utf8(), false);
            if cur + cw > w {
                lines.push(String::new());
                cur = 0;
            }
            lines.last_mut().unwrap().push(ch);
            cur += cw;
        }
        lines
    }

    /// ボタンの位置（項目の番号, 行, 始めの列, 終わりの列）。右下に左から項目の順
    pub fn dialog_buttons(&self, r: Rect) -> Vec<(usize, usize, usize, usize)> {
        let width = |s: &str| nagi_core::display_col(s, s.len(), false);
        let row = r.y + r.h - 2;
        let items: Vec<(usize, usize)> = self.items.iter().enumerate().filter(|(_, it)| it.run.is_some()).map(|(i, it)| (i, width(&it.label) + 4)).collect();
        let total: usize = items.iter().map(|x| x.1 + 1).sum();
        let mut x = (r.x + r.w).saturating_sub(total + 1);
        items.into_iter().map(|(i, w)| {
            let b = (i, row, x, x + w);
            x += w + 1;
            b
        }).collect()
    }

    /// 箱（枠の内側。画面 `cols`×`rows` に収める）
    pub fn rect(&self, cols: usize, rows: usize) -> Rect {
        if self.dialog {
            let width = |s: &str| nagi_core::display_col(s, s.len(), false);
            let buttons: usize = self.items.iter().filter(|it| it.run.is_some()).map(|it| width(&it.label) + 5).sum::<usize>() + 2;
            let w = (self.title.as_deref().map_or(0, width) + dialog_indent() + 2).max(buttons).max(crate::metrics::cols(48)).min(dialog_w()).min(cols.saturating_sub(4));
            let h = self.dialog_lines(w.saturating_sub(dialog_indent() + 2)).len() + 5;
            return Rect { x: cols.saturating_sub(w) / 2, y: (rows / 3).min(rows.saturating_sub(h)), w, h };
        }
        let width = |s: &str| nagi_core::display_col(s, s.len(), false);
        // ショートカットは名前の右に 3 列あけて
        let w = self.items.iter().map(|it| width(&it.label) + it.shortcut.as_deref().map_or(0, |s| width(s) + 3)).chain(self.title.as_deref().map(width)).max().unwrap_or(0) + 4;
        let h = self.items.len() + self.head();
        let x = self.col.min(cols.saturating_sub(w + 1));
        let y = if self.row + 1 + h <= rows { self.row + 1 } else { self.row.saturating_sub(h) };
        Rect { x, y, w, h }
    }
}

impl Editor {
    /// 向いている部品へキーを流す（Vim を使わないときの絞り込みを通さない。部品が無ければ本文へ）
    pub(crate) fn panel_keys(&mut self, keys: &[Key]) {
        for &k in keys {
            match crate::component::focused(self) {
                Some(c) => {
                    c.key(self, k);
                }
                None => {
                    self.key(k);
                }
            }
        }
    }

    /// 確かめる（画面の真ん中に問いと「はい」「いいえ」。y・n・Enter・Esc・クリックで答える）。はいなら `yes`
    pub(crate) fn ask(&mut self, question: impl Into<String>, yes: impl FnOnce(&mut Editor) + 'static) {
        self.ask_or(question, yes, |_| {});
    }

    /// 確かめる（いいえ・Esc・外を押したら `no`）
    pub(crate) fn ask_or(&mut self, question: impl Into<String>, yes: impl FnOnce(&mut Editor) + 'static, no: impl FnOnce(&mut Editor) + 'static) {
        let m = Menu::new(0, 0).item_key("はい（Y）", 'y', yes).item_key("いいえ（N）", 'n', no);
        self.ask_menu(question, m, 1);
    }

    /// 選択肢を並べて確かめる（`m` の項目を画面の真ん中に。Esc・外を押したら `cancel` 番目の項目）
    pub(crate) fn ask_menu(&mut self, question: impl Into<String>, mut m: Menu, cancel: usize) {
        let q = question.into();
        let w = nagi_core::display_col(&q, q.len(), false) + 4;
        let (cols, rows) = self.screen;
        (m.row, m.col) = ((rows / 2).saturating_sub(2), cols.saturating_sub(w) / 2);
        m.title = Some(q);
        m.cancel = cancel;
        // ボタンが横に並びきれば、確かめるダイアログの形にする
        let width = |s: &str| nagi_core::display_col(s, s.len(), false);
        m.dialog = m.items.iter().filter(|it| it.run.is_some()).map(|it| width(&it.label) + 5).sum::<usize>() + 2 <= dialog_w().min(cols.saturating_sub(4));
        self.open_menu(m);
    }

    /// メニューを開く（閉じたら今の状態へ戻る）
    /// 入力欄の右クリック（描いた入力欄の上なら）: 切り取り・コピー・貼り付け・すべて選択（IntelliJ の入力欄と同じ）。出したら true
    pub(crate) fn field_menu(&mut self, row: usize, col: usize) -> bool {
        let Some(&(_, _, _, vim)) = self.field_hits.borrow().iter().find(|(r, c0, c1, _)| *r == row && col >= *c0 && col < *c1) else { return false };
        let m = if vim {
            Menu::new(row, col)
                .item("切り取り", |e| { e.key(Key::Cut); })
                .item("コピー", |e| { e.key(Key::Copy); })
                .item("貼り付け", |e| e.paste_clipboard())
                .sep()
                .item("すべて選択", |e| { e.key(Key::SelectAll); })
        } else {
            // コマンドライン・検索の帯（選ぶ範囲を持たない欄）
            Menu::new(row, col).item("貼り付け", |e| e.paste_clipboard())
        };
        self.open_menu(m);
        true
    }

    pub(crate) fn open_menu(&mut self, mut m: Menu) {
        if !m.items.iter().any(|it| it.run.is_some()) {
            return;
        }
        while m.items.last().is_some_and(|it| it.run.is_none() && it.label.is_empty()) {
            m.items.pop();
        }
        m.back = self.state();
        m.sel = nagi_ui::list::snap(0, 0, m.items.len(), |i| m.selectable(i));
        self.menu = Some(m);
        self.set_state(State::Menu);
    }

    /// やめる（確認なら「いいえ」の処理をする）
    fn cancel_menu(&mut self) {
        let i = self.menu.as_ref().map_or(usize::MAX, |m| m.cancel);
        if self.menu.as_ref().is_some_and(|m| m.items.get(i).is_some_and(|it| it.run.is_some())) {
            self.menu_run(i);
        } else {
            self.close_menu();
        }
    }

    fn close_menu(&mut self) -> Option<Menu> {
        let m = self.menu.take()?;
        self.set_state(m.back);
        Some(m)
    }

    /// `i` 番目の項目を行う（閉じて元の部品へ向き直ってから）
    pub(crate) fn menu_run(&mut self, i: usize) {
        if !self.menu.as_ref().is_some_and(|m| m.selectable(i)) {
            return;
        }
        let Some(mut m) = self.close_menu() else { return };
        if let Some(run) = m.items.get_mut(i).and_then(|it| it.run.take()) {
            run(self);
        }
    }

    pub(crate) fn menu_key(&mut self, key: Key) {
        let Some(m) = &mut self.menu else {
            self.set_state(State::Normal);
            return;
        };
        let n = m.items.len();
        // ダイアログはボタンが横に並ぶので ← → でも選ぶ
        let key = match key {
            Key::Right if m.dialog => Key::Down,
            Key::Left if m.dialog => Key::Up,
            k => k,
        };
        let to = match key {
            // 標準のキーだけ（ADR 0019）。文字は項目の頭の字（確認の y・n など）
            Key::Down | Key::Tab => Some((m.sel + 1).min(n.saturating_sub(1))),
            Key::Up => Some(m.sel.saturating_sub(1)),
            Key::Home => Some(0),
            Key::End => Some(n.saturating_sub(1)),
            _ => None,
        };
        if let Some(to) = to {
            m.sel = nagi_ui::list::snap(m.sel, to, n, |i| m.items[i].run.is_some());
            return;
        }
        if let Key::Char(c) = key {
            if let Some(i) = m.items.iter().position(|it| it.accel == Some(c.to_ascii_lowercase())) {
                self.menu_run(i);
                return;
            }
        }
        match key {
            Key::Enter | Key::Char(' ') => {
                let i = m.sel;
                self.menu_run(i);
            }
            Key::Esc | Key::Char('q') | Key::Ctrl('c') | Key::Ctrl('[') => self.cancel_menu(),
            _ => {}
        }
    }
}

/// メニューへのキー（component.rs）
pub struct MenuComponent;

impl crate::component::Component for MenuComponent {
    fn key(&self, ed: &mut Editor, key: Key) -> Effect {
        ed.menu_key(key);
        Effect::None
    }

    fn status(&self, ed: &Editor) -> Option<crate::component::Status> {
        let _ = ed;
        crate::component::Status::new("-- メニュー --", "↑↓ 選ぶ  Enter 行う  Esc 閉じる ")
    }
}

/// メニューの描き方とクリック（layout.rs の View。画面全体を持ち、外を押したら閉じる）
pub struct MenuView;

impl crate::layout::View for MenuView {
    fn rect(&self, ed: &Editor, s: crate::layout::Screen) -> Option<Rect> {
        ed.menu.as_ref()?;
        Some(Rect { x: 0, y: 0, w: s.cols, h: s.rows + ed.status_rows() })
    }

    fn draw(&self, ed: &Editor, p: &mut crate::view::Painter, r: Rect, s: crate::layout::Screen) {
        let _ = r;
        if let Some(m) = &ed.menu {
            draw::draw_menu(p, m, s.cols, s.rows);
        }
    }

    fn click(&self, ed: &mut Editor, r: Rect, at: crate::layout::At, clicks: u32) -> crate::layout::Click {
        let (row, col) = at.screen;
        let _ = r;
        let Some(m) = &ed.menu else { return crate::layout::Click::Pass };
        let b = m.rect(ed.screen.0, ed.screen.1);
        if m.dialog {
            // ボタンを押したらその項目。ダイアログの中のほかの所は何もしない
            match m.dialog_buttons(b).into_iter().find(|x| x.1 == row && col >= x.2 && col < x.3) {
                Some((i, ..)) => ed.menu_run(i),
                None if !b.contains(row, col) => ed.cancel_menu(),
                None => {}
            }
            return crate::layout::Click::Done;
        }
        if b.contains(row, col) {
            // 題の行は何もしない
            let Some(i) = row.checked_sub(b.y + m.head()) else { return crate::layout::Click::Done };
            let mut sel = m.sel;
            // OS のメニューと同じく 1 回で決める（区切りの行は何もしない）
            if let Some(nagi_ui::list::Pick::Accept(i)) = nagi_ui::list::pick(&mut sel, m.selectable(i).then_some(i), clicks, 1) {
                ed.menu_run(i);
            }
        } else {
            ed.cancel_menu();
        }
        crate::layout::Click::Done
    }

    fn wheel(&self, ed: &mut Editor, r: Rect, at: crate::layout::At, dy: isize, dx: isize) -> bool {
        let _ = (at, r, dx);
        crate::layout::wheel_keys(ed, dy)
    }
}

pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    /// 確かめるダイアログ: 題の帯、？のアイコンと問い、右下にボタン（選んでいるものは青）
    fn draw_dialog(p: &mut Painter, m: &Menu, cols: usize, rows: usize) {
        let cell = p.fonts.cell();
        let b = m.rect(cols, rows);
        let (x0, y0, w, h) = (b.x as f32 * cell.w, b.y as f32 * cell.h, b.w as f32 * cell.w, b.h as f32 * cell.h);
        popup_frame(p, x0, y0, w, h);
        let saved = p.cols;
        p.cols = b.x + b.w - 1;
        p.text(" 確認", b.x, y0, FG_BRIGHT);
        let px = crate::metrics::icon(crate::metrics::Icon::Dialog);
        crate::icons::draw(p, "g_questionDialog", x0 + cell.w * 1.5, y0 + cell.h * 2.0 - px * 0.3, px, None);
        for (k, l) in m.dialog_lines(b.w.saturating_sub(dialog_indent() + 2)).iter().enumerate() {
            p.text(l, b.x + dialog_indent(), (b.y + 2 + k) as f32 * cell.h, FG);
        }
        for (i, row, c0, c1) in m.dialog_buttons(b) {
            let on = i == m.sel;
            p.out.push(Instance::rounded(c0 as f32 * cell.w, row as f32 * cell.h + crate::metrics::pad(), (c1 - c0) as f32 * cell.w, cell.h - 2.0 * crate::metrics::pad(), crate::metrics::radius(crate::metrics::Radius::Control), if on { ACCENT_FG } else { RAISED_BG }));
            let label = &m.items[i].label;
            let lw = p.width(label);
            p.text(label, c0 + (c1 - c0).saturating_sub(lw) / 2, row as f32 * cell.h, if on { FG_BRIGHT } else { FG });
        }
        p.cols = saved;
    }

    pub(crate) fn draw_menu(p: &mut Painter, m: &Menu, cols: usize, rows: usize) {
        if m.dialog {
            draw_dialog(p, m, cols, rows);
            return;
        }
        let cell = p.fonts.cell();
        let b = m.rect(cols, rows);
        let (x, y0) = (b.x as f32 * cell.w, b.y as f32 * cell.h);
        popup_frame(p, x, y0, b.w as f32 * cell.w, b.h as f32 * cell.h);
        let saved = p.cols;
        p.cols = b.x + b.w;
        if let Some(t) = &m.title {
            p.text(&format!("  {t}"), b.x, y0, STATUS_FG);
        }
        let y0 = y0 + m.head() as f32 * cell.h;
        for (i, it) in m.items.iter().enumerate() {
            let y = y0 + i as f32 * cell.h;
            if it.run.is_none() {
                p.out.push(Instance::fill(x + cell.w, y + cell.h * 0.5, (b.w - 2) as f32 * cell.w, 1.0, LINE_NO));
                continue;
            }
            let sel = i == m.sel;
            if sel {
                selection(p, x, y, b.w as f32 * cell.w, cell.h, LIST_SEL_BG);
            }
            let color = if !it.enabled { LINE_NO } else if sel { FG_BRIGHT } else { FG };
            p.text(&format!("  {}", it.label), b.x, y, color);
            if let Some(k) = &it.shortcut {
                let w = p.width(k);
                p.text(k, (b.x + b.w).saturating_sub(w + 2), y, if it.enabled { STATUS_FG } else { LINE_NO });
            }
        }
        p.cols = saved;
    }
}
