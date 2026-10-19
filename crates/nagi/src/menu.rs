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
}

impl Menu {
    pub fn new(row: usize, col: usize) -> Menu {
        Menu { title: None, items: Vec::new(), sel: 0, row, col, back: State::Normal, cancel: usize::MAX }
    }

    pub fn item(mut self, label: impl Into<String>, run: impl FnOnce(&mut Editor) + 'static) -> Menu {
        self.items.push(MenuItem { label: label.into(), run: Some(Box::new(run)), accel: None });
        self
    }

    /// 選ぶと、向いている部品へキーを流す項目（キーの処理とメニューの処理を 1 つにする。
    /// Vim を使わないときにキーを絞っても、メニューからは呼べる）
    pub fn keys(self, label: impl Into<String>, keys: &[Key]) -> Menu {
        let ks = keys.to_vec();
        self.item(label, move |e| e.panel_keys(&ks))
    }

    /// 文字のキーでも選べる項目
    pub fn item_key(mut self, label: impl Into<String>, accel: char, run: impl FnOnce(&mut Editor) + 'static) -> Menu {
        self.items.push(MenuItem { label: label.into(), run: Some(Box::new(run)), accel: Some(accel) });
        self
    }

    pub fn sep(mut self) -> Menu {
        self.items.push(MenuItem { label: String::new(), run: None, accel: None });
        self
    }

    /// 上の題の行数
    fn head(&self) -> usize {
        usize::from(self.title.is_some())
    }

    fn selectable(&self, i: usize) -> bool {
        self.items.get(i).is_some_and(|it| it.run.is_some())
    }

    /// 箱（枠の内側。画面 `cols`×`rows` に収める）
    pub fn rect(&self, cols: usize, rows: usize) -> Rect {
        let width = |s: &str| nagi_core::display_col(s, s.len(), false);
        let w = self.items.iter().map(|it| width(&it.label)).chain(self.title.as_deref().map(width)).max().unwrap_or(0) + 4;
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
        let q = question.into();
        let w = nagi_core::display_col(&q, q.len(), false) + 4;
        let (cols, rows) = self.screen;
        let mut m = Menu::new((rows / 2).saturating_sub(2), cols.saturating_sub(w) / 2).item_key("はい（Y）", 'y', yes).item_key("いいえ（N）", 'n', no);
        m.title = Some(q);
        m.cancel = 1;
        self.open_menu(m);
    }

    /// メニューを開く（閉じたら今の状態へ戻る）
    pub(crate) fn open_menu(&mut self, mut m: Menu) {
        if !m.items.iter().any(|it| it.run.is_some()) {
            return;
        }
        m.back = self.state();
        m.sel = crate::list::snap(0, 0, m.items.len(), |i| m.selectable(i));
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
        let to = match key {
            Key::Char('j') | Key::Down | Key::Ctrl('n') | Key::Tab => Some((m.sel + 1).min(n.saturating_sub(1))),
            Key::Char('k') | Key::Up | Key::Ctrl('p') => Some(m.sel.saturating_sub(1)),
            Key::Char('g') | Key::Home => Some(0),
            Key::Char('G') | Key::End => Some(n.saturating_sub(1)),
            _ => None,
        };
        if let Some(to) = to {
            m.sel = crate::list::snap(m.sel, to, n, |i| m.items[i].run.is_some());
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
        crate::component::Status::keys("-- メニュー --", "j/k 選ぶ  Enter 行う  Esc 閉じる ", "↑↓ 選ぶ  Enter 行う  Esc 閉じる ")
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

    fn click(&self, ed: &mut Editor, r: Rect, row: usize, col: usize, clicks: u32) -> crate::layout::Click {
        let _ = r;
        let Some(m) = &ed.menu else { return crate::layout::Click::Pass };
        let b = m.rect(ed.screen.0, ed.screen.1);
        if b.contains(row, col) {
            // 題の行は何もしない
            let Some(i) = row.checked_sub(b.y + m.head()) else { return crate::layout::Click::Done };
            let mut sel = m.sel;
            // OS のメニューと同じく 1 回で決める（区切りの行は何もしない）
            if let Some(crate::list::Pick::Accept(i)) = crate::list::pick(&mut sel, m.selectable(i).then_some(i), clicks, 1) {
                ed.menu_run(i);
            }
        } else {
            ed.cancel_menu();
        }
        crate::layout::Click::Done
    }

    fn wheel(&self, ed: &mut Editor, r: Rect, row: usize, col: usize, dy: isize, dx: isize) -> bool {
        let _ = (r, row, col, dx);
        crate::layout::wheel_keys(ed, dy)
    }
}

pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    pub(crate) fn draw_menu(p: &mut Painter, m: &Menu, cols: usize, rows: usize) {
        let cell = p.fonts.cell();
        let b = m.rect(cols, rows);
        let (x, y0) = (b.x as f32 * cell.w, b.y as f32 * cell.h);
        p.out.push(Instance::fill(x - 1.0, y0 - 1.0, b.w as f32 * cell.w + 2.0, b.h as f32 * cell.h + 2.0, LINE_NO));
        p.out.push(Instance::fill(x, y0, b.w as f32 * cell.w, b.h as f32 * cell.h, rgb(0x141414)));
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
                p.out.push(Instance::fill(x, y, b.w as f32 * cell.w, cell.h, FG));
            }
            p.text(&format!("  {}", it.label), b.x, y, if sel { BG } else { FG });
        }
        p.cols = saved;
    }
}
