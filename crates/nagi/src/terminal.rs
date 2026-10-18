//! 下の欄の「ターミナル」（シェル）。中身は段階 2 で作る（今は欄だけ）。

use nagi_vim::Key;

use crate::editor::{Editor, Mode};
use crate::result_grid::BottomKind;

impl Editor {
    /// ターミナルのタブを出して移る
    pub(crate) fn open_terminal(&mut self) {
        self.bottom_hidden = false;
        self.bottom_kind = BottomKind::Terminal;
        self.mode = Mode::Terminal;
        self.pending_keys.clear();
        self.rows = self.text_rows(self.active_rect());
    }

    pub(crate) fn terminal_key(&mut self, key: Key) {
        if matches!(key, Key::Esc | Key::Ctrl('c')) {
            self.mode = Mode::Normal;
        }
    }

    pub(crate) fn terminal_click(&mut self) {
        self.mode = Mode::Terminal;
    }
}
