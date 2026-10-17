//! クエリの結果の表（下のパネル。全文検索の結果と入れ替えて出す）。
//!
//! 見出しの行に「件数・時間・続きがあるか」、その下に列名と行。j/k で行、h/l で列を選び、y でセル、Y で行
//! （タブ区切り）、gy で表全体（タブ区切り。Excel などに貼れる）をコピーする。NULL は薄い `<null>`。

use nagi_vim::Key;

use crate::editor::{Editor, Mode, Side};

/// 下のパネルに出しているもの
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BottomKind {
    #[default]
    Grep,
    Result,
}

pub struct ResultGrid {
    pub title: String,
    pub cols: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    /// 列の幅（セル）
    pub widths: Vec<usize>,
    /// 件数などの補足（「120 行 · 35 ms」「3 行を更新」「誤り: …」）
    pub info: String,
    pub error: bool,
    pub running: bool,
    pub row: usize,
    pub col: usize,
    pub scroll: usize,
    /// 左に隠した列の数
    pub hscroll: usize,
    pending_g: bool,
}

/// 表に出す 1 つの値（改行とタブは記号に）
pub fn show(v: &Option<String>) -> String {
    match v {
        None => "<null>".into(),
        Some(s) => s.replace('\n', "↵").replace('\r', "").replace('\t', "→"),
    }
}

impl ResultGrid {
    pub fn running(title: &str) -> Self {
        ResultGrid {
            title: title.into(),
            cols: Vec::new(),
            rows: Vec::new(),
            widths: Vec::new(),
            info: "実行しています…".into(),
            error: false,
            running: true,
            row: 0,
            col: 0,
            scroll: 0,
            hscroll: 0,
            pending_g: false,
        }
    }

    pub fn set_rows(&mut self, cols: Vec<String>, rows: Vec<Vec<Option<String>>>, more: bool, ms: u64) {
        let w = |s: &str| nagi_core::display_col(s, s.len(), false);
        self.widths = (0..cols.len()).map(|c| rows.iter().take(300).map(|r| w(&show(&r[c]))).chain([w(&cols[c])]).max().unwrap_or(4).clamp(4, 40)).collect();
        self.info = format!("{} 行{} · {ms} ms", rows.len(), if more { "（続きあり）" } else { "" });
        self.cols = cols;
        self.rows = rows;
        self.running = false;
        self.error = false;
        self.row = 0;
        self.col = 0;
        self.scroll = 0;
        self.hscroll = 0;
    }

    pub fn set_info(&mut self, info: String, error: bool) {
        self.info = info;
        self.error = error;
        self.running = false;
        self.cols.clear();
        self.rows.clear();
        self.widths.clear();
    }

    /// 表全体をタブ区切りに（1 行目は列名）
    pub fn to_tsv(&self) -> String {
        let mut s = self.cols.join("\t");
        for r in &self.rows {
            s.push('\n');
            s.push_str(&r.iter().map(|v| v.clone().unwrap_or_default()).collect::<Vec<_>>().join("\t"));
        }
        s
    }
}

impl Editor {
    /// クエリの結果のパネルを開く（全文検索の結果とは入れ替え）
    pub(crate) fn show_result(&mut self, grid: ResultGrid) {
        self.db_result = Some(grid);
        self.bottom_kind = BottomKind::Result;
        self.bottom_hidden = false;
        self.rows = self.text_rows(self.active_rect());
    }

    fn copy_text(&mut self, s: String, what: &str) {
        if let Some(cb) = &self.clipboard {
            (cb.set)(&s);
        }
        let n = s.lines().count();
        self.registers.insert('"', (s, false));
        self.message = format!("{what}をコピーしました{}", if n > 1 { format!("（{n} 行）") } else { String::new() });
    }

    /// 結果の表にいる間のキー
    pub(crate) fn result_key(&mut self, key: Key) -> crate::editor::Effect {
        let half = (self.panel_rect().map_or(10, |r| r.h) / 2).max(1);
        let Some(g) = &mut self.db_result else {
            self.mode = Mode::Normal;
            return crate::editor::Effect::None;
        };
        let last_row = g.rows.len().saturating_sub(1);
        let last_col = g.cols.len().saturating_sub(1);
        let pending = std::mem::take(&mut g.pending_g);
        match key {
            Key::Char('j') | Key::Down => g.row = (g.row + 1).min(last_row),
            Key::Char('k') | Key::Up => g.row = g.row.saturating_sub(1),
            Key::Char('l') | Key::Right | Key::Tab => g.col = (g.col + 1).min(last_col),
            Key::Char('h') | Key::Left => g.col = g.col.saturating_sub(1),
            Key::Char('0') | Key::Home => g.col = 0,
            Key::Char('$') | Key::End => g.col = last_col,
            Key::Ctrl('d') => g.row = (g.row + half).min(last_row),
            Key::Ctrl('u') => g.row = g.row.saturating_sub(half),
            Key::Char('G') => g.row = last_row,
            Key::Char('g') if pending => g.row = 0,
            Key::Char('y') if pending => {
                let s = g.to_tsv();
                self.copy_text(s, "表");
            }
            Key::Char('g') => g.pending_g = true,
            Key::Char('y') => {
                if let Some(v) = g.rows.get(g.row).and_then(|r| r.get(g.col)) {
                    let s = v.clone().unwrap_or_default();
                    self.copy_text(s, "値");
                }
            }
            Key::Char('Y') => {
                if let Some(r) = g.rows.get(g.row) {
                    let s = r.iter().map(|v| v.clone().unwrap_or_default()).collect::<Vec<_>>().join("\t");
                    self.copy_text(s, "行");
                }
            }
            Key::Char('q') => {
                self.db_result = None;
                self.bottom_kind = BottomKind::Grep;
                self.mode = Mode::Normal;
                self.rows = self.text_rows(self.active_rect());
            }
            Key::Esc | Key::Ctrl('w') => self.mode = Mode::Normal,
            Key::Char(':') => {
                self.mode = Mode::Command;
                self.cmdline = Default::default();
            }
            _ => {}
        }
        crate::editor::Effect::None
    }

    /// 結果の表のクリック（`row` はパネルの中の行。0 は見出し、1 は列名）
    pub(crate) fn result_click(&mut self, row: usize, col: usize) {
        self.list_side = Side::Bottom;
        self.mode = Mode::List;
        let Some(g) = &mut self.db_result else { return };
        if row < 2 {
            return;
        }
        let i = g.scroll + row - 2;
        if i < g.rows.len() {
            g.row = i;
        }
        // 列: 左から幅を足して探す
        let mut x = 0;
        for c in g.hscroll..g.cols.len() {
            let w = g.widths[c] + 3;
            if col < x + w {
                g.col = c;
                break;
            }
            x += w;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid() {
        let mut g = ResultGrid::running("t");
        g.set_rows(vec!["ID".into(), "NAME".into()], vec![vec![Some("1".into()), None], vec![Some("2".into()), Some("a\tb".into())]], true, 12);
        assert_eq!(g.info, "2 行（続きあり） · 12 ms");
        assert_eq!(g.widths, vec![4, 6]);
        assert_eq!(g.to_tsv(), "ID\tNAME\n1\t\n2\ta\tb");
        assert_eq!(show(&None), "<null>");
    }
}
