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

/// タブ（左の欄と同じく、いつも全部をアイコンで並べる。名前は出しているタブの横に出す）
pub const RTABS: [(&str, RightTab); 3] = [("データベース", RightTab::Database), ("Maven", RightTab::Maven), ("プレビュー", RightTab::Preview)];

/// 右のストライプのラベル（縦書き。見出し, タブ, 上端の行）
pub const RSTRIPE: [(&str, RightTab, usize); 3] = [("データベース", RightTab::Database, 0), ("Maven", RightTab::Maven, 7), ("プレビュー", RightTab::Preview, 13)];

pub fn rstripe_at(row: usize) -> Option<RightTab> {
    RSTRIPE.iter().find(|(l, _, top)| row >= *top && row < top + l.chars().count()).map(|s| s.1)
}

/// 1 行目の（欄の左端から数えた）`col` にあるタブ
pub fn rtab_at(col: usize) -> Option<RightTab> {
    RTABS.get(col / crate::tree_panel::TAB_W).map(|t| t.1)
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

/// 描き方（view.rs の Painter で描く）
pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    /// 右の欄（データベース・Maven）。1 行目はタブ、2 行目から木
    pub(crate) fn draw_right_panel(p: &mut Painter, ed: &Editor, r: Rect) {
        use crate::right_panel::RightTab;
        let cell = p.fonts.cell();
        let (x0, y0, wpx) = (r.x as f32 * cell.w, r.y as f32 * cell.h, r.w as f32 * cell.w);
        p.out.push(Instance::fill(x0, y0, wpx, r.h as f32 * cell.h, BG));
        p.out.push(Instance::fill(x0 - cell.w * 0.5, y0, 1.0, r.h as f32 * cell.h, LINE_NO));
        let focused = ed.right_focused();
        let tab = ed.right_visible().unwrap_or_default();
        // 左の欄と同じく、タブはいつも全部をアイコンで並べる
        let tabs: Vec<(TabIcon, &'static str, bool)> = crate::right_panel::RTABS
            .iter()
            .map(|(l, t)| {
                let icon = match t {
                    RightTab::Database => TabIcon::Database,
                    RightTab::Maven => TabIcon::Maven,
                    RightTab::Preview => TabIcon::Preview,
                };
                (icon, *l, *t == tab)
            })
            .collect();
        draw_icon_tabs(p, r, &tabs, focused, "");
        let saved = p.cols;
        p.cols = r.x + r.w;
        if tab == RightTab::Preview {
            if let Some(pv) = &ed.md_preview {
                draw_md_preview(p, pv, r, focused);
            }
            p.cols = saved;
            return;
        }
        // (字下げ, 開け閉めの印, 種類の印と色, 本文, 補足, 本文の色)
        let mut lines: Vec<(usize, &str, (char, [f32; 4]), String, String, [f32; 4])> = Vec::new();
        let mut top = 1;
        let (selected, scroll) = match tab {
            // 上で描いて戻っている
            RightTab::Preview => (0, 0),
            RightTab::Maven => {
                let Some(m) = &ed.maven else {
                    p.cols = saved;
                    return;
                };
                for row in &m.rows {
                    let (mark, color) = match &row.kind {
                        crate::maven_panel::RowKind::Folder(k) => (if m.is_open(k) { "▾ " } else { "▸ " }, if row.depth == 0 || k.starts_with("p:") { FG } else { FINDER_MATCH }),
                        crate::maven_panel::RowKind::Goal(..) => ("· ", FG),
                        crate::maven_panel::RowKind::Leaf => ("  ", STATUS_FG),
                    };
                    lines.push((row.depth, mark, (' ', FG), row.label.clone(), row.note.clone(), color));
                }
                (m.selected, m.scroll)
            }
            RightTab::Database => {
                let Some(d) = &ed.db else {
                    p.cols = saved;
                    return;
                };
                // 2 行目: 絞り込みの欄
                let fy = y0 + cell.h;
                let c = p.text(" / ", r.x, fy, LINE_NO);
                match &d.filter {
                    Some(f) if focused => {
                        draw_vim_field(p, f, c, fy, &ed.preedit);
                    }
                    _ if d.filter_text.is_empty() => {
                        p.text("テーブルや列を絞り込み", c, fy, LINE_NO);
                    }
                    _ => {
                        p.text(&d.filter_text, c, fy, FG);
                    }
                }
                p.out.push(Instance::fill(x0, fy + cell.h - 1.0, wpx, 1.0, rgb(0x2a2a2a)));
                top = 2;
                if d.sources.is_empty() {
                    p.text(" 接続先がありません。a（＋）で足す", r.x, y0 + 2.0 * cell.h, STATUS_FG);
                }
                for (k, row) in d.rows.iter().enumerate() {
                    use crate::db_panel::DbRow;
                    let (_, mark, text, note) = d.row_label(row);
                    let depth = d.depth(k);
                    let open = d.is_open(row);
                    let fold = if row.key().is_some() { if open { "▾ " } else { "▸ " } } else { "  " };
                    let icon = match mark {
                        'T' => ('T', GIT_ADD),
                        'V' => ('V', rgb(0xaf87d7)),
                        'S' | 'R' => (mark, STATUS_FG),
                        'K' => ('K', rgb(0xd7af5f)),
                        'F' => ('F', FINDER_MATCH),
                        'I' => ('I', STATUS_FG),
                        '●' => ('●', GIT_ADD),
                        '◌' => ('◌', GIT_MOD),
                        '○' => ('○', STATUS_FG),
                        _ => (' ', FG),
                    };
                    let color = match row {
                        DbRow::Source(i) if d.conns[*i].error.is_some() => DIAG_ERROR,
                        DbRow::Source(_) | DbRow::Schema(..) | DbRow::Column(..) => FG,
                        DbRow::Info(_, _, s) if s.starts_with("誤り") => DIAG_ERROR,
                        DbRow::Info(..) | DbRow::Group(..) | DbRow::Sub(..) | DbRow::Others(_) => STATUS_FG,
                        _ => FG,
                    };
                    lines.push((depth, fold, icon, text, note, color));
                }
                (d.selected, d.scroll)
            }
        };
        let body = r.h.saturating_sub(top + 1);
        for (k, (depth, fold, (icon, icolor), text, note, color)) in lines.iter().enumerate().skip(scroll).take(body) {
            let y = y0 + (top + k - scroll) as f32 * cell.h;
            let sel = k == selected;
            let reversed = sel && focused && ed.db.as_ref().is_none_or(|d| d.filter.is_none() || tab != RightTab::Database);
            if reversed {
                p.out.push(Instance::fill(x0, y, wpx, cell.h, FG));
            } else if sel {
                p.out.push(Instance::fill(x0, y, wpx, cell.h, MATCH_BG));
            }
            let mut c = p.text(&format!(" {}{fold}", "  ".repeat(*depth)), r.x, y, if reversed { BG } else { STATUS_FG });
            if *icon != ' ' {
                let mut b = [0u8; 4];
                c = p.text(icon.encode_utf8(&mut b), c, y, if reversed { BG } else { *icolor });
                c = p.text(" ", c, y, FG);
            }
            let c = p.text(text, c, y, if reversed { BG } else { *color });
            if !note.is_empty() {
                p.text(&format!("  {note}"), c, y, if reversed { BG } else { LINE_NO });
            }
        }
        // 下の行: パスワード・接続先のフォーム・ドライバーの準備
        if let (RightTab::Database, Some(d)) = (tab, &ed.db) {
            if let Some((i, prompt)) = &d.password {
                let y = y0 + (r.h - 1) as f32 * cell.h;
                p.out.push(Instance::fill(x0, y, wpx, cell.h, rgb(0x1c1c1c)));
                let user = &d.sources[*i].user;
                let c = p.text(&format!(" {user} のパスワード: "), r.x, y, STATUS_FG);
                let before = prompt.text[..prompt.cursor.min(prompt.text.len())].chars().count();
                let after = prompt.text.chars().count() - before;
                let x = p.text(&"●".repeat(before), c, y, FG);
                p.text(&"●".repeat(after), x, y, FG);
                p.caret(x, y);
            } else if let Some(i) = d.setup_ask {
                let lines = [
                    format!(" {} にはドライバーが要ります", d.sources[i].name),
                    format!(" Oracle Instant Client Basic Light（約 {} MB）", nagi_workers::drivers::ORACLE_CLIENT_SIZE_MB),
                    format!(" 置き場所: {}", nagi_workers::drivers::drivers_dir().join("oracle").display()),
                    format!(" {} に同意のうえ", nagi_workers::drivers::ORACLE_LICENSE),
                    " y でダウンロード ・ ほかのキーでやめる".to_string(),
                ];
                draw_panel_box(p, r, &lines, 0);
            } else if let Some(f) = &d.form {
                let mut lines = vec![" 接続先を足す".to_string()];
                // 見出しは表示の幅（12 セル）でそろえる
                let pad = |p: &Painter, l: &str| format!("{l}{}", " ".repeat(12usize.saturating_sub(p.width(l))));
                for (k, (label, prompt)) in f.fields.iter().enumerate() {
                    let v = if k == 0 { format!("‹ {} ›", prompt.text()) } else if k == f.at { String::new() } else { prompt.text() };
                    lines.push(format!(" {}{}  {v}", pad(p, label), if k == f.at { "▸" } else { " " }));
                }
                lines.push(" Tab 次の欄 ・ 最後の欄で Enter 保存してつなぐ ・ Esc やめる".to_string());
                let (bx, by) = draw_panel_box(p, r, &lines, f.at + 1);
                if f.at > 0 {
                    let label_w = 1 + 12.max(p.width(f.fields[f.at].0)) + p.width("▸  ");
                    draw_vim_field(p, &f.fields[f.at].1, bx + label_w, by + (f.at + 1) as f32 * cell.h, &ed.preedit);
                }
            }
        }
        p.cols = saved;
    }
}
