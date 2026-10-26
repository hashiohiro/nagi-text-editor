//! 実行の構成の欄（左の欄の「実行」タブ。IntelliJ の Services のように、構成を並べてその場で動かす）。
//!
//! 1 行目はタブ、2 行目は「▶ 実行  ■ 止める  ✎ 編集」のボタン（選んでいる構成に効く）、3 行目から構成の一覧。
//! 一覧で選んだ構成が、Shift+F10・状態行の「▶ 名前」で動かす構成になる。動いている構成は緑の ●。
//! Enter・ダブルクリックで実行、右クリックで操作のメニュー。最後の行の「＋ 構成を追加・編集…」で構成の窓を開く。

use nagi_vim::Key;
use nagi_workers::runconfig::RunConfig;

use crate::editor::{Editor, Side, State};
use crate::run_configs::RUN_PREFIX;

pub struct RunPanel {
    pub items: Vec<RunConfig>,
    /// 選んでいる行（items.len() は最後の「追加・編集」の行）
    pub selected: usize,
    pub scroll: usize,
}

/// 一覧の上の行数（タブとボタン）
pub const HEAD: usize = 2;

/// 2 行目のボタン（字, 何をするか）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Run,
    Stop,
    Edit,
    Add,
}

/// 追加は一覧の最後の行（「＋ 構成を追加・編集…」）でする（欄の幅に収めるため、ボタンは 3 つ）
pub const TOOLS: [(&str, Tool); 3] = [("▶ 実行", Tool::Run), ("■ 止める", Tool::Stop), ("✎ 編集", Tool::Edit)];

/// ボタンの並び: (左端の列（欄の左端から）, 幅, ボタン)。字の幅は `width` で測る
pub fn tool_spans(width: impl Fn(&str) -> usize) -> Vec<(usize, usize, Tool)> {
    let mut x = 1;
    TOOLS
        .iter()
        .map(|(l, t)| {
            let w = width(l) + 2;
            let s = (x, w, *t);
            x += w + 1;
            s
        })
        .collect()
}

impl RunPanel {
    /// 一覧の行の数（最後の「追加・編集」を含む）
    pub fn len(&self) -> usize {
        self.items.len() + 1
    }

    pub fn current(&self) -> Option<&RunConfig> {
        self.items.get(self.selected)
    }
}

impl Editor {
    /// 構成の名前が動いているか
    pub fn run_is_running(&self, name: &str) -> bool {
        self.terminals.list.iter().any(|t| t.name.strip_prefix(RUN_PREFIX) == Some(name) && !t.exited)
    }

    /// 実行の欄を開いて移る（構成を読み直し、選んでいる構成の行を選ぶ）
    pub(crate) fn open_run_panel(&mut self) {
        let mut items = self.load_run_configs();
        if let Some(c) = self.current_file_config() {
            items.push(c);
        }
        let at = self.run_selected.as_ref().and_then(|s| items.iter().position(|c| &c.name == s)).unwrap_or(0);
        let p = self.run_panel.get_or_insert(RunPanel { items: Vec::new(), selected: 0, scroll: 0 });
        p.items = items;
        p.selected = at.min(p.len() - 1);
        self.left_tab = crate::tree_panel::LeftTab::Run;
        self.left_hidden = false;
        self.list_side = Side::Run;
        self.set_state(State::List);
        self.rows = self.text_rows(self.active_rect());
    }

    pub(crate) fn close_run_panel(&mut self) {
        self.run_panel = None;
        if self.state() == State::List && self.list_side == Side::Run {
            self.set_state(State::Normal);
        }
        self.rows = self.text_rows(self.active_rect());
    }

    /// 実行の欄の矩形（左の欄。1 行目はタブ）
    pub fn run_panel_rect(&self) -> Option<crate::panes::Rect> {
        (self.left_visible()? == crate::tree_panel::LeftTab::Run).then(|| self.left_rect()).flatten()
    }

    /// 選んでいる行を動かす構成にする
    fn run_panel_choose(&mut self) {
        if let Some(c) = self.run_panel.as_ref().and_then(|p| p.current()) {
            self.run_selected = Some(c.name.clone());
        }
    }

    /// ボタン・キーの操作（選んでいる構成に）
    pub(crate) fn run_panel_tool(&mut self, tool: Tool) {
        let cur = self.run_panel.as_ref().and_then(|p| p.current().cloned());
        match (tool, cur) {
            (Tool::Run, Some(c)) => self.run_config(c),
            (Tool::Run, None) | (Tool::Add, _) => self.open_run_dialog(None),
            (Tool::Stop, Some(c)) => self.run_stop_named(&c.name),
            (Tool::Stop, None) => self.run_stop(),
            (Tool::Edit, Some(c)) => self.open_run_dialog(Some(&c.name)),
            (Tool::Edit, None) => self.open_run_dialog(None),
        }
    }

    /// 名前の構成を止める（出力は残す）
    pub(crate) fn run_stop_named(&mut self, name: &str) {
        match self.terminals.list.iter_mut().find(|t| t.name.strip_prefix(RUN_PREFIX) == Some(name) && !t.exited) {
            Some(t) => {
                t.term.kill();
                self.notify.info(format!("止めました: {name}"));
            }
            None => self.notify.info(format!("{name} は動いていません")),
        }
    }

    pub(crate) fn run_panel_key(&mut self, key: Key) -> crate::editor::Effect {
        let h = self.run_panel_rect().map_or(10, |r| r.h.saturating_sub(HEAD));
        let Some(p) = &mut self.run_panel else { return crate::editor::Effect::None };
        if let Some(i) = crate::list::step(key, p.selected, p.len(), h) {
            p.selected = i;
            self.run_panel_choose();
            return crate::editor::Effect::None;
        }
        match key {
            Key::Enter => self.run_panel_tool(Tool::Run),
            Key::Char('s') => self.run_panel_tool(Tool::Stop),
            Key::Char('e') => self.run_panel_tool(Tool::Edit),
            Key::Char('a') => self.run_panel_tool(Tool::Add),
            Key::Char('r') => self.open_run_panel(),
            Key::Char('q') => self.close_run_panel(),
            Key::Esc => self.set_state(State::Normal),
            Key::Char(':') => self.enter_command(""),
            _ => {}
        }
        crate::editor::Effect::None
    }

    /// 欄の中の (行, 列) を押した（0 行目はタブ。LeftTabsView が受け取る）
    pub(crate) fn run_panel_click(&mut self, row: usize, col: usize, clicks: u32) {
        self.list_side = Side::Run;
        self.set_state(State::List);
        if row == 1 {
            let spans = tool_spans(|s| nagi_core::display_col(s, s.len(), false));
            if let Some(&(_, _, t)) = spans.iter().find(|(x, w, _)| col >= *x && col < x + w) {
                self.run_panel_tool(t);
            }
            return;
        }
        let Some(p) = &mut self.run_panel else { return };
        let len = p.len();
        let pick = crate::list::pick_row(&mut p.selected, p.scroll, row, HEAD, len, clicks);
        if pick.is_some() {
            self.run_panel_choose();
        }
        if let Some(crate::list::Pick::Accept(_)) = pick {
            self.run_panel_tool(Tool::Run);
        }
    }
}

/// この部品の入口（component.rs）
pub struct RunPanelComponent;

impl crate::component::Component for RunPanelComponent {
    fn status(&self, ed: &Editor) -> Option<crate::component::Status> {
        let _ = ed;
        crate::component::Status::keys("-- 実行 --", "Enter 実行  s 止める  e 編集  a 追加  r 読み直す  q 閉じる ", "Enter・ダブルクリック 実行  右クリック 操作  Esc 本文へ ")
    }

    fn key(&self, ed: &mut Editor, key: Key) -> crate::editor::Effect {
        ed.run_panel_key(key)
    }
}

/// 実行の欄（layout.rs の View）
pub struct RunPanelView;

impl crate::layout::View for RunPanelView {
    fn rect(&self, ed: &Editor, s: crate::layout::Screen) -> Option<crate::panes::Rect> {
        let _ = s;
        ed.run_panel.as_ref()?;
        ed.run_panel_rect()
    }

    fn draw(&self, ed: &Editor, p: &mut crate::view::Painter, r: crate::panes::Rect, s: crate::layout::Screen) {
        let _ = s;
        if let Some(rp) = &ed.run_panel {
            draw::draw_run_panel(p, ed, rp, r);
        }
    }

    fn click(&self, ed: &mut Editor, r: crate::panes::Rect, row: usize, col: usize, clicks: u32) -> crate::layout::Click {
        ed.run_panel_click(row - r.y, col - r.x, clicks);
        crate::layout::Click::Done
    }

    fn wheel(&self, ed: &mut Editor, r: crate::panes::Rect, row: usize, col: usize, dy: isize, dx: isize) -> bool {
        let _ = (row, col, dx);
        if let Some(p) = &mut ed.run_panel {
            let len = p.len();
            crate::list::wheel(&mut p.selected, &mut p.scroll, len, r.h.saturating_sub(HEAD), dy);
        }
        true
    }

    fn context(&self, ed: &mut Editor, r: crate::panes::Rect, row: usize, col: usize) -> bool {
        use nagi_vim::Key::*;
        // 押した行を選ぶ（ボタンの行・タブの行ならメニューを出さない）
        if row - r.y < HEAD {
            return false;
        }
        ed.run_panel_click(row - r.y, col - r.x, 1);
        let Some(c) = ed.run_panel.as_ref().and_then(|p| p.current().cloned()) else {
            let m = crate::menu::Menu::new(row, col).keys("構成を追加…", &[Char('a')]).keys("読み直す", &[Char('r')]).sep().keys("欄を閉じる", &[Char('q')]);
            ed.open_menu(m);
            return true;
        };
        let mut m = crate::menu::Menu::new(row, col).keys(format!("▶ {} を実行", c.name), &[Enter]);
        if ed.run_is_running(&c.name) {
            m = m.keys("■ 止める", &[Char('s')]);
        }
        let m = m.keys("✎ 構成の編集…", &[Char('e')]).keys("＋ 構成を追加…", &[Char('a')]).sep().keys("読み直す", &[Char('r')]).keys("欄を閉じる", &[Char('q')]);
        ed.open_menu(m);
        true
    }
}

/// 描き方（view.rs の Painter で描く）
pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    /// 実行の欄（左の欄の「実行」タブ）
    pub(crate) fn draw_run_panel(p: &mut Painter, ed: &Editor, rp: &RunPanel, r: Rect) {
        let cell = p.fonts.cell();
        let (x0, y0, wpx) = (r.x as f32 * cell.w, r.y as f32 * cell.h, r.w as f32 * cell.w);
        p.out.push(Instance::fill(x0, y0, wpx, r.h as f32 * cell.h, PANEL_BG));
        p.out.push(Instance::fill(((r.x + r.w) as f32 + 0.5) * cell.w, y0, 1.0, r.h as f32 * cell.h, LINE_NO));
        let focused = ed.state() == State::List && ed.list_side == Side::Run;
        let saved = p.cols;
        p.cols = r.x + r.w;
        draw_left_tabs(p, r, crate::tree_panel::LeftTab::Run, focused, "");
        // ボタン（止めるは動いているときだけ明るく）
        let y = y0 + cell.h;
        let running = rp.current().is_some_and(|c| ed.run_is_running(&c.name));
        for (x, _, t) in tool_spans(|s| p.width(s)) {
            let label = TOOLS.iter().find(|x| x.1 == t).map_or("", |x| x.0);
            let color = match t {
                Tool::Run if rp.current().is_some() => GIT_ADD,
                Tool::Stop if running => DIAG_ERROR,
                Tool::Stop | Tool::Run => LINE_NO,
                _ => STATUS_FG,
            };
            p.text(&format!(" {label} "), r.x + x, y, color);
        }
        p.out.push(Instance::fill(x0, y + cell.h - 1.0, wpx, 1.0, SEPARATOR));
        // 一覧
        for k in 0..r.h.saturating_sub(HEAD) {
            let i = rp.scroll + k;
            if i >= rp.len() {
                break;
            }
            let y = (r.y + HEAD + k) as f32 * cell.h;
            let selected = i == rp.selected;
            let reversed = selected && focused;
            if reversed {
                selection(p, x0, y, wpx, cell.h, LIST_SEL_BG);
            } else if selected {
                p.out.push(Instance::fill(x0, y, wpx, cell.h, MATCH_BG));
            }
            let Some(c) = rp.items.get(i) else {
                p.text(" ＋ 構成を追加・編集…", r.x, y, if reversed { FG_BRIGHT } else { STATUS_FG });
                continue;
            };
            let on = ed.run_is_running(&c.name);
            let mark = if on { " ● " } else { "   " };
            let x = p.text(mark, r.x, y, if reversed { FG_BRIGHT } else { GIT_ADD });
            let x = p.text(&c.name, x, y, if reversed { FG_BRIGHT } else { FG });
            let from = match c.from {
                "nagi" => "  run.conf",
                "file" => "  今のファイル",
                _ => "",
            };
            p.text(&format!("  {}{from}", c.kind_label()), x, y, if reversed { FG_BRIGHT } else { STATUS_FG });
        }
        if rp.items.is_empty() {
            let y = (r.y + HEAD + 2) as f32 * cell.h;
            p.text(" 構成がありません（IntelliJ の構成も読みます）", r.x, y, LINE_NO);
        }
        p.cols = saved;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    /// 実行のタブ: 構成を並べ、選ぶと動かす構成になり、Enter で動かす。左の欄のタブは 3 つ
    #[test]
    fn run_tab_lists_and_runs() {
        let dir = std::env::temp_dir().join(format!("nagi-runtab-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join(".nagi")).unwrap();
        std::fs::write(dir.join(".nagi/run.conf"), "一 = echo one\n二 = echo two\n").unwrap();
        std::fs::write(dir.join("a.txt"), "x\n").unwrap();
        let mut e = Editor::new(Buffer::open(&dir.join("a.txt")).unwrap(), None);
        e.buf.path = Some(dir.join("a.txt"));
        e.set_screen(120, 40);
        e.toggle_left_tab(crate::tree_panel::LeftTab::Run);
        assert_eq!(e.left_visible(), Some(crate::tree_panel::LeftTab::Run));
        assert_eq!((e.state(), e.list_side), (State::List, Side::Run));
        let p = e.run_panel.as_ref().unwrap();
        assert_eq!(p.items.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), vec!["一", "二"]);
        assert_eq!(p.len(), 3, "最後は追加・編集の行");
        // ↓ で選ぶと、動かす構成になる
        e.key(Key::Down);
        assert_eq!(e.run_selected.as_deref(), Some("二"));
        // ほかのタブと行き来できる（実行の欄は残る）
        e.select_left_tab(crate::tree_panel::LeftTab::Project);
        assert_eq!(e.left_visible(), Some(crate::tree_panel::LeftTab::Project));
        e.select_left_tab(crate::tree_panel::LeftTab::Run);
        assert_eq!(e.run_panel.as_ref().unwrap().selected, 1);
        // Enter で動かす
        e.key(Key::Enter);
        assert_eq!(e.terminals.list.len(), 1);
        assert_eq!(e.terminals.list[0].name, "▶ 二");
        // もう一度押すと隠す
        e.list_side = Side::Run;
        e.set_state(State::List);
        e.toggle_left_tab(crate::tree_panel::LeftTab::Run);
        assert_eq!(e.left_visible(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tool_buttons_do_not_overlap() {
        let s = tool_spans(|s| nagi_core::display_col(s, s.len(), false));
        for w in s.windows(2) {
            assert!(w[0].0 + w[0].1 < w[1].0);
        }
    }
}
