//! 画面の組み立て: 行番号・本文・カーソル・最下行をインスタンス列にする。
//!
//! 1 行は「書記素ごとの部品（文字・文字色・背景色・下線）」の並びとして組み立ててから描く。
//! 選択範囲・検索の一致・置換の予告表示・カーソル・IME の変換中の文字列は、どれもこの部品の差し替えで表す。

use std::borrow::Cow;

use nagi_render::{Instance, Renderer};
use nagi_text::Fonts;
use unicode_segmentation::UnicodeSegmentation;

use crate::editor::{Editor, GUTTER, Mode};
use crate::panes::Rect;

const fn rgb(hex: u32) -> [f32; 4] {
    [((hex >> 16) & 0xff) as f32 / 255.0, ((hex >> 8) & 0xff) as f32 / 255.0, (hex & 0xff) as f32 / 255.0, 1.0]
}

// 見た目はターミナルの Vim と同じにする（architecture.md §9 の原則）。地は単色、強調は反転だけ。
pub const BG: [f32; 4] = rgb(0x0c0c0c);
const FG: [f32; 4] = rgb(0xcccccc);
const LINE_NO: [f32; 4] = rgb(0x4e4e4e);
const TILDE: [f32; 4] = rgb(0x3a3a3a);
const CURSOR: [f32; 4] = rgb(0xcccccc);
/// Visual で選んだ行末（空行など）にあるカーソル。選択の反転とも地の色とも見分けられる灰色
const CURSOR_IN_SELECTION: [f32; 4] = rgb(0x6c6c6c);
const STATUS_FG: [f32; 4] = rgb(0x8a8a8a);
/// git の変更マーカー（足した行・変えた行・消した位置）と、差分の地の色
const GIT_ADD: [f32; 4] = rgb(0x87af87);
const GIT_MOD: [f32; 4] = rgb(0x87afd7);
const GIT_DEL: [f32; 4] = rgb(0xd75f5f);
const DIFF_ADD_BG: [f32; 4] = rgb(0x16261a);
const DIFF_DEL_BG: [f32; 4] = rgb(0x2c1a1a);
const DIFF_WORD_BG: [f32; 4] = rgb(0x2c4a70);
const DIFF_WORD_DEL_BG: [f32; 4] = rgb(0x6a2a2a);
/// 診断のある行の行番号（エラーと警告）
const DIAG_ERROR: [f32; 4] = rgb(0xd75f5f);
const DIAG_WARN: [f32; 4] = rgb(0xd7af5f);
/// 検索の一致（今いるもの以外）の背景
const MATCH_BG: [f32; 4] = rgb(0x3a3a3a);

/// ハイライトの色（ターミナルの 256 色に近い、控えめな数色）
fn hl_color(h: nagi_syntax::Hl) -> [f32; 4] {
    use nagi_syntax::Hl;
    match h {
        Hl::Keyword => rgb(0x87afd7),
        Hl::String => rgb(0xafaf87),
        Hl::Comment => rgb(0x6c6c6c),
        Hl::Type | Hl::Attribute => rgb(0xd7af87),
        Hl::Number | Hl::Escape => rgb(0xd7875f),
        Hl::Function => FG,
    }
}

/// 1 書記素分の部品。文字は元の行を借りる（1 文字ごとに確保しない）
struct Item<'a> {
    g: Cow<'a, str>,
    width: usize,
    fg: [f32; 4],
    bg: Option<[f32; 4]>,
    underline: bool,
}

/// 描画先と、そのフレームの寸法
struct Painter<'a> {
    out: Vec<Instance>,
    fonts: &'a mut Fonts,
    r: &'a mut Renderer,
    cols: usize,
    /// ターミナルにはできない細さ: 縦棒カーソルと下線は数 px で描く
    bar: f32,
    underline: f32,
}

impl Painter<'_> {
    fn items<'s>(&self, s: &'s str, fg: [f32; 4]) -> Vec<Item<'s>> {
        s.graphemes(true).map(|g| Item { g: Cow::Borrowed(g), width: self.cell_width(g), fg, bg: None, underline: false }).collect()
    }

    /// 書記素 1 つのセル幅（タブは 4）
    fn cell_width(&self, g: &str) -> usize {
        if g == "\t" { 4 } else { self.fonts.width(g) }
    }

    /// 部品の並びを (col, y) から描く。戻り値は描き終えた列
    fn draw(&mut self, items: &[Item<'_>], col: usize, y: f32) -> usize {
        let cell = self.fonts.cell();
        let mut c = col;
        for it in items {
            if c + it.width > self.cols {
                break;
            }
            let x = c as f32 * cell.w;
            if let Some(bg) = it.bg {
                self.out.push(Instance::fill(x, y, it.width as f32 * cell.w, cell.h, bg));
            }
            if let Some(key) = self.fonts.glyph_for(&it.g, it.width) {
                if let Some(a) = self.r.glyph(self.fonts, key) {
                    self.out.push(Instance { rect: [x + a.left, y + a.top, a.uv[2], a.uv[3]], uv: a.uv, color: it.fg, kind: if a.color { 2 } else { 1 } });
                }
            }
            if it.underline {
                self.out.push(Instance::fill(x, y + cell.h - self.underline, it.width as f32 * cell.w, self.underline, it.fg));
            }
            c += it.width;
        }
        c
    }

    fn text(&mut self, s: &str, col: usize, y: f32, fg: [f32; 4]) -> usize {
        let items = self.items(s, fg);
        self.draw(&items, col, y)
    }

    fn caret(&mut self, col: usize, y: f32) {
        let cell = self.fonts.cell();
        self.out.push(Instance::fill(col as f32 * cell.w, y, self.bar, cell.h, CURSOR));
    }

    fn width(&self, s: &str) -> usize {
        nagi_text::graphemes(self.fonts, s).map(|(_, _, w)| w).sum()
    }
}

/// 英数字・記号（ASCII の印字可能文字）をまとめてアトラスへ載せる。初めて打った文字をその場で
/// ラスタライズすると 1 文字 0.5〜0.8ms かかり、その打鍵だけ遅くなるため。表示の後、空いた時間に呼ぶ
pub fn prefetch_ascii(fonts: &mut Fonts, r: &mut Renderer) {
    let mut keys = Vec::new();
    let mut buf = [0u8; 4];
    for c in '!'..='~' {
        if let Some(k) = fonts.glyph_for(c.encode_utf8(&mut buf), 1) {
            keys.push(k);
        }
    }
    r.prefetch(fonts, &keys);
}

/// 右下の列番号。行の先頭からカーソルまでの表示幅を足すので、数 MB の行の奥では
/// 代わりに byte 位置を出す（Vim も表示列と byte 位置が違うときは両方出す）
fn ruler_col(ed: &Editor) -> String {
    const MAX: usize = 1 << 16;
    if ed.cursor.byte > MAX {
        return format!("{}-", ed.cursor.byte + 1);
    }
    (nagi_core::display_col(&ed.buf.line_head(ed.cursor.line, ed.cursor.byte + 4).0, ed.cursor.byte, false) + 1).to_string()
}

/// 1 行のうち取り出す byte 数。1 文字は最大 4 byte で、少なくとも 1 セル使うので、これで画面の幅を必ず埋められる
fn line_budget(cols: usize) -> usize {
    cols * 4 + 16
}

fn screen_rows(fonts: &Fonts, r: &Renderer) -> usize {
    ((r.size().1 as f32 / fonts.cell().h) as usize).saturating_sub(1).max(1)
}

/// 空いた時間に、画面の上下 1 画面ぶんのグリフを先に載せておく（スクロールや検索で飛んだ先で、
/// 初めて出る漢字をその場でラスタライズすると 1 字 0.5〜1ms かかるため）
pub fn prefetch_around(ed: &Editor, fonts: &mut Fonts, r: &mut Renderer) {
    let rows = screen_rows(fonts, r);
    prefetch_lines(ed, fonts, r, ed.top.saturating_sub(rows), ed.top + 2 * rows + 1);
}

fn prefetch_lines(ed: &Editor, fonts: &mut Fonts, r: &mut Renderer, from: usize, to: usize) {
    let keys = glyph_keys(ed, fonts, from, to);
    r.prefetch(fonts, &keys);
}

/// 起動時、GPU の準備を待つ間に最初の画面のグリフをラスタライズしておく（アトラスに載せるのは後）
pub fn rasterize_first_screen(ed: &Editor, fonts: &mut Fonts, height: u32) -> Vec<(nagi_text::GlyphKey, Option<nagi_text::GlyphImage>)> {
    let rows = ((height as f32 / fonts.cell().h) as usize).saturating_sub(1).max(1);
    let mut keys = glyph_keys(ed, fonts, ed.top, ed.top + rows + 1);
    keys.sort_by_key(|k| (k.face, k.glyph, k.width));
    keys.dedup();
    fonts.rasterize_many(&keys)
}

fn glyph_keys(ed: &Editor, fonts: &mut Fonts, from: usize, to: usize) -> Vec<nagi_text::GlyphKey> {
    let total = ed.buf.line_count();
    let mut keys = Vec::new();
    let mut lines: Vec<String> = (from.min(total)..to.min(total)).map(|l| ed.buf.line_head(l, line_budget(400)).0).collect();
    lines.push("~ 0123456789,[No Name]-- INSERT VISUAL LINE --/?:→".into());
    for line in &lines {
        let items: Vec<(String, usize)> = nagi_text::graphemes(fonts, line).map(|(g, _, w)| (g.to_string(), w)).collect();
        for (g, w) in items {
            if let Some(k) = fonts.glyph_for(&g, w) {
                keys.push(k);
            }
        }
    }
    keys
}

/// 1 つの窓に描くもの。作業中の窓だけがカーソル・Visual・入力中の検索の強調を持つ
struct Pane<'a> {
    buf: &'a nagi_core::Buffer,
    /// 診断のある行（行 → エラーか）。行番号の色に使う
    diags: std::collections::HashMap<usize, bool>,
    /// git の行頭の変更マーカー
    marks: std::collections::HashMap<usize, crate::git_ui::Marker>,
    folds: &'a crate::folds::Folds,
    syntax: Option<&'a nagi_syntax::Syntax>,
    cursor: nagi_core::Cursor,
    top: usize,
    frac: f32,
    active: bool,
}

/// 窓 1 つを `rect`（本文の行だけ。状態行は含まない）に描く
fn draw_pane(p: &mut Painter, ed: &Editor, pane: &Pane, rect: Rect) {
    let cell = p.fonts.cell();
    // 作業中の窓に blame を出しているときは、行番号の左にその欄を足す
    let gutter = if pane.active { ed.gutter() } else { GUTTER };
    let bw = gutter - GUTTER;
    let now = crate::blame::now_secs();
    let saved_cols = p.cols;
    p.cols = rect.x + rect.w;
    let total = pane.buf.line_count();
    let debug = std::env::var_os("NAGI_DEBUG_BUILD").is_some();
    let t0 = std::time::Instant::now();
    // 強調は画面に見えている範囲だけを探す（入力中も、確定後の hlsearch も）。
    // 件数（3/12）のための全件は、検索の処理が別に（大きなファイルでは裏で）数える
    let searching = matches!(ed.mode, Mode::Search | Mode::Replace);
    let highlight = searching || ed.search.highlight;
    let t_matches = t0.elapsed();
    // スムーズスクロール中は 1 行多く描き、全体を frac 行ぶん上へずらす
    let frac = pane.frac;
    let drawn = rect.h + usize::from(frac > 0.0);

    // 画面に見えている範囲だけにハイライトのクエリをかける
    // 見える行（閉じた折りたたみは頭の行だけ）
    let shown: Vec<usize> = if pane.folds.is_empty() { (pane.top..(pane.top + drawn).min(total)).collect() } else { pane.folds.visible_from(pane.top, drawn, total) };
    let view_start = pane.buf.line_start(shown.first().copied().unwrap_or(pane.top).min(total));
    let view_end = pane.buf.line_start(shown.last().map_or(pane.top, |l| l + 1).min(total));
    // 行は画面に入る分だけ取り出す（折り返さないので、それより右は見えない。数 MB の 1 行でも毎回全体を作らない）
    let budget = line_budget(rect.w);
    let heads: Vec<(String, usize, bool)> = shown
        .iter()
        .map(|&ln| {
            let (s, cut) = pane.buf.line_head(ln, budget);
            (s, pane.buf.line_start(ln), cut)
        })
        .collect();
    let spans = match pane.syntax {
        // 長い行があるとき（と折りたたみで行が飛ぶとき）は、行ごとに見えている部分だけを問い合わせる
        Some(syn) if heads.iter().any(|h| h.2) || !pane.folds.is_empty() => heads.iter().flat_map(|(s, abs, _)| syn.highlight(pane.buf, *abs..*abs + s.len())).collect(),
        Some(syn) => syn.highlight(pane.buf, view_start..view_end.max(view_start)),
        None => Vec::new(),
    };
    let t_hl = t0.elapsed();
        let visible_matches: Vec<(usize, usize)>;
    let all_matches: &[(usize, usize)] = if highlight && heads.iter().any(|h| h.2) {
        // 長い行は見えている部分だけを探す
        visible_matches = heads.iter().flat_map(|(s, abs, _)| ed.visible_matches_in(pane.buf, *abs, *abs + s.len())).collect();
        &visible_matches
    } else if highlight {
        let end = if pane.top + drawn >= total { pane.buf.len_bytes() } else { view_end };
        visible_matches = ed.visible_matches_in(pane.buf, view_start, end);
        &visible_matches
    } else {
        &[]
    };
    let visual = match ed.mode {
        Mode::Visual { block: false, .. } if pane.active => Some(ed.visual_range()),
        _ => None,
    };
    // 矩形の選択は、見えている行ごとに列の範囲から求める
    let block = (pane.active && ed.is_block()).then(|| ed.block_sel());
    // 行単位の Visual は、選んでいる行の範囲にしておく（1 文字ごとに位置を引き直さない）
    let visual_lines = visual.map(|(vs, ve, _)| (pane.buf.pos_of(vs).0, pane.buf.pos_of(ve.saturating_sub(1).max(vs)).0));
    let replacing = pane.active && ed.mode == Mode::Replace && !ed.query.text.is_empty();

    for row in 0..drawn {
        let y = (rect.y as f32 + row as f32 - frac) * cell.h;
        let Some(&ln) = shown.get(row) else {
            // ファイル末尾より後ろは Vim と同じく ~
            p.text("~", rect.x, y, TILDE);
            continue;
        };
        // 診断のある行は行番号の色で示す（エラーは赤、警告は黄色）
        let num_color = match pane.diags.get(&ln) {
            Some(true) => DIAG_ERROR,
            Some(false) => DIAG_WARN,
            None => LINE_NO,
        };
        // blame の欄（コミットが変わる行だけ文字。新しいほど明るい）
        if bw > 0 {
            if let Some((text, level, wip)) = ed.blame_cell(ln, now) {
                let color = if wip { GIT_MOD } else { [rgb(0x6c6c6c), rgb(0x8a8a8a), rgb(0xaaaaaa), rgb(0xd0d0d0)][level as usize] };
                let saved = p.cols;
                p.cols = rect.x + bw - 1;
                p.text(&format!(" {text}"), rect.x, y, color);
                p.cols = saved;
            }
            p.out.push(Instance::fill((rect.x + bw) as f32 * cell.w - cell.w * 0.5, y, 1.0, cell.h, MATCH_BG));
        }
        p.text(&format!("{:>5} ", ln + 1), rect.x + bw, y, num_color);
        // git の変更マーカー（行番号の左端の細い帯。消した位置は赤い短い線）
        if let Some(m) = pane.marks.get(&ln) {
            let x = (rect.x + bw) as f32 * cell.w + 1.0;
            match m {
                crate::git_ui::Marker::Add => p.out.push(Instance::fill(x, y, 3.0, cell.h, GIT_ADD)),
                crate::git_ui::Marker::Mod => p.out.push(Instance::fill(x, y, 3.0, cell.h, GIT_MOD)),
                crate::git_ui::Marker::DelAbove => p.out.push(Instance::fill(x, y - 1.0, cell.w * 1.2, 3.0, GIT_DEL)),
            }
        }
        // 閉じた折りたたみは 1 行で「+-- 12 行: 頭の行」と薄く出す（Vim の Folded と同じ形）
        if let Some((s, e)) = pane.folds.outer_at(ln).filter(|f| f.0 == ln) {
            let x = (rect.x + gutter) as f32 * cell.w;
            p.out.push(Instance::fill(x, y, (rect.w.saturating_sub(gutter)) as f32 * cell.w, cell.h, MATCH_BG));
            let label = format!("+--{:>3} 行: {}", e - s + 1, heads[row].0.trim());
            let saved = p.cols;
            p.cols = rect.x + rect.w;
            let end = p.text(&label, rect.x + gutter, y, STATUS_FG);
            let _ = end;
            p.cols = saved;
            if pane.active && pane.cursor.line == ln && !matches!(ed.mode, Mode::Command | Mode::Search | Mode::Replace | Mode::Finder | Mode::GrepPrompt | Mode::List) {
                p.out.push(Instance::fill(x, y, cell.w, cell.h, CURSOR));
            }
            continue;
        }
        let (line, abs, cut) = &heads[row];
        let (line, abs, cut) = (line.as_str(), *abs, *cut);
        // byte ごとの文字色。外側のキャプチャの後に内側が来るので、後から塗ったものが勝つ
        let mut colors = vec![FG; line.len()];
        for (rg, hl) in spans.iter().filter(|(rg, _)| rg.end > abs && rg.start < abs + line.len()) {
            let (a, b) = (rg.start.max(abs) - abs, rg.end.min(abs + line.len()) - abs);
            colors[a..b].fill(hl_color(*hl));
        }
        // この行にかかる一致（一致は位置順に並んでいる）。改行の位置にかかる一致も含める
        let first = all_matches.partition_point(|&(_, e)| e <= abs);
        let last = all_matches.partition_point(|&(s, _)| s <= abs + line.len());
        let line_matches = &all_matches[first..last.max(first)];
        let mut mi = 0;
        let line_selected = match (visual, visual_lines) {
            (Some((_, _, true)), Some((a, b))) => a <= ln && ln <= b,
            _ => false,
        };
        let is_cursor_line = pane.active && ln == pane.cursor.line;
        let block_span = block.filter(|b| ln >= b.l1 && ln <= b.l2).map(|b| crate::block::span_in_line(line, b.left, b.right));
        let mut items: Vec<Item> = Vec::with_capacity(line.len());
        let mut cursor_item = None;
        let mut gs = line.grapheme_indices(true).peekable();
        while let Some((b, g)) = gs.next() {
            let a = abs + b;
            while mi < line_matches.len() && line_matches[mi].1 <= a && line_matches[mi].0 < a {
                mi += 1;
            }
            // 置換の予告表示: 一致をその場で置換後の文字列に差し替える（下線付き）
            if replacing {
                if let Some(&(ms, me)) = line_matches.get(mi).filter(|(ms, _)| *ms == a) {
                    let rep = ed.replacement(ms, me);
                    let mut rep_items: Vec<Item> = p.items(&rep, FG).into_iter().map(|i| Item { g: Cow::Owned(i.g.into_owned()), ..i }).collect();
                    rep_items.iter_mut().for_each(|i| i.underline = true);
                    items.extend(rep_items);
                    while gs.peek().is_some_and(|(nb, _)| abs + nb < me) {
                        gs.next();
                    }
                    mi += 1;
                    continue;
                }
            }
            let width = p.cell_width(g);
            let mut it = Item { g: Cow::Borrowed(g), width, fg: colors.get(b).copied().unwrap_or(FG), bg: None, underline: false };
            if let Some(&(ms, me)) = line_matches.get(mi) {
                if a >= ms && a < me {
                    if pane.active && Some((ms, me)) == ed.search.current && matches!(ed.mode, Mode::Search) {
                        it.bg = Some(it.fg);
                        it.fg = BG;
                    } else {
                        it.bg = Some(MATCH_BG);
                    }
                }
            }
            if let Some((vs, ve, lw)) = visual {
                if if lw { line_selected } else { a >= vs && a < ve } {
                    it.bg = Some(it.fg);
                    it.fg = BG;
                }
            }
            if block_span.is_some_and(|(s, e)| b >= s && b < e) {
                it.bg = Some(it.fg);
                it.fg = BG;
            }
            if is_cursor_line && b == pane.cursor.byte {
                cursor_item = Some(items.len());
            }
            items.push(it);
        }
        // Visual で改行まで選んでいる行は、Vim と同じく行末を 1 セル反転して見せる（空行が選ばれていると分かるように）
        let mut eol_selected = false;
        if let Some((vs, ve, lw)) = visual {
            let eol = abs + line.len();
            eol_selected = if lw { line_selected } else { eol >= vs && eol < ve };
            if eol_selected && !(is_cursor_line && pane.cursor.byte >= line.len()) {
                items.push(Item { g: Cow::Borrowed(" "), width: 1, fg: BG, bg: Some(FG), underline: false });
            }
        }
        if !is_cursor_line || matches!(ed.mode, Mode::Command | Mode::Search | Mode::Replace | Mode::Finder | Mode::GrepPrompt | Mode::List | Mode::CommitMsg | Mode::Diff | Mode::Log | Mode::Branches) {
            p.draw(&items, rect.x + gutter, y);
            continue;
        }
        if ed.mode == Mode::Insert {
            // 本文の途中に変換中の文字列（下線）を差し込み、カーソルは縦棒
            let at = cursor_item.unwrap_or(items.len());
            let mut pre = p.items(&ed.preedit, FG);
            pre.iter_mut().for_each(|i| i.underline = true);
            let caret_in_pre = match ed.preedit_cursor {
                Some((a, _)) if !ed.preedit.is_empty() => p.width(&ed.preedit[..a]),
                _ => p.width(&ed.preedit),
            };
            let before: usize = items[..at].iter().map(|i| i.width).sum();
            items.splice(at..at, pre);
            p.draw(&items, rect.x + gutter, y);
            p.caret(rect.x + gutter + before + caret_in_pre, y);
        } else {
            // ブロックカーソル（文字を反転）。空行や行末では 1 セルの塗り
            // 選択の中では反転をもう一度反転して元の色に戻す（ターミナルの Vim と同じ見え方。行単位の Visual でも位置が分かる）
            match cursor_item {
                Some(i) if (visual.is_some() || block.is_some()) && items[i].fg == BG && items[i].bg.is_some() => {
                    items[i].fg = items[i].bg.take().unwrap_or(FG);
                    p.draw(&items, rect.x + gutter, y);
                }
                Some(i) => {
                    items[i].bg = Some(CURSOR);
                    items[i].fg = BG;
                    p.draw(&items, rect.x + gutter, y);
                }
                None => {
                    let c = p.draw(&items, rect.x + gutter, y);
                    // 切った行でカーソルが見えている部分より右にあるときは描かない
                    if !cut {
                        let color = if eol_selected { CURSOR_IN_SELECTION } else { CURSOR };
                        p.out.push(Instance::fill(c as f32 * cell.w, y, cell.w, cell.h, color));
                    }
                }
            }
        }
    }

    let t_lines = t0.elapsed();
    if debug {
        eprintln!("build matches={:?} highlight={:?} lines={:?} n_matches={}", t_matches, t_hl - t_matches, t_lines - t_hl, all_matches.len());
    }
    p.cols = saved_cols;
}

pub fn build(ed: &mut Editor, fonts: &mut Fonts, r: &mut Renderer) -> Vec<Instance> {
    let cell = fonts.cell();
    let (w, h) = r.size();
    let cols = (w as f32 / cell.w) as usize;
    let rows = ((h as f32 / cell.h) as usize).saturating_sub(ed.status_rows()).max(1);
    ed.set_screen(cols, rows);
    let bar = (cell.w / 6.0).round().max(2.0);
    let underline = (cell.h / 16.0).round().max(1.0);
    let mut p = Painter { out: Vec::with_capacity(cols * rows + 64), fonts, r, cols, bar, underline };
    if let (Some(r), Some(t)) = (ed.tree_rect(), &mut ed.tree) {
        let h = r.h.saturating_sub(1).max(1);
        if t.selected < t.scroll {
            t.scroll = t.selected;
        } else if t.selected >= t.scroll + h {
            t.scroll = t.selected + 1 - h;
        }
    }
    if let (Some(r), Some(g)) = (ed.panel_rect(), &mut ed.grep) {
        // 選んでいる結果が見える範囲に入るように送る（1 行目は見出し）
        let h = r.h.saturating_sub(1).max(1);
        if g.selected < g.scroll {
            g.scroll = g.selected;
        } else if g.selected >= g.scroll + h {
            g.scroll = g.selected + 1 - h;
        }
    }
    if let (Some(r), Some(c)) = (ed.commit_rect(), &mut ed.commit) {
        // 選んでいる項目（見出しの行を含めた位置）が見える範囲に入るように送る
        let h = crate::commit_panel::layout(r.h).list_h;
        let tracked = c.tracked();
        let at = c.selected + 1 + usize::from(c.selected >= tracked);
        if at < c.scroll {
            c.scroll = at;
        } else if at >= c.scroll + h {
            c.scroll = at + 1 - h;
        }
    }
    if let Some(r) = ed.right_rect() {
        let h = r.h.saturating_sub(2).max(1);
        let fit = |sel: usize, scroll: &mut usize| {
            if sel < *scroll {
                *scroll = sel;
            } else if sel >= *scroll + h {
                *scroll = sel + 1 - h;
            }
        };
        if let Some(m) = &mut ed.maven {
            fit(m.selected, &mut m.scroll);
        }
        if let Some(d) = &mut ed.db {
            fit(d.selected, &mut d.scroll);
        }
    }
    if let (Some(r), Some(g)) = (ed.panel_rect(), &mut ed.db_result) {
        let h = r.h.saturating_sub(2).max(1);
        if g.row < g.scroll {
            g.scroll = g.row;
        } else if g.row >= g.scroll + h {
            g.scroll = g.row + 1 - h;
        }
        // 選んだ列が見えるように横へ送る
        if g.col < g.hscroll {
            g.hscroll = g.col;
        }
        while g.hscroll < g.col && g.widths[g.hscroll..=g.col].iter().map(|w| w + 3).sum::<usize>() + 1 > r.w {
            g.hscroll += 1;
        }
    }
    if let Some(f) = &mut ed.finder {
        // 選んでいる候補が見える範囲に入るように送る
        let h = finder_height(rows);
        if f.selected < f.scroll {
            f.scroll = f.selected;
        } else if f.selected >= f.scroll + h {
            f.scroll = f.selected + 1 - h;
        }
    }
    // 見えている窓のバッファの git の差分（版が変わったときだけ計算し直す）
    ed.prepare_git();
    let ed: &Editor = ed;
    let split = ed.is_split();
    let rects = ed.window_rects();
    for &(win, rect) in &rects {
        let Some(st) = ed.win_state(win) else { continue };
        let active = win == ed.cur_win;
        let (buf, syntax, folds) = if st.doc == ed.cur_doc {
            (&ed.buf, ed.syntax.as_ref(), &ed.folds)
        } else {
            match ed.parked_doc(st.doc) {
                Some(d) => (&d.buf, d.syntax.as_ref(), &d.folds),
                None => continue,
            }
        };
        // ほかの窓で同じバッファを編集して短くなっていたら、表示位置を詰める
        let last = buf.line_count().saturating_sub(1);
        let mut cursor = st.cursor;
        cursor.line = cursor.line.min(last);
        let pane = Pane { buf, marks: ed.git_markers(buf.path.as_deref()), diags: ed.diag_lines(buf.path.as_deref()), folds, syntax, cursor, top: st.top.min(last), frac: if active { ed.frac } else { 0.0 }, active };
        draw_pane(&mut p, ed, &pane, Rect { h: ed.text_rows(rect), ..rect });
    }
    // 窓ごとの状態行（分割中だけ。Vim の laststatus=1 と同じ）と、左右の窓の間の 1px の区切り線
    if split {
        for &(win, rect) in &rects {
            if rect.x > 0 {
                let x = ((rect.x as f32 - 0.5) * cell.w).round();
                p.out.push(Instance::fill(x, rect.y as f32 * cell.h, 1.0, rect.h as f32 * cell.h, LINE_NO));
            }
            let Some(st) = ed.win_state(win) else { continue };
            let active = win == ed.cur_win;
            let y = (rect.y + rect.h - 1) as f32 * cell.h;
            // はみ出した本文（スムーズスクロール中の 1 行）を地で隠してから書く
            p.out.push(Instance::fill(rect.x as f32 * cell.w, y, rect.w as f32 * cell.w, cell.h, BG));
            let (path, dirty, line) = if st.doc == ed.cur_doc {
                (ed.buf.path.as_ref(), ed.buf.dirty, st.cursor.line)
            } else {
                match ed.parked_doc(st.doc) {
                    Some(d) => (d.buf.path.as_ref(), d.buf.dirty, st.cursor.line),
                    None => continue,
                }
            };
            let name = path.map_or("[No Name]".to_string(), |p| p.display().to_string());
            let color = if active { FG } else { LINE_NO };
            let saved = p.cols;
            p.cols = rect.x + rect.w;
            let left = format!("{name}{}", if dirty { " [+]" } else { "" });
            let end = p.text(&left, rect.x, y, color);
            let pos = if active { format!("{},{} ", line + 1, ruler_col(ed)) } else { format!("{} ", line + 1) };
            let pc = (rect.x + rect.w).saturating_sub(p.width(&pos));
            if end + 1 < pc {
                p.text(&pos, pc, y, color);
            }
            // 作業中の窓は、状態行に細い下線を引いて見分けやすくする
            if active {
                p.out.push(Instance::fill(rect.x as f32 * cell.w, y + cell.h - p.underline, rect.w as f32 * cell.w, p.underline, LINE_NO));
            }
            p.cols = saved;
        }
    }
    if let (Some(r), Some(t)) = (ed.tree_rect(), &ed.tree) {
        draw_tree(&mut p, t, r, ed.mode == Mode::List && ed.list_side == crate::editor::Side::Left, ed.buf.path.as_deref());
    }
    if let (Some(r), Some(c)) = (ed.commit_rect(), &ed.commit) {
        draw_commit_panel(&mut p, ed, c, r);
    }
    if let (Some(l), None) = (&ed.log, &ed.diff) {
        let (x, w) = ed.windows_x();
        draw_log_view(&mut p, l, Rect { x, y: 0, w, h: ed.windows_rows() });
    }
    if let Some(d) = &ed.diff {
        let (x, w) = ed.windows_x();
        draw_diff_view(&mut p, d, Rect { x, y: 0, w, h: ed.windows_rows() });
    }
    if let Some(r) = ed.right_rect() {
        draw_right_panel(&mut p, ed, r);
    }
    if ed.bottom_kind == crate::result_grid::BottomKind::Result {
        if let (Some(r), Some(g)) = (ed.panel_rect(), &ed.db_result) {
            draw_result_grid(&mut p, g, r, ed.mode == Mode::List && ed.list_side == crate::editor::Side::Bottom);
        }
    } else if let (Some(r), Some(g)) = (ed.panel_rect(), &ed.grep) {
        draw_grep_panel(&mut p, g, r, matches!(ed.mode, Mode::List | Mode::GrepPrompt));
    }
    if let Some(f) = &ed.finder {
        draw_finder(&mut p, f, rows, cols);
    }
    if let Some(b) = &ed.branch_popup {
        draw_branch_popup(&mut p, b, rows, cols);
    }
    if ed.git.popup.is_some() && ed.mode == Mode::Normal {
        draw_hunk_popup(&mut p, ed, rows, cols);
    } else if ed.blame_active() && ed.mode == Mode::Normal {
        draw_blame_popup(&mut p, ed, rows);
    }
    if let (Some(c), Mode::Insert) = (&ed.completion, ed.mode) {
        draw_completion(&mut p, ed, c, rows, cols);
    }
    if let (Some(c), Mode::Command) = (&ed.cmd_complete, ed.mode) {
        draw_cmd_complete(&mut p, ed, c, rows, cols);
    }
    draw_toggle_buttons(&mut p, ed, cols);
    match ed.stripe {
        Some(crate::editor::Side::Left) => draw_stripe_left(&mut p, ed.windows_rows(), ed.left_visible()),
        Some(crate::editor::Side::Bottom) => draw_stripe_bottom(&mut p, rows, cols, ed.bottom_visible()),
        Some(crate::editor::Side::Right) => draw_stripe_right(&mut p, ed.windows_rows(), cols, ed.right_visible()),
        Some(crate::editor::Side::Commit) | None => {}
    }
    // ---- 最下行: 左にモード・メッセージ・入力欄、右に位置（Vim の showmode と ruler と同じ形）
    let y = rows as f32 * cell.h;
    // はみ出した本文の最終行（スムーズスクロール中）を最下行の地で隠す
    p.out.push(Instance::fill(0.0, y, w as f32, h as f32 - y, BG));
    let name = ed.buf.path.as_ref().map_or("[No Name]".to_string(), |p| p.display().to_string());
    let keys = format!("{}{}", ed.pending_keys.iter().map(|k| k.label()).collect::<String>(), ed.showcmd);
    // git のブランチ（main ↑1）
    let keys = match ed.git_branch_label() {
        Some(b) => format!("{keys}   {b}"),
        None => keys,
    };
    // 分割中は位置を窓ごとの状態行に出すので、最下行には入力中のキーだけを出す
    let ruler = if split {
        format!("{keys} ")
    } else {
        format!(
            "{keys}   {}{}   {}{},{} ",
            name,
            if ed.buf.dirty { " [+]" } else { "" },
            ed.syntax.as_ref().map_or(String::new(), |s| format!("{}   ", s.lang.name())),
            ed.cursor.line + 1,
            ruler_col(ed)
        )
    };
    let re = if ed.search.regex { "  [.*]" } else { "" };
    let count = match (ed.search.current, ed.match_count()) {
        _ if ed.query.text.is_empty() => format!("{re} "),
        (Some(_), Some((Some(i), n))) => format!("{}/{n}{re} ", i + 1),
        // 全件は裏で数えている途中
        (Some(_), _) => format!("?/…{re} "),
        _ => format!("0/0{re} "),
    };
    match ed.mode {
        Mode::Command => prompt_line(&mut p, ":", &ed.cmdline.text, ed.cmdline.cursor, &ed.preedit, y, true),
        Mode::GrepPrompt if ed.grep.is_some() => {
            let g = ed.grep.as_ref().unwrap();
            prompt_line(&mut p, crate::grep_panel::GREP_LEAD, &g.query.text, g.query.cursor, &ed.preedit, y, true);
            let info = format!("{}{} 件{} ", if g.regex { "[.*]  " } else { "" }, g.hits.len(), if g.searching() { " …" } else { "" });
            let pc = cols.saturating_sub(p.width(&info));
            p.text(&info, pc, y, STATUS_FG);
        }
        Mode::List if ed.list_side == crate::editor::Side::Commit => {
            let end = p.text(if ed.message.is_empty() { "-- コミット --" } else { &ed.message }, 0, y, FG);
            let help = "Space 選択  d 差分  u 戻す  c メッセージ  Ctrl-Enter コミット  P プッシュも  Alt-1 プロジェクト  q 閉じる ";
            let pc = cols.saturating_sub(p.width(help));
            if end + 2 <= pc {
                p.text(help, pc, y, STATUS_FG);
            }
        }
        Mode::CommitMsg => {
            p.text(if ed.message.is_empty() { "-- コミットメッセージ --   Esc 一覧へ   Ctrl-Enter コミット" } else { &ed.message }, 0, y, FG);
        }
        Mode::Branches => {
            let left = if ed.message.is_empty() { "-- ブランチ --".to_string() } else { ed.message.clone() };
            let end = p.text(&left, 0, y, FG);
            let help = "打つと絞り込み  ↑↓ 移動  Enter チェックアウト  → 操作  Esc 閉じる ";
            let pc = cols.saturating_sub(p.width(help));
            if end + 2 <= pc {
                p.text(help, pc, y, STATUS_FG);
            }
        }
        Mode::Log => {
            let l = ed.log.as_ref();
            let left = if !ed.message.is_empty() {
                ed.message.clone()
            } else if l.is_some_and(|l| l.filtering) {
                format!("/{}", l.map_or("", |l| l.filter.text.as_str()))
            } else {
                "-- 履歴 --".into()
            };
            let end = p.text(&left, 0, y, FG);
            let help = "j/k 移動  / 絞り込み  Tab ファイル  Enter 差分  o チェックアウト  b ブランチ  c チェリーピック  y ハッシュ  q 閉じる ";
            let pc = cols.saturating_sub(p.width(help));
            if end + 2 <= pc {
                p.text(help, pc, y, STATUS_FG);
            }
        }
        Mode::Diff => {
            let d = ed.diff.as_ref();
            let n = d.map_or(0, |d| d.hunk_rows.len());
            let i = d.map_or(0, |d| d.hunk_rows.iter().rposition(|&r| r <= d.cursor).map_or(0, |i| i + 1));
            let left = if ed.message.is_empty() { format!("-- 差分 --   変更 {i}/{n}") } else { ed.message.clone() };
            let end = p.text(&left, 0, y, FG);
            let help = "]c 次  [c 前  s ステージ  u 戻す  Enter 開く  q 閉じる ";
            let pc = cols.saturating_sub(p.width(help));
            if end + 2 <= pc {
                p.text(help, pc, y, STATUS_FG);
            }
        }
        Mode::List if ed.list_side == crate::editor::Side::Right => {
            let (name, help) = match ed.right_visible() {
                Some(crate::right_panel::RightTab::Maven) => ("-- Maven --", "Enter 実行・開閉  l/h 開く・閉じる  r 読み直す  q 閉じる  Ctrl-w h 窓へ "),
                _ => ("-- データベース --", "Enter つなぐ・開閉  o 中身  c コンソール  x 切る  r 読み直す  Ctrl-w h 窓へ "),
            };
            let end = p.text(if ed.message.is_empty() { name } else { &ed.message }, 0, y, FG);
            let pc = cols.saturating_sub(p.width(help));
            if end + 2 <= pc {
                p.text(help, pc, y, STATUS_FG);
            }
        }
        Mode::List if ed.list_side == crate::editor::Side::Bottom && ed.bottom_kind == crate::result_grid::BottomKind::Result => {
            let end = p.text(if ed.message.is_empty() { "-- クエリの結果 --" } else { &ed.message }, 0, y, FG);
            let help = "hjkl 移動  y 値  Y 行  gy 表全体（タブ区切り）  q 閉じる ";
            let pc = cols.saturating_sub(p.width(help));
            if end + 2 <= pc {
                p.text(help, pc, y, STATUS_FG);
            }
        }
        Mode::List if ed.list_side == crate::editor::Side::Left => {
            p.text(if ed.message.is_empty() { "-- プロジェクト --" } else { &ed.message }, 0, y, FG);
        }
        Mode::List if ed.grep.is_some() => {
            let g = ed.grep.as_ref().unwrap();
            let end = p.text(if ed.message.is_empty() { "-- 検索結果 --" } else { &ed.message }, 0, y, FG);
            let info = format!("{}/{} ", (g.selected + 1).min(g.hits.len()), g.hits.len());
            let pc = cols.saturating_sub(p.width(&info));
            if end + 2 <= pc {
                p.text(&info, pc, y, STATUS_FG);
            }
        }
        Mode::Finder if ed.finder.is_some() => {
            let f = ed.finder.as_ref().unwrap();
            prompt_line(&mut p, "> ", &f.query.text, f.query.cursor, &ed.preedit, y, true);
            let info = format!("{}  {}/{}{} ", f.source.label(), f.hits.len(), f.items.len(), if f.walking() { " …" } else { "" });
            let pc = cols.saturating_sub(p.width(&info));
            p.text(&info, pc, y, STATUS_FG);
        }
        Mode::Search => {
            let lead = if ed.search.forward { "/" } else { "?" };
            prompt_line(&mut p, lead, &ed.query.text, ed.query.cursor, &ed.preedit, y, true);
            let pc = cols.saturating_sub(p.width(&count));
            p.text(&count, pc, y, STATUS_FG);
        }
        Mode::Replace => {
            let y2 = y + cell.h;
            prompt_line(&mut p, "/", &ed.query.text, ed.query.cursor, &ed.preedit, y, ed.replace_field == 0);
            prompt_line(&mut p, "→", &ed.repl.text, ed.repl.cursor, &ed.preedit, y2, ed.replace_field == 1);
            let c = format!("Enter: 1 件  Alt+A: 全件   {count}");
            let pc = cols.saturating_sub(p.width(&c));
            p.text(&c, pc, y2, STATUS_FG);
        }
        _ => {
            let left = if !ed.message.is_empty() {
                ed.message.clone()
            } else {
                let mode = match ed.mode {
                    Mode::Insert => "-- INSERT --",
                    Mode::Visual { block: true, .. } => "-- VISUAL BLOCK --",
                    Mode::Visual { line: false, .. } => "-- VISUAL --",
                    Mode::Visual { line: true, .. } => "-- VISUAL LINE --",
                    _ => "",
                };
                // マクロの記録中は Vim と同じく「recording @a」を出す
                match &ed.recording {
                    Some((r, _)) => format!("{mode}recording @{r}"),
                    None => mode.to_string(),
                }
            };
            // 何も出していなければ、カーソルの行の診断を出す
            let (left, color) = match ed.diag_at_cursor() {
                Some(d) if left.is_empty() && ed.mode == Mode::Normal => (format!("{}: {}", if d.error { "error" } else { "warning" }, d.msg), if d.error { DIAG_ERROR } else { DIAG_WARN }),
                _ => (left, FG),
            };
            let end = p.text(&left, 0, y, color);
            // 長いメッセージと重なるときは位置表示を出さない（メッセージを優先する）
            let pc = cols.saturating_sub(p.width(&ruler));
            if end + 2 <= pc {
                p.text(&ruler, pc, y, STATUS_FG);
            }
        }
    }
    p.out
}

/// 左のストライプ（マウスを左端に寄せている間だけ）。幅 2 セルの帯に「プロジェクト」「コミット」を縦に書く。出しているタブは反転
fn draw_stripe_left(p: &mut Painter, rows: usize, shown: Option<crate::tree_panel::LeftTab>) {
    let cell = p.fonts.cell();
    let w = 2.0 * cell.w;
    p.out.push(Instance::fill(0.0, 0.0, w, rows as f32 * cell.h, BG));
    p.out.push(Instance::fill(w, 0.0, 1.0, rows as f32 * cell.h, LINE_NO));
    for (label, tab, top) in crate::tree_panel::STRIPE {
        let open = shown == Some(tab);
        let n = label.chars().count();
        let y0 = top as f32 * cell.h;
        if open {
            p.out.push(Instance::fill(0.0, y0, w, n as f32 * cell.h, FG));
        }
        for (k, c) in label.chars().enumerate() {
            // 縦書きでは長音を縦の線にする
            let c = if c == 'ー' { '｜' } else { c };
            let mut buf = [0u8; 4];
            p.text(c.encode_utf8(&mut buf), 0, y0 + k as f32 * cell.h, if open { BG } else { STATUS_FG });
        }
    }
}

/// 右のストライプ（マウスを右端に寄せている間だけ）。「データベース」「Maven」を縦に書く
fn draw_stripe_right(p: &mut Painter, rows: usize, cols: usize, shown: Option<crate::right_panel::RightTab>) {
    let cell = p.fonts.cell();
    let x0 = cols.saturating_sub(2);
    let w = 2.0 * cell.w;
    p.out.push(Instance::fill(x0 as f32 * cell.w, 0.0, w, rows as f32 * cell.h, BG));
    p.out.push(Instance::fill(x0 as f32 * cell.w, 0.0, 1.0, rows as f32 * cell.h, LINE_NO));
    for (label, tab, top) in crate::right_panel::RSTRIPE {
        let open = shown == Some(tab);
        let n = label.chars().count();
        let y0 = top as f32 * cell.h;
        if open {
            p.out.push(Instance::fill(x0 as f32 * cell.w, y0, w, n as f32 * cell.h, FG));
        }
        for (k, c) in label.chars().enumerate() {
            let c = if c == 'ー' { '｜' } else { c };
            let mut buf = [0u8; 4];
            // 半角の字は帯の真ん中に
            let x = if c.is_ascii() { x0 + 1 } else { x0 };
            p.text(c.encode_utf8(&mut buf), x, y0 + k as f32 * cell.h, if open { BG } else { STATUS_FG });
        }
    }
}

/// 下のストライプ（マウスを最下行に寄せている間だけ）。最下行のすぐ上に「検索」を出す
fn draw_stripe_bottom(p: &mut Painter, rows: usize, cols: usize, open: bool) {
    let cell = p.fonts.cell();
    let y = (rows - 1) as f32 * cell.h;
    p.out.push(Instance::fill(0.0, y, cols as f32 * cell.w, cell.h, BG));
    p.out.push(Instance::fill(0.0, y, cols as f32 * cell.w, 1.0, LINE_NO));
    let label = " 検索 ";
    let w = p.width(label);
    if open {
        p.out.push(Instance::fill(0.0, y, w as f32 * cell.w, cell.h, FG));
    }
    p.text(label, 0, y, if open { BG } else { STATUS_FG });
}

/// 出す・隠すのボタン。ふだんは薄い 1 文字、マウスを載せると明るくして名前を横に出す
fn draw_toggle_buttons(p: &mut Painter, ed: &Editor, cols: usize) {
    let cell = p.fonts.cell();
    for (b, row, col) in ed.toggle_buttons() {
        let hover = ed.hover_button == Some(b);
        let (x, y) = (col as f32 * cell.w, row as f32 * cell.h);
        if hover {
            p.out.push(Instance::fill(x, y, cell.w, cell.h, MATCH_BG));
        }
        p.text(b.glyph(), col, y, if hover { FG } else { STATUS_FG });
        if hover {
            let label = format!(" {} ", b.label(ed));
            let w = p.width(&label);
            let lx = if col + 2 + w <= cols { col + 2 } else { col.saturating_sub(w + 1) };
            let (fx, fw) = (lx as f32 * cell.w, w as f32 * cell.w);
            p.out.push(Instance::fill(fx - 1.0, y - 1.0, fw + 2.0, cell.h + 2.0, STATUS_FG));
            p.out.push(Instance::fill(fx, y, fw, cell.h, rgb(0x141414)));
            p.text(&label, lx, y, FG);
        }
    }
}

/// 左の欄の 1 行目: タブ（プロジェクト・コミット）。出しているタブは明るい字と下線
fn draw_left_tabs(p: &mut Painter, r: Rect, active: crate::tree_panel::LeftTab, focused: bool, extra: &str) {
    let tabs: Vec<(&str, bool)> = crate::tree_panel::TABS.iter().map(|(l, t)| (*l, *t == active)).collect();
    draw_tabs(p, r, &tabs, focused, extra);
}

/// 欄の 1 行目のタブ（見出し, 出しているか）。右端の隠すボタンの手前で切る
fn draw_tabs(p: &mut Painter, r: Rect, tabs: &[(&str, bool)], focused: bool, extra: &str) {
    let cell = p.fonts.cell();
    let y = r.y as f32 * cell.h;
    let mut x = r.x;
    // 右端の隠すボタンの手前で切る（細くしたとき）
    let saved_cols = p.cols;
    p.cols = (r.x + r.w).saturating_sub(3);
    for &(label, on) in tabs {
        let end = p.text(label, x, y, if on && focused { FG } else if on { rgb(0xc8c8c8) } else { STATUS_FG });
        if on {
            p.out.push(Instance::fill(x as f32 * cell.w, y + cell.h - 2.0, (end - x) as f32 * cell.w, 2.0, if focused { GIT_MOD } else { STATUS_FG }));
        }
        x = end;
    }
    p.cols = saved_cols;
    if !extra.is_empty() {
        let saved = p.cols;
        p.cols = (r.x + r.w).saturating_sub(3);
        p.text(&format!(" {extra}"), x, y, LINE_NO);
        p.cols = saved;
    }
    p.out.push(Instance::fill(r.x as f32 * cell.w, y + cell.h - 1.0, r.w as f32 * cell.w, 1.0, LINE_NO));
}

/// 右の欄（データベース・Maven）。1 行目はタブ、2 行目から木
fn draw_right_panel(p: &mut Painter, ed: &Editor, r: Rect) {
    use crate::right_panel::RightTab;
    let cell = p.fonts.cell();
    let (x0, y0, wpx) = (r.x as f32 * cell.w, r.y as f32 * cell.h, r.w as f32 * cell.w);
    p.out.push(Instance::fill(x0, y0, wpx, r.h as f32 * cell.h, BG));
    p.out.push(Instance::fill(x0 - cell.w * 0.5, y0, 1.0, r.h as f32 * cell.h, LINE_NO));
    let focused = ed.right_focused();
    let tab = ed.right_visible().unwrap_or_default();
    let tabs: Vec<(&str, bool)> = crate::right_panel::RTABS.iter().map(|(l, t)| (*l, *t == tab)).collect();
    let extra = match tab {
        RightTab::Maven => ed.maven.as_ref().and_then(|m| m.root.file_name()).map_or(String::new(), |n| n.to_string_lossy().into_owned()),
        RightTab::Database => String::new(),
    };
    draw_tabs(p, r, &tabs, focused, &extra);
    let saved = p.cols;
    p.cols = r.x + r.w;
    // (字下げ, 開け閉めの印, 本文, 補足, 本文の色)
    let mut lines: Vec<(usize, &str, String, String, [f32; 4])> = Vec::new();
    let (selected, scroll) = match tab {
        RightTab::Maven => {
            let Some(m) = &ed.maven else { return };
            for row in &m.rows {
                let (mark, color) = match &row.kind {
                    crate::maven_panel::RowKind::Folder(k) => (if m.is_open(k) { "▾ " } else { "▸ " }, if row.depth == 0 || k.starts_with("p:") { FG } else { FINDER_MATCH }),
                    crate::maven_panel::RowKind::Goal(..) => ("· ", FG),
                    crate::maven_panel::RowKind::Leaf => ("  ", STATUS_FG),
                };
                lines.push((row.depth, mark, row.label.clone(), row.note.clone(), color));
            }
            (m.selected, m.scroll)
        }
        RightTab::Database => {
            let Some(d) = &ed.db else { return };
            if d.sources.is_empty() {
                p.text(" 接続先がありません", r.x, y0 + cell.h, STATUS_FG);
                p.text(" :DbAdd 名前 jdbc:URL 利用者", r.x, y0 + 2.0 * cell.h, STATUS_FG);
            }
            for row in &d.rows {
                let (depth, text, note) = d.row_label(row);
                let (mark, color) = match row {
                    crate::db_panel::DbRow::Source(i) => {
                        let open = d.expanded.contains(&format!("s{i}"));
                        let c = &d.conns[*i];
                        (if open { "▾ " } else { "▸ " }, if c.error.is_some() { DIAG_ERROR } else if c.connected { GIT_ADD } else { FG })
                    }
                    crate::db_panel::DbRow::Table(i, t) => (if d.expanded.contains(&format!("t{i}:{t}")) { "▾ " } else { "▸ " }, FINDER_MATCH),
                    crate::db_panel::DbRow::Column(..) => ("  ", FG),
                    crate::db_panel::DbRow::Info(_, s) => ("  ", if s.starts_with("誤り") { DIAG_ERROR } else { STATUS_FG }),
                };
                lines.push((depth, mark, text, note, color));
            }
            (d.selected, d.scroll)
        }
    };
    let body = r.h.saturating_sub(2);
    for (k, (depth, mark, text, note, color)) in lines.iter().enumerate().skip(scroll).take(body) {
        let y = y0 + (1 + k - scroll) as f32 * cell.h;
        let sel = k == selected;
        let reversed = sel && focused;
        if reversed {
            p.out.push(Instance::fill(x0, y, wpx, cell.h, FG));
        } else if sel {
            p.out.push(Instance::fill(x0, y, wpx, cell.h, MATCH_BG));
        }
        let c = p.text(&format!(" {}{mark}", "  ".repeat(*depth)), r.x, y, if reversed { BG } else { STATUS_FG });
        let c = p.text(text, c, y, if reversed { BG } else { *color });
        if !note.is_empty() {
            p.text(&format!("  {note}"), c, y, if reversed { BG } else { LINE_NO });
        }
    }
    // パスワードの入力（欄の下の行）
    if let Some((i, prompt, _)) = ed.db.as_ref().and_then(|d| d.password.as_ref()) {
        let y = y0 + (r.h - 1) as f32 * cell.h;
        p.out.push(Instance::fill(x0, y, wpx, cell.h, rgb(0x1c1c1c)));
        let user = ed.db.as_ref().map_or("", |d| d.sources[*i].user.as_str());
        let c = p.text(&format!(" {user} のパスワード: "), r.x, y, STATUS_FG);
        let c = p.text(&"●".repeat(prompt.text.chars().count()), c, y, FG);
        p.caret(c, y);
    }
    p.cols = saved;
}

/// クエリの結果の表（下のパネル）。1 行目は見出し、2 行目は列名、3 行目から行
fn draw_result_grid(p: &mut Painter, g: &crate::result_grid::ResultGrid, r: Rect, focused: bool) {
    let cell = p.fonts.cell();
    let (x0, y0, wpx) = (r.x as f32 * cell.w, r.y as f32 * cell.h, r.w as f32 * cell.w);
    p.out.push(Instance::fill(x0, y0, wpx, r.h as f32 * cell.h, BG));
    p.out.push(Instance::fill(x0, y0, wpx, 1.0, LINE_NO));
    let saved = p.cols;
    p.cols = (r.x + r.w).saturating_sub(3);
    let end = p.text(&format!("{}   ", g.title), r.x, y0, STATUS_FG);
    p.text(&g.info, end, y0, if g.error { DIAG_ERROR } else if g.running { STATUS_FG } else { FG });
    p.cols = r.x + r.w;
    if g.cols.is_empty() {
        p.cols = saved;
        return;
    }
    // 見える列（hscroll から、幅に収まるだけ）
    let mut xs = Vec::new();
    let mut x = r.x + 1;
    for c in g.hscroll..g.cols.len() {
        if x >= r.x + r.w {
            break;
        }
        xs.push((c, x));
        x += g.widths[c] + 3;
    }
    let hy = y0 + cell.h;
    for &(c, x) in &xs {
        p.text(&g.cols[c], x, hy, if c == g.col && focused { FG } else { FINDER_MATCH });
    }
    p.out.push(Instance::fill(x0, hy + cell.h - 1.0, wpx, 1.0, LINE_NO));
    let body = r.h.saturating_sub(2);
    for (k, row) in g.rows.iter().enumerate().skip(g.scroll).take(body) {
        let y = y0 + (2 + k - g.scroll) as f32 * cell.h;
        if k == g.row {
            p.out.push(Instance::fill(x0, y, wpx, cell.h, MATCH_BG));
        }
        for &(c, x) in &xs {
            let v = &row[c];
            let text = crate::result_grid::show(v);
            let sel = k == g.row && c == g.col && focused;
            if sel {
                p.out.push(Instance::fill((x - 1) as f32 * cell.w, y, (g.widths[c] + 2) as f32 * cell.w, cell.h, FG));
            }
            let lim = p.cols;
            p.cols = (x + g.widths[c]).min(r.x + r.w);
            p.text(&text, x, y, if sel { BG } else if v.is_none() { LINE_NO } else { FG });
            p.cols = lim;
            // 列の区切り
            let sx = (x + g.widths[c] + 1) as f32 * cell.w + cell.w * 0.5;
            p.out.push(Instance::fill(sx, y, 1.0, cell.h, LINE_NO));
        }
    }
    p.cols = saved;
}

/// コミットのパネル（左の欄の「コミット」タブ）。上から ブランチ、変えたファイル、メッセージの欄、amend、ボタン
fn draw_commit_panel(p: &mut Painter, ed: &Editor, c: &crate::commit_panel::CommitPanel, r: Rect) {
    use nagi_workers::git::FileState;
    let cell = p.fonts.cell();
    let (x0, y0, wpx) = (r.x as f32 * cell.w, r.y as f32 * cell.h, r.w as f32 * cell.w);
    p.out.push(Instance::fill(x0, y0, wpx, r.h as f32 * cell.h, BG));
    p.out.push(Instance::fill(((r.x + r.w) as f32 + 0.5) * cell.w, y0, 1.0, r.h as f32 * cell.h, LINE_NO));
    let focused = ed.mode == Mode::List && ed.list_side == crate::editor::Side::Commit;
    let editing = ed.mode == Mode::CommitMsg;
    let l = crate::commit_panel::layout(r.h);
    let row_y = |k: usize| y0 + k as f32 * cell.h;
    let saved = p.cols;
    p.cols = r.x + r.w;
    draw_left_tabs(p, r, crate::tree_panel::LeftTab::Commit, focused || editing, "");
    // ブランチと件数（動いている git があればその名前）
    let info = format!(
        " {}   変更 {}{}",
        ed.git_branch_label().unwrap_or_default(),
        c.items.len(),
        c.busy_label.as_ref().map_or(String::new(), |b| format!("   {b}"))
    );
    p.text(&info, r.x, row_y(l.info), STATUS_FG);
    if let Some((dest, list)) = &c.confirm_push {
        // 送る前の確認（一覧の場所に出す）
        p.text(&format!(" プッシュ: {dest}"), r.x, row_y(l.list), FG);
        p.text(&format!(" 送るコミット {} 件", list.len()), r.x, row_y(l.list + 1), STATUS_FG);
        for (k, s) in list.iter().take(l.list_h.saturating_sub(3)).enumerate() {
            p.text(&format!("   {s}"), r.x, row_y(l.list + 2 + k), FG);
        }
        p.text(" Enter / y で送る   Esc でやめる", r.x, row_y(l.list + l.list_h - 1), FG);
    } else {
        // 一覧（見出しの行を含めて並べる）
        let tracked = c.tracked();
        let mut lines: Vec<Option<usize>> = vec![None];
        lines.extend((0..tracked).map(Some));
        if tracked < c.items.len() {
            lines.push(None);
            lines.extend((tracked..c.items.len()).map(Some));
        }
        for (k, li) in lines.iter().enumerate().skip(c.scroll).take(l.list_h) {
            let y = row_y(l.list + k - c.scroll);
            match li {
                None => {
                    let label = if k == 0 { format!(" ▾ 変更 ({tracked})") } else { format!(" ▾ 未追跡 ({})", c.items.len() - tracked) };
                    p.text(&label, r.x, y, STATUS_FG);
                }
                Some(i) => {
                    let it = &c.items[*i];
                    let sel = *i == c.selected;
                    let reversed = sel && focused;
                    if reversed {
                        p.out.push(Instance::fill(x0, y, wpx, cell.h, FG));
                    } else if sel {
                        p.out.push(Instance::fill(x0, y, wpx, cell.h, MATCH_BG));
                    }
                    let (fg, dim) = if reversed { (BG, BG) } else { (FG, STATUS_FG) };
                    let state_color = if reversed {
                        BG
                    } else {
                        match it.change.state {
                            FileState::Added => GIT_ADD,
                            FileState::Deleted | FileState::Conflicted => GIT_DEL,
                            FileState::Untracked => STATUS_FG,
                            FileState::Renamed => rgb(0xd7af5f),
                            FileState::Modified => GIT_MOD,
                        }
                    };
                    let (dir, name) = it.change.path.rsplit_once('/').map_or(("", it.change.path.as_str()), |(d, n)| (d, n));
                    let mut c2 = p.text(&format!("   [{}] ", if it.checked { 'x' } else { ' ' }), r.x, y, fg);
                    c2 = p.text(&it.change.state.letter().to_string(), c2, y, state_color);
                    c2 = p.text(&format!(" {name}"), c2, y, fg);
                    p.text(&format!("  {dir}"), c2, y, dim);
                }
            }
        }
        if c.items.is_empty() {
            p.text("   変更はありません", r.x, row_y(l.list + 1), STATUS_FG);
        }
    }
    // メッセージの欄
    p.out.push(Instance::fill(x0, row_y(l.title) - cell.h * 0.5, wpx, 1.0, LINE_NO));
    let checked = c.items.iter().filter(|i| i.checked).count();
    p.text(&format!(" コミットメッセージ（{checked} ファイル）"), r.x, row_y(l.title), STATUS_FG);
    let mx = r.x + 1;
    let mw = r.w.saturating_sub(2);
    let box_top = row_y(l.msg);
    let (bx, bw, bh) = (mx as f32 * cell.w - 2.0, mw as f32 * cell.w + 4.0, l.msg_h as f32 * cell.h);
    let border = if editing { FG } else { LINE_NO };
    p.out.push(Instance::fill(bx, box_top - 1.0, bw, 1.0, border));
    p.out.push(Instance::fill(bx, box_top + bh, bw, 1.0, border));
    p.out.push(Instance::fill(bx, box_top - 1.0, 1.0, bh + 2.0, border));
    p.out.push(Instance::fill(bx + bw - 1.0, box_top - 1.0, 1.0, bh + 2.0, border));
    p.cols = mx + mw;
    if c.msg.is_empty() && !editing {
        p.text("c で書く（1 行目が件名）", mx, box_top, STATUS_FG);
    }
    let (cl, cc) = c.cursor_pos();
    let first = cl.saturating_sub(l.msg_h.saturating_sub(1));
    for (k, line) in c.msg.split('\n').enumerate().skip(first).take(l.msg_h) {
        let y = box_top + (k - first) as f32 * cell.h;
        p.text(line, mx, y, FG);
        if editing && k == cl {
            let col = mx + p.width(&line[..cc.min(line.len())]);
            let pre = if ed.preedit.is_empty() { 0 } else { p.width(&ed.preedit) };
            if pre > 0 {
                let mut items = p.items(&ed.preedit, FG);
                items.iter_mut().for_each(|i| i.underline = true);
                p.draw(&items, col, y);
            }
            p.caret(col + pre, y);
        }
    }
    p.cols = r.x + r.w;
    // amend とボタン（右寄せ）
    let ay = row_y(l.amend);
    p.text(&format!(" [{}] 直前のコミットを直す（A）", if c.amend { 'x' } else { ' ' }), r.x, ay, STATUS_FG);
    let by = row_y(l.buttons);
    for (label, x, w, push) in crate::commit_panel::buttons(r.w) {
        let bg = if push { MATCH_BG } else { FG };
        p.out.push(Instance::fill((r.x + x) as f32 * cell.w, by, w as f32 * cell.w, cell.h, bg));
        p.text(label, r.x + x, by, if push { FG } else { BG });
    }
    p.cols = saved;
}

/// ブランチのポップアップ（画面の上の中央。IntelliJ の Git Branches と同じ並び）
fn draw_branch_popup(p: &mut Painter, b: &crate::branches::BranchPopup, rows: usize, cols: usize) {
    use crate::branches::Entry;
    let cell = p.fonts.cell();
    let w = 56.min(cols.saturating_sub(4));
    let list_h = b.entries.len().min(rows.saturating_sub(8)).max(1);
    let h = list_h + 1;
    let x0 = (cols.saturating_sub(w)) / 2;
    let top = 2;
    let (x, y) = (x0 as f32 * cell.w, top as f32 * cell.h);
    p.out.push(Instance::fill(x - 1.0, y - 1.0, w as f32 * cell.w + 2.0, h as f32 * cell.h + 2.0, STATUS_FG));
    p.out.push(Instance::fill(x, y, w as f32 * cell.w, h as f32 * cell.h, rgb(0x141414)));
    let saved = p.cols;
    p.cols = x0 + w;
    // 上の欄: 名前を打っているときはその欄、そうでなければ絞り込み
    let (label, text) = match &b.naming {
        Some((crate::branches::Naming::New(from), pr)) => (format!(" 新しいブランチ{}: ", from.as_ref().map_or(String::new(), |f| format!("（{f} から）"))), pr.text.clone()),
        Some((crate::branches::Naming::Rename(old), pr)) => (format!(" {old} の新しい名前: "), pr.text.clone()),
        None => (" 🔍 ".to_string(), b.query.text.clone()),
    };
    let c = p.text(&label, x0, y, STATUS_FG);
    let c = p.text(&text, c, y, FG);
    p.out.push(Instance::fill(c as f32 * cell.w, y + 2.0, 2.0, cell.h - 4.0, FG));
    p.out.push(Instance::fill(x, y + cell.h - 1.0, w as f32 * cell.w, 1.0, LINE_NO));
    // 一覧（選んだ行が見えるように流す）
    let scroll = (b.selected + 1).saturating_sub(list_h);
    for (k, e) in b.entries.iter().enumerate().skip(scroll).take(list_h) {
        let yy = y + (1 + k - scroll) as f32 * cell.h;
        let sel = k == b.selected && b.naming.is_none();
        if sel {
            p.out.push(Instance::fill(x, yy, w as f32 * cell.w, cell.h, rgb(0x2c4a70)));
        }
        match e {
            Entry::NewBranch => {
                p.text(" ＋ 新しいブランチ…", x0, yy, FG);
            }
            Entry::Fetch => {
                p.text(" ⟳ フェッチ", x0, yy, FG);
            }
            Entry::Header(t) => {
                p.text(&format!(" {t}"), x0, yy, STATUS_FG);
            }
            Entry::Branch(i) => {
                let br = &b.branches[*i];
                let mark = if br.current { "●" } else { " " };
                let c = p.text(&format!("   {mark} "), x0, yy, GIT_ADD);
                let c = p.text(&br.name, c, yy, if br.current { rgb(0xe8e8e8) } else { FG });
                if !br.track.is_empty() {
                    let t = br.track.replace("ahead", "↑").replace("behind", "↓").replace(['[', ']'], "");
                    p.text(&format!("  {t}"), c, yy, STATUS_FG);
                }
                if sel && b.ops.is_none() {
                    p.text("›", x0 + w - 2, yy, FG);
                }
            }
        }
    }
    p.cols = saved;
    // 操作の一覧（選んだ行の右に）
    if let Some((ops, osel)) = &b.ops {
        let cur = b.current_name();
        let labels: Vec<String> = ops.iter().map(|o| format!(" {} ", o.label(&cur))).collect();
        let ow = labels.iter().map(|l| p.width(l)).max().unwrap_or(10);
        let ox = (x0 + w + 1).min(cols.saturating_sub(ow + 1));
        let oy = top + 1 + b.selected.saturating_sub(scroll);
        let (fx, fy) = (ox as f32 * cell.w, oy as f32 * cell.h);
        p.out.push(Instance::fill(fx - 1.0, fy - 1.0, ow as f32 * cell.w + 2.0, labels.len() as f32 * cell.h + 2.0, STATUS_FG));
        p.out.push(Instance::fill(fx, fy, ow as f32 * cell.w, labels.len() as f32 * cell.h, rgb(0x1c1c1c)));
        let saved = p.cols;
        p.cols = ox + ow;
        for (k, l) in labels.iter().enumerate() {
            let yy = fy + k as f32 * cell.h;
            if k == *osel {
                p.out.push(Instance::fill(fx, yy, ow as f32 * cell.w, cell.h, rgb(0x2c4a70)));
            }
            p.text(l, ox, yy, FG);
        }
        p.cols = saved;
    }
}

/// 履歴（窓の領域全体）。左にブランチ、中央にグラフ付きのコミット、右に選んだコミットの中身
fn draw_log_view(p: &mut Painter, l: &crate::log_view::LogView, r: Rect) {
    const LANES: [[f32; 4]; 5] = [rgb(0x87afd7), rgb(0xd7af87), rgb(0x87af87), rgb(0xd787af), rgb(0xafafd7)];
    let cell = p.fonts.cell();
    let (x0, y0) = (r.x as f32 * cell.w, r.y as f32 * cell.h);
    p.out.push(Instance::fill(x0, y0, r.w as f32 * cell.w, r.h as f32 * cell.h, BG));
    let bw = 20.min(r.w / 6);
    let dw = 42.min(r.w / 3);
    let cx = r.x + bw + 1;
    let cw = r.w.saturating_sub(bw + dw + 2);
    let dx = cx + cw + 1;
    // 上の行: 絞り込みと件数
    let n = l.visible.iter().filter(|&&i| l.rows[i].commit.is_some()).count();
    let head = format!(
        " {}{}   {} 件{}",
        if l.filter.text.is_empty() { "/ で絞り込み".to_string() } else { format!("/{}", l.filter.text) },
        l.path.as_ref().map_or(String::new(), |p| format!("   ファイル: {p}")),
        n,
        if l.loading { " …" } else { "" }
    );
    p.text(&head, r.x, y0, if l.filtering { FG } else { STATUS_FG });
    p.out.push(Instance::fill(x0, y0 + cell.h - 1.0, r.w as f32 * cell.w, 1.0, LINE_NO));
    let body = r.h.saturating_sub(1);
    // 左: ブランチ
    let saved = p.cols;
    p.cols = r.x + bw;
    let mut y = y0 + cell.h;
    let mut last_sec = "";
    for (sec, name, cur) in &l.branches {
        if y >= y0 + body as f32 * cell.h {
            break;
        }
        if sec != last_sec {
            p.text(&format!(" {sec}"), r.x, y, STATUS_FG);
            y += cell.h;
            last_sec = sec;
        }
        p.text(&format!(" {} {name}", if *cur { '*' } else { ' ' }), r.x, y, if *cur { FG } else { STATUS_FG });
        y += cell.h;
    }
    p.cols = saved;
    p.out.push(Instance::fill((r.x + bw) as f32 * cell.w + cell.w * 0.5, y0 + cell.h, 1.0, body as f32 * cell.h, LINE_NO));
    // 中央: コミット
    let saved = p.cols;
    p.cols = cx + cw;
    for (k, &i) in l.visible.iter().enumerate().skip(l.top).take(body) {
        let row = &l.rows[i];
        let y = y0 + (1 + k - l.top) as f32 * cell.h;
        let sel = k == l.selected;
        let focused = sel && !l.focus_files;
        if focused {
            p.out.push(Instance::fill(cx as f32 * cell.w, y, cw as f32 * cell.w, cell.h, FG));
        } else if sel {
            p.out.push(Instance::fill(cx as f32 * cell.w, y, cw as f32 * cell.w, cell.h, MATCH_BG));
        }
        let mut c = cx + 1;
        for (j, ch) in crate::log_view::pretty_graph(&row.graph).chars().enumerate() {
            let mut b = [0u8; 4];
            let color = if focused { BG } else { LANES[(j / 2) % LANES.len()] };
            c = p.text(ch.encode_utf8(&mut b), c, y, color);
        }
        if let Some(cm) = &row.commit {
            let (fg, dim, refc) = if focused { (BG, BG, BG) } else { (FG, STATUS_FG, rgb(0xd7af87)) };
            // 作者と日時は表示の幅で揃える（全角は 2 セル）
            let pad = |p: &Painter, s: &str, w: usize| {
                let t: String = s.chars().scan(0, |acc, ch| {
                    *acc += p.width(ch.encode_utf8(&mut [0u8; 4]));
                    (*acc <= w).then_some(ch)
                }).collect();
                let tw = p.width(&t);
                format!("{t}{}", " ".repeat(w.saturating_sub(tw)))
            };
            let right = format!("{} {} {}", pad(p, &cm.author, 10), pad(p, &cm.date, 9), cm.short);
            let rw = p.width(&right);
            let mut c2 = c + 1;
            if !cm.refs.is_empty() {
                c2 = p.text(&format!("{}  ", cm.refs), c2, y, refc);
            }
            let limit = (cx + cw).saturating_sub(rw + 1);
            let saved2 = p.cols;
            p.cols = limit.saturating_sub(1);
            p.text(&cm.subject, c2, y, fg);
            p.cols = saved2;
            p.text(&right, limit, y, dim);
        }
    }
    p.cols = saved;
    p.out.push(Instance::fill((cx + cw) as f32 * cell.w + cell.w * 0.5, y0 + cell.h, 1.0, body as f32 * cell.h, LINE_NO));
    // 右: 選んだコミットの中身
    let saved = p.cols;
    p.cols = r.x + r.w;
    if let Some(d) = &l.detail {
        let mut y = y0 + cell.h;
        let line = |p: &mut Painter, y: &mut f32, s: &str, color: [f32; 4]| {
            p.text(s, dx + 1, *y, color);
            *y += cell.h;
        };
        line(p, &mut y, &format!("{}   {}   {}", &d.hash[..7.min(d.hash.len())], d.author, d.date), STATUS_FG);
        line(p, &mut y, &format!("親 {}", d.parents.split(' ').map(|h| &h[..7.min(h.len())]).collect::<Vec<_>>().join(" ")), STATUS_FG);
        y += cell.h * 0.5;
        for (k, m) in d.message.lines().enumerate().take(8) {
            line(p, &mut y, m, if k == 0 { rgb(0xe8e8e8) } else { STATUS_FG });
        }
        y += cell.h * 0.5;
        line(p, &mut y, &format!("変えたファイル（{}）", d.files.len()), STATUS_FG);
        let limit = y0 + r.h as f32 * cell.h;
        for (k, (st, path)) in d.files.iter().enumerate() {
            if y + cell.h > limit {
                break;
            }
            let sel = l.focus_files && k == l.file_sel;
            if sel {
                p.out.push(Instance::fill(dx as f32 * cell.w, y, (r.x + r.w - dx) as f32 * cell.w, cell.h, FG));
            }
            let sc = if sel {
                BG
            } else {
                match st {
                    'A' => GIT_ADD,
                    'D' => GIT_DEL,
                    'M' => GIT_MOD,
                    _ => rgb(0xd7af5f),
                }
            };
            let c = p.text(&format!("{st}"), dx + 1, y, sc);
            p.text(&format!("  {path}"), c, y, if sel { BG } else { FG });
            y += cell.h;
        }
    } else if !l.rows.is_empty() {
        p.text("…", dx + 1, y0 + cell.h, STATUS_FG);
    }
    p.cols = saved;
}

/// 左右並べの差分（窓の領域全体）。足した行は緑、消した行は赤、変えた行は青の地、対の行は変わった部分を濃く
fn draw_diff_view(p: &mut Painter, d: &crate::diff_view::DiffView, r: Rect) {
    use crate::diff_view::RowKind;
    let cell = p.fonts.cell();
    let (x0, y0) = (r.x as f32 * cell.w, r.y as f32 * cell.h);
    p.out.push(Instance::fill(x0, y0, r.w as f32 * cell.w, r.h as f32 * cell.h, BG));
    let half = r.w.saturating_sub(1) / 2;
    let rx = r.x + half + 1;
    p.text(&format!(" {}", d.left_title), r.x, y0, STATUS_FG);
    p.text(&format!(" {}", d.right_title), rx, y0, FG);
    p.out.push(Instance::fill(x0, y0 + cell.h - 1.0, r.w as f32 * cell.w, 1.0, LINE_NO));
    let body = r.h.saturating_sub(1);
    let mod_bg = rgb(0x17202e);
    let fill_bg = rgb(0x141414);
    for (k, row) in d.rows.iter().enumerate().skip(d.top).take(body) {
        let y = y0 + (1 + k - d.top) as f32 * cell.h;
        let (lbg, rbg) = match row.kind {
            RowKind::Eq => (BG, BG),
            RowKind::Add => (fill_bg, DIFF_ADD_BG),
            RowKind::Del => (DIFF_DEL_BG, fill_bg),
            RowKind::Mod => (mod_bg, mod_bg),
        };
        p.out.push(Instance::fill(x0, y, half as f32 * cell.w, cell.h, lbg));
        p.out.push(Instance::fill(rx as f32 * cell.w, y, (r.w - half - 1) as f32 * cell.w, cell.h, rbg));
        let spans = match (&row.left, &row.right, row.kind) {
            (Some(l), Some(n), RowKind::Mod) => {
                let (a, b) = nagi_core::linediff::changed_span(&l.1, &n.1);
                (Some(a), Some(b))
            }
            _ => (None, None),
        };
        let cursor = k == d.cursor;
        for (side, col, w, span, word) in [(&row.left, r.x, half, spans.0, DIFF_WORD_DEL_BG), (&row.right, rx, r.w - half - 1, spans.1, DIFF_WORD_BG)] {
            let saved = p.cols;
            p.cols = col + w;
            if let Some((n, text)) = side {
                let num = format!("{:>5} ", n + 1);
                let c = if cursor {
                    p.out.push(Instance::fill(col as f32 * cell.w, y, 5.0 * cell.w, cell.h, FG));
                    p.text(&num, col, y, BG)
                } else {
                    p.text(&num, col, y, LINE_NO)
                };
                let mut items = Vec::new();
                for (b, g) in text.grapheme_indices(true) {
                    let width = p.cell_width(g);
                    let hit = span.as_ref().is_some_and(|s| b >= s.start && b < s.end);
                    items.push(Item { g: Cow::Borrowed(g), width, fg: FG, bg: if hit { Some(word) } else { None }, underline: false });
                }
                p.draw(&items, c, y);
            } else if cursor {
                p.out.push(Instance::fill(col as f32 * cell.w, y, 5.0 * cell.w, cell.h, MATCH_BG));
            }
            p.cols = saved;
        }
    }
    p.out.push(Instance::fill((r.x + half) as f32 * cell.w + cell.w * 0.5, y0 + cell.h, 1.0, body as f32 * cell.h, LINE_NO));
}

/// blame: カーソルの行のコミットの詳しい内容（カーソルの行の下に枠で）
fn draw_blame_popup(p: &mut Painter, ed: &Editor, rows: usize) {
    let Some(c) = ed.blame_at_cursor() else { return };
    if c.uncommitted() {
        return;
    }
    let cell = p.fonts.cell();
    let rect = ed.active_rect();
    if ed.cursor.line < ed.top {
        return;
    }
    let row = rect.y + (ed.cursor.line - ed.top) + 1;
    let lines = [
        format!(" {}   {}   {}", &c.hash[..7], c.author, crate::blame::relative_time(c.time, crate::blame::now_secs())),
        format!(" {}", c.summary),
        " :Gblame で閉じる".to_string(),
    ];
    let h = lines.len();
    let top = if row + h <= rows { row } else { row.saturating_sub(h + 1) };
    let x0 = rect.x + crate::blame::BLAME_W + 2;
    let w = lines.iter().map(|l| p.width(l)).max().unwrap_or(10) + 2;
    let w = w.min(rect.w.saturating_sub(crate::blame::BLAME_W + 3));
    let (x, y) = (x0 as f32 * cell.w, top as f32 * cell.h);
    p.out.push(Instance::fill(x - 1.0, y - 1.0, w as f32 * cell.w + 2.0, h as f32 * cell.h + 2.0, STATUS_FG));
    p.out.push(Instance::fill(x, y, w as f32 * cell.w, h as f32 * cell.h, rgb(0x141414)));
    let saved = p.cols;
    p.cols = x0 + w;
    for (k, l) in lines.iter().enumerate() {
        let color = if k == 1 { rgb(0xe8e8e8) } else { STATUS_FG };
        p.text(l, x0, y + k as f32 * cell.h, color);
    }
    p.cols = saved;
}

/// 行頭のマーカーから出る差分のポップアップ（変更のまとまりの下。古い行は赤、新しい行は緑の地、変わった部分を濃く）
fn draw_hunk_popup(p: &mut Painter, ed: &Editor, rows: usize, cols: usize) {
    const MAX_LINES: usize = 12;
    let Some((old, new, h)) = ed.popup_lines() else { return };
    let cell = p.fonts.cell();
    let rect = ed.active_rect();
    let last = if new.is_empty() { h.new.start.saturating_sub(1) } else { h.new.end - 1 };
    if last < ed.top {
        return;
    }
    let below = rect.y + (last - ed.top) + 1;
    let x0 = rect.x + ed.gutter() - 1;
    let w = rect.w.saturating_sub(ed.gutter()).min(110).max(40).min(cols.saturating_sub(x0));
    let n_old = old.len().min(MAX_LINES);
    let n_new = new.len().min(MAX_LINES);
    let h_rows = 1 + n_old + n_new;
    // 下に入らなければ、まとまりの上に出す
    let top = if below + h_rows <= rows { below } else { (rect.y + h.new.start.saturating_sub(ed.top)).saturating_sub(h_rows) };
    let (x, y0) = (x0 as f32 * cell.w, top as f32 * cell.h);
    p.out.push(Instance::fill(x - 1.0, y0 - 1.0, w as f32 * cell.w + 2.0, h_rows as f32 * cell.h + 2.0, LINE_NO));
    p.out.push(Instance::fill(x, y0, w as f32 * cell.w, h_rows as f32 * cell.h, rgb(0x141414)));
    let saved = p.cols;
    p.cols = x0 + w;
    let total = ed.git.popup.map(|i| i + 1).unwrap_or(0);
    let kind = match h.kind {
        nagi_core::linediff::HunkKind::Add => format!("{} 行を足した", new.len()),
        nagi_core::linediff::HunkKind::Del => format!("{} 行を消した", old.len()),
        nagi_core::linediff::HunkKind::Mod => format!("{} 行を変えた", new.len().max(old.len())),
    };
    p.text(&format!(" 変更 {total}   {kind}     [c 前  ]c 次  u 戻す  s ステージ  Esc 閉じる"), x0, y0, STATUS_FG);
    let mut y = y0 + cell.h;
    // 対になる行は変わった部分を濃く塗る
    for (i, l) in old.iter().take(n_old).enumerate() {
        p.out.push(Instance::fill(x, y, w as f32 * cell.w, cell.h, DIFF_DEL_BG));
        let span = new.get(i).map(|n| nagi_core::linediff::changed_span(l, n).0);
        draw_diff_line(p, "−", l, span, DIFF_WORD_DEL_BG, GIT_DEL, x0, y);
        y += cell.h;
    }
    for (i, l) in new.iter().take(n_new).enumerate() {
        p.out.push(Instance::fill(x, y, w as f32 * cell.w, cell.h, DIFF_ADD_BG));
        let span = old.get(i).map(|o| nagi_core::linediff::changed_span(o, l).1);
        draw_diff_line(p, "+", l, span, DIFF_WORD_BG, GIT_ADD, x0, y);
        y += cell.h;
    }
    p.cols = saved;
}

/// 差分の 1 行（先頭の印と、変わった部分の地）
#[allow(clippy::too_many_arguments)]
fn draw_diff_line(p: &mut Painter, sign: &str, text: &str, span: Option<std::ops::Range<usize>>, word_bg: [f32; 4], sign_color: [f32; 4], col: usize, y: f32) {
    let lead = format!(" {sign} ");
    let mut items = p.items(&lead, sign_color);
    for (b, g) in text.grapheme_indices(true) {
        let width = p.cell_width(g);
        let hit = span.as_ref().is_some_and(|s| b >= s.start && b < s.end);
        items.push(Item { g: Cow::Borrowed(g), width, fg: FG, bg: if hit { Some(word_bg) } else { None }, underline: false });
    }
    p.draw(&items, col, y);
}

/// `:` のコマンドラインの候補（最下行のすぐ上に最大 10 件。補う語の位置から）
fn draw_cmd_complete(p: &mut Painter, ed: &Editor, c: &crate::cmdline_complete::CmdComplete, rows: usize, cols: usize) {
    let max = crate::cmdline_complete::MAX_SHOWN.min(rows);
    let cell = p.fonts.cell();
    let n = c.items.len().min(max);
    let first = c.sel.map_or(0, |s| s.saturating_sub(max - 1));
    let shown: Vec<&(String, String)> = c.items.iter().skip(first).take(n).collect();
    let name_w = shown.iter().map(|(a, _)| p.width(a)).max().unwrap_or(1);
    let desc_w = shown.iter().map(|(_, d)| p.width(d)).max().unwrap_or(0);
    let width = (name_w + desc_w + if desc_w > 0 { 5 } else { 2 }).min(cols);
    let col = (1 + p.width(&ed.cmdline.text[..c.start.min(ed.cmdline.text.len())])).min(cols.saturating_sub(width));
    let top = rows - n;
    for (k, (name, desc)) in shown.iter().enumerate() {
        let y = (top + k) as f32 * cell.h;
        let selected = c.sel == Some(first + k);
        let (bg, fg, dim) = if selected { (FG, BG, BG) } else { (MATCH_BG, FG, STATUS_FG) };
        p.out.push(Instance::fill(col as f32 * cell.w, y, width as f32 * cell.w, cell.h, bg));
        let end = p.text(&format!(" {name}"), col, y, fg);
        if !desc.is_empty() {
            p.text(desc, end.max(col + name_w + 1) + 2, y, dim);
        }
    }
    // 件数が多いときは、残りの数を一覧の右上に
    if c.items.len() > n {
        let more = format!(" {}/{} ", c.sel.map_or(0, |s| s + 1), c.items.len());
        let x = (col + width).saturating_sub(p.width(&more));
        p.text(&more, x, (top as f32 - 1.0).max(0.0) * cell.h, STATUS_FG);
    }
}

/// 補完の候補（カーソルの下に最大 10 件。下に余白が無ければ上に）。ターミナルの Vim の補完メニューと同じ形
fn draw_completion(p: &mut Painter, ed: &Editor, c: &crate::complete::Completion, rows: usize, cols: usize) {
    const MAX: usize = 10;
    let cell = p.fonts.cell();
    let rect = ed.active_rect();
    let (sl, sb) = ed.buf.pos_of(c.start);
    if sl < ed.top {
        return;
    }
    let row = rect.y + (sl - ed.top);
    let line = ed.buf.line_head(sl, sb + 4).0;
    let col = (rect.x + ed.gutter() + nagi_core::display_col(&line, sb.min(line.len()), false)).saturating_sub(1);
    let n = c.items.len().min(MAX);
    // 選んでいる候補が見える範囲
    let first = c.sel.map_or(0, |s| s.saturating_sub(MAX - 1));
    let width = c.items.iter().skip(first).take(n).map(|i| p.width(i)).max().unwrap_or(1) + 2;
    let col = col.min(cols.saturating_sub(width));
    let below = row + 1 + n <= rows;
    let top = if below { row + 1 } else { row.saturating_sub(n) };
    for (k, item) in c.items.iter().skip(first).take(n).enumerate() {
        let y = (top + k) as f32 * cell.h;
        let selected = c.sel == Some(first + k);
        let (bg, fg) = if selected { (FG, BG) } else { (MATCH_BG, FG) };
        p.out.push(Instance::fill(col as f32 * cell.w, y, width as f32 * cell.w, cell.h, bg));
        p.text(&format!(" {item}"), col, y, fg);
    }
}

/// ファイルツリー。1 行目はタブと根の名前、2 行目から字下げした木（フォルダは青で `▸` `▾`）。右端に 1px の区切り線
fn draw_tree(p: &mut Painter, t: &crate::tree_panel::TreePanel, r: Rect, focused: bool, current: Option<&std::path::Path>) {
    let cell = p.fonts.cell();
    let (x, y0, w) = (r.x as f32 * cell.w, r.y as f32 * cell.h, r.w as f32 * cell.w);
    p.out.push(Instance::fill(x, y0, w, r.h as f32 * cell.h, BG));
    p.out.push(Instance::fill(((r.x + r.w) as f32 + 0.5) * cell.w, y0, 1.0, r.h as f32 * cell.h, LINE_NO));
    let saved = p.cols;
    p.cols = r.x + r.w;
    let name = t.root.file_name().map_or(t.root.display().to_string(), |n| n.to_string_lossy().into_owned());
    draw_left_tabs(p, r, crate::tree_panel::LeftTab::Project, focused, &format!("{name}/{}", if t.loading() { " …" } else { "" }));
    let current = current.and_then(|c| std::path::absolute(c).ok());
    for k in 0..r.h.saturating_sub(1) {
        let Some(row) = t.rows.get(t.scroll + k) else { break };
        let y = (r.y + 1 + k) as f32 * cell.h;
        let selected = t.scroll + k == t.selected;
        let reversed = selected && focused;
        if reversed {
            p.out.push(Instance::fill(x, y, w, cell.h, FG));
        } else if selected {
            p.out.push(Instance::fill(x, y, w, cell.h, MATCH_BG));
        }
        let mark = if row.dir { if t.is_expanded(&row.path) { "▾ " } else { "▸ " } } else { "  " };
        let is_current = current.as_deref() == Some(row.path.as_path());
        let fg = if reversed { BG } else if row.dir { FINDER_MATCH } else if is_current { FG } else { STATUS_FG };
        let label = format!("{}{mark}{}{}", "  ".repeat(row.depth), row.name, if row.dir { "/" } else { "" });
        p.text(&label, r.x, y, fg);
    }
    p.cols = saved;
}

/// 全文検索のパネル。1 行目は見出し（検索語と件数）、2 行目から `path:行: 本文`（ripgrep の vimgrep と同じ形）
fn draw_grep_panel(p: &mut Painter, g: &crate::grep_panel::GrepPanel, r: Rect, focused: bool) {
    let cell = p.fonts.cell();
    let (x, y0, w) = (r.x as f32 * cell.w, r.y as f32 * cell.h, r.w as f32 * cell.w);
    p.out.push(Instance::fill(x, y0, w, r.h as f32 * cell.h, BG));
    p.out.push(Instance::fill(x, y0, w, 1.0, LINE_NO));
    let head = match (&g.error, &g.title) {
        (Some(e), _) => format!("正規表現の誤り: {}", e.lines().last().unwrap_or("")),
        (None, Some(t)) => format!("{t}   {} 件{}", g.hits.len(), if g.searching() { " …" } else { "" }),
        (None, None) => format!(
            "全文検索: {}{}   {} 件 / {} ファイル{}{}",
            g.query.text,
            if g.regex { "  [.*]" } else { "" },
            g.hits.len(),
            g.files(),
            if g.hits.len() >= nagi_workers::grep::MAX_HITS { "（上限）" } else { "" },
            if g.searching() { " …" } else { "" }
        ),
    };
    let saved = p.cols;
    p.cols = (r.x + r.w).saturating_sub(3);
    p.text(&head, r.x, y0, STATUS_FG);
    p.cols = saved;
    for k in 0..r.h.saturating_sub(1) {
        let Some(h) = g.hits.get(g.scroll + k) else { break };
        let y = (r.y + 1 + k) as f32 * cell.h;
        let selected = g.scroll + k == g.selected;
        // 選んだ行は、パネルにいる（か検索語を打っている）ときは反転、そうでなければ控えめな地の色
        let reversed = selected && focused;
        if reversed {
            p.out.push(Instance::fill(x, y, w, cell.h, FG));
        } else if selected {
            p.out.push(Instance::fill(x, y, w, cell.h, MATCH_BG));
        }
        let (fg, dim, accent) = if reversed { (BG, BG, BG) } else { (FG, STATUS_FG, FINDER_MATCH) };
        // ファイルを持たない行（コマンドの出力）は本文だけ
        let mut items = Vec::new();
        let num = format!(":{}: ", h.line);
        if !h.path.is_empty() {
            items.extend(p.items(&h.path, dim));
            items.extend(p.items(&num, dim));
        }
        for (b, gr) in h.text.grapheme_indices(true) {
            let m = b >= h.range.0 && b < h.range.1;
            let width = p.cell_width(gr);
            items.push(Item { g: Cow::Borrowed(gr), width, fg: if m { accent } else { fg }, bg: None, underline: m && reversed });
        }
        p.draw(&items, r.x, y);
    }
}

/// ファインダーの候補を出す高さ（画面の 4 割、少なくとも 8 行）
fn finder_height(rows: usize) -> usize {
    (rows * 2 / 5).max(8).min(rows)
}

/// 当たった文字の色（ハイライトのキーワードと同じ控えめな青）
const FINDER_MATCH: [f32; 4] = rgb(0x87afd7);

/// ファインダーの候補。最下行のすぐ上が一番良い候補で、上へ行くほど点数が下がる（fzf と同じ並び）
fn draw_finder(p: &mut Painter, f: &crate::finder::Finder, rows: usize, cols: usize) {
    let cell = p.fonts.cell();
    let h = finder_height(rows);
    let top = rows - h;
    // 下の本文を隠し、上端に 1px の線を引く
    p.out.push(Instance::fill(0.0, top as f32 * cell.h, cols as f32 * cell.w, h as f32 * cell.h, BG));
    p.out.push(Instance::fill(0.0, top as f32 * cell.h, cols as f32 * cell.w, 1.0, LINE_NO));
    let q = nagi_core::fuzzy::Query::new(&f.query.text);
    for k in 0..h {
        let Some(&(i, _)) = f.hits.get(f.scroll + k) else { break };
        let text = &f.items[i as usize];
        let y = (rows - 1 - k) as f32 * cell.h;
        let selected = f.scroll + k == f.selected;
        let hit: std::collections::HashSet<usize> = if q.is_empty() { Default::default() } else { q.positions(text).into_iter().collect() };
        if selected {
            p.out.push(Instance::fill(0.0, y, cols as f32 * cell.w, cell.h, FG));
        }
        let (fg, accent) = if selected { (BG, BG) } else { (FG, FINDER_MATCH) };
        let mut items = p.items(if selected { "> " } else { "  " }, fg);
        let mut ci = 0;
        for g in text.graphemes(true) {
            let m = hit.contains(&ci);
            let width = p.cell_width(g);
            items.push(Item { g: Cow::Borrowed(g), width, fg: if m { accent } else { fg }, bg: None, underline: m && selected });
            ci += g.chars().count();
        }
        p.draw(&items, 0, y);
    }
}

/// 最下行の入力欄（`:` `/` `?` `→`）。`active` なら縦棒カーソルと IME の変換中の文字列を出す
fn prompt_line(p: &mut Painter, lead: &str, text: &str, cursor: usize, preedit: &str, y: f32, active: bool) {
    let c = p.text(lead, 0, y, if active { FG } else { STATUS_FG });
    let mut items = p.items(&text[..cursor], FG);
    let caret = c + items.iter().map(|i| i.width).sum::<usize>();
    let mut pre = Vec::new();
    if active && !preedit.is_empty() {
        pre = p.items(preedit, FG);
        pre.iter_mut().for_each(|i| i.underline = true);
    }
    let pre_w: usize = pre.iter().map(|i| i.width).sum();
    items.extend(pre);
    items.extend(p.items(&text[cursor..], FG));
    p.draw(&items, c, y);
    if active {
        p.caret(caret + pre_w, y);
    }
}
