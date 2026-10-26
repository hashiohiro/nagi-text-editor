//! 1 つの入力欄を打つダイアログ（IntelliJ の Rename・Go to Line と同じ形）。
//!
//! 題・説明・入力欄・補足の行・「OK」「キャンセル」。Enter か OK で `ok` に打った文字を渡し、Esc かキャンセルで閉じる。
//! 名前の変更（rename.rs）と行へ移動（Ctrl+G）が使う。

use nagi_vim::Key;

use crate::editor::{Editor, Effect, Prompt, State};
use crate::panes::Rect;

/// 決めたときの処理（打った文字を渡す）
pub type Ok = Box<dyn FnOnce(&mut Editor, String)>;

pub struct InputDialog {
    pub title: String,
    /// 欄の上の説明
    pub label: String,
    pub field: Prompt,
    /// 欄の下に薄く出す補足
    pub notes: Vec<String>,
    ok: Option<Ok>,
    /// 選んでいるボタン（0: OK、1: キャンセル。Tab で替える）
    pub button: usize,
}

/// ダイアログの幅（列。等幅の文字で 64 列ぶん、部品の文字の大きさに合わせる）
fn dialog_w() -> usize {
    crate::metrics::cols(64)
}

/// ダイアログの矩形（画面の上の方の真ん中）
pub fn rect(d: &InputDialog, cols: usize, rows: usize) -> Rect {
    let w = dialog_w().min(cols.saturating_sub(4));
    let h = 7 + d.notes.len();
    Rect { x: cols.saturating_sub(w) / 2, y: (rows / 4).min(rows.saturating_sub(h)), w, h }
}

/// ボタンの位置（番号, 行, 始めの列, 終わりの列）。右下に「OK」「キャンセル」
pub fn buttons(r: Rect) -> [(usize, usize, usize, usize); 2] {
    let row = r.y + r.h - 2;
    let cancel = (1, row, r.x + r.w - 13, r.x + r.w - 2);
    let ok = (0, row, cancel.2 - 7, cancel.2 - 1);
    [ok, cancel]
}

impl Editor {
    /// 入力のダイアログを開く
    pub(crate) fn open_input(&mut self, title: impl Into<String>, label: impl Into<String>, initial: &str, notes: Vec<String>, ok: impl FnOnce(&mut Editor, String) + 'static) {
        let mut field = Prompt::default();
        field.set(initial);
        self.input = Some(InputDialog { title: title.into(), label: label.into(), field, notes, ok: Some(Box::new(ok)), button: 0 });
        self.set_state(State::Input);
    }

    /// 行へ移動（IntelliJ の Go to Line:Column）。`行` か `行:列` を打つ
    pub(crate) fn goto_line_dialog(&mut self) {
        let total = self.buf.line_count();
        let col = self.buf.line(self.cursor.line)[..self.cursor.byte.min(self.buf.line_len(self.cursor.line))].chars().count() + 1;
        let here = format!("{}:{col}", self.cursor.line + 1);
        self.open_input("行へ移動", "行 [:列]", &here, vec![format!("全 {total} 行")], |e, t| {
            let mut it = t.trim().split(':');
            let Some(line) = it.next().and_then(|s| s.trim().parse::<usize>().ok()).filter(|l| *l > 0) else {
                e.notify.warn(format!("行の番号ではありません: {t}"));
                return;
            };
            let col = it.next().and_then(|s| s.trim().parse::<usize>().ok()).unwrap_or(1).max(1);
            e.push_jump();
            let line = (line - 1).min(e.buf.line_count().saturating_sub(1));
            let text = e.buf.line(line);
            e.cursor.line = line;
            e.cursor.byte = text.char_indices().nth(col - 1).map_or(text.len(), |(b, _)| b);
            e.cursor.want_col = nagi_core::display_col(&text, e.cursor.byte, false);
            e.clamp_normal();
            e.top = line.saturating_sub(e.rows / 3);
        });
    }

    fn close_input(&mut self) {
        self.input = None;
        self.set_state(State::Normal);
    }

    /// 決める（OK）
    fn accept_input(&mut self) {
        let Some(mut d) = self.input.take() else { return };
        self.set_state(State::Normal);
        if let Some(ok) = d.ok.take() {
            ok(self, d.field.text);
        }
    }

    pub(crate) fn input_key(&mut self, key: Key) {
        let Some(d) = &mut self.input else {
            self.set_state(State::Normal);
            return;
        };
        match key {
            Key::Esc | Key::Ctrl('c') | Key::Ctrl('[') => self.close_input(),
            Key::Enter if d.button == 1 => self.close_input(),
            Key::Enter => self.accept_input(),
            Key::Tab => d.button = 1 - d.button,
            k => {
                d.field.key(k);
            }
        }
    }

    pub(crate) fn input_click(&mut self, row: usize, col: usize) {
        let Some(d) = &self.input else { return };
        let r = rect(d, self.screen.0, self.screen.1);
        match buttons(r).into_iter().find(|b| b.1 == row && col >= b.2 && col < b.3).map(|b| b.0) {
            Some(0) => self.accept_input(),
            Some(_) => self.close_input(),
            None => {}
        }
    }
}

/// この部品の入口（component.rs）
pub struct InputComponent;

impl crate::component::Component for InputComponent {
    fn key(&self, ed: &mut Editor, key: Key) -> Effect {
        ed.input_key(key);
        Effect::None
    }

    fn text(&self, ed: &mut Editor, text: &str) {
        if let Some(d) = &mut ed.input {
            d.field.insert(text);
        }
    }

    fn click(&self, ed: &mut Editor, row: usize, col: usize, clicks: u32) -> bool {
        let _ = clicks;
        ed.input_click(row, col);
        true
    }

    fn status(&self, ed: &Editor) -> Option<crate::component::Status> {
        let _ = ed;
        crate::component::Status::new("", "Enter 決める  Tab ボタン  Esc 閉じる ")
    }
}

/// 描き方（layout.rs の View。画面全体を覆う）
pub struct InputView;

impl crate::layout::View for InputView {
    fn rect(&self, ed: &Editor, s: crate::layout::Screen) -> Option<Rect> {
        ed.input.as_ref()?;
        Some(Rect { x: 0, y: 0, w: s.cols, h: s.rows })
    }

    fn draw(&self, ed: &Editor, p: &mut crate::view::Painter, r: Rect, s: crate::layout::Screen) {
        let _ = r;
        if let Some(d) = &ed.input {
            draw::draw_input(p, ed, d, s.cols, s.rows);
        }
    }
}

pub(crate) mod draw {
    use super::*;
    use crate::view::*;

    pub(crate) fn draw_input(p: &mut Painter, ed: &Editor, d: &InputDialog, cols: usize, rows: usize) {
        let cell = p.fonts.cell();
        let r = rect(d, cols, rows);
        let (x0, y0, w, h) = (r.x as f32 * cell.w, r.y as f32 * cell.h, r.w as f32 * cell.w, r.h as f32 * cell.h);
        popup_frame(p, x0, y0, w, h);
        let saved = p.cols;
        p.cols = r.x + r.w - 1;
        let y = |k: usize| (r.y + k) as f32 * cell.h;
        p.text(&format!(" {}", d.title), r.x, y(0), FG_BRIGHT);
        p.text(&format!("  {}", d.label), r.x, y(1) + cell.h * 0.3, FG);
        // 欄（枠と、本文の地）
        let (fx, fw) = ((r.x + 2) as f32 * cell.w, (r.w - 4) as f32 * cell.w);
        p.out.push(Instance::fill(fx - 1.0, y(2) + cell.h * 0.3 - 1.0, fw + 2.0, cell.h + 2.0, ACCENT_FG));
        p.out.push(Instance::fill(fx, y(2) + cell.h * 0.3, fw, cell.h, BG));
        field(p, &d.field.text, d.field.cursor, &ed.preedit, r.x + 3, y(2) + cell.h * 0.3, true);
        for (k, n) in d.notes.iter().enumerate() {
            p.text(&format!("  {n}"), r.x, y(4 + k), STATUS_FG);
        }
        for (i, row, c0, c1) in buttons(r) {
            let label = if i == 0 { "OK" } else { "キャンセル" };
            let on = i == d.button;
            p.out.push(Instance::rounded(c0 as f32 * cell.w, row as f32 * cell.h + crate::metrics::pad(), (c1 - c0) as f32 * cell.w, cell.h - 2.0 * crate::metrics::pad(), crate::metrics::radius(crate::metrics::Radius::Control), if on { ACCENT_FG } else { RAISED_BG }));
            let lw = p.width(label);
            p.text(label, c0 + (c1 - c0).saturating_sub(lw) / 2, row as f32 * cell.h, if on { FG_BRIGHT } else { FG });
        }
        p.cols = saved;
    }
}
