//! 欄の見出しの右端の「隠す」ボタン（左の欄 `«`・下の欄 `▾`・右の欄 `»`）。マウスを載せると明るくなり、名前を横に出す。
//! 出すのは左右のアイコンの列（chrome.rs）。隠しても中身（ツリーの開き具合、書きかけのコミットメッセージ、検索の結果）は消さない。

use crate::editor::{Editor, State, Side};
use crate::tree_panel::LeftTab;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    HideLeft,
    HideBottom,
    HideRight,
}

impl Button {
    pub fn glyph(self) -> &'static str {
        match self {
            Button::HideLeft => "«",
            Button::HideBottom => "▾",
            Button::HideRight => "»",
        }
    }

    /// 2 セルの幅か
    pub fn wide(self) -> bool {
        false
    }

    /// マウスを載せたときに横に出す名前
    pub fn label(self, ed: &Editor) -> String {
        let _ = ed;
        "隠す".into()
    }
}

impl Editor {
    /// 左の欄が出ていれば、最後に選んだタブ（隠していても覚えている）
    pub fn left_last(&self) -> LeftTab {
        self.left_open().unwrap_or(LeftTab::Project)
    }

    /// 下の欄に出すものがあるか（検索・ログ・クエリの結果は中身があるとき、ほかのタブはいつも）
    pub fn bottom_exists(&self) -> bool {
        use crate::result_grid::BottomKind::*;
        match self.bottom_kind {
            Grep | Output => self.grep.is_some(),
            Result => !self.db_results.is_empty(),
            Git => self.log.as_ref().is_some_and(|l| l.docked),
            Problems | Terminal | Debug => true,
        }
    }

    /// 下のパネルが出ているか
    pub fn bottom_visible(&self) -> bool {
        self.bottom_exists() && !self.bottom_hidden
    }

    /// 画面にあるボタン（ボタン, 行, 列）。幅はどれも 1 セル。
    /// 欄を出すのは左右のアイコンの列（chrome.rs）、設定は上の帯の ⚙ なので、ここは欄の見出しの「隠す」だけ
    pub fn toggle_buttons(&self) -> Vec<(Button, usize, usize)> {
        let mut v = Vec::new();
        if let Some(r) = self.left_rect() {
            v.push((Button::HideLeft, r.y, r.x + r.w - 2));
        }
        if let Some(r) = self.panel_rect() {
            v.push((Button::HideBottom, r.y, r.x));
        }
        if let Some(r) = self.right_rect() {
            v.push((Button::HideRight, r.y, r.x + r.w - 2));
        }
        v
    }

    pub fn button_at(&self, row: usize, col: usize) -> Option<Button> {
        self.toggle_buttons().into_iter().find(|&(b, r, c)| r == row && (c == col || (b.wide() && c + 1 == col))).map(|b| b.0)
    }

    pub(crate) fn press_button(&mut self, b: Button) {
        match b {
            Button::HideLeft => self.hide_left(),
            Button::HideBottom => self.hide_bottom(),
            Button::HideRight => self.hide_right(),
        }
    }

    /// 左の欄を隠す（中身は残す）
    pub(crate) fn hide_left(&mut self) {
        self.left_hidden = true;
        if self.state() == State::CommitMsg || (self.state() == State::List && matches!(self.list_side, Side::Left | Side::Commit | Side::Run)) {
            self.set_state(State::Normal);
        }
        self.rows = self.text_rows(self.active_rect());
    }

    /// 下のパネルを隠す（結果は残す）
    pub(crate) fn hide_bottom(&mut self) {
        self.bottom_hidden = true;
        if self.state() == State::GrepPrompt || (self.state() == State::List && self.list_side == Side::Bottom) {
            self.set_state(State::Normal);
        }
        self.rows = self.text_rows(self.active_rect());
    }

    /// Alt-1 / Alt-0: そのタブを出して移る。もう出ていて移っていれば隠す（IntelliJ と同じ）
    pub(crate) fn toggle_left_tab(&mut self, tab: LeftTab) {
        let focused = match tab {
            LeftTab::Project => self.state() == State::List && self.list_side == Side::Left,
            LeftTab::Commit => self.state() == State::CommitMsg || (self.state() == State::List && self.list_side == Side::Commit),
            LeftTab::Run => self.state() == State::List && self.list_side == Side::Run,
        };
        if self.left_visible() == Some(tab) && focused {
            self.hide_left();
        } else {
            self.select_left_tab(tab);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    #[test]
    fn hide_and_show_left() {
        let dir = std::env::temp_dir().join(format!("nagi-toggles-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        let mut buf = Buffer::open(&dir.join("a.txt")).unwrap();
        buf.path = Some(dir.join("a.txt"));
        let mut e = Editor::new(buf, None);
        e.set_screen(120, 40);
        // 左のアイコンの列の「プロジェクト」で出す
        let (row, _, t) = e.left_stripe()[0];
        assert_eq!(e.chrome_at(row, 0), Some(crate::chrome::Hit::Tool(crate::chrome::Tool::Left(LeftTab::Project))));
        e.toggle_tool(t);
        assert_eq!(e.left_visible(), Some(LeftTab::Project));
        let a = e.area();
        assert_eq!(e.windows_x(), (a.x + e.left_width() + 1, a.w - e.left_width() - 1));
        // 見出しの右端の「隠す」で隠れ、窓が広がる。ツリーは残る
        let (b, r, c) = e.toggle_buttons()[0];
        assert_eq!((b, r, c), (Button::HideLeft, a.y, a.x + e.left_width() - 2));
        e.press_button(b);
        assert_eq!((e.left_visible(), e.windows_x(), e.state()), (None, (a.x, a.w), State::Normal));
        assert!(e.tree.is_some());
        // Alt-1 で出して移り、もう一度で隠す
        e.key(nagi_vim::Key::Alt('1'));
        assert_eq!((e.left_visible(), e.state()), (Some(LeftTab::Project), State::List));
        e.key(nagi_vim::Key::Alt('1'));
        assert_eq!(e.left_visible(), None);
        // 右のアイコンの列で右の欄を出す
        let (row, _, t) = e.right_stripe()[0];
        assert_eq!(e.chrome_at(row, 119), Some(crate::chrome::Hit::Tool(t)));
        e.toggle_tool(t);
        assert_eq!(e.right_visible(), Some(crate::right_panel::RightTab::Database));
        // 右の欄の中で : を押しても、コマンドの候補が出る
        e.key(nagi_vim::Key::Char(':'));
        assert_eq!(e.state(), State::Command);
        assert!(e.cmd_complete.as_ref().is_some_and(|c| c.items.len() > 20), "パネルの中の : でも候補を出す");
        let (_, r, c) = e.toggle_buttons().into_iter().find(|b| b.0 == Button::HideRight).unwrap();
        e.key(nagi_vim::Key::Esc);
        e.press_button(Button::HideRight);
        assert_eq!(e.right_visible(), None);
        let _ = (r, c);
        // 入力欄の Ctrl+V は貼り付け（1 行にする）
        e.clipboard = Some(crate::editor::Clipboard { get: || Some("abc\r\ndef\r\n".into()), set: |_| {} });
        e.key(nagi_vim::Key::Char(':'));
        e.key(nagi_vim::Key::Ctrl('v'));
        assert_eq!(e.cmdline.text, "abc def");
    }
}
