//! 画面の組み立て: 行番号・本文・カーソル・最下行をインスタンス列にする。
//!
//! 1 行は「書記素ごとの部品（文字・文字色・背景色・下線）」の並びとして組み立ててから描く。
//! 選択範囲・検索の一致・置換の予告表示・カーソル・IME の変換中の文字列は、どれもこの部品の差し替えで表す。

use std::borrow::Cow;

use nagi_render::{Instance, Renderer};
use nagi_text::Fonts;
use unicode_segmentation::UnicodeSegmentation;

use crate::editor::{Editor, GUTTER, Mode};

const fn rgb(hex: u32) -> [f32; 4] {
    [((hex >> 16) & 0xff) as f32 / 255.0, ((hex >> 8) & 0xff) as f32 / 255.0, (hex & 0xff) as f32 / 255.0, 1.0]
}

// 見た目はターミナルの Vim と同じにする（architecture.md §9 の原則）。地は単色、強調は反転だけ。
pub const BG: [f32; 4] = rgb(0x0c0c0c);
const FG: [f32; 4] = rgb(0xcccccc);
const LINE_NO: [f32; 4] = rgb(0x4e4e4e);
const TILDE: [f32; 4] = rgb(0x3a3a3a);
const CURSOR: [f32; 4] = rgb(0xcccccc);
const STATUS_FG: [f32; 4] = rgb(0x8a8a8a);
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
    let mut lines: Vec<String> = (from.min(total)..to.min(total)).map(|l| ed.buf.line(l)).collect();
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

pub fn build(ed: &mut Editor, fonts: &mut Fonts, r: &mut Renderer) -> Vec<Instance> {
    let cell = fonts.cell();
    let (w, h) = r.size();
    let cols = (w as f32 / cell.w) as usize;
    let rows = ((h as f32 / cell.h) as usize).saturating_sub(ed.status_rows()).max(1);
    let bar = (cell.w / 6.0).round().max(2.0);
    let underline = (cell.h / 16.0).round().max(1.0);
    let mut p = Painter { out: Vec::with_capacity(cols * rows + 64), fonts, r, cols, bar, underline };
    let total = ed.buf.line_count();
    let debug = std::env::var_os("NAGI_DEBUG_BUILD").is_some();
    let t0 = std::time::Instant::now();
    // 検索・置換の入力中は件数（3/12）を出すので全体を数える。Normal で強調だけ残っているとき（hlsearch）は、
    // 画面に見えている範囲だけを探す（編集のたびにファイル全体を探し直さない。n / N は自分で全体を数える）
    let searching = matches!(ed.mode, Mode::Search | Mode::Replace);
    if searching {
        ed.refresh_matches();
    }
    let t_matches = t0.elapsed();
    // スムーズスクロール中は 1 行多く描き、全体を frac 行ぶん上へずらす
    let frac = ed.frac;
    let drawn = rows + usize::from(frac > 0.0);

    // 画面に見えている範囲だけにハイライトのクエリをかける
    let view_start = ed.buf.line_start(ed.top);
    let view_end = ed.buf.line_start((ed.top + drawn).min(total));
    let spans = match &ed.syntax {
        Some(syn) => syn.highlight(&ed.buf, view_start..view_end.max(view_start)),
        None => Vec::new(),
    };
    let t_hl = t0.elapsed();
    let ed: &Editor = ed;
    let visible_matches;
    let all_matches: &[(usize, usize)] = if searching {
        &ed.search.matches
    } else if ed.search.highlight {
        let end = if ed.top + drawn >= total { ed.buf.len_bytes() } else { view_end };
        visible_matches = ed.visible_matches(view_start, end);
        &visible_matches
    } else {
        &[]
    };
    let visual = match ed.mode {
        Mode::Visual { .. } => Some(ed.visual_range()),
        _ => None,
    };
    // 行単位の Visual は、選んでいる行の範囲にしておく（1 文字ごとに位置を引き直さない）
    let visual_lines = visual.map(|(vs, ve, _)| (ed.buf.pos_of(vs).0, ed.buf.pos_of(ve.saturating_sub(1).max(vs)).0));
    let replacing = ed.mode == Mode::Replace && !ed.query.text.is_empty();

    for row in 0..drawn {
        let ln = ed.top + row;
        let y = (row as f32 - frac) * cell.h;
        if ln >= total {
            // ファイル末尾より後ろは Vim と同じく ~
            p.text("~", 0, y, TILDE);
            continue;
        }
        p.text(&format!("{:>5} ", ln + 1), 0, y, LINE_NO);
        let line = ed.buf.line(ln);
        let abs = ed.buf.line_start(ln);
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
        let is_cursor_line = ln == ed.cursor.line;
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
                    if Some(first + mi) == ed.search.current && matches!(ed.mode, Mode::Search) {
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
            if is_cursor_line && b == ed.cursor.byte {
                cursor_item = Some(items.len());
            }
            items.push(it);
        }
        // Visual で改行まで選んでいる行は、Vim と同じく行末を 1 セル反転して見せる（空行が選ばれていると分かるように）
        if let Some((vs, ve, lw)) = visual {
            let eol = abs + line.len();
            let selected = if lw { line_selected } else { eol >= vs && eol < ve };
            if selected && !(is_cursor_line && ed.cursor.byte >= line.len()) {
                items.push(Item { g: Cow::Borrowed(" "), width: 1, fg: BG, bg: Some(FG), underline: false });
            }
        }
        if !is_cursor_line || matches!(ed.mode, Mode::Command | Mode::Search | Mode::Replace) {
            p.draw(&items, GUTTER, y);
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
            p.draw(&items, GUTTER, y);
            p.caret(GUTTER + before + caret_in_pre, y);
        } else {
            // ブロックカーソル（文字を反転）。空行や行末では 1 セルの塗り
            match cursor_item {
                Some(i) => {
                    items[i].bg = Some(CURSOR);
                    items[i].fg = BG;
                    p.draw(&items, GUTTER, y);
                }
                None => {
                    let c = p.draw(&items, GUTTER, y);
                    p.out.push(Instance::fill(c as f32 * cell.w, y, cell.w, cell.h, CURSOR));
                }
            }
        }
    }

    let t_lines = t0.elapsed();
    if debug {
        eprintln!("build matches={:?} highlight={:?} lines={:?} n_matches={}", t_matches, t_hl - t_matches, t_lines - t_hl, all_matches.len());
    }
    // ---- 最下行: 左にモード・メッセージ・入力欄、右に位置（Vim の showmode と ruler と同じ形）
    let y = rows as f32 * cell.h;
    if frac > 0.0 {
        // はみ出した本文の最終行を最下行の地で隠す
        p.out.push(Instance::fill(0.0, y, w as f32, h as f32 - y, BG));
    }
    let name = ed.buf.path.as_ref().map_or("[No Name]".to_string(), |p| p.display().to_string());
    let ruler = format!(
        "{}{}   {}{}   {}{},{} ",
        ed.pending_keys.iter().map(|k| k.label()).collect::<String>(),
        ed.showcmd,
        name,
        if ed.buf.dirty { " [+]" } else { "" },
        ed.syntax.as_ref().map_or(String::new(), |s| format!("{}   ", s.lang.name())),
        ed.cursor.line + 1,
        nagi_core::display_col(&ed.buf.line(ed.cursor.line), ed.cursor.byte, false) + 1
    );
    let n = ed.search.matches.len();
    let re = if ed.search.regex { "  [.*]" } else { "" };
    let count = match ed.search.current {
        Some(i) if n > 0 => format!("{}/{n}{re} ", i + 1),
        _ if ed.query.text.is_empty() => format!("{re} "),
        _ => format!("0/0{re} "),
    };
    match ed.mode {
        Mode::Command => prompt_line(&mut p, ":", &ed.cmdline.text, ed.cmdline.cursor, &ed.preedit, y, true),
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
                match ed.mode {
                    Mode::Insert => "-- INSERT --".into(),
                    Mode::Visual { line: false } => "-- VISUAL --".into(),
                    Mode::Visual { line: true } => "-- VISUAL LINE --".into(),
                    _ => String::new(),
                }
            };
            let end = p.text(&left, 0, y, FG);
            // 長いメッセージと重なるときは位置表示を出さない（メッセージを優先する）
            let pc = cols.saturating_sub(p.width(&ruler));
            if end + 2 <= pc {
                p.text(&ruler, pc, y, STATUS_FG);
            }
        }
    }
    p.out
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
