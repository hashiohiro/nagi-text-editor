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
        p.text(&format!("{:>5} ", ln + 1), rect.x, y, num_color);
        // 閉じた折りたたみは 1 行で「+-- 12 行: 頭の行」と薄く出す（Vim の Folded と同じ形）
        if let Some((s, e)) = pane.folds.outer_at(ln).filter(|f| f.0 == ln) {
            let x = (rect.x + GUTTER) as f32 * cell.w;
            p.out.push(Instance::fill(x, y, (rect.w.saturating_sub(GUTTER)) as f32 * cell.w, cell.h, MATCH_BG));
            let label = format!("+--{:>3} 行: {}", e - s + 1, heads[row].0.trim());
            let saved = p.cols;
            p.cols = rect.x + rect.w;
            let end = p.text(&label, rect.x + GUTTER, y, STATUS_FG);
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
        if !is_cursor_line || matches!(ed.mode, Mode::Command | Mode::Search | Mode::Replace | Mode::Finder | Mode::GrepPrompt | Mode::List) {
            p.draw(&items, rect.x + GUTTER, y);
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
            p.draw(&items, rect.x + GUTTER, y);
            p.caret(rect.x + GUTTER + before + caret_in_pre, y);
        } else {
            // ブロックカーソル（文字を反転）。空行や行末では 1 セルの塗り
            // 選択の中では反転をもう一度反転して元の色に戻す（ターミナルの Vim と同じ見え方。行単位の Visual でも位置が分かる）
            match cursor_item {
                Some(i) if (visual.is_some() || block.is_some()) && items[i].fg == BG && items[i].bg.is_some() => {
                    items[i].fg = items[i].bg.take().unwrap_or(FG);
                    p.draw(&items, rect.x + GUTTER, y);
                }
                Some(i) => {
                    items[i].bg = Some(CURSOR);
                    items[i].fg = BG;
                    p.draw(&items, rect.x + GUTTER, y);
                }
                None => {
                    let c = p.draw(&items, rect.x + GUTTER, y);
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
    if let Some(f) = &mut ed.finder {
        // 選んでいる候補が見える範囲に入るように送る
        let h = finder_height(rows);
        if f.selected < f.scroll {
            f.scroll = f.selected;
        } else if f.selected >= f.scroll + h {
            f.scroll = f.selected + 1 - h;
        }
    }
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
        let pane = Pane { buf, diags: ed.diag_lines(buf.path.as_deref()), folds, syntax, cursor, top: st.top.min(last), frac: if active { ed.frac } else { 0.0 }, active };
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
    if let (Some(r), Some(g)) = (ed.panel_rect(), &ed.grep) {
        draw_grep_panel(&mut p, g, r, matches!(ed.mode, Mode::List | Mode::GrepPrompt));
    }
    if let Some(f) = &ed.finder {
        draw_finder(&mut p, f, rows, cols);
    }
    if let (Some(c), Mode::Insert) = (&ed.completion, ed.mode) {
        draw_completion(&mut p, ed, c, rows, cols);
    }
    match ed.stripe {
        Some(crate::editor::Side::Left) => draw_stripe_left(&mut p, ed.windows_rows(), ed.tree.is_some()),
        Some(crate::editor::Side::Bottom) => draw_stripe_bottom(&mut p, rows, cols, ed.grep.is_some()),
        None => {}
    }
    // ---- 最下行: 左にモード・メッセージ・入力欄、右に位置（Vim の showmode と ruler と同じ形）
    let y = rows as f32 * cell.h;
    // はみ出した本文の最終行（スムーズスクロール中）を最下行の地で隠す
    p.out.push(Instance::fill(0.0, y, w as f32, h as f32 - y, BG));
    let name = ed.buf.path.as_ref().map_or("[No Name]".to_string(), |p| p.display().to_string());
    let keys = format!("{}{}", ed.pending_keys.iter().map(|k| k.label()).collect::<String>(), ed.showcmd);
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
        Mode::List if ed.list_side == crate::editor::Side::Left => {
            p.text(if ed.message.is_empty() { "-- ツリー --" } else { &ed.message }, 0, y, FG);
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

/// 左のストライプ（マウスを左端に寄せている間だけ）。幅 2 セルの帯に「ツリー」を縦に書く。開いているパネルは反転
fn draw_stripe_left(p: &mut Painter, rows: usize, open: bool) {
    let cell = p.fonts.cell();
    let w = 2.0 * cell.w;
    p.out.push(Instance::fill(0.0, 0.0, w, rows as f32 * cell.h, BG));
    p.out.push(Instance::fill(w, 0.0, 1.0, rows as f32 * cell.h, LINE_NO));
    let label = "ツリー";
    let n = label.chars().count();
    if open {
        p.out.push(Instance::fill(0.0, 0.0, w, n as f32 * cell.h, FG));
    }
    for (k, c) in label.chars().enumerate() {
        // 縦書きでは長音を縦の線にする
        let c = if c == 'ー' { '｜' } else { c };
        let mut buf = [0u8; 4];
        p.text(c.encode_utf8(&mut buf), 0, k as f32 * cell.h, if open { BG } else { STATUS_FG });
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
    let col = (rect.x + GUTTER + nagi_core::display_col(&line, sb.min(line.len()), false)).saturating_sub(1);
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

/// ファイルツリー。1 行目は根の名前、2 行目から字下げした木（フォルダは青で `▸` `▾`）。右端に 1px の区切り線
fn draw_tree(p: &mut Painter, t: &crate::tree_panel::TreePanel, r: Rect, focused: bool, current: Option<&std::path::Path>) {
    let cell = p.fonts.cell();
    let (x, y0, w) = (r.x as f32 * cell.w, r.y as f32 * cell.h, r.w as f32 * cell.w);
    p.out.push(Instance::fill(x, y0, w, r.h as f32 * cell.h, BG));
    p.out.push(Instance::fill(((r.x + r.w) as f32 + 0.5) * cell.w, y0, 1.0, r.h as f32 * cell.h, LINE_NO));
    let saved = p.cols;
    p.cols = r.x + r.w;
    let name = t.root.file_name().map_or(t.root.display().to_string(), |n| n.to_string_lossy().into_owned());
    p.text(&format!("{name}/{}", if t.loading() { " …" } else { "" }), r.x, y0, STATUS_FG);
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
    p.text(&head, r.x, y0, STATUS_FG);
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
        let num = format!(":{}: ", h.line);
        let mut items = p.items(&h.path, dim);
        items.extend(p.items(&num, dim));
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
