//! 本文の行を描いた結果（Instance の並び）を覚えて使い回す。
//!
//! カーソルを動かしたり 1 行送ったりしても、ほとんどの行は前のフレームと同じ文字・同じ色で描く。
//! 1 文字ずつ色・幅を決めて字形を引く手間（1 フレームで本文の組み立ての 2 割ほど）を、変わらない行では省く。
//! 覚えるのは飾りのない行（カーソル・一致・選択・置換の予告・ヤンクの光りがかからない、折り返さない行）だけ。
//! 鍵には描いた結果を決めるものをすべて入れる（文字・色分けの範囲・始まりの列・右端・横の送り・幅の設定・セル・アトラスの版）。
//! 縦の位置だけは鍵に入れず、行の上端からの位置で覚えて描くときに足す（送っても同じ行は同じ覚えを使う）。

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use nagi_render::Instance;

/// 行の文字と色分け以外で、描いた結果を決めるもの
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct Frame {
    /// 本文の始まりの列・描いてよい右端の列・横の送り
    pub x0: usize,
    pub cols: usize,
    pub left: usize,
    /// タブの表示幅・曖昧な幅を全角にするか・空白を見せる設定（list・全角の空白）
    pub tab: usize,
    pub wide: bool,
    pub show_ws: String,
    pub show_zen: bool,
    /// セルの大きさ（f32 のビット）とアトラスの版（字形の位置が変わったら覚えは使えない）
    pub cell: (u32, u32),
    pub atlas: u64,
}

type Spans = Vec<(usize, usize, nagi_syntax::Hl)>;

struct Line {
    /// 鍵（ハッシュが同じでも、中身を見比べてから使う）: 描いた設定・行の文字・その行にかかる色分け（行の頭からの位置と種類）
    frame: Frame,
    src: String,
    spans: Spans,
    /// y は行の上端からの位置
    out: Vec<Instance>,
    /// `--replay` の試験に控える文字
    text: String,
}

impl Line {
    fn is(&self, frame: &Frame, src: &str, spans: impl Iterator<Item = (usize, usize, nagi_syntax::Hl)>) -> bool {
        self.src == src && self.frame == *frame && self.spans.iter().copied().eq(spans)
    }
}

/// 今のフレームで使った分と、1 つ前のフレームで使った分だけを持つ（それより前のものは捨てる）。鍵のハッシュで引く
#[derive(Default)]
pub(crate) struct LineCache {
    cur: HashMap<u64, Line>,
    prev: HashMap<u64, Line>,
}

impl LineCache {
    /// フレームの始まりに呼ぶ
    pub fn next_frame(&mut self) {
        self.prev = std::mem::take(&mut self.cur);
    }

    /// 覚えがあれば `out` に y をずらして足し、控える文字（`--replay` の試験）を返す。
    /// 引くときは鍵を写さない（1 フレームで見える行の数だけ引く。写すのは覚えが無かった行だけ）
    pub fn draw<I>(&mut self, frame: &Frame, src: &str, spans: I, y: f32, out: &mut Vec<Instance>) -> Result<&str, PendingKey>
    where
        I: Iterator<Item = (usize, usize, nagi_syntax::Hl)> + Clone,
    {
        let mut h = std::hash::DefaultHasher::new();
        frame.hash(&mut h);
        src.hash(&mut h);
        spans.clone().for_each(|s| s.hash(&mut h));
        let hash = h.finish();
        if !self.cur.get(&hash).is_some_and(|l| l.is(frame, src, spans.clone())) {
            match self.prev.remove(&hash) {
                Some(l) if l.is(frame, src, spans.clone()) => {
                    self.cur.insert(hash, l);
                }
                _ => return Err(PendingKey { hash, frame: frame.clone(), src: src.to_string(), spans: spans.collect() }),
            }
        }
        let l = &self.cur[&hash];
        out.extend(l.out.iter().map(|i| Instance { rect: [i.rect[0], i.rect[1] + y, i.rect[2], i.rect[3]], ..*i }));
        Ok(&l.text)
    }

    /// 覚えが無かった行を描いた結果（`out` は今描いた分。y はその行の上端）を覚える
    pub fn store(&mut self, key: PendingKey, out: &[Instance], y: f32, text: String) {
        let out = out.iter().map(|i| Instance { rect: [i.rect[0], i.rect[1] - y, i.rect[2], i.rect[3]], ..*i }).collect();
        self.cur.insert(key.hash, Line { frame: key.frame, src: key.src, spans: key.spans, out, text });
    }
}

/// 覚えが無かった行の鍵（描いてから store に渡す）
pub(crate) struct PendingKey {
    hash: u64,
    frame: Frame,
    src: String,
    spans: Spans,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> Frame {
        Frame { x0: 7, cols: 80, left: 0, tab: 4, wide: false, show_ws: "off".into(), show_zen: true, cell: (8, 16), atlas: 0 }
    }

    fn glyph(x: f32, y: f32) -> Instance {
        Instance { rect: [x, y, 7.0, 12.0], uv: [1.0, 2.0, 7.0, 12.0], color: [1.0; 4], kind: 1 }
    }

    #[test]
    fn reuse_at_another_row() {
        let mut c = LineCache::default();
        let mut out = Vec::new();
        let Err(k) = c.draw(&frame(), "ab", std::iter::empty(), 32.0, &mut out) else { panic!("初めは覚えが無い") };
        c.store(k, &[glyph(56.0, 34.0), glyph(64.0, 35.0)], 32.0, "ab".into());
        // 次のフレームで 1 行上に送った: 同じ行は覚えを使い、y だけ変わる
        c.next_frame();
        let text = c.draw(&frame(), "ab", std::iter::empty(), 16.0, &mut out).ok().expect("覚えを使う");
        assert_eq!(text, "ab");
        assert_eq!(out.iter().map(|i| (i.rect[0], i.rect[1])).collect::<Vec<_>>(), [(56.0, 18.0), (64.0, 19.0)]);
    }

    #[test]
    fn key_covers_text_spans_and_frame() {
        let mut c = LineCache::default();
        let mut out = Vec::new();
        let Err(k) = c.draw(&frame(), "ab", std::iter::empty(), 0.0, &mut out) else { panic!() };
        c.store(k, &[glyph(0.0, 0.0)], 0.0, "ab".into());
        assert!(c.draw(&frame(), "ac", std::iter::empty(), 0.0, &mut out).is_err(), "文字が違う");
        assert!(c.draw(&frame(), "ab", [(0, 1, nagi_syntax::Hl::Keyword)].into_iter(), 0.0, &mut out).is_err(), "色が違う");
        assert!(c.draw(&Frame { atlas: 1, ..frame() }, "ab", std::iter::empty(), 0.0, &mut out).is_err(), "アトラスを作り直した");
        assert!(c.draw(&Frame { left: 1, ..frame() }, "ab", std::iter::empty(), 0.0, &mut out).is_err(), "横に送った");
        assert!(c.draw(&frame(), "ab", std::iter::empty(), 0.0, &mut out).is_ok());
    }

    #[test]
    fn drops_lines_unused_for_a_frame() {
        let mut c = LineCache::default();
        let mut out = Vec::new();
        let Err(k) = c.draw(&frame(), "ab", std::iter::empty(), 0.0, &mut out) else { panic!() };
        c.store(k, &[glyph(0.0, 0.0)], 0.0, "ab".into());
        c.next_frame();
        c.next_frame();
        assert!(c.draw(&frame(), "ab", std::iter::empty(), 0.0, &mut out).is_err(), "2 フレーム使わなければ捨てる");
    }
}
