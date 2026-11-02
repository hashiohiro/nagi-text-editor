//! 右の欄（IntelliJ の右のツールウィンドウと同じ）。「データベース」と「Maven」のタブ。
//!
//! 左の欄と同じく、1 行目にタブ、右端に隠すボタン。両方開いていれば最後に選んだ方を出す。
//! 隠しても中身（つないだ DB、開いた木）は残す。幅は左の欄との間の線をドラッグして変える（resize.rs）。

use nagi_vim::Key;

use crate::editor::{Editor, Effect, State, Side};
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
/// 右の欄の上の行数（タブと見出し。見える範囲で使う）
pub const HEAD: usize = 2;

pub const RTABS: [(&str, RightTab); 3] = [("データベース", RightTab::Database), ("Maven", RightTab::Maven), ("プレビュー", RightTab::Preview)];

impl Editor {
    /// 中身のあるタブ
    fn right_open_tabs(&self) -> Vec<RightTab> {
        let mut v = Vec::new();
        if self.panels.db.is_some() {
            v.push(RightTab::Database);
        }
        if self.panels.maven.is_some() {
            v.push(RightTab::Maven);
        }
        if self.panels.md_preview.is_some() {
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

    /// 右の欄の幅（既定は 40 列。狭い画面では 1/3 まで。窓に 20 列は残す）
    pub fn right_width(&self) -> usize {
        let cols = self.area().w;
        let left = self.left_rect().map_or(0, |r| r.w + 1);
        // プレビューは広め（画面の 45%）
        let def = if self.right_visible() == Some(RightTab::Preview) { cols * 45 / 100 } else { 40.min(cols / 3) };
        let w = self.svc.sizes.get("right_width").unwrap_or(def.max(10));
        w.min(cols.saturating_sub(left + 21)).max(10.min(cols))
    }

    pub fn right_rect(&self) -> Option<Rect> {
        self.right_visible()?;
        let a = self.area();
        let w = self.right_width();
        Some(Rect { x: (a.x + a.w).saturating_sub(w), y: a.y, w, h: self.windows_rows().saturating_sub(a.y) })
    }

    /// 右の欄にいるか（リストモード）
    pub fn right_focused(&self) -> bool {
        self.state() == State::List && self.list_side == Side::Right
    }

    pub(crate) fn select_right_tab(&mut self, tab: RightTab) {
        match tab {
            RightTab::Database => self.open_db_panel(),
            RightTab::Maven => self.open_maven(),
            RightTab::Preview => {
                if self.panels.md_preview.is_none() {
                    self.toggle_md_preview();
                }
                if self.panels.md_preview.is_some() {
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
        self.set_state(State::List);
        self.pending_keys.clear();
        self.rows = self.text_rows(self.active_rect());
    }

    pub(crate) fn hide_right(&mut self) {
        self.right_hidden = true;
        if self.right_focused() {
            self.set_state(State::Normal);
        }
        self.rows = self.text_rows(self.active_rect());
    }

    /// 右の欄にいる間のキー（タブごとに振り分ける。Ctrl-w h / Esc で窓へ戻る）
    pub(crate) fn right_key(&mut self, key: Key) -> Effect {
        match self.right_visible() {
            Some(RightTab::Maven) => self.maven_key(key),
            Some(RightTab::Database) => self.db_key(key),
            Some(RightTab::Preview) => self.md_preview_key(key),
            None => {
                self.set_state(State::Normal);
                Effect::None
            }
        }
    }

    /// マウス: 右の欄の中の (行, 列) を押した（矩形の中の位置）
    pub(crate) fn right_click(&mut self, row: usize, col: usize, clicks: u32) {
        let _ = col;
        // 見出しを押したら、その欄へ移る（タブを替えるのは右のアイコンの列）
        if row == 0 {
            if let Some(tab) = self.right_visible() {
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


/// この部品の入口（component.rs）
pub struct RightComponent;

impl crate::component::Component for RightComponent {
    fn status(&self, ed: &crate::editor::Editor) -> Option<crate::component::Status> {
        match ed.right_visible() {
            Some(RightTab::Maven) => crate::component::Status::new("-- Maven --", "Enter 実行・開閉  ←→ 開く・閉じる  右クリック 操作  Esc 窓へ "),
            Some(RightTab::Preview) => crate::component::Status::new("-- プレビュー --", "↑↓ 送る  Enter リンクを開く  右クリック 操作  Esc 窓へ "),
            _ => crate::component::Status::new("-- データベース --", "Enter 接続・開閉  ＋ データソースの作成  右クリック 操作  打つと絞り込み  Esc 窓へ "),
        }
    }

    fn key(&self, ed: &mut crate::editor::Editor, key: nagi_vim::Key) -> crate::editor::Effect {
        ed.right_key(key)
    }

    fn text(&self, ed: &mut crate::editor::Editor, text: &str) {
        ed.db_insert(text);
    }

    fn scope(&self, ed: &crate::editor::Editor) -> Option<crate::ui_action::Scope> {
        use crate::ui_action::Scope;
        Some(match ed.right_visible()? {
            RightTab::Database => Scope::Database,
            RightTab::Maven => Scope::Maven,
            RightTab::Preview => Scope::Preview,
        })
    }
}


/// 右の欄（データベース・Maven・プレビュー）（layout.rs の View）
pub struct RightView;

/// データソースのフォームの枠の行（題・欄・接続のテスト・案内）。描き方と押した位置で共通
pub(crate) fn form_lines(f: &crate::db_panel::Form) -> Vec<String> {
    let mut lines = vec![if f.edit.is_some() { " データソースのプロパティ" } else { " データソースの作成" }.to_string()];
    // 見出しは部品の文字（幅がそろわない）なので、値と ▸ は見出しの最も広い幅の右に、列を決めて描く
    lines.extend(f.fields.iter().map(|(label, _)| format!(" {label}")));
    lines.push(String::new());
    lines.push(if f.edit.is_some() { " Tab 次の欄・Enter 保存・Esc やめる" } else { " Tab 次の欄・Enter 保存してつなぐ・Esc やめる" }.to_string());
    lines
}

/// データベースの見出しの ＋ の列（画面の列。右端の「隠す」の 1 列手前をあけて）
pub(crate) fn plus_col(r: crate::panes::Rect) -> usize {
    (r.x + r.w).saturating_sub(5)
}

impl crate::layout::View for RightView {
    fn rect(&self, ed: &crate::editor::Editor, s: crate::layout::Screen) -> Option<crate::panes::Rect> {
        let _ = s;
        ed.right_rect()
    }

    fn draw(&self, ed: &crate::editor::Editor, p: &mut crate::view::Painter, r: crate::panes::Rect, s: crate::layout::Screen) {
        let _ = (r, s);
        draw::draw_right_panel(p, ed, r);
    }

    fn click(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, at: crate::layout::At, clicks: u32) -> crate::layout::Click {
        // データソースのフォーム: 欄の行を押すとその欄へ、「接続のテスト」の行で確かめる
        if let Some(f) = ed.panels.db.as_ref().and_then(|p| p.form.as_ref()).filter(|_| ed.right_visible() == Some(RightTab::Database)) {
            let n = form_lines(f).len();
            let top = r.h.saturating_sub(n + 1);
            if at.row >= top && at.row < top + n {
                let k = at.row - top;
                let fields = f.fields.len();
                ed.focus_right(RightTab::Database);
                if k == fields + 1 {
                    ed.run_ui_action("db.testConnection");
                } else if (1..=fields).contains(&k) {
                    if let Some(f) = ed.panels.db.as_mut().and_then(|p| p.form.as_mut()) {
                        f.at = k - 1;
                        f.fields[f.at].1.reinsert();
                    }
                }
                return crate::layout::Click::Done;
            }
        }
        // データベースの見出しの ＋
        let c0 = plus_col(r) - r.x;
        if at.row == 0 && ed.right_visible() == Some(RightTab::Database) && (c0..c0 + 2).contains(&at.col) {
            ed.run_ui_action("db.addSource");
            return crate::layout::Click::Done;
        }
        ed.right_click(at.row, at.col, clicks);
        crate::layout::Click::Done
    }

    /// データベースの木の右クリック: 押した行（接続先・スキーマ・テーブル・列など）のメニュー

    fn wheel(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, at: crate::layout::At, dy: isize, dx: isize) -> bool {
        let _ = (at, dx);
        let h = r.h.saturating_sub(HEAD);
        match ed.right_visible() {
            Some(RightTab::Maven) => {
                if let Some(m) = &mut ed.panels.maven {
                    nagi_ui::list::wheel(&mut m.selected, &mut m.scroll, m.rows.len(), h, dy);
                }
            }
            Some(RightTab::Database) => {
                if let Some(d) = &mut ed.panels.db {
                    nagi_ui::list::wheel(&mut d.selected, &mut d.scroll, d.rows.len(), h, dy);
                }
            }
            Some(RightTab::Preview) => {
                if let Some(pv) = &mut ed.panels.md_preview {
                    // 手で送ったら、エディタのカーソルには付いていかない（欄で j / k を押したときと同じ）
                    pv.follow = false;
                    nagi_ui::list::wheel(&mut pv.selected, &mut pv.scroll, pv.lines.len(), r.h.saturating_sub(1), dy);
                }
            }
            None => {}
        }
        true
    }

    fn context(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, at: crate::layout::At) -> bool {
        let _ = r;
        // 押した行を選ぶだけ（1 回のクリックのように開け閉めしない）
        let row = at.row;
        if row == 0 {
            let Some(t) = ed.right_visible() else { return false };
            ed.tool_menu(crate::chrome::Tool::Right(t), at.screen.0, at.screen.1);
            return true;
        }
        let m = crate::menu::Menu::new(at.screen.0, at.screen.1);
        let m = match ed.right_visible() {
            // データベースは接続先・テーブルごとの詳しいメニュー（db_panel.rs）
            Some(RightTab::Database) => return ed.db_context(row, at.screen),
            Some(RightTab::Maven) => {
                let Some(mv) = &mut ed.panels.maven else { return false };
                let len = mv.rows.len();
                let on_row = nagi_ui::list::pick_row(&mut mv.selected, mv.scroll, row, 1, len, 1).is_some();
                ed.focus_right(RightTab::Maven);
                // 行の無い所は欄全体の操作だけ
                let m = if on_row { m.action(ed, "maven.run").sep() } else { m };
                m.action(ed, "maven.reload").sep().action(ed, "maven.close")
            }
            Some(RightTab::Preview) => {
                let Some(pv) = &mut ed.panels.md_preview else { return false };
                let len = pv.lines.len();
                let on_row = nagi_ui::list::pick_row(&mut pv.selected, pv.scroll, row, 1, len, 1).is_some();
                ed.focus_right(RightTab::Preview);
                let m = if on_row { m.action(ed, "preview.openLink").sep() } else { m };
                m.action(ed, "preview.close")
            }
            None => return false,
        };
        ed.open_menu(m);
        true
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
        p.out.push(Instance::fill(x0, y0, wpx, r.h as f32 * cell.h, PANEL_BG));
        p.out.push(Instance::fill(x0 - cell.w * 0.5, y0, 1.0, r.h as f32 * cell.h, LINE_NO));
        let focused = ed.right_focused();
        let tab = ed.right_visible().unwrap_or_default();
        // 見出しは出しているタブの名前（タブを替えるのは右のアイコンの列）
        let name = crate::right_panel::RTABS.iter().find(|t| t.1 == tab).map_or("", |t| t.0);
        draw_panel_title(p, r, name, focused, "");
        // データベース: 見出しの右に ＋（接続先を足す。IntelliJ のデータベースの欄と同じ）
        if tab == RightTab::Database {
            let c0 = super::plus_col(r);
            let saved = p.cols;
            p.cols = r.x + r.w;
            let ui = std::mem::replace(&mut p.ui, true);
            p.text("＋", c0, y0, if focused { FG_BRIGHT } else { FG });
            p.ui = ui;
            p.cols = saved;
            p.tab_hits.push((r.y, c0, c0 + 2, "データソースの作成…"));
        }
        let saved = p.cols;
        p.cols = r.x + r.w;
        if tab == RightTab::Preview {
            if let Some(pv) = &ed.panels.md_preview {
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
                let Some(m) = &ed.panels.maven else {
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
                let Some(d) = &ed.panels.db else {
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
                p.out.push(Instance::fill(x0, fy + cell.h - 1.0, wpx, 1.0, SEPARATOR));
                top = 2;
                if d.sources.is_empty() {
                    p.text(" 接続先がありません。ここか ＋ を押して足す", r.x, y0 + 2.0 * cell.h, STATUS_FG);
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
            let reversed = sel && focused && ed.panels.db.as_ref().is_none_or(|d| d.filter.is_none() || tab != RightTab::Database);
            if reversed {
                selection(p, x0, y, wpx, cell.h, LIST_SEL_BG);
            } else if sel {
                p.out.push(Instance::fill(x0, y, wpx, cell.h, MATCH_BG));
            }
            let mut c = p.text(&format!(" {}{fold}", "  ".repeat(*depth)), r.x, y, if reversed { FG_BRIGHT } else { STATUS_FG });
            if *icon != ' ' {
                let mut b = [0u8; 4];
                c = p.text(icon.encode_utf8(&mut b), c, y, if reversed { FG_BRIGHT } else { *icolor });
                c = p.text(" ", c, y, FG);
            }
            let c = p.text(text, c, y, if reversed { FG_BRIGHT } else { *color });
            if !note.is_empty() {
                p.text(&format!("  {note}"), c, y, if reversed { FG_BRIGHT } else { LINE_NO });
            }
        }
        // 下の行: パスワード・接続先のフォーム・ドライバーの準備
        if let (RightTab::Database, Some(d)) = (tab, &ed.panels.db) {
            if let Some((i, prompt)) = &d.password {
                let y = y0 + (r.h - 1) as f32 * cell.h;
                p.out.push(Instance::fill(x0, y, wpx, cell.h, RAISED_BG));
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
                ];
                draw_panel_box(p, r, &lines, 0);
            } else if let Some(f) = &d.form {
                let lines = super::form_lines(f);
                let (bx, by) = draw_panel_box(p, r, &lines, f.at + 1);
                // 値は枠の中で切る（長い URL）
                let saved_cols = p.cols;
                p.cols = (r.x + r.w).saturating_sub(1);
                let mark = bx + 2 + f.fields.iter().map(|(label, _)| p.width(&format!(" {label}"))).max().unwrap_or(0);
                let val = mark + p.width("▸") + 1;
                for (k, (_, field)) in f.fields.iter().enumerate() {
                    let y = by + (k + 1) as f32 * cell.h;
                    if k == f.at {
                        p.text("▸", mark, y, FG);
                    }
                    let label = f.fields[k].0;
                    if f.kind_row(k) {
                        p.text(&format!("‹ {} ›", field.text()), val, y, FG);
                    } else if label == "パスワード" {
                        // 伏せ字（打っている間は終わりに縦棒）
                        let x = p.text(&"●".repeat(field.text().chars().count()), val, y, FG);
                        if k == f.at {
                            p.caret(x, y);
                        }
                    } else if k == f.at {
                        draw_vim_field(p, field, val, y, &ed.preedit);
                    } else {
                        p.text(&field.text(), val, y, if k == 0 { STATUS_FG } else { FG });
                    }
                }
                // 接続のテスト（ボタンと結果）
                let y = by + (f.fields.len() + 1) as f32 * cell.h;
                let c = p.text(" 接続のテスト ", bx + 2, y, if f.kind == nagi_workers::db::Kind::Salesforce { LINE_NO } else { ACCENT_FG });
                let (msg, color) = match (&f.test, f.testing) {
                    (_, true) => ("つないでいます…".to_string(), STATUS_FG),
                    (Some(Ok(product)), _) => (format!("成功: {product}"), GIT_ADD),
                    (Some(Err(e)), _) => (format!("失敗: {e}"), DIAG_ERROR),
                    (None, _) => (String::new(), STATUS_FG),
                };
                p.text(&format!("  {msg}"), c, y, color);
                p.cols = saved_cols;
            }
        }
        p.cols = saved;
    }
}
