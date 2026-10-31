//! 区切りをマウスでドラッグして大きさを変える。
//!
//! - 左の欄と窓の間の縦線（左の欄の幅）
//! - 窓の領域と下のパネルの間（パネルの見出しの行をつかむ。パネルの高さ）
//! - 左右に並べた窓の間の縦線、上下に並べた窓の間（上の窓の状態行。Vim と同じ）
//! - Git の履歴の、左のブランチの列と真ん中の間・真ん中と右の中身の列の間の縦線（列の幅）、
//!   真ん中のコミットの題名と作者・作者と日時の境目（列の幅）
//!
//! 線の上ではマウスのカーソルを ↔ / ↕ にする。ダブルクリックで元の大きさ（等分）に戻す。
//!
//! 窓の並びの区切りのほかは、大きさを `Editor::sizes`（nagi_ui::Sizes）の名前で持ち、区切りは `nagi_ui::Splitter` で表す
//! （ADR 0018 の段 2）。区切りは持ち主が返す（欄は `frame_splitters`、履歴の列は log_view.rs の `log_splitters`）。
//! 大きさの名前（覚えるときは設定の `state.名前`。_width・_height で終わる）:
//! left_width・right_width・bottom_height・log_branch_width・log_detail_width・log_author_width・log_date_width

use nagi_ui::{Axis, Splitter};

use crate::editor::Editor;
use crate::panes::{Layout, Rect, split_sizes};

/// つかんだ区切り
#[derive(Clone, Debug, PartialEq)]
pub enum Sep {
    /// 大きさの表の区切り（欄・履歴の列）
    Size(Splitter),
    /// 窓の並び（根からの道筋）の、`idx` 番目と次の子の間。`area` はその並びの矩形
    Split { path: Vec<usize>, idx: usize, vertical: bool, area: Rect },
}

impl Sep {
    /// 左右に動かす区切りか（カーソルの形）
    pub fn horizontal_drag(&self) -> bool {
        match self {
            Sep::Size(s) => s.axis == Axis::Cols,
            Sep::Split { vertical, .. } => *vertical,
        }
    }

    /// 下の欄の見出しの区切りか（つかみつつ、従来どおり欄へも渡す）
    pub fn is_bottom(&self) -> bool {
        matches!(self, Sep::Size(s) if s.key == "bottom_height")
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
        let cols = self.area().w;
        let w = self.svc.sizes.get("left_width").unwrap_or_else(|| crate::tree_panel::tree_width(cols));
        // 右の欄が出ていればその分も除いて、窓に 20 列は残す
        let right = if self.right_visible().is_some() { self.svc.sizes.get("right_width").unwrap_or(10).min(40) + 1 } else { 0 };
        w.clamp(10.min(cols), cols.saturating_sub(20 + right).max(10))
    }

    /// 下のパネルの高さ（ドラッグで変えていればその高さ）
    pub fn bottom_height(&self) -> usize {
        let rows = self.screen.1;
        let h = self.svc.sizes.get("bottom_height").unwrap_or_else(|| crate::grep_panel::panel_height(rows));
        h.clamp(3.min(rows), rows.saturating_sub(4).max(1))
    }

    /// 欄（左・右・下）の区切り。全画面の履歴は欄を覆うので無い
    fn frame_splitters(&self) -> Vec<Splitter> {
        let mut out = Vec::new();
        if self.log_fullscreen() {
            return out;
        }
        let a = self.area();
        if let Some(r) = self.left_rect() {
            out.push(Splitter { key: "left_width", axis: Axis::Cols, line: r.x + r.w, span: r.y..r.y + r.h, origin: a.x, forward: true, extra: 0 });
        }
        if let Some(r) = self.right_rect().filter(|r| r.x > a.x) {
            out.push(Splitter { key: "right_width", axis: Axis::Cols, line: r.x - 1, span: r.y..r.y + r.h, origin: a.x + a.w, forward: false, extra: 1 });
        }
        if let Some(r) = self.panel_rect() {
            out.push(Splitter { key: "bottom_height", axis: Axis::Rows, line: r.y, span: 0..usize::MAX, origin: self.screen.1, forward: false, extra: 0 });
        }
        out
    }

    /// 画面の (行, 列) にある区切り（履歴の列、欄、窓の並びの順に当てる）
    pub fn sep_at(&self, row: usize, col: usize) -> Option<Sep> {
        let mut seps = self.log_splitters();
        seps.extend(self.frame_splitters());
        if let Some(s) = nagi_ui::hit(&seps, row, col) {
            return Some(Sep::Size(s.clone()));
        }
        if self.log_fullscreen() {
            return None;
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
            Sep::Size(s) => s.drag(&mut self.svc.sizes, row, col),
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
            Sep::Size(s) => self.svc.sizes.reset(s.key),
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
        let a = e.area();
        let w0: Vec<Rect> = e.window_rects().into_iter().map(|r| r.1).collect();
        let half = w0[0].w;
        assert_eq!((w0[0].x, w0[1].x, w0[0].w + w0[1].w + 1), (a.x, a.x + half + 1, a.w), "区切りの 1 列を除いた枠の内側を分ける");
        // 間の 1 列をつかんで、枠の左端から 30 列目へ
        let sep = e.sep_at(5, a.x + half).expect("左右の間の線");
        assert!(sep.horizontal_drag());
        e.drag_sep(&sep, 5, a.x + 30);
        let w1: Vec<Rect> = e.window_rects().into_iter().map(|r| r.1).collect();
        assert_eq!((w1[0].w, w1[1].x, w1[1].w), (30, a.x + 31, a.w - 31));
        assert_eq!(e.sep_at(5, a.x + 30), Some(Sep::Split { path: vec![], idx: 0, vertical: true, area: Rect { x: a.x, y: a.y, w: a.w, h: a.h } }));
        // 端までは行かない
        e.drag_sep(&sep, 5, 0);
        assert_eq!(e.window_rects()[0].1.w, 3);
        // ダブルクリックで等分
        e.reset_sep(&sep);
        assert_eq!(e.window_rects()[0].1.w, half);
        // 上下の窓: 上の窓の状態行（19 行目）をつかんで 9 行目へ
        keys(&mut e, ":sp<CR>");
        let r = e.active_rect();
        let sep = e.sep_at(r.y + r.h - 1, r.x + 2).expect("上の窓の状態行");
        assert!(!sep.horizontal_drag());
        e.drag_sep(&sep, 9, r.x + 2);
        let top = e.window_rects().into_iter().map(|r| r.1).find(|q| q.x == r.x && q.y == a.y).unwrap();
        assert_eq!(top.h, 10 - a.y);
        // 左の欄の幅
        e.open_tree();
        let w = e.left_width();
        let left = e.sep_at(3, a.x + w).expect("左の欄の右の線");
        assert!(matches!(&left, Sep::Size(s) if s.key == "left_width") && left.horizontal_drag());
        e.drag_sep(&left, 3, a.x + 50);
        assert_eq!((e.left_width(), e.windows_x()), (50, (a.x + 51, a.w - 51)));
        e.drag_sep(&left, 3, 95);
        assert_eq!(e.left_width(), a.w - 20, "窓に 20 列は残す");
        e.reset_sep(&left);
        assert_eq!(e.left_width(), crate::tree_panel::tree_width(a.w));
        // 下の欄の高さ（見出しの行をつかむ）と、覚える・読み込む（設定の state.名前）
        e.toggle_bottom_tab(crate::result_grid::BottomKind::Problems);
        let p = e.panel_rect().unwrap();
        let bottom = e.sep_at(p.y, p.x + 3).expect("下の欄の見出し");
        assert!(bottom.is_bottom() && !bottom.horizontal_drag());
        e.drag_sep(&bottom, p.y - 2, p.x + 3);
        assert_eq!(e.bottom_height(), p.h + 2);
        e.drag_sep(&left, 3, a.x + 30);
        let saved: Vec<(String, usize)> = e.svc.sizes.iter().map(|(k, v)| (k.to_string(), v)).collect();
        assert_eq!(saved, [("bottom_height".to_string(), p.h + 2), ("left_width".to_string(), 30)]);
    }
}
