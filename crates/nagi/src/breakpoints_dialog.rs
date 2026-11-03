//! ブレークポイントのダイアログ（IntelliJ の「ブレークポイント」。Ctrl+Shift+F8）。
//!
//! すべてのファイルのブレークポイントを並べ、頭の □ で有効・無効、選んだものの条件を下に出す。
//! 下のボタンで 条件…・除去・ソースに移動・閉じる。キーは ↑↓ で選ぶ、Space で有効・無効、Enter でソースに移動、
//! Delete で除去、F2 で条件（ボタンと右クリックのメニューにも同じものがある）。

use std::path::PathBuf;

use nagi_vim::Key;

use crate::editor::{Editor, Effect, State};
use crate::panes::Rect;

pub struct BreakpointsDialog {
    pub selected: usize,
    pub scroll: usize,
}

/// ボタン（番号, 見出し）。左から 条件…・除去・ソースに移動、右に 閉じる
const BUTTONS: [(usize, &str); 4] = [(0, "条件…"), (1, "除去"), (2, "ソースに移動"), (3, "閉じる")];

/// ダイアログの矩形（画面の真ん中）
pub fn rect(cols: usize, rows: usize) -> Rect {
    let w = crate::metrics::cols(96).min(cols.saturating_sub(4));
    let h = 20.min(rows.saturating_sub(4));
    Rect { x: cols.saturating_sub(w) / 2, y: rows.saturating_sub(h) / 2, w, h }
}

/// 一覧の行の範囲（ダイアログの中の行。上 2 行は題と見出し、下 4 行は条件とボタン）
fn list_rows(r: Rect) -> std::ops::Range<usize> {
    2..r.h.saturating_sub(4)
}

/// ボタンの位置（番号, 行, 始めの列, 終わりの列）
pub fn buttons(r: Rect) -> Vec<(usize, usize, usize, usize)> {
    let row = r.y + r.h - 2;
    let w = |s: &str| nagi_core::display_col(s, s.len(), false) + 4;
    let mut x = r.x + 2;
    let mut v = Vec::new();
    for (i, label) in &BUTTONS[..3] {
        v.push((*i, row, x, x + w(label)));
        x += w(label) + 1;
    }
    v.push((3, row, r.x + r.w - 2 - w("閉じる"), r.x + r.w - 2));
    v
}

impl Editor {
    /// ブレークポイントの並び（ファイル, 行）。ファイルの名前と行の順
    pub(crate) fn breakpoint_list(&self) -> Vec<(PathBuf, usize)> {
        self.breakpoints.iter().flat_map(|(p, s)| s.keys().map(move |l| (p.clone(), *l))).collect()
    }

    /// Ctrl+Shift+F8・デバッグの欄の「ブレークポイント」
    pub(crate) fn open_breakpoints_dialog(&mut self) {
        // 今の行のブレークポイントがあれば、それを選んで開く
        let here = self.core.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok()).unwrap_or_default();
        let line = self.core.cursor.line;
        self.open_breakpoints_dialog_at(&here, line);
    }

    /// ファイルの行のブレークポイントを選んで開く（無ければ先頭）
    pub(crate) fn open_breakpoints_dialog_at(&mut self, path: &std::path::Path, line: usize) {
        let at = self.breakpoint_list().iter().position(|(p, l)| p == path && *l == line).unwrap_or(0);
        let h = list_rows(rect(self.screen.0, self.screen.1)).len().max(1);
        let mut d = BreakpointsDialog { selected: at, scroll: 0 };
        nagi_ui::list::fit(d.selected, &mut d.scroll, h);
        self.panels.breakpoints = Some(d);
        self.set_state(State::Breakpoints);
    }

    fn close_breakpoints_dialog(&mut self) {
        self.panels.breakpoints = None;
        self.set_state(State::Normal);
    }

    /// 選んでいるブレークポイント
    fn chosen_breakpoint(&self) -> Option<(PathBuf, usize)> {
        let d = self.panels.breakpoints.as_ref()?;
        self.breakpoint_list().get(d.selected).cloned()
    }

    fn breakpoints_press(&mut self, button: usize) {
        let Some((path, line)) = self.chosen_breakpoint() else {
            if button == 3 || self.breakpoint_list().is_empty() {
                self.close_breakpoints_dialog();
            }
            return;
        };
        match button {
            0 => {
                // 書き終えたらダイアログに戻る
                let keep = self.panels.breakpoints.take();
                self.set_state(State::Normal);
                let back = path.clone();
                self.edit_breakpoint_condition(path, line, move |e| e.open_breakpoints_dialog_at(&back, line));
                if self.state() != State::Input {
                    self.panels.breakpoints = keep;
                    self.set_state(State::Breakpoints);
                }
            }
            1 => {
                self.remove_breakpoint(&path, line);
                let n = self.breakpoint_list().len();
                if let Some(d) = &mut self.panels.breakpoints {
                    d.selected = d.selected.min(n.saturating_sub(1));
                }
            }
            2 => {
                self.close_breakpoints_dialog();
                self.jump_to(&path, line, 0);
                self.with_motion(|m, c| m.first_non_blank(c));
            }
            _ => self.close_breakpoints_dialog(),
        }
    }

    /// 選んでいるものの有効・無効を切り替える
    fn breakpoints_toggle(&mut self) {
        if let Some((path, line)) = self.chosen_breakpoint() {
            self.update_breakpoint(&path, line, |o| o.enabled = !o.enabled);
        }
    }

    pub(crate) fn breakpoints_key(&mut self, key: Key) {
        let n = self.breakpoint_list().len();
        let Some(d) = &mut self.panels.breakpoints else {
            self.set_state(State::Normal);
            return;
        };
        let h = list_rows(rect(self.screen.0, self.screen.1)).len().max(1);
        if let Some(i) = nagi_ui::list::nav(key, d.selected, n, h) {
            d.selected = i;
            nagi_ui::list::fit(d.selected, &mut d.scroll, h);
            return;
        }
        match key {
            Key::Esc | Key::Ctrl('c') => self.close_breakpoints_dialog(),
            Key::Enter => self.breakpoints_press(2),
            Key::Char(' ') => self.breakpoints_toggle(),
            Key::Delete => self.breakpoints_press(1),
            Key::F(2) => self.breakpoints_press(0),
            _ => {}
        }
    }

    pub(crate) fn breakpoints_click(&mut self, row: usize, col: usize, clicks: u32) {
        let r = rect(self.screen.0, self.screen.1);
        if let Some(b) = buttons(r).into_iter().find(|b| b.1 == row && col >= b.2 && col < b.3).map(|b| b.0) {
            self.breakpoints_press(b);
            return;
        }
        let n = self.breakpoint_list().len();
        let Some(d) = &mut self.panels.breakpoints else { return };
        let rows = list_rows(r);
        if row < r.y + rows.start || row >= r.y + rows.end {
            return;
        }
        let i = d.scroll + row - r.y - rows.start;
        if i >= n {
            return;
        }
        d.selected = i;
        // 頭の □ を押したら有効・無効、ダブルクリックでソースに移動
        if col < r.x + 5 {
            self.breakpoints_toggle();
        } else if clicks >= 2 {
            self.breakpoints_press(2);
        }
    }

    /// 右クリック（一覧の行の上なら、その行を選んで操作のメニュー）
    pub(crate) fn breakpoints_context(&mut self, row: usize, col: usize) {
        self.breakpoints_click(row, col.max(rect(self.screen.0, self.screen.1).x + 5), 1);
        let Some((path, line)) = self.chosen_breakpoint() else { return };
        let on = self.breakpoints.get(&path).and_then(|s| s.get(&line)).is_some_and(|o| o.enabled);
        let m = crate::menu::Menu::new(row, col)
            .item(format!("{}有効", if on { "✓ " } else { "   " }), |e| e.breakpoints_toggle())
            .item("条件…", |e| e.breakpoints_press(0))
            .item("ソースに移動", |e| e.breakpoints_press(2))
            .sep()
            .item("除去", |e| e.breakpoints_press(1));
        self.open_menu(m);
    }
}

/// この部品の入口（component.rs）
pub struct BreakpointsComponent;

impl crate::component::Component for BreakpointsComponent {
    fn key(&self, ed: &mut Editor, key: Key) -> Effect {
        ed.breakpoints_key(key);
        Effect::None
    }

    fn click(&self, ed: &mut Editor, row: usize, col: usize, clicks: u32) -> bool {
        ed.breakpoints_click(row, col, clicks);
        true
    }

    fn status(&self, ed: &Editor) -> Option<crate::component::Status> {
        let _ = ed;
        crate::component::Status::new("-- ブレークポイント --", "↑↓ 選ぶ  Space 有効・無効  F2 条件  Delete 除去  Enter ソースに移動  Esc 閉じる ")
    }
}

/// 描き方（layout.rs の View。画面全体を覆う）
pub struct BreakpointsView;

impl crate::layout::View for BreakpointsView {
    fn rect(&self, ed: &Editor, s: crate::layout::Screen) -> Option<Rect> {
        ed.panels.breakpoints.as_ref()?;
        Some(Rect { x: 0, y: 0, w: s.cols, h: s.rows })
    }

    fn draw(&self, ed: &Editor, p: &mut crate::view::Painter, r: Rect, s: crate::layout::Screen) {
        let _ = r;
        if let Some(d) = &ed.panels.breakpoints {
            draw::draw_breakpoints(p, ed, d, s.cols, s.rows);
        }
    }

    fn context(&self, ed: &mut Editor, r: Rect, at: crate::layout::At) -> bool {
        let _ = r;
        ed.breakpoints_context(at.screen.0, at.screen.1);
        true
    }
}

pub(crate) mod draw {
    use super::*;
    use crate::view::*;

    pub(crate) fn draw_breakpoints(p: &mut Painter, ed: &Editor, d: &BreakpointsDialog, cols: usize, rows: usize) {
        let cell = p.fonts.cell();
        let r = rect(cols, rows);
        let (x0, y0, w, h) = (r.x as f32 * cell.w, r.y as f32 * cell.h, r.w as f32 * cell.w, r.h as f32 * cell.h);
        popup_frame(p, x0, y0, w, h);
        let saved = p.cols;
        p.cols = r.x + r.w - 1;
        let y = |k: usize| (r.y + k) as f32 * cell.h;
        p.text(" ブレークポイント", r.x, y(0), FG_BRIGHT);
        let list = ed.breakpoint_list();
        p.text(&format!("  行ブレークポイント   {} 件", list.len()), r.x, y(1), STATUS_FG);
        let lr = list_rows(r);
        for (k, (path, line)) in list.iter().enumerate().skip(d.scroll).take(lr.len()) {
            let yy = y(lr.start + k - d.scroll);
            if k == d.selected {
                selection(p, x0, yy, w, cell.h, LIST_SEL_BG);
            }
            let o = ed.breakpoints.get(path).and_then(|s| s.get(line)).cloned().unwrap_or_default();
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let x = p.text(if o.enabled { "  ☑ " } else { "  ☐ " }, r.x, yy, if o.enabled { ACCENT_FG } else { STATUS_FG });
            let x = p.text(&format!("{name}:{}", line + 1), x, yy, if o.enabled { FG } else { STATUS_FG });
            // 行の中身と条件
            let text = if ed.core.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok()).as_ref() == Some(path) { ed.core.buf.line(*line) } else { String::new() };
            let x = p.text(&format!("   {}", text.trim()), x, yy, LINE_NO_CUR);
            if !o.condition.is_empty() {
                p.text(&format!("   条件: {}", o.condition), x, yy, DIAG_WARN);
            }
        }
        if list.is_empty() {
            p.text("  ブレークポイントはありません（行番号の欄を押すか F9 で付ける）", r.x, y(lr.start), STATUS_FG);
        }
        // 選んでいるものの条件
        let cy = r.h.saturating_sub(4);
        p.out.push(Instance::fill(x0, y(cy), w, 1.0, SEPARATOR));
        if let Some(o) = list.get(d.selected).and_then(|(p, l)| ed.breakpoints.get(p).and_then(|s| s.get(l))) {
            let x = p.text(&format!("  {}有効   ", if o.enabled { "☑ " } else { "☐ " }), r.x, y(cy), FG);
            p.text(&format!("条件: {}", if o.condition.is_empty() { "（なし）" } else { o.condition.as_str() }), x, y(cy), FG);
        }
        for (i, row, c0, c1) in buttons(r) {
            let label = BUTTONS.iter().find(|b| b.0 == i).map_or("", |b| b.1);
            let enabled = i == 3 || !list.is_empty();
            p.out.push(Instance::rounded(c0 as f32 * cell.w, row as f32 * cell.h + crate::metrics::pad(), (c1 - c0) as f32 * cell.w, cell.h - 2.0 * crate::metrics::pad(), crate::metrics::radius(crate::metrics::Radius::Control), RAISED_BG));
            let lw = p.width(label);
            p.text(label, c0 + (c1 - c0).saturating_sub(lw) / 2, row as f32 * cell.h, if enabled { FG } else { LINE_NO });
        }
        p.cols = saved;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    /// ダイアログ: ブレークポイントを並べ、□ で無効に、条件を書き、除去し、ソースに移動する。
    /// 行番号の欄の右クリックからも 条件…・有効 を選べる
    #[test]
    fn list_toggle_condition_remove_jump() {
        let dir = std::env::temp_dir().join(format!("nagi-bpdlg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("A.java");
        std::fs::write(&file, "class A {\n  void f() {\n    int i = 0;\n    i++;\n  }\n}\n").unwrap();
        let mut e = Editor::new(Buffer::open(&file).unwrap(), None);
        e.core.buf.path = Some(file.clone());
        e.set_screen(140, 40);
        let abs = std::path::absolute(&file).unwrap();
        e.toggle_breakpoint(Some(2));
        e.toggle_breakpoint(Some(3));
        e.core.cursor.line = 3;
        e.open_breakpoints_dialog();
        assert_eq!(e.state(), State::Breakpoints);
        assert_eq!(e.panels.breakpoints.as_ref().unwrap().selected, 1, "今の行のものを選んで開く");
        // □ を押して無効に（印は輪、デバッグには送らない）
        let r = rect(140, 40);
        e.breakpoints_click(r.y + 2 + 1, r.x + 2, 1);
        assert_eq!(e.break_marks(Some(&abs)), [(2, crate::debugger::BpMark::Normal), (3, crate::debugger::BpMark::Disabled)]);
        // 条件…（ボタン）: 入力のダイアログで書くと、閉じた後にダイアログに戻る
        e.breakpoints_key(Key::Up);
        let b = buttons(r).into_iter().find(|b| b.0 == 0).unwrap();
        e.breakpoints_click(b.1, b.2, 1);
        assert_eq!(e.state(), State::Input);
        for c in "i == 3".chars() {
            e.input_key(Key::Char(c));
        }
        e.input_key(Key::Enter);
        assert_eq!(e.state(), State::Breakpoints);
        assert_eq!(e.panels.breakpoints.as_ref().unwrap().selected, 0, "書いたものを選んだまま戻る");
        assert_eq!(e.breakpoints[&abs][&2].condition, "i == 3");
        assert_eq!(e.break_marks(Some(&abs))[0].1, crate::debugger::BpMark::Conditional);
        // Delete で除去
        e.breakpoints_key(Key::Down);
        e.breakpoints_key(Key::Delete);
        assert_eq!(e.breaks_for(Some(&abs)), [2]);
        // Enter でソースに移動
        e.core.cursor.line = 0;
        e.breakpoints_key(Key::Enter);
        assert_eq!((e.state(), e.core.cursor.line), (State::Normal, 2));
        // 行番号の欄の右クリックのメニュー
        e.gutter_context(2, (5, 5));
        let labels: Vec<String> = e.menu.as_ref().unwrap().items.iter().map(|i| i.label.to_string()).collect();
        assert!(labels.iter().any(|l| l.contains("条件…")) && labels.iter().any(|l| l.contains("有効")), "{labels:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 行末の値: 関数の頭から止まった行までの、行に出てくる変数を並べる（コメントの中は見ない）
    #[test]
    fn inline_values() {
        let lines: Vec<String> = ["int f(int a) {", "  int b = a + 1; // a", "  String s = \"x\" + b;", "  return b;"].iter().map(|s| s.to_string()).collect();
        let vals = vec![("a".to_string(), "2".to_string()), ("b".to_string(), "3".to_string()), ("s".to_string(), "\"x3\"".to_string())];
        let m = crate::debugger::inline_values(&lines, 0, 2, &vals);
        assert_eq!(m.get(&0).map(String::as_str), Some("a: 2"));
        assert_eq!(m.get(&1).map(String::as_str), Some("b: 3   a: 2"));
        assert_eq!(m.get(&2).map(String::as_str), Some("s: \"x3\"   b: 3"));
        assert!(!m.contains_key(&3), "止まった行より後には出さない");
    }
}
