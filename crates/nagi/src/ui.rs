//! 画面の部品で共通のもの（ダイアログ・候補の窓）。
//!
//! マウスの当たり判定: 描くときに「どの行のどの列からどの列までに何を描いたか」を `Hits` に入れ直し、
//! 押したときにそれを引く（描く処理と押したときの処理で位置の計算を二重に持たない）。

use std::cell::RefCell;

/// 描いた場所 (行, 列の始め, 列の終わり, 何か)
pub struct Hits<T: Copy>(RefCell<Vec<(usize, usize, usize, T)>>);

impl<T: Copy> Default for Hits<T> {
    fn default() -> Self {
        Self(RefCell::new(Vec::new()))
    }
}

impl<T: Copy> Hits<T> {
    /// 描き直したものに入れ替える
    pub fn set(&self, v: Vec<(usize, usize, usize, T)>) {
        *self.0.borrow_mut() = v;
    }

    /// (行, 列) にあるもの
    pub fn at(&self, row: usize, col: usize) -> Option<T> {
        self.0.borrow().iter().find(|(r, c0, c1, _)| *r == row && col >= *c0 && col < *c1).map(|x| x.3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hits() {
        let h: Hits<char> = Hits::default();
        h.set(vec![(3, 10, 14, 'a'), (3, 14, 20, 'b')]);
        assert_eq!(h.at(3, 14), Some('b'));
        assert_eq!(h.at(3, 9), None);
        assert_eq!(h.at(4, 12), None);
    }
}
