//! ファジーファインダーの採点（fzf の v1 に近い考え方）。
//!
//! 検索語の文字が候補の中に順に現れれば一致とし、次の点で加点・減点する。
//! - 区切り（`/` `\` `_` `-` `.` 空白）の直後や、小文字の後の大文字（camelCase）に当たると加点
//! - 続けて当たると加点し、間が空くと減点
//! - ファイル名の部分（最後の区切りより後ろ）に当たると加点
//!
//! 大文字小文字は区別しない。検索語の空白は区切りとして扱い、空白で分けた語がすべて一致するものを残す（fzf と同じ）。

const SCORE_MATCH: i32 = 16;
const BONUS_BOUNDARY: i32 = 10;
const BONUS_CAMEL: i32 = 8;
const BONUS_CONSECUTIVE: i32 = 6;
const BONUS_FIRST: i32 = 8;
const BONUS_BASENAME: i32 = 4;
const PENALTY_GAP_START: i32 = 3;
const PENALTY_GAP: i32 = 1;

/// 候補の 1 文字。ASCII のパスは byte のまま（文字の配列を作らずに）採点する
trait Ch: Copy + PartialEq {
    fn low(self) -> Self;
    fn sep(self) -> bool;
    fn upper(self) -> bool;
    fn lower_case(self) -> bool;
}

impl Ch for u8 {
    fn low(self) -> Self {
        self.to_ascii_lowercase()
    }
    fn sep(self) -> bool {
        matches!(self, b'/' | b'\\' | b'_' | b'-' | b'.' | b' ' | b':')
    }
    fn upper(self) -> bool {
        self.is_ascii_uppercase()
    }
    fn lower_case(self) -> bool {
        self.is_ascii_lowercase()
    }
}

impl Ch for char {
    fn low(self) -> Self {
        lower(self)
    }
    fn sep(self) -> bool {
        matches!(self, '/' | '\\' | '_' | '-' | '.' | ' ' | ':')
    }
    fn upper(self) -> bool {
        self.is_uppercase()
    }
    fn lower_case(self) -> bool {
        self.is_lowercase()
    }
}

fn lower(c: char) -> char {
    if c.is_ascii() { c.to_ascii_lowercase() } else { c.to_lowercase().next().unwrap_or(c) }
}

/// 1 語の採点。`hay` は候補の文字、`base` はファイル名の始まりの位置（文字単位）。
/// 一致すれば点数を返し、`positions` があれば当たった位置を足す
fn score_word<T: Ch>(hay: &[T], base: usize, needle: &[T], positions: Option<&mut Vec<usize>>) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    // 前から順に当てて、最後の文字が当たった位置を決める（当たらなければここで落ちる）
    let mut ni = 0;
    let mut end = None;
    for (i, &c) in hay.iter().enumerate() {
        if c.low() == needle[ni] {
            ni += 1;
            if ni == needle.len() {
                end = Some(i);
                break;
            }
        }
    }
    let end = end?;
    // 後ろから戻って、できるだけ短い範囲にする（fzf v1 と同じ）
    let mut ni = needle.len();
    let mut start = end;
    for i in (0..=end).rev() {
        if hay[i].low() == needle[ni - 1] {
            ni -= 1;
            if ni == 0 {
                start = i;
                break;
            }
        }
    }
    // その範囲の中で、前から当てながら採点する
    let mut score = 0;
    let mut ni = 0;
    let mut prev: Option<usize> = None;
    let mut out = positions;
    for i in start..=end {
        if ni == needle.len() {
            break;
        }
        if hay[i].low() != needle[ni] {
            continue;
        }
        let boundary = i == 0 || hay[i - 1].sep();
        let camel = i > 0 && hay[i].upper() && hay[i - 1].lower_case();
        let mut s = SCORE_MATCH;
        if boundary {
            s += BONUS_BOUNDARY;
        } else if camel {
            s += BONUS_CAMEL;
        }
        match prev {
            Some(p) if p + 1 == i => s += BONUS_CONSECUTIVE,
            Some(p) => s -= PENALTY_GAP_START + PENALTY_GAP * (i - p - 2) as i32,
            None if i == 0 => s += BONUS_FIRST,
            None => {}
        }
        if i >= base {
            s += BONUS_BASENAME;
        }
        score += s;
        prev = Some(i);
        if let Some(o) = out.as_deref_mut() {
            o.push(i);
        }
        ni += 1;
    }
    Some(score)
}

/// 検索語（空白で分けた語の並び。小文字にしたもの）
pub struct Query {
    words: Vec<Vec<char>>,
    /// すべての語が ASCII なら byte の版（ASCII の候補はこちらで採点する）
    ascii: Option<Vec<Vec<u8>>>,
}

impl Query {
    pub fn new(q: &str) -> Self {
        let words: Vec<Vec<char>> = q.split_whitespace().map(|w| w.chars().map(lower).collect()).collect();
        let ascii = q.is_ascii().then(|| words.iter().map(|w| w.iter().map(|&c| c as u8).collect()).collect());
        Self { words, ascii }
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// 候補 `s` の点数。一致しなければ None。短い候補をわずかに優先する
    pub fn score(&self, s: &str) -> Option<i32> {
        self.score_with(s, None)
    }

    /// 当たった位置（文字単位）も返す（画面に出す候補の強調用）
    pub fn positions(&self, s: &str) -> Vec<usize> {
        let mut pos = Vec::new();
        self.score_with(s, Some(&mut pos));
        pos.sort_unstable();
        pos.dedup();
        pos
    }

    fn score_with(&self, s: &str, mut pos: Option<&mut Vec<usize>>) -> Option<i32> {
        if self.words.is_empty() {
            return Some(0);
        }
        let mut total = 0;
        let len;
        if let (Some(words), true) = (&self.ascii, s.is_ascii()) {
            let hay = s.as_bytes();
            len = hay.len();
            let base = hay.iter().rposition(|&c| c == b'/' || c == b'\\').map_or(0, |i| i + 1);
            for w in words {
                total += score_word(hay, base, w, pos.as_deref_mut())?;
            }
        } else {
            let hay: Vec<char> = s.chars().collect();
            len = hay.len();
            let base = hay.iter().rposition(|&c| c == '/' || c == '\\').map_or(0, |i| i + 1);
            for w in &self.words {
                total += score_word(&hay, base, w, pos.as_deref_mut())?;
            }
        }
        Some(total - (len as i32 / 8))
    }
}

/// 候補を絞って点数の高い順に並べる。`(候補の添字, 点数)`
pub fn rank<'a>(q: &Query, items: impl Iterator<Item = (u32, &'a str)>) -> Vec<(u32, i32)> {
    let mut hits: Vec<(u32, i32)> = items.filter_map(|(i, s)| q.score(s).map(|sc| (i, sc))).collect();
    sort_hits(&mut hits);
    hits
}

/// これより多い候補は、スレッドに分けて採点する
const PAR_MIN: usize = 16_384;

/// `ids` の候補を採点して、当たったものを点数の高い順に返す。多いときは CPU の数だけスレッドに分ける
pub fn rank_ids<S: AsRef<str> + Sync>(q: &Query, items: &[S], ids: &[u32]) -> Vec<(u32, i32)> {
    let one = |ids: &[u32]| -> Vec<(u32, i32)> { ids.iter().filter_map(|&i| q.score(items[i as usize].as_ref()).map(|s| (i, s))).collect() };
    let mut hits = if ids.len() < PAR_MIN {
        one(ids)
    } else {
        let n = std::thread::available_parallelism().map_or(4, |n| n.get()).min(16);
        let chunk = ids.len().div_ceil(n);
        std::thread::scope(|sc| {
            let parts: Vec<_> = ids.chunks(chunk).map(|c| sc.spawn(move || one(c))).collect();
            parts.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
        })
    };
    sort_hits(&mut hits);
    hits
}

/// 点数の高い順。同点なら元の順（短い候補を先に並べておけば短い順）
pub fn sort_hits(hits: &mut [(u32, i32)]) {
    hits.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn best<'a>(q: &str, items: &[&'a str]) -> Vec<&'a str> {
        let q = Query::new(q);
        rank(&q, items.iter().enumerate().map(|(i, s)| (i as u32, *s))).into_iter().map(|(i, _)| items[i as usize]).collect()
    }

    #[test]
    fn subsequence_and_case() {
        let q = Query::new("EdRs");
        assert!(q.score("crates/nagi/src/editor.rs").is_some());
        assert!(q.score("crates/nagi/src/view.rs").is_none());
        assert!(Query::new("").score("x").is_some());
    }

    #[test]
    fn prefers_basename_and_boundaries() {
        let items = ["docs/editor-notes/readme.md", "crates/nagi/src/editor.rs", "src/ed/itor.rs"];
        assert_eq!(best("editor", &items)[0], "crates/nagi/src/editor.rs");
        // 区切りの直後の頭文字（main_view の m と v）
        let items = ["src/mv.rs.bak/zz", "src/main_view.rs", "crates/nagi-render/src/lib.rs"];
        assert_eq!(best("mv", &items)[0], "src/main_view.rs");
        let items = ["crates/nagi/src/panes.rs", "crates/nagi-core/src/textops.rs"];
        assert_eq!(best("pan", &items)[0], "crates/nagi/src/panes.rs");
    }

    #[test]
    fn words_all_match_and_positions() {
        let items = ["crates/nagi/src/view.rs", "crates/nagi-render/src/lib.rs", "docs/view.md"];
        assert_eq!(best("nagi view", &items), vec!["crates/nagi/src/view.rs"]);
        let q = Query::new("vrs");
        assert_eq!(q.positions("src/view.rs"), vec![4, 9, 10]);
        // 日本語も大文字小文字なしで当たる
        assert!(Query::new("設計").score("docs/設計メモ.md").is_some());
    }
}
