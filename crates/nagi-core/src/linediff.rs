//! 行の差分（Myers の O(ND) 法）。git の変更マーカー・差分の表示・変更の単位（hunk）に使う。
//!
//! 前後の同じ行を先に除いてから、残りだけを比べる（変更の少ない大きなファイルでも速い）。

use std::ops::Range;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HunkKind {
    /// 行を足した（old は空）
    Add,
    /// 行を消した（new は空。new.start の手前で消えた）
    Del,
    /// 行を変えた（両方ある）
    Mod,
}

/// 1 つの変更のまとまり（行の範囲。0 始まり、終わりは含まない）
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Hunk {
    pub old: Range<usize>,
    pub new: Range<usize>,
    pub kind: HunkKind,
}

fn hash(s: &str) -> u64 {
    // FNV-1a（行の比べ合わせを速くするだけ。一致したら文字列でも確かめる）
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// `old` から `new` への変更のまとまり
pub fn diff_lines(old: &[&str], new: &[&str]) -> Vec<Hunk> {
    // 前後の同じ行を除く
    let mut pre = 0;
    while pre < old.len() && pre < new.len() && old[pre] == new[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < old.len() - pre && suf < new.len() - pre && old[old.len() - 1 - suf] == new[new.len() - 1 - suf] {
        suf += 1;
    }
    let a: Vec<u64> = old[pre..old.len() - suf].iter().map(|s| hash(s)).collect();
    let b: Vec<u64> = new[pre..new.len() - suf].iter().map(|s| hash(s)).collect();
    let eq = |i: usize, j: usize| a[i] == b[j] && old[pre + i] == new[pre + j];
    // Myers: 編集の道筋を戻りながら、同じ行の対応 (i, j) を集める
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = (n + m) as usize;
    let off = max as isize;
    let mut v = vec![0isize; 2 * max + 2];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut found = n == 0 && m == 0;
    if !found {
        'outer: for d in 0..=max as isize {
            trace.push(v.clone());
            let mut k = -d;
            while k <= d {
                let mut x = if k == -d || (k != d && v[(off + k - 1) as usize] < v[(off + k + 1) as usize]) { v[(off + k + 1) as usize] } else { v[(off + k - 1) as usize] + 1 };
                let mut y = x - k;
                while x < n && y < m && eq(x as usize, y as usize) {
                    x += 1;
                    y += 1;
                }
                v[(off + k) as usize] = x;
                if x >= n && y >= m {
                    found = true;
                    break 'outer;
                }
                k += 2;
            }
        }
    }
    debug_assert!(found);
    // 道筋を後ろからたどって、対応する行の組を作る
    let mut pairs = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (0..trace.len() as isize).rev() {
        let v = &trace[d as usize];
        let k = x - y;
        let pk = if k == -d || (k != d && v[(off + k - 1) as usize] < v[(off + k + 1) as usize]) { k + 1 } else { k - 1 };
        let px = v[(off + pk) as usize];
        let py = px - pk;
        while x > px && y > py {
            x -= 1;
            y -= 1;
            pairs.push((x as usize, y as usize));
        }
        if d > 0 {
            x = px;
            y = py;
        }
    }
    while x > 0 && y > 0 {
        x -= 1;
        y -= 1;
        pairs.push((x as usize, y as usize));
    }
    pairs.reverse();
    // 対応の間が変更のまとまり
    let mut hunks = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    let mut push = |oi: usize, oe: usize, ni: usize, ne: usize| {
        if oi == oe && ni == ne {
            return;
        }
        let kind = if oi == oe {
            HunkKind::Add
        } else if ni == ne {
            HunkKind::Del
        } else {
            HunkKind::Mod
        };
        hunks.push(Hunk { old: pre + oi..pre + oe, new: pre + ni..pre + ne, kind });
    };
    for (pi, pj) in pairs {
        push(i, pi, j, pj);
        i = pi + 1;
        j = pj + 1;
    }
    push(i, a.len(), j, b.len());
    hunks
}

/// 文字列の行に分ける（末尾の改行の後ろの空の行は数えない。CRLF の \r も除く）
pub fn split_lines(s: &str) -> Vec<&str> {
    if s.is_empty() {
        return Vec::new();
    }
    let mut v: Vec<&str> = s.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    if s.ends_with('\n') {
        v.pop();
    }
    v
}

/// 2 つの行の、変わった部分（前後の同じ部分を除いた byte 範囲）。語の単位の強調に使う
pub fn changed_span(a: &str, b: &str) -> (Range<usize>, Range<usize>) {
    let pre = a.char_indices().zip(b.chars()).take_while(|((_, x), y)| x == y).last().map_or(0, |((i, c), _)| i + c.len_utf8());
    let pre = if a[..pre.min(a.len())] == b[..pre.min(b.len())] { pre } else { 0 };
    let ar = a[pre..].chars().rev();
    let br = b[pre..].chars().rev();
    let suf: usize = ar.zip(br).take_while(|(x, y)| x == y).map(|(x, _)| x.len_utf8()).sum();
    (pre..a.len() - suf, pre..b.len() - suf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(a: &str, b: &str) -> Vec<(Range<usize>, Range<usize>, HunkKind)> {
        diff_lines(&split_lines(a), &split_lines(b)).into_iter().map(|h| (h.old, h.new, h.kind)).collect()
    }

    #[test]
    fn kinds() {
        use HunkKind::*;
        assert_eq!(d("a\nb\nc\n", "a\nb\nc\n"), vec![]);
        assert_eq!(d("a\nc\n", "a\nb\nc\n"), vec![(1..1, 1..2, Add)]);
        assert_eq!(d("a\nb\nc\n", "a\nc\n"), vec![(1..2, 1..1, Del)]);
        assert_eq!(d("a\nb\nc\n", "a\nB\nc\n"), vec![(1..2, 1..2, Mod)]);
        // 離れた 2 か所
        assert_eq!(d("1\n2\n3\n4\n5\n6\n", "1\nx\n3\n4\n6\n"), vec![(1..2, 1..2, Mod), (4..5, 4..4, Del)]);
        // 先頭と末尾
        assert_eq!(d("a\nb\n", "z\na\nb\ny\n"), vec![(0..0, 0..1, Add), (2..2, 3..4, Add)]);
        assert_eq!(d("", "a\nb\n"), vec![(0..0, 0..2, Add)]);
        assert_eq!(d("a\nb\n", ""), vec![(0..2, 0..0, Del)]);
        // 改行コードの違いは見ない
        assert_eq!(d("a\r\nb\r\n", "a\nb\n"), vec![]);
    }

    #[test]
    fn many_lines_fast() {
        let a: String = (0..200_000).map(|i| format!("line {i}\n")).collect();
        let mut b = a.clone();
        b.insert_str(a.len() / 2, "new\n");
        let t = std::time::Instant::now();
        let h = d(&a, &b);
        assert_eq!(h.len(), 1);
        assert!(t.elapsed().as_millis() < 500, "{:?}", t.elapsed());
    }

    #[test]
    fn spans() {
        assert_eq!(changed_span("check(order);", "validator.check(order, 3);"), (0..11, 0..24));
        assert_eq!(changed_span("let x = 1;", "let x = 2;"), (8..9, 8..9));
        assert_eq!(changed_span("日本語", "日本人"), (6..9, 6..9));
    }
}
