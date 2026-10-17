//! 区切りをマウスでドラッグして大きさを変える。
//!
//! - 左の欄と窓の間の縦線（左の欄の幅）
//! - 窓の領域と下のパネルの間（パネルの見出しの行をつかむ。パネルの高さ）
//! - 左右に並べた窓の間の縦線、上下に並べた窓の間（上の窓の状態行。Vim と同じ）
//!
//! 線の上ではマウスのカーソルを ↔ / ↕ にする。ダブルクリックで元の大きさ（等分）に戻す。

use crate::editor::Editor;
use crate::panes::{Layout, Rect, split_sizes};

/// つかんだ区切り
#[derive(Clone, Debug, PartialEq)]
pub enum Sep {
    Left,
    Right,
    Bottom,
    /// 窓の並び（根からの道筋）の、`idx` 番目と次の子の間。`area` はその並びの矩形
    Split { path: Vec<usize>, idx: usize, vertical: bool, area: Rect },
}

impl Sep {
    /// 左右に動かす区切りか（カーソルの形）
    pub fn horizontal_drag(&self) -> bool {
        match self {
            Sep::Left | Sep::Right => true,
            Sep::Bottom => false,
            Sep::Split { vertical, .. } => *vertical,
        }
    }
}

/// 並びの中の区切りを集める（区切り, 線の位置, 線の範囲の始めと終わり）
fn collect(l: &Layout, area: Rect, path: &mut Vec<usize>, out: &mut Vec<(Sep, usize, usize, usize)>) {
    let Layout::Split { vertical, kids, weights } = l else { return };
    let n = kids.len();
    let (total, gap) = if *vertical { (area.w, 1) } else { (area.h, 0) };
    let sizes = split_sizes(total.saturating_sub(gap * (n - 1)), weights);
    let mut at = 0;
    for (i, k) in kids.iter().enumerate() {
        let size = sizes[i];
        let r = if *vertical { Rect { x: area.x + at, w: size, ..area } } else { Rect { y: area.y + at, h: size, ..area } };
        if i + 1 < n {
            let sep = Sep::Split { path: path.clone(), idx: i, vertical: *vertical, area };
            if *vertical {
                // 左右の間の 1 列
                out.push((sep, r.x + r.w, r.y, r.y + r.h));
            } else {
                // 上の窓の状態行
                out.push((sep, (r.y + r.h).saturating_sub(1), r.x, r.x + r.w));
            }
        }
        path.push(i);
        collect(k, r, path, out);
        path.pop();
        at += size + gap;
    }
}

fn node_mut<'a>(l: &'a mut Layout, path: &[usize]) -> Option<&'a mut Layout> {
    match path.split_first() {
        None => Some(l),
        Some((&i, rest)) => match l {
            Layout::Split { kids, .. } => node_mut(kids.get_mut(i)?, rest),
            Layout::Leaf(_) => None,
        },
    }
}

impl Editor {
    /// 左の欄の幅（ドラッグで変えていればその幅）
    pub fn left_width(&self) -> usize {
        let cols = self.screen.0;
        let w = self.left_w.unwrap_or_else(|| crate::tree_panel::tree_width(cols));
        // 右の欄が出ていればその分も除いて、窓に 20 列は残す
        let right = if self.right_visible().is_some() { self.right_w.unwrap_or(10).min(40) + 1 } else { 0 };
        w.clamp(10.min(cols), cols.saturating_sub(20 + right).max(10))
    }

    /// 下のパネルの高さ（ドラッグで変えていればその高さ）
    pub fn bottom_height(&self) -> usize {
        let rows = self.screen.1;
        let h = self.bottom_h.unwrap_or_else(|| crate::grep_panel::panel_height(rows));
        h.clamp(3.min(rows), rows.saturating_sub(4).max(1))
    }

    /// 画面の (行, 列) にある区切り
    pub fn sep_at(&self, row: usize, col: usize) -> Option<Sep> {
        if self.diff.is_some() || self.log.is_some() {
            return None;
        }
        if let Some(r) = self.left_rect() {
            if col == r.x + r.w && row < r.y + r.h {
                return Some(Sep::Left);
            }
        }
        if let Some(r) = self.right_rect() {
            if r.x > 0 && col == r.x - 1 && row < r.y + r.h {
                return Some(Sep::Right);
            }
        }
        if let Some(r) = self.panel_rect() {
            if row == r.y {
                return Some(Sep::Bottom);
            }
        }
        let ((x, cols), rows) = (self.windows_x(), self.windows_rows());
        let mut out = Vec::new();
        let top = self.windows_top();
        collect(&self.layout, Rect { x, y: top, w: cols, h: rows.saturating_sub(top) }, &mut Vec::new(), &mut out);
        out.into_iter()
            .find(|(sep, pos, a, b)| {
                let Sep::Split { vertical, .. } = sep else { return false };
                if *vertical { col == *pos && row >= *a && row < *b } else { row == *pos && col >= *a && col < *b }
            })
            .map(|s| s.0)
    }

    /// ドラッグ中: マウスの (行, 列) に区切りを合わせる
    pub(crate) fn drag_sep(&mut self, sep: &Sep, row: usize, col: usize) {
        match sep {
            Sep::Left => self.left_w = Some(col),
            Sep::Right => self.right_w = Some(self.screen.0.saturating_sub(col + 1)),
            Sep::Bottom => self.bottom_h = Some(self.screen.1.saturating_sub(row)),
            Sep::Split { path, idx, vertical, area } => {
                let Some(Layout::Split { kids, weights, .. }) = node_mut(&mut self.layout, path) else { return };
                let n = kids.len();
                let (total, gap, start, pos) = if *vertical { (area.w, 1, area.x, col) } else { (area.h, 0, area.y, row + 1) };
                let sizes = split_sizes(total.saturating_sub(gap * (n - 1)), weights);
                let begin = start + sizes[..*idx].iter().map(|s| s + gap).sum::<usize>();
                let pair = sizes[*idx] + sizes[*idx + 1];
                // どちらの窓も 3 セル（上下なら本文 2 行と状態行）は残す
                let min = 3.min(pair / 2);
                let first = pos.saturating_sub(begin).clamp(min, pair - min);
                // 比率はセルの数そのものにする（丸めずに思った位置へ）
                let mut w: Vec<f32> = sizes.iter().map(|&s| s as f32).collect();
                w[*idx] = first as f32;
                w[*idx + 1] = (pair - first) as f32;
                *weights = w;
            }
        }
        self.rows = self.text_rows(self.active_rect());
    }

    /// ダブルクリック: 元の大きさ（等分）に戻す
    pub(crate) fn reset_sep(&mut self, sep: &Sep) {
        match sep {
            Sep::Left => self.left_w = None,
            Sep::Right => self.right_w = None,
            Sep::Bottom => self.bottom_h = None,
            Sep::Split { path, .. } => {
                if let Some(Layout::Split { weights, .. }) = node_mut(&mut self.layout, path) {
                    *weights = vec![1.0; weights.len()];
                }
            }
        }
        self.rows = self.text_rows(self.active_rect());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    fn keys(e: &mut Editor, k: &str) {
        for key in nagi_vim::parse_keys(k, " ") {
            e.key(key);
        }
    }

    #[test]
    fn drag_split_and_left() {
        let mut e = Editor::new(Buffer::from_str("x\n"), None);
        e.set_screen(100, 40);
        keys(&mut e, ":vs<CR>");
        let w0: Vec<Rect> = e.window_rects().into_iter().map(|r| r.1).collect();
        assert_eq!((w0[0].w, w0[1].x), (50, 51), "区切りの 1 列を除いた 99 列を分ける");
        // 間の 1 列（50 列目）をつかんで 30 列目へ
        let sep = e.sep_at(5, 50).expect("左右の間の線");
        assert!(sep.horizontal_drag());
        e.drag_sep(&sep, 5, 30);
        let w1: Vec<Rect> = e.window_rects().into_iter().map(|r| r.1).collect();
        assert_eq!((w1[0].w, w1[1].x, w1[1].w), (30, 31, 69));
        assert_eq!(e.sep_at(5, 30), Some(Sep::Split { path: vec![], idx: 0, vertical: true, area: Rect { x: 0, y: 0, w: 100, h: 40 } }));
        // 端までは行かない
        e.drag_sep(&sep, 5, 0);
        assert_eq!(e.window_rects()[0].1.w, 3);
        // ダブルクリックで等分
        e.reset_sep(&sep);
        assert_eq!(e.window_rects()[0].1.w, 50);
        // 上下の窓: 上の窓の状態行（19 行目）をつかんで 9 行目へ
        keys(&mut e, ":sp<CR>");
        let r = e.active_rect();
        let sep = e.sep_at(r.y + r.h - 1, r.x + 2).expect("上の窓の状態行");
        assert!(!sep.horizontal_drag());
        e.drag_sep(&sep, 9, r.x + 2);
        let top = e.window_rects().into_iter().map(|r| r.1).find(|q| q.x == r.x && q.y == 0).unwrap();
        assert_eq!(top.h, 10);
        // 左の欄の幅
        e.open_tree();
        let w = e.left_width();
        assert_eq!(e.sep_at(3, w), Some(Sep::Left));
        e.drag_sep(&Sep::Left, 3, 50);
        assert_eq!((e.left_width(), e.windows_x()), (50, (51, 49)));
        e.drag_sep(&Sep::Left, 3, 95);
        assert_eq!(e.left_width(), 80, "窓に 20 列は残す");
        e.reset_sep(&Sep::Left);
        assert_eq!(e.left_width(), 33);
    }
}
