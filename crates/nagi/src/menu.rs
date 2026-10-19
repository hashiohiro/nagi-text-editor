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
}

pub struct Menu {
    pub items: Vec<MenuItem>,
    pub sel: usize,
    /// 押した所（画面の行・列）。箱はその下に出す（入らなければ上・左へずらす）
    pub row: usize,
    pub col: usize,
    /// 閉じたら戻る状態
    back: State,
}

impl Menu {
    pub fn new(row: usize, col: usize) -> Menu {
        Menu { items: Vec::new(), sel: 0, row, col, back: State::Normal }
    }

    pub fn item(mut self, label: impl Into<String>, run: impl FnOnce(&mut Editor) + 'static) -> Menu {
        self.items.push(MenuItem { label: label.into(), run: Some(Box::new(run)) });
        self
    }

    pub fn sep(mut self) -> Menu {
        self.items.push(MenuItem { label: String::new(), run: None });
        self
    }

    fn selectable(&self, i: usize) -> bool {
        self.items.get(i).is_some_and(|it| it.run.is_some())
    }

    /// 箱（枠の内側。画面 `cols`×`rows` に収める）
    pub fn rect(&self, cols: usize, rows: usize) -> Rect {
        let w = self.items.iter().map(|it| nagi_core::display_col(&it.label, it.label.len(), false)).max().unwrap_or(0) + 4;
        let h = self.items.len();
        let x = self.col.min(cols.saturating_sub(w + 1));
        let y = if self.row + 1 + h <= rows { self.row + 1 } else { self.row.saturating_sub(h) };
        Rect { x, y, w, h }
    }
}

impl Editor {
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
        match key {
            Key::Enter | Key::Char(' ') => {
                let i = m.sel;
                self.menu_run(i);
            }
            Key::Esc | Key::Char('q') | Key::Ctrl('c') | Key::Ctrl('[') => {
                self.close_menu();
            }
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
        crate::component::Status::new("-- メニュー --", "j/k 選ぶ  Enter 行う  Esc 閉じる ")
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
            let i = row - b.y;
            let mut sel = m.sel;
            // OS のメニューと同じく 1 回で決める（区切りの行は何もしない）
            if let Some(crate::list::Pick::Accept(i)) = crate::list::pick(&mut sel, m.selectable(i).then_some(i), clicks, 1) {
                ed.menu_run(i);
            }
        } else {
            ed.close_menu();
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
