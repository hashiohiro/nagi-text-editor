//! 一覧の選択とスクロール（パネル・ダイアログ・表で共通）。
//!
//! 一覧は「選んでいる行」と「先頭に見せている行」の 2 つの数を持つ。キーでの移動は `step`、
//! 描く前に選んでいる行を見える範囲に収めるのは `fit`。パネル独自のキー（Enter・o・g など）はパネルが先に見る。

use nagi_vim::Key;

/// 移動のキーなら、選び直した位置（`len` 行の中で。`page` は 1 画面の行数）。移動のキーでなければ None。
/// j k ↓ ↑・Ctrl+d Ctrl+u（半画面）・Ctrl+f Ctrl+b PageDown PageUp（1 画面）・G End（最後）・Home（最初）
pub fn step(key: Key, sel: usize, len: usize, page: usize) -> Option<usize> {
    let last = len.saturating_sub(1);
    let page = page.max(1);
    let half = (page / 2).max(1);
    Some(match key {
        Key::Char('j') | Key::Down => (sel + 1).min(last),
        Key::Char('k') | Key::Up => sel.saturating_sub(1),
        Key::Ctrl('d') => (sel + half).min(last),
        Key::Ctrl('u') => sel.saturating_sub(half),
        Key::Ctrl('f') | Key::PageDown => (sel + page).min(last),
        Key::Ctrl('b') | Key::PageUp => sel.saturating_sub(page),
        Key::Char('G') | Key::End => last,
        Key::Home => 0,
        _ => return None,
    })
}

/// 打って絞り込む一覧（入力欄があり j・k は文字になる）の移動: ↓ Ctrl+n Ctrl+j Tab・↑ Ctrl+p Ctrl+k
pub fn step_typing(key: Key, sel: usize, len: usize) -> Option<usize> {
    match key {
        Key::Down | Key::Ctrl('n') | Key::Ctrl('j') | Key::Tab => Some((sel + 1).min(len.saturating_sub(1))),
        Key::Up | Key::Ctrl('p') | Key::Ctrl('k') => Some(sel.saturating_sub(1)),
        _ => None,
    }
}

/// 押した行の一覧の番号（`row` は欄の中の行、上の `head` 行は見出し。見出しや一覧の外なら None）
pub fn row_at(scroll: usize, row: usize, head: usize, len: usize) -> Option<usize> {
    let i = scroll + row.checked_sub(head)?;
    (i < len).then_some(i)
}

/// 選べない行（見出し・グラフだけの行）を飛ばす: `from` から `to` へ動いた先が選べなければ、動いた向きに次の選べる行へ。
/// その向きに無ければ逆向きに、どちらにも無ければ `from` のまま
pub fn snap(from: usize, to: usize, len: usize, ok: impl Fn(usize) -> bool) -> usize {
    if len == 0 {
        return from;
    }
    let to = to.min(len - 1);
    let fwd = (to..len).find(|&i| ok(i));
    let back = (0..=to).rev().find(|&i| ok(i));
    let pick = if to >= from { fwd.or(back) } else { back.or(fwd) };
    pick.unwrap_or(from)
}

/// 選んでいる行 `sel` が、`h` 行の見える範囲に入るように先頭 `top` を送る
pub fn fit(sel: usize, top: &mut usize, h: usize) {
    let h = h.max(1);
    if sel < *top {
        *top = sel;
    } else if sel >= *top + h {
        *top = sel + 1 - h;
    }
}

/// ホイール: 先頭 `top` を `n` 行動かし（`len` 行のうち `h` 行が見える）、選んでいる行 `sel` は見える範囲に留める（本文の Vim と同じ）
pub fn wheel(sel: &mut usize, top: &mut usize, len: usize, h: usize, n: isize) {
    let h = h.max(1);
    let max = len.saturating_sub(h);
    *top = (*top as isize + n).clamp(0, max as isize) as usize;
    if len > 0 {
        *sel = (*sel).clamp(*top, (*top + h - 1).min(len - 1));
    }
}

/// 横のホイール: 左端の列 `left` を `n` 列動かす（`width` は中身の幅、`w` は見える幅）
pub fn hwheel(left: &mut usize, width: usize, w: usize, n: isize) {
    *left = (*left as isize + n).clamp(0, width.saturating_sub(w.max(1)) as isize) as usize;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_and_fit() {
        assert_eq!(step(Key::Char('j'), 9, 10, 4), Some(9), "最後で止まる");
        assert_eq!(step(Key::Ctrl('d'), 0, 10, 4), Some(2));
        assert_eq!(step(Key::PageDown, 0, 10, 4), Some(4));
        assert_eq!(step(Key::Char('G'), 0, 10, 4), Some(9));
        assert_eq!(step(Key::Char('j'), 0, 0, 4), Some(0), "空の一覧");
        assert_eq!(step(Key::Enter, 0, 10, 4), None);
        assert_eq!(step_typing(Key::Char('j'), 0, 10), None, "打つ一覧では j は文字");
        let mut top = 0;
        fit(7, &mut top, 5);
        assert_eq!(top, 3);
        fit(1, &mut top, 5);
        assert_eq!(top, 1);
    }

    #[test]
    fn wheels() {
        let (mut sel, mut top) = (0, 0);
        wheel(&mut sel, &mut top, 20, 5, 3);
        assert_eq!((sel, top), (3, 3), "選んでいる行は見える範囲の上端へ");
        wheel(&mut sel, &mut top, 20, 5, 100);
        assert_eq!((sel, top), (15, 15), "末尾が下端に来るまで");
        sel = 19;
        wheel(&mut sel, &mut top, 20, 5, -10);
        assert_eq!((sel, top), (9, 5), "上へ動かすと下端に留まる");
        wheel(&mut sel, &mut top, 3, 5, 2);
        assert_eq!((sel, top), (2, 0), "全部見えている一覧は動かない");
        let mut left = 0;
        hwheel(&mut left, 50, 20, 8);
        assert_eq!(left, 8);
        hwheel(&mut left, 50, 20, 100);
        assert_eq!(left, 30);
        hwheel(&mut left, 50, 20, -100);
        assert_eq!(left, 0);
    }

    #[test]
    fn rows_and_headers() {
        // 見出し 1 行・3 行目まで送った一覧
        assert_eq!(row_at(3, 0, 1, 10), None);
        assert_eq!(row_at(3, 1, 1, 10), Some(3));
        assert_eq!(row_at(3, 8, 1, 10), None);
        // 0 と 3 は見出し。下へは次の行へ、上の端の見出しでは動かない、末尾へは手前へ
        let ok = |i: usize| i != 0 && i != 3;
        assert_eq!(snap(2, 3, 6, ok), 4);
        assert_eq!(snap(1, 0, 6, ok), 1);
        assert_eq!(snap(4, 3, 6, ok), 2);
        // 読み込み中（行が無い）なら行を見ない
        let empty: Vec<bool> = Vec::new();
        assert_eq!(snap(0, 1, 0, |i| empty[i]), 0);
    }
}
