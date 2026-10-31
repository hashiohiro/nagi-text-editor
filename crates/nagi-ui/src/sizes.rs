//! ドラッグで変える大きさの表（`Sizes`）と、その区切り（`Splitter`）。
//!
//! 欄の幅・高さや列の幅は、区切りごとにフィールドを持たず、名前（`left_width` など）→ 大きさ の 1 つの表に置く。
//! 覚える（設定の `state.名前`）・戻す（ダブルクリック）も表で行う。表に無い名前は「既定の大きさ」で、決め方は使う側が持つ。
//!
//! 区切りは、それを持つ部品が `Splitter` として返す（線の位置・範囲と、マウスの位置から大きさを出す式）。
//! 当たり判定とドラッグは、集めた区切りに対して 1 か所で行う。

use std::collections::BTreeMap;
use std::ops::Range;

/// ドラッグで変えた大きさ（名前 → セルの数）
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sizes(BTreeMap<String, usize>);

impl Sizes {
    /// 変えていれば、その大きさ
    pub fn get(&self, key: &str) -> Option<usize> {
        self.0.get(key).copied()
    }

    pub fn set(&mut self, key: &str, v: usize) {
        self.0.insert(key.to_string(), v);
    }

    /// 既定の大きさに戻す
    pub fn reset(&mut self, key: &str) {
        self.0.remove(key);
    }

    /// まだ変えていなければ入れる（覚えていた大きさを読み込むとき）
    pub fn set_default(&mut self, key: &str, v: usize) {
        self.0.entry(key.to_string()).or_insert(v);
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, usize)> {
        self.0.iter().map(|(k, v)| (k.as_str(), *v))
    }
}

/// 区切りを動かす向き
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// 縦の線を左右に動かす（列の数が変わる）
    Cols,
    /// 横の線を上下に動かす（行の数が変わる）
    Rows,
}

/// 区切り 1 本。`line` の列（Rows なら行）の、`span` の行（Rows なら列）の範囲をつかめる。
/// 大きさは `origin` から測る: `forward` なら マウスの位置 − origin − extra（左の欄の幅のように、線が大きさの終わり）、
/// でなければ origin − マウスの位置 − extra（右の欄の幅のように、線が大きさの始まりの手前）
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Splitter {
    /// Sizes の名前
    pub key: &'static str,
    pub axis: Axis,
    pub line: usize,
    pub span: Range<usize>,
    pub origin: usize,
    pub forward: bool,
    pub extra: usize,
}

impl Splitter {
    /// 画面の (行, 列) がこの区切りの上か
    pub fn hit(&self, row: usize, col: usize) -> bool {
        match self.axis {
            Axis::Cols => col == self.line && self.span.contains(&row),
            Axis::Rows => row == self.line && self.span.contains(&col),
        }
    }

    /// マウスが (行, 列) にあるときの大きさ
    pub fn size_at(&self, row: usize, col: usize) -> usize {
        let pos = match self.axis {
            Axis::Cols => col,
            Axis::Rows => row,
        };
        if self.forward { pos.saturating_sub(self.origin + self.extra) } else { self.origin.saturating_sub(pos + self.extra) }
    }

    /// 区切りをドラッグした: 大きさを表に入れる
    pub fn drag(&self, sizes: &mut Sizes, row: usize, col: usize) {
        sizes.set(self.key, self.size_at(row, col));
    }
}

/// 区切りの並び（上に描いたものから）で、(行, 列) にある最初のもの
pub fn hit(seps: &[Splitter], row: usize, col: usize) -> Option<&Splitter> {
    seps.iter().find(|s| s.hit(row, col))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_table() {
        let mut s = Sizes::default();
        assert_eq!(s.get("left_width"), None);
        s.set_default("left_width", 30);
        s.set_default("left_width", 40);
        assert_eq!(s.get("left_width"), Some(30), "覚えていた大きさは、変えた大きさを上書きしない");
        s.set("left_width", 50);
        s.reset("left_width");
        assert_eq!(s.get("left_width"), None);
    }

    #[test]
    fn splitter_sizes() {
        // 左の欄: 2 列目から始まり、線は 2 + 幅 の列
        let left = Splitter { key: "left_width", axis: Axis::Cols, line: 32, span: 1..40, origin: 2, forward: true, extra: 0 };
        assert!(left.hit(5, 32) && !left.hit(5, 31) && !left.hit(0, 32));
        assert_eq!(left.size_at(5, 40), 38);
        // 右の欄: 右端の次が 120、線は幅の手前の列
        let right = Splitter { key: "right_width", axis: Axis::Cols, line: 79, span: 1..40, origin: 119, forward: false, extra: 0 };
        assert_eq!(right.size_at(5, 79), 40);
        // 下の欄: 見出しの行をつかみ、画面の行の数から引く
        let bottom = Splitter { key: "bottom_height", axis: Axis::Rows, line: 30, span: 0..120, origin: 40, forward: false, extra: 0 };
        assert!(bottom.hit(30, 7));
        let mut s = Sizes::default();
        bottom.drag(&mut s, 25, 7);
        assert_eq!(s.get("bottom_height"), Some(15));
        assert_eq!(hit(&[left.clone(), bottom], 30, 7).map(|s| s.key), Some("bottom_height"));
        // 端を越えても 0 で止まる
        assert_eq!(left.size_at(0, 0), 0);
    }
}
