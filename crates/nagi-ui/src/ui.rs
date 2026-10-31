//! 割り付け（`Ui`）: 部品の中の矩形と、そこに置くもの（行・ボタン・欄）の対応。
//!
//! 部品は割り付けを Painter を使わない関数で 1 回だけ作り、描くときはその矩形に描き、押されたときは同じ割り付けで当たりを引く
//! （ADR 0018 の §1）。描き方と押した位置の計算を二重に持たないので、ずれない。Painter が要らないので試験でも作れる。
//! 座標は部品の中（部品の矩形の左上が 0, 0）。

/// セル単位の矩形
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Rect {
    pub fn contains(&self, row: usize, col: usize) -> bool {
        row >= self.y && row < self.y + self.h && col >= self.x && col < self.x + self.w
    }

    /// 1 行の矩形
    pub fn row(y: usize, x: usize, w: usize) -> Rect {
        Rect { x, y, w, h: 1 }
    }

    /// 部品の中の矩形 `inner` を、この矩形（画面の位置）の中に置いた画面の位置
    pub fn place(&self, inner: Rect) -> Rect {
        Rect { x: self.x + inner.x, y: self.y + inner.y, ..inner }
    }
}

/// 押した位置。`row`・`col` は部品の中（rect の左上が 0, 0）。`screen` は画面の位置で、メニューを出す所や、
/// 画面の位置で割り付けを覚えている部品（画面全体を持つ小窓・候補の窓）が使う
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct At {
    pub row: usize,
    pub col: usize,
    pub screen: (usize, usize),
}

impl At {
    /// 画面の (行, 列) を、矩形 `r` の中の位置に
    pub fn new(r: Rect, row: usize, col: usize) -> At {
        At { row: row - r.y, col: col - r.x, screen: (row, col) }
    }
}

/// 部品の割り付け: 矩形とそこに置くもの。重なるときは先に入れたものが当たる（行を先に、その下の空いた所を後に入れる）
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ui<T> {
    pub items: Vec<(Rect, T)>,
}

impl<T> Default for Ui<T> {
    fn default() -> Self {
        Ui { items: Vec::new() }
    }
}

impl<T> Ui<T> {
    pub fn put(&mut self, r: Rect, t: T) {
        if r.w > 0 && r.h > 0 {
            self.items.push((r, t));
        }
    }

    /// 部品の中の (行, 列) にあるものと、その矩形
    pub fn hit(&self, row: usize, col: usize) -> Option<(&T, Rect)> {
        self.items.iter().find(|(r, _)| r.contains(row, col)).map(|(r, t)| (t, *r))
    }

    pub fn iter(&self) -> impl Iterator<Item = (Rect, &T)> {
        self.items.iter().map(|(r, t)| (*r, t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_first_put_wins() {
        let mut ui = Ui::default();
        ui.put(Rect::row(1, 0, 10), "行");
        ui.put(Rect { x: 0, y: 1, w: 10, h: 5 }, "空き");
        ui.put(Rect::row(9, 0, 0), "幅 0 は入れない");
        assert_eq!(ui.hit(1, 3).map(|h| *h.0), Some("行"));
        assert_eq!(ui.hit(2, 3).map(|h| *h.0), Some("空き"));
        assert_eq!(ui.hit(9, 0), None);
        let r = Rect { x: 10, y: 5, w: 30, h: 20 };
        assert_eq!(r.place(Rect::row(1, 2, 4)), Rect { x: 12, y: 6, w: 4, h: 1 });
        assert_eq!(At::new(r, 7, 15), At { row: 2, col: 5, screen: (7, 15) });
    }
}
