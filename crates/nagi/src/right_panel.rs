//! 右の欄（IntelliJ の右のツールウィンドウと同じ）。「データベース」と「Maven」のタブ。
//!
//! 左の欄と同じく、1 行目にタブ、右端に隠すボタン。両方開いていれば最後に選んだ方を出す。
//! 隠しても中身（つないだ DB、開いた木）は残す。幅は左の欄との間の線をドラッグして変える（resize.rs）。

use nagi_vim::Key;

use crate::editor::{Editor, Effect, Mode, Side};
use crate::panes::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RightTab {
    #[default]
    Database,
    Maven,
    /// Markdown のプレビュー
    Preview,
}

pub const RTABS: [(&str, RightTab); 3] = [(" データベース ", RightTab::Database), (" Maven ", RightTab::Maven), (" プレビュー ", RightTab::Preview)];

/// 右のストライプのラベル（縦書き。見出し, タブ, 上端の行）
pub const RSTRIPE: [(&str, RightTab, usize); 3] = [("データベース", RightTab::Database, 0), ("Maven", RightTab::Maven, 7), ("プレビュー", RightTab::Preview, 13)];

pub fn rstripe_at(row: usize) -> Option<RightTab> {
    RSTRIPE.iter().find(|(l, _, top)| row >= *top && row < top + l.chars().count()).map(|s| s.1)
}

/// 1 行目の（欄の左端から数えた）`col` にあるタブ
pub fn rtab_at(col: usize) -> Option<RightTab> {
    let mut x = 0;
    for (label, tab) in RTABS {
        let w = nagi_core::display_col(label, label.len(), false);
        if col >= x && col < x + w {
            return Some(tab);
        }
        x += w;
    }
    None
}

impl Editor {
    /// 中身のあるタブ
    fn right_open_tabs(&self) -> Vec<RightTab> {
        let mut v = Vec::new();
        if self.db.is_some() {
            v.push(RightTab::Database);
        }
        if self.maven.is_some() {
            v.push(RightTab::Maven);
        }
        if self.md_preview.is_some() {
            v.push(RightTab::Preview);
        }
        v
    }

    pub fn right_visible(&self) -> Option<RightTab> {
        if self.right_hidden {
            return None;
        }
        let open = self.right_open_tabs();
        if open.contains(&self.right_tab) { Some(self.right_tab) } else { open.first().copied() }
    }

    /// 最後に選んだタブ（隠していても覚えている）
    pub fn right_last(&self) -> RightTab {
        let open = self.right_open_tabs();
        if open.contains(&self.right_tab) { self.right_tab } else { open.first().copied().unwrap_or(RightTab::Database) }
    }

    /// 右の欄の幅（既定は 40 列。狭い画面では 1/3 まで。窓に 20 列は残す）
    pub fn right_width(&self) -> usize {
        let cols = self.screen.0;
        let left = self.left_rect().map_or(0, |r| r.w + 1);
        // プレビューは広め（画面の 45%）
        let def = if self.right_visible() == Some(RightTab::Preview) { cols * 45 / 100 } else { 40.min(cols / 3) };
        let w = self.right_w.unwrap_or(def.max(10));
        w.min(cols.saturating_sub(left + 21)).max(10.min(cols))
    }

    pub fn right_rect(&self) -> Option<Rect> {
        self.right_visible()?;
        let cols = self.screen.0;
        let w = self.right_width();
        Some(Rect { x: cols.saturating_sub(w), y: 0, w, h: self.windows_rows() })
    }

    /// 右の欄にいるか（リストモード）
    pub fn right_focused(&self) -> bool {
        self.mode == Mode::List && self.list_side == Side::Right
    }

    pub(crate) fn select_right_tab(&mut self, tab: RightTab) {
        match tab {
            RightTab::Database => self.open_db_panel(),
            RightTab::Maven => self.open_maven(),
            RightTab::Preview => {
                if self.md_preview.is_none() {
                    self.toggle_md_preview();
                }
                if self.md_preview.is_some() {
                    self.focus_right(RightTab::Preview);
                }
            }
        }
    }

    /// そのタブを出して移る。もう出ていて移っていれば隠す
    pub(crate) fn toggle_right_tab(&mut self, tab: RightTab) {
        if self.right_visible() == Some(tab) && self.right_focused() {
            self.hide_right();
        } else {
            self.select_right_tab(tab);
        }
    }

    pub(crate) fn focus_right(&mut self, tab: RightTab) {
        self.right_tab = tab;
        self.right_hidden = false;
        self.list_side = Side::Right;
        self.mode = Mode::List;
        self.pending_keys.clear();
        self.rows = self.text_rows(self.active_rect());
    }

    pub(crate) fn hide_right(&mut self) {
        self.right_hidden = true;
        if self.right_focused() {
            self.mode = Mode::Normal;
        }
        self.rows = self.text_rows(self.active_rect());
    }

    /// 右の欄にいる間のキー（タブごとに振り分ける。Ctrl-w h / Esc で窓へ戻る）
    pub(crate) fn right_key(&mut self, key: Key) -> Effect {
        if std::mem::take(&mut self.right_pending_w) {
            if matches!(key, Key::Char('h') | Key::Ctrl('h') | Key::Char('w') | Key::Ctrl('w') | Key::Char('p')) {
                self.mode = Mode::Normal;
            }
            return Effect::None;
        }
        if key == Key::Ctrl('w') {
            self.right_pending_w = true;
            return Effect::None;
        }
        match self.right_visible() {
            Some(RightTab::Maven) => self.maven_key(key),
            Some(RightTab::Database) => self.db_key(key),
            Some(RightTab::Preview) => self.md_preview_key(key),
            None => {
                self.mode = Mode::Normal;
                Effect::None
            }
        }
    }

    /// マウス: 右の欄の中の (行, 列) を押した（矩形の中の位置）
    pub(crate) fn right_click(&mut self, row: usize, col: usize, clicks: u32) {
        if row == 0 {
            if let Some(tab) = rtab_at(col) {
                self.select_right_tab(tab);
            }
            return;
        }
        match self.right_visible() {
            Some(RightTab::Maven) => self.maven_click(row, clicks),
            Some(RightTab::Database) => self.db_click(row, clicks),
            Some(RightTab::Preview) => self.md_preview_click(row, clicks),
            None => {}
        }
    }
}
