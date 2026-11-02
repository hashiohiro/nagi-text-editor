//! 表のセルの範囲の選び方（DataGrip と同じ）。結果の表とテーブルの編集で共通。
//!
//! 選んでいるセル（行・列）と、範囲の起点（Shift を押して動かし始めた所）を持つ。Shift+矢印・Home・End・PageUp・PageDown で
//! 範囲を広げ、Ctrl+A で全部を選ぶ。修飾の無い移動・クリックで範囲を外す（呼ぶ側が起点を None にする）。

use nagi_vim::{Key, Mods, Nav};

/// 範囲（行の始め・終わり、列の始め・終わり。両端を含む）。起点が無ければ選んでいるセルだけ
pub fn rect(row: usize, col: usize, anchor: Option<(usize, usize)>) -> (usize, usize, usize, usize) {
    let (ar, ac) = anchor.unwrap_or((row, col));
    (ar.min(row), ar.max(row), ac.min(col), ac.max(col))
}

/// 範囲を広げるキーなら受けて true（`rows`・`cols` は表の大きさ、`page` は 1 画面の行数）
pub fn key(key: Key, row: &mut usize, col: &mut usize, anchor: &mut Option<(usize, usize)>, rows: usize, cols: usize, page: usize) -> bool {
    let (last_r, last_c) = (rows.saturating_sub(1), cols.saturating_sub(1));
    match key {
        Key::Ctrl('a') => {
            *anchor = Some((0, 0));
            (*row, *col) = (last_r, last_c);
            true
        }
        Key::Mod(nav, Mods { shift: true, ctrl: false, alt: false }) => {
            anchor.get_or_insert((*row, *col));
            match nav {
                Nav::Up => *row = row.saturating_sub(1),
                Nav::Down => *row = (*row + 1).min(last_r),
                Nav::Left => *col = col.saturating_sub(1),
                Nav::Right => *col = (*col + 1).min(last_c),
                Nav::Home => *col = 0,
                Nav::End => *col = last_c,
                Nav::PageUp => *row = row.saturating_sub(page.max(1)),
                Nav::PageDown => *row = (*row + page.max(1)).min(last_r),
                _ => return false,
            }
            true
        }
        _ => false,
    }
}

/// 範囲の値を、タブと改行で区切った文字に（表計算に貼れる形）
pub fn tsv(r: (usize, usize, usize, usize), value: impl Fn(usize, usize) -> Option<String>) -> String {
    (r.0..=r.1).map(|k| (r.2..=r.3).map(|c| value(k, c).unwrap_or_default()).collect::<Vec<_>>().join("\t")).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_and_copy() {
        let (mut row, mut col, mut anchor) = (1, 1, None);
        let shift = |n| Key::Mod(n, Mods { shift: true, ..Default::default() });
        assert!(key(shift(Nav::Down), &mut row, &mut col, &mut anchor, 5, 4, 10));
        assert!(key(shift(Nav::Right), &mut row, &mut col, &mut anchor, 5, 4, 10));
        assert_eq!(rect(row, col, anchor), (1, 2, 1, 2));
        // 起点の逆へ動いても範囲は起点から
        assert!(key(shift(Nav::Home), &mut row, &mut col, &mut anchor, 5, 4, 10));
        assert_eq!(rect(row, col, anchor), (1, 2, 0, 1));
        assert!(!key(Key::Down, &mut row, &mut col, &mut anchor, 5, 4, 10), "修飾の無い移動は呼ぶ側");
        assert!(key(Key::Ctrl('a'), &mut row, &mut col, &mut anchor, 5, 4, 10));
        assert_eq!(rect(row, col, anchor), (0, 4, 0, 3));
        let v = |r: usize, c: usize| (c != 1).then(|| format!("{r}{c}"));
        assert_eq!(tsv((0, 1, 0, 2), v), "00\t\t02\n10\t\t12");
    }
}
