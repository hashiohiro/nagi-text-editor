//! 画面の組み立て: 行番号・本文・カーソル・最下行をインスタンス列にする。
//!
//! 1 行は「書記素ごとの部品（文字・文字色・背景色・下線）」の並びとして組み立ててから描く。
//! 選択範囲・検索の一致・置換の予告表示・カーソル・IME の変換中の文字列は、どれもこの部品の差し替えで表す。

pub(crate) use std::borrow::Cow;

pub(crate) use nagi_render::{Instance, Renderer};
pub(crate) use nagi_text::Fonts;
pub(crate) use unicode_segmentation::UnicodeSegmentation;

pub(crate) use crate::editor::{Editor, GUTTER, State};
pub(crate) use crate::panes::Rect;

pub(crate) const fn rgb(hex: u32) -> [f32; 4] {
    [((hex >> 16) & 0xff) as f32 / 255.0, ((hex >> 8) & 0xff) as f32 / 255.0, (hex & 0xff) as f32 / 255.0, 1.0]
}

// 見た目はターミナルの Vim と同じにする（architecture.md §9 の原則）。地は単色、強調は反転だけ。
pub const BG: [f32; 4] = rgb(0x0c0c0c);

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
// 部品の中でほかの部品の描き方を使うもの（下の欄が検索・ログ・ターミナルを、右の欄がプレビューを描く）
pub(crate) use crate::finder::draw::finder_height;
pub(crate) use crate::grep_panel::draw::draw_grep_panel;
pub(crate) use crate::log_view::draw::draw_log_view;
pub(crate) use crate::md_preview::draw::draw_md_preview;
pub(crate) use crate::read_view::draw::draw_read_view;
pub(crate) use crate::result_grid::draw::draw_db_results;
pub(crate) use crate::terminal::draw::draw_terminal;

/// 配色（設定の「配色」: 0 ダーク・1 ブラック・2 ダークグレー・3 ライト）と強調の色（0xRRGGBB）。設定を適用したときに入れる
pub(crate) static THEME: AtomicU8 = AtomicU8::new(0);
pub(crate) static ACCENT: AtomicU32 = AtomicU32::new(DEFAULT_ACCENT);
pub(crate) const DEFAULT_ACCENT: u32 = 0x87afd7;
/// 欄のタブの名前をいつも出すか（設定の「欄のタブの名前」）
pub(crate) static TAB_NAMES: AtomicBool = AtomicBool::new(false);

/// 設定の外観（配色・強調の色・タブの名前）を描き方に入れる
pub fn set_appearance(theme: &str, accent: &str, tab_names_always: bool) {
    let t = match theme {
        "black" => 1,
        "dim" => 2,
        "light" => 3,
        _ => 0,
    };
    THEME.store(t, Ordering::Relaxed);
    let a = u32::from_str_radix(accent.trim().trim_start_matches('#'), 16).ok().filter(|v| *v <= 0xffffff).unwrap_or(DEFAULT_ACCENT);
    ACCENT.store(a, Ordering::Relaxed);
    TAB_NAMES.store(tab_names_always, Ordering::Relaxed);
}

/// 描いた色を配色で置き換える（ダークの色を基準に、灰色は明るさを、色のあるものは濃さを変える）
pub fn theme_color(c: [f32; 4]) -> [f32; 4] {
    let default_accent = rgb(DEFAULT_ACCENT);
    let near = |a: f32, b: f32| (a - b).abs() < 0.002;
    if near(c[0], default_accent[0]) && near(c[1], default_accent[1]) && near(c[2], default_accent[2]) {
        let mut a = rgb(ACCENT.load(Ordering::Relaxed));
        a[3] = c[3];
        return a;
    }
    let gray = near(c[0], c[1]) && near(c[1], c[2]);
    let map = |f: &dyn Fn(f32) -> f32| [f(c[0]), f(c[1]), f(c[2]), c[3]];
    match THEME.load(Ordering::Relaxed) {
        // ブラック: 暗い灰色をもっと暗く（地は真っ黒）
        1 if gray && c[0] < 0.3 => map(&|v| ((v - 0.047) * 1.15).max(0.0)),
        // ダークグレー: 暗い灰色を持ち上げる（地は #1e1e1e ほど）
        2 if gray && c[0] < 0.35 => map(&|v| 0.118 + v * 0.75),
        // ライト: 灰色は明るさを反転、色のあるものは濃くする（白地で読めるように）
        3 if gray => map(&|v| (1.0 - v * 0.92).clamp(0.0, 1.0)),
        3 => map(&|v| v * 0.6),
        _ => c,
    }
}
pub(crate) const FG: [f32; 4] = rgb(0xcccccc);
pub(crate) const LINE_NO: [f32; 4] = rgb(0x4e4e4e);
pub(crate) const TILDE: [f32; 4] = rgb(0x3a3a3a);
pub(crate) const CURSOR: [f32; 4] = rgb(0xcccccc);
/// Visual で選んだ行末（空行など）にあるカーソル。選択の反転とも地の色とも見分けられる灰色
pub(crate) const CURSOR_IN_SELECTION: [f32; 4] = rgb(0x6c6c6c);
pub(crate) const STATUS_FG: [f32; 4] = rgb(0x8a8a8a);
/// git の変更マーカー（足した行・変えた行・消した位置）と、差分の地の色
pub(crate) const GIT_ADD: [f32; 4] = rgb(0x87af87);
pub(crate) const GIT_MOD: [f32; 4] = rgb(0x87afd7);
pub(crate) const GIT_DEL: [f32; 4] = rgb(0xd75f5f);
pub(crate) const DIFF_ADD_BG: [f32; 4] = rgb(0x16261a);
pub(crate) const DIFF_DEL_BG: [f32; 4] = rgb(0x2c1a1a);
pub(crate) const DIFF_WORD_BG: [f32; 4] = rgb(0x2c4a70);
pub(crate) const DIFF_WORD_DEL_BG: [f32; 4] = rgb(0x6a2a2a);
/// 診断のある行の行番号（エラーと警告）
pub(crate) const DIAG_ERROR: [f32; 4] = rgb(0xd75f5f);
pub(crate) const DIAG_WARN: [f32; 4] = rgb(0xd7af5f);
/// 検索の一致（今いるもの以外）の背景
pub(crate) const MATCH_BG: [f32; 4] = rgb(0x3a3a3a);
/// 強調の色（選んでいる欄・決めるボタン。配色の設定で描いた後に置き換わる）
pub(crate) const ACCENT_FG: [f32; 4] = rgb(DEFAULT_ACCENT);

/// ハイライトの色（ターミナルの 256 色に近い、控えめな数色）
pub(crate) fn hl_color(h: nagi_syntax::Hl) -> [f32; 4] {
    use nagi_syntax::Hl;
    match h {
        Hl::Keyword => rgb(0x87afd7),
        Hl::String => rgb(0xafaf87),
        Hl::Comment => rgb(0x6c6c6c),
        Hl::Type | Hl::Attribute => rgb(0xd7af87),
        Hl::Number | Hl::Escape => rgb(0xd7875f),
        Hl::Function => rgb(0xdcdcaa),
        Hl::Variable => rgb(0x9fcfe0),
        Hl::Property => rgb(0xb294bb),
    }
}

/// 1 書記素分の部品。文字は元の行を借りる（1 文字ごとに確保しない）
pub(crate) struct Item<'a> {
    pub(crate) g: Cow<'a, str>,
    pub(crate) width: usize,
    pub(crate) fg: [f32; 4],
    pub(crate) bg: Option<[f32; 4]>,
    pub(crate) underline: bool,
}

/// 描画先と、そのフレームの寸法
pub(crate) struct Painter<'a> {
    pub(crate) out: Vec<Instance>,
    pub(crate) fonts: &'a mut Fonts,
    pub(crate) r: &'a mut Renderer,
    pub(crate) cols: usize,
    /// ターミナルにはできない細さ: 縦棒カーソルと下線は数 px で描く
    pub(crate) bar: f32,
    pub(crate) underline: f32,
    /// 最後に描いた縦棒カーソル（列, y）。IME の候補の窓をそこに出す
    pub(crate) caret_at: Option<(usize, f32)>,
    /// 描いたタブのアイコン（行, 列の始め, 終わり, 名前）。マウスを載せたら名前を出す
    pub(crate) tab_hits: Vec<(usize, usize, usize, &'static str)>,
    /// 折り返し（設定）: 折り返す幅（0 なら折り返さない）と、使ってよい画面の行数。draw が使う
    pub(crate) wrap_w: usize,
    pub(crate) wrap_rows: usize,
    /// 最後に draw で描き終えた行の y（折り返したときの行末のカーソル用）
    pub(crate) end_y: f32,
}

impl Painter<'_> {
    pub(crate) fn items<'s>(&self, s: &'s str, fg: [f32; 4]) -> Vec<Item<'s>> {
        s.graphemes(true).map(|g| Item { g: Cow::Borrowed(g), width: self.cell_width(g), fg, bg: None, underline: false }).collect()
    }

    /// 書記素 1 つのセル幅（タブは 4）
    pub(crate) fn cell_width(&self, g: &str) -> usize {
        if g == "\t" { 4 } else { self.fonts.width(g) }
    }

    /// 部品の並びを (col, y) から描く。戻り値は描き終えた列
    pub(crate) fn draw(&mut self, items: &[Item<'_>], col: usize, y: f32) -> usize {
        let cell = self.fonts.cell();
        let mut c = col;
        let mut y = y;
        let mut row = 1;
        for it in items {
            // 折り返す（設定）: 幅を超えたら次の行の頭へ
            if self.wrap_w > 0 && c + it.width > col + self.wrap_w {
                if row >= self.wrap_rows {
                    break;
                }
                row += 1;
                c = col;
                y += cell.h;
            }
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
        self.end_y = y;
        c
    }

    pub(crate) fn text(&mut self, s: &str, col: usize, y: f32, fg: [f32; 4]) -> usize {
        let items = self.items(s, fg);
        self.draw(&items, col, y)
    }

    pub(crate) fn caret(&mut self, col: usize, y: f32) {
        self.caret_at = Some((col, y));
        let cell = self.fonts.cell();
        self.out.push(Instance::fill(col as f32 * cell.w, y, self.bar, cell.h, CURSOR));
    }

    /// 文字を `scale` 倍に縮めて px の位置 (x, y) から描く（アイコンの中の文字用）
    pub(crate) fn small_text(&mut self, s: &str, x: f32, y: f32, scale: f32, color: [f32; 4]) {
        let cw = self.fonts.cell().w;
        let mut x = x;
        for g in s.graphemes(true) {
            if let Some(key) = self.fonts.glyph_for(g, 1) {
                if let Some(a) = self.r.glyph(self.fonts, key) {
                    self.out.push(Instance { rect: [x + a.left * scale, y + a.top * scale, a.uv[2] * scale, a.uv[3] * scale], uv: a.uv, color, kind: if a.color { 2 } else { 1 } });
                }
            }
            x += cw * scale;
        }
    }

    pub(crate) fn width(&self, s: &str) -> usize {
        nagi_text::graphemes(self.fonts, s).map(|(_, _, w)| w).sum()
    }

    /// ステータスバーのメッセージ: 少し小さい字で (col, y) から（下に寄せる）。戻り値は描き終えた列（切り上げ）
    pub(crate) fn msg_text(&mut self, s: &str, col: usize, y: f32, fg: [f32; 4]) -> usize {
        const S: f32 = 0.82;
        let cell = self.fonts.cell();
        let limit = self.cols as f32 * cell.w;
        let mut x = col as f32 * cell.w;
        let y = (y + cell.h * (1.0 - S) * 0.7).round();
        for g in s.graphemes(true) {
            let w = self.cell_width(g);
            let adv = w as f32 * cell.w * S;
            if x + adv > limit {
                break;
            }
            if let Some(key) = self.fonts.glyph_for(g, w) {
                if let Some(a) = self.r.glyph(self.fonts, key) {
                    self.out.push(Instance { rect: [x + a.left * S, y + a.top * S, a.uv[2] * S, a.uv[3] * S], uv: a.uv, color: fg, kind: if a.color { 2 } else { 1 } });
                }
            }
            x += adv;
        }
        (x / cell.w).ceil() as usize
    }
}

/// 知らせの色（エラーは赤・警告は黄、ほかは `fg`）
pub(crate) fn msg_color(ed: &Editor, fg: [f32; 4]) -> [f32; 4] {
    match ed.notify.level {
        Some(nagi_core::log::Level::Error) if !ed.notify.text.is_empty() => DIAG_ERROR,
        Some(nagi_core::log::Level::Warn) if !ed.notify.text.is_empty() => DIAG_WARN,
        _ => fg,
    }
}

/// ステータスバーの左: `small` ならメッセージとして少し小さい字で
pub(crate) fn status_text(p: &mut Painter, s: &str, small: bool, y: f32, fg: [f32; 4]) -> usize {
    if small { p.msg_text(s, 0, y, fg) } else { p.text(s, 0, y, fg) }
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
pub(crate) fn ruler_col(ed: &Editor) -> String {
    const MAX: usize = 1 << 16;
    if ed.cursor.byte > MAX {
        return format!("{}-", ed.cursor.byte + 1);
    }
    (nagi_core::display_col(&ed.buf.line_head(ed.cursor.line, ed.cursor.byte + 4).0, ed.cursor.byte, false) + 1).to_string()
}

/// 1 行のうち取り出す byte 数。1 文字は最大 4 byte で、少なくとも 1 セル使うので、これで画面の幅を必ず埋められる
pub(crate) fn line_budget(cols: usize) -> usize {
    cols * 4 + 16
}

pub(crate) fn screen_rows(fonts: &Fonts, r: &Renderer) -> usize {
    ((r.size().1 as f32 / fonts.cell().h) as usize).saturating_sub(1).max(1)
}

/// 空いた時間に、画面の上下 1 画面ぶんのグリフを先に載せておく（スクロールや検索で飛んだ先で、
/// 初めて出る漢字をその場でラスタライズすると 1 字 0.5〜1ms かかるため）
pub fn prefetch_around(ed: &Editor, fonts: &mut Fonts, r: &mut Renderer) {
    let rows = screen_rows(fonts, r);
    prefetch_lines(ed, fonts, r, ed.top.saturating_sub(rows), ed.top + 2 * rows + 1);
}

pub(crate) fn prefetch_lines(ed: &Editor, fonts: &mut Fonts, r: &mut Renderer, from: usize, to: usize) {
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

pub(crate) fn glyph_keys(ed: &Editor, fonts: &mut Fonts, from: usize, to: usize) -> Vec<nagi_text::GlyphKey> {
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
pub(crate) struct Pane<'a> {
    pub(crate) buf: &'a nagi_core::Buffer,
    /// 診断のある行（行 → エラーか）。行番号の色に使う
    pub(crate) diags: std::collections::HashMap<usize, bool>,
    /// 波線を引く診断（行, 列（文字）, エラーか）
    pub(crate) squiggles: Vec<(usize, usize, bool)>,
    /// git の行頭の変更マーカー
    pub(crate) marks: std::collections::HashMap<usize, crate::git_ui::Marker>,
    pub(crate) folds: &'a crate::folds::Folds,
    pub(crate) syntax: Option<&'a nagi_syntax::Syntax>,
    pub(crate) cursor: nagi_core::Cursor,
    pub(crate) top: usize,
    pub(crate) frac: f32,
    pub(crate) active: bool,
}

/// 窓 1 つを `rect`（本文の行だけ。状態行は含まない）に描く
pub(crate) fn draw_pane(p: &mut Painter, ed: &Editor, pane: &Pane, rect: Rect) {
    let cell = p.fonts.cell();
    // 作業中の窓に blame を出しているときは、行番号の左にその欄を足す
    let gutter = if pane.active { ed.gutter() } else { GUTTER };
    let bw = gutter - GUTTER;
    let now = crate::blame::now_secs();
    let stmt = if pane.active { ed.console_statement() } else { None };
    let saved_cols = p.cols;
    p.cols = rect.x + rect.w;
    let total = pane.buf.line_count();
    let debug = std::env::var_os("NAGI_DEBUG_BUILD").is_some();
    let t0 = std::time::Instant::now();
    // 強調は画面に見えている範囲だけを探す（入力中も、確定後の hlsearch も）。
    // 件数（3/12）のための全件は、検索の処理が別に（大きなファイルでは裏で）数える
    let searching = matches!(ed.state(), State::Search | State::Replace);
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
    // 折り返すときは、画面に入るだけの行ぶん
    let budget = if ed.settings.bool("editor.wrap") { line_budget(rect.w * drawn) } else { line_budget(rect.w) };
    let heads: Vec<(String, usize, bool)> = shown
        .iter()
        .map(|&ln| {
            let (s, cut) = pane.buf.line_head(ln, budget);
            (s, pane.buf.line_start(ln), cut)
        })
        .collect();
    let md = ed.custom_spans(pane.buf);
    let spans = match pane.syntax {
        // Markdown・Sass: 文章全体を解析した覚えから、見えている範囲だけ
        _ if md.is_some() => md.as_ref().unwrap().iter().filter(|(r, _)| r.end > view_start && r.start < view_end).cloned().collect(),
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
    let visual = match ed.state() {
        State::Visual { block: false, .. } if pane.active => Some(ed.visual_range()),
        _ => None,
    };
    // 矩形の選択は、見えている行ごとに列の範囲から求める
    let block = (pane.active && ed.is_block()).then(|| ed.block_sel());
    // 行単位の Visual は、選んでいる行の範囲にしておく（1 文字ごとに位置を引き直さない）
    let visual_lines = visual.map(|(vs, ve, _)| (pane.buf.pos_of(vs).0, pane.buf.pos_of(ve.saturating_sub(1).max(vs)).0));
    let replacing = pane.active && ed.state() == State::Replace && !ed.query.text.is_empty();
    // 折り返し（設定）: 本文の幅で折り返す。1 行が使う画面の行を数えながら並べる
    let wrapw = if ed.settings.bool("editor.wrap") { rect.w.saturating_sub(gutter) } else { 0 };

    let mut vrow = 0usize;
    let mut next = 0usize;
    while vrow < drawn {
        let i = next;
        next += 1;
        let y = (rect.y as f32 + vrow as f32 - frac) * cell.h;
        let Some(&ln) = shown.get(i) else {
            // ファイル末尾より後ろは Vim と同じく ~
            p.text("~", rect.x, y, TILDE);
            vrow += 1;
            continue;
        };
        let folded = pane.folds.outer_at(ln).is_some_and(|f| f.0 == ln);
        let line_rows = if wrapw > 0 && !folded {
            let extra = if pane.active && ln == pane.cursor.line && ed.state() == State::Insert { p.width(&ed.preedit) } else { 0 };
            (p.width(&heads[i].0) + extra + 1).div_ceil(wrapw).max(1).min(drawn - vrow)
        } else {
            1
        };
        vrow += line_rows;
        p.wrap_w = wrapw;
        p.wrap_rows = line_rows;
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
        // 行番号（設定: 絶対・相対（今の行は絶対）・出さない）
        match ed.settings.get("editor.line_numbers").as_str() {
            "off" => {}
            "relative" if ln != pane.cursor.line => {
                p.text(&format!("{:>5} ", ln.abs_diff(pane.cursor.line)), rect.x + bw, y, num_color);
            }
            "relative" => {
                p.text(&format!("{:<5} ", ln + 1), rect.x + bw, y, if num_color == LINE_NO { STATUS_FG } else { num_color });
            }
            _ => {
                p.text(&format!("{:>5} ", ln + 1), rect.x + bw, y, num_color);
            }
        }
        // SQL コンソール: カーソルのある文（Ctrl+Enter で流す範囲）に緑の線
        if stmt.is_some_and(|(s, e)| ln >= s && ln < e) {
            p.out.push(Instance::fill((rect.x + gutter) as f32 * cell.w - cell.w * 0.5 - 2.0, y, 3.0, cell.h, GIT_ADD));
        }
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
            let label = format!("+--{:>3} 行: {}", e - s + 1, heads[i].0.trim());
            let saved = p.cols;
            p.cols = rect.x + rect.w;
            let end = p.text(&label, rect.x + gutter, y, STATUS_FG);
            let _ = end;
            p.cols = saved;
            if pane.active && pane.cursor.line == ln && !matches!(ed.state(), State::Command | State::Search | State::Replace | State::Finder | State::GrepPrompt | State::List) {
                p.out.push(Instance::fill(x, y, cell.w, cell.h, CURSOR));
            }
            continue;
        }
        let (line, abs, cut) = &heads[i];
        let (line, abs, cut) = (line.as_str(), *abs, *cut);
        // 診断の波線（エラーは赤、警告は黄色。IntelliJ と同じく語の下に）
        for &(_, col, error) in pane.squiggles.iter().filter(|s| s.0 == ln) {
            let (a, b) = squiggle_range(line, col);
            if b > a {
                let (da, db) = (nagi_core::display_col(line, a, false), nagi_core::display_col(line, b, false));
                let color = if error { DIAG_ERROR } else { DIAG_WARN };
                if wrapw == 0 {
                    draw_squiggle(p, (rect.x + gutter + da) as f32 * cell.w, (rect.x + gutter + db) as f32 * cell.w, y + cell.h, color);
                } else {
                    // 折り返した行ごとに分けて引く
                    let mut s0 = da;
                    while s0 < db {
                        let r = s0 / wrapw;
                        let e0 = db.min((r + 1) * wrapw);
                        let base = rect.x + gutter;
                        draw_squiggle(p, (base + s0 - r * wrapw) as f32 * cell.w, (base + e0 - r * wrapw) as f32 * cell.w, y + (r + 1) as f32 * cell.h, color);
                        s0 = e0;
                    }
                }
            }
        }
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
                    if pane.active && Some((ms, me)) == ed.search.current && matches!(ed.state(), State::Search) {
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
        if !is_cursor_line || matches!(ed.state(), State::Command | State::Search | State::Replace | State::Finder | State::GrepPrompt | State::List | State::CommitMsg | State::Diff | State::Log | State::Branches | State::DbTable) {
            p.draw(&items, rect.x + gutter, y);
            continue;
        }
        if ed.state() == State::Insert {
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
            // 折り返していれば、縦棒もその行へ
            let off = before + caret_in_pre;
            let (cc, cy) = if wrapw > 0 { (rect.x + gutter + off % wrapw, y + (off / wrapw) as f32 * cell.h) } else { (rect.x + gutter + off, y) };
            p.caret(cc, cy);
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
                        // 折り返していて、行末が幅いっぱいなら次の行の頭
                        let (cx, cy) = if wrapw > 0 && c >= rect.x + gutter + wrapw { (rect.x + gutter, p.end_y + cell.h) } else { (c, p.end_y) };
                        p.out.push(Instance::fill(cx as f32 * cell.w, cy, cell.w, cell.h, color));
                    }
                }
            }
        }
    }

    p.wrap_w = 0;
    p.wrap_rows = 1;
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
    // Markdown のプレビュー: 打った内容と幅に合わせて作り直し、カーソルの行へ送る
    ed.refresh_md_preview();
    // プロジェクトの木を開いているファイルに合わせる
    ed.sync_tree();
    // ターミナルを欄の大きさに合わせる
    ed.sync_terminal();
    let bar = (cell.w / 6.0).round().max(2.0);
    let underline = (cell.h / 16.0).round().max(1.0);
    let mut p = Painter { out: Vec::with_capacity(cols * rows + 64), fonts, r, cols, bar, underline, caret_at: None, tab_hits: Vec::new(), wrap_w: 0, wrap_rows: 1, end_y: 0.0 };
    if let (Some(r), Some(t)) = (ed.tree_rect(), &mut ed.tree) {
        crate::list::fit(t.selected, &mut t.scroll, r.h.saturating_sub(1));
    }
    if let (Some(r), Some(g)) = (ed.panel_rect(), &mut ed.grep) {
        // 選んでいる結果が見える範囲に入るように送る（1 行目は見出し）
        crate::list::fit(g.selected, &mut g.scroll, r.h.saturating_sub(1));
    }
    if let (Some(r), Some(c)) = (ed.commit_rect(), &mut ed.commit) {
        // 選んでいる項目（見出しの行を含めた位置）が見える範囲に入るように送る
        let h = crate::commit_panel::layout(r.h).list_h;
        let tracked = c.tracked();
        let at = c.selected + 1 + usize::from(c.selected >= tracked);
        crate::list::fit(at, &mut c.scroll, h);
    }
    if let Some(r) = ed.right_rect() {
        let h = r.h.saturating_sub(2);
        if let Some(m) = &mut ed.maven {
            crate::list::fit(m.selected, &mut m.scroll, h);
        }
        if let Some(d) = &mut ed.db {
            crate::list::fit(d.selected, &mut d.scroll, h);
        }
    }
    // 表: 選んだ行と列が見える範囲に入るように送る
    let fit_grid = |row: usize, col: usize, scroll: &mut usize, hscroll: &mut usize, widths: &[usize], h: usize, w: usize| {
        crate::list::fit(row, scroll, h);
        if col < *hscroll {
            *hscroll = col;
        }
        while *hscroll < col && widths[*hscroll..=col.min(widths.len().saturating_sub(1))].iter().map(|w| w + 3).sum::<usize>() + 8 > w {
            *hscroll += 1;
        }
    };
    if let Some(r) = ed.panel_rect() {
        if let Some(t) = ed.db_results.active_tab_mut() {
            let head = if t.filterable { 4 } else { 3 };
            fit_grid(t.row, t.col, &mut t.scroll, &mut t.hscroll, &t.widths.clone(), r.h.saturating_sub(head), r.w);
        }
    }
    let (wx, ww) = ed.windows_x();
    let wrows = ed.windows_rows();
    let _ = wx;
    if let Some(te) = &mut ed.db_table {
        let widths = te.widths.clone();
        fit_grid(te.row, te.col, &mut te.scroll, &mut te.hscroll, &widths, wrows.saturating_sub(4), ww);
    }
    if let Some(f) = &mut ed.finder {
        // 選んでいる候補が見える範囲に入るように送る
        crate::list::fit(f.selected, &mut f.scroll, finder_height(rows));
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
        let pane = Pane { buf, marks: ed.git_markers(buf.path.as_deref()), diags: ed.diag_lines(buf.path.as_deref()), squiggles: if ed.settings.bool("editor.squiggles") { ed.diag_spans(buf.path.as_deref()) } else { Vec::new() }, folds, syntax, cursor, top: st.top.min(last), frac: if active { ed.frac } else { 0.0 }, active };
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
    // 欄・小窓（layout.rs の表の順に下から）
    crate::layout::draw(crate::layout::Layer::Panels, ed, &mut p, crate::layout::Screen { rows, cols });
    draw_toggle_buttons(&mut p, ed, cols);
    match ed.stripe {
        Some(crate::editor::Side::Left) => draw_stripe_left(&mut p, ed.windows_rows(), ed.left_visible()),
        Some(crate::editor::Side::Bottom) => draw_stripe_bottom(&mut p, rows, cols, ed),
        Some(crate::editor::Side::Right) => draw_stripe_right(&mut p, ed.windows_rows(), cols, ed.right_visible()),
        Some(crate::editor::Side::Commit) | None => {}
    }
    // ---- 最下行: 左にモード・メッセージ・入力欄、右に位置（Vim の showmode と ruler と同じ形）
    let y = rows as f32 * cell.h;
    // はみ出した本文の最終行（スムーズスクロール中）を最下行の地で隠す
    p.out.push(Instance::fill(0.0, y, w as f32, h as f32 - y, BG));
    let name = ed.buf.path.as_ref().map_or("[No Name]".to_string(), |p| p.display().to_string());
    let keys = format!("{}{}", ed.pending_keys.iter().map(|k| k.label()).collect::<String>(), ed.showcmd);
    // 選んでいる実行の構成（Shift+F10 で動かすもの）
    // （設定の「状態の行」で出すものを選べる）
    let st = &ed.settings;
    let keys = match &ed.run_selected {
        Some(r) if st.bool("appearance.status_run") => format!("{keys}   ▶ {r}"),
        _ => keys,
    };
    // git のブランチ（main ↑1）
    let keys = match ed.git_branch_label() {
        Some(b) if st.bool("appearance.status_branch") => format!("{keys}   {b}"),
        _ => keys,
    };
    let name = if st.bool("appearance.status_path") { name } else { String::new() };
    // 分割中は位置を窓ごとの状態行に出すので、最下行には入力中のキーだけを出す
    let ruler = if split {
        format!("{keys} ")
    } else {
        format!(
            "{keys}   {}{}   {}{},{} ",
            name,
            if ed.buf.dirty { " [+]" } else { "" },
            ed.syntax.as_ref().map(|s| s.lang.name()).or_else(|| {
                // tree-sitter を使わない言語
                let ext = ed.buf.path.as_deref()?.extension()?.to_string_lossy().to_ascii_lowercase();
                match ext.as_str() {
                    "md" | "markdown" | "mdx" => Some("Markdown"),
                    "sass" => Some("Sass"),
                    _ => None,
                }
            }).filter(|_| st.bool("appearance.status_lang")).map_or(String::new(), |n| format!("{n}   ")),
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
    // 部品の状態行（component.rs の status）
    let panel_status = crate::component::focused(ed).and_then(|c| c.status(ed));
    match ed.state() {
        _ if panel_status.is_some() => {
            let st = panel_status.as_ref().unwrap();
            let notified = !ed.notify.text.is_empty();
            let left = if notified { ed.notify.text.as_str() } else { st.name.as_str() };
            let color = if notified { msg_color(ed, FG) } else if st.warn { DIAG_WARN } else { FG };
            let end = status_text(&mut p, left, notified, y, color);
            let pc = cols.saturating_sub(p.width(&st.help));
            if !st.help.is_empty() && end + 2 <= pc {
                p.text(&st.help, pc, y, STATUS_FG);
            }
        }
        State::Command => prompt_line(&mut p, ":", &ed.cmdline.text, ed.cmdline.cursor, &ed.preedit, y, true),
        State::GrepPrompt if ed.grep.is_some() => {
            let g = ed.grep.as_ref().unwrap();
            prompt_line(&mut p, crate::grep_panel::GREP_LEAD, &g.query.text, g.query.cursor, &ed.preedit, y, true);
            let info = format!("{}{} 件{} ", if g.regex { "[.*]  " } else { "" }, g.hits.len(), if g.searching() { " …" } else { "" });
            let pc = cols.saturating_sub(p.width(&info));
            p.text(&info, pc, y, STATUS_FG);
        }
        State::Finder if ed.finder.is_some() => {
            let f = ed.finder.as_ref().unwrap();
            if f.field.is_insert() {
                prompt_line(&mut p, "> ", &f.query.text, f.query.cursor, &ed.preedit, y, true);
            } else {
                // ノーマル・Visual: Vim の欄（四角のカーソル・選んだ所の反転）
                p.text("> ", 0, y, FG);
                draw_vim_line(&mut p, &f.field, 0, 2, y, "", true);
            }
            let mode = if f.field.is_insert() { String::new() } else { format!("-- {} --  j/k 選ぶ  Esc 閉じる   ", f.field.mode_label()) };
            let info = format!("{mode}{}  {}/{}{} ", f.source.label(), f.hits.len(), f.items.len(), if f.walking() { " …" } else { "" });
            let pc = cols.saturating_sub(p.width(&info));
            p.text(&info, pc, y, STATUS_FG);
        }
        State::Search => {
            let lead = if ed.search.forward { "/" } else { "?" };
            prompt_line(&mut p, lead, &ed.query.text, ed.query.cursor, &ed.preedit, y, true);
            let pc = cols.saturating_sub(p.width(&count));
            p.text(&count, pc, y, STATUS_FG);
        }
        State::Replace => {
            let y2 = y + cell.h;
            prompt_line(&mut p, "/", &ed.query.text, ed.query.cursor, &ed.preedit, y, ed.replace_field == 0);
            prompt_line(&mut p, "→", &ed.repl.text, ed.repl.cursor, &ed.preedit, y2, ed.replace_field == 1);
            let c = format!("Enter: 1 件  Alt+A: 全件   {count}");
            let pc = cols.saturating_sub(p.width(&c));
            p.text(&c, pc, y2, STATUS_FG);
        }
        _ => {
            let left = if !ed.notify.text.is_empty() {
                ed.notify.text.clone()
            } else {
                let mode = match ed.state() {
                    State::Insert => "-- INSERT --",
                    State::Visual { block: true, .. } => "-- VISUAL BLOCK --",
                    State::Visual { line: false, .. } => "-- VISUAL --",
                    State::Visual { line: true, .. } => "-- VISUAL LINE --",
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
                Some(d) if left.is_empty() && ed.state() == State::Normal => (format!("{}: {}", if d.error { "error" } else { "warning" }, d.msg), if d.error { DIAG_ERROR } else { DIAG_WARN }),
                _ => (left, msg_color(ed, FG)),
            };
            let end = status_text(&mut p, &left, !ed.notify.text.is_empty(), y, color);
            // 長いメッセージと重なるときは位置表示を出さない（メッセージを優先する）
            let pc = cols.saturating_sub(p.width(&ruler));
            if end + 2 <= pc {
                p.text(&ruler, pc, y, STATUS_FG);
            }
        }
    }
    // 画面を覆うダイアログ（layout.rs）
    crate::layout::draw(crate::layout::Layer::Overlays, ed, &mut p, crate::layout::Screen { rows, cols });
    // タブのアイコンにマウスを載せていれば、その下に名前
    if let Some((row, col, label)) = ed.hover_tab.as_ref().filter(|_| ed.run_dialog.is_none() && ed.launcher.is_none()) {
        let cell = p.fonts.cell();
        let text = format!(" {label} ");
        let w = p.width(&text);
        let x = (*col).min(cols.saturating_sub(w + 1));
        let y = (row + 1) as f32 * cell.h + 2.0;
        p.out.push(Instance::fill(x as f32 * cell.w - 1.0, y - 1.0, w as f32 * cell.w + 2.0, cell.h + 2.0, STATUS_FG));
        p.out.push(Instance::fill(x as f32 * cell.w, y, w as f32 * cell.w, cell.h, rgb(0x141414)));
        let saved = p.cols;
        p.cols = cols;
        p.text(&text, x, y, FG);
        p.cols = saved;
    }
    *ed.tab_hits.borrow_mut() = std::mem::take(&mut p.tab_hits);
    // 配色（設定）: 描いた色を置き換える
    if THEME.load(Ordering::Relaxed) != 0 || ACCENT.load(Ordering::Relaxed) != DEFAULT_ACCENT {
        for i in p.out.iter_mut().filter(|i| i.kind != 2) {
            i.color = theme_color(i.color);
        }
    }
    // 入力欄の縦棒の位置（IME の候補の窓をそこに出す）
    ed.prompt_caret.set(p.caret_at);
    p.out
}

/// 左のストライプ（マウスを左端に寄せている間だけ）。幅 2 セルの帯に「プロジェクト」「コミット」を縦に書く。出しているタブは反転
pub(crate) fn draw_stripe_left(p: &mut Painter, rows: usize, shown: Option<crate::tree_panel::LeftTab>) {
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
pub(crate) fn draw_stripe_right(p: &mut Painter, rows: usize, cols: usize, shown: Option<crate::right_panel::RightTab>) {
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
pub(crate) fn draw_stripe_bottom(p: &mut Painter, rows: usize, cols: usize, ed: &Editor) {
    let cell = p.fonts.cell();
    let y = (rows - 1) as f32 * cell.h;
    p.out.push(Instance::fill(0.0, y, cols as f32 * cell.w, cell.h, BG));
    p.out.push(Instance::fill(0.0, y, cols as f32 * cell.w, 1.0, LINE_NO));
    // 下の欄のタブの名前（出しているタブは反転）。押すとそのタブを出す・隠す
    let mut x = 0;
    for (label, k) in ed.bottom_tabs() {
        let open = ed.bottom_visible() && ed.bottom_kind == k;
        let w = p.width(label);
        if open {
            p.out.push(Instance::fill(x as f32 * cell.w, y, w as f32 * cell.w, cell.h, FG));
        }
        x = p.text(label, x, y, if open { BG } else { STATUS_FG });
    }
}

/// 出す・隠すのボタン。ふだんは薄い 1 文字、マウスを載せると明るくして名前を横に出す
pub(crate) fn draw_toggle_buttons(p: &mut Painter, ed: &Editor, cols: usize) {
    let cell = p.fonts.cell();
    for (b, row, col) in ed.toggle_buttons() {
        let hover = ed.hover_button == Some(b);
        let (x, y) = (col as f32 * cell.w, row as f32 * cell.h);
        // 本文の長い行と重なっても見えるように地を塗る
        p.out.push(Instance::fill(x, y, cell.w, cell.h, if hover { MATCH_BG } else { BG }));
        let color = if hover { FG } else { STATUS_FG };
        match b {
            // 下の欄は、左右の « » に合わせて縦の二重の山形を線で描く（フォントに揃った字が無いため）
            crate::toggles::Button::ShowBottom | crate::toggles::Button::HideBottom => draw_double_chevron(p, x, y, b == crate::toggles::Button::ShowBottom, color),
            _ => {
                p.text(b.glyph(), col, y, color);
            }
        }
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

/// 縦の二重の山形（︽ ︾）をセル (x, y) に描く。`up` なら上向き
pub(crate) fn draw_double_chevron(p: &mut Painter, x: f32, y: f32, up: bool, c: [f32; 4]) {
    let cell = p.fonts.cell();
    let px = (cell.h / 22.0).max(1.0).round();
    // 山の半幅（ドット数）と、2 つの山の間
    let half = ((cell.w * 0.32) / px).round().max(2.0) as i32;
    let gap = px * (half as f32 * 0.8).round().max(2.0);
    let total = half as f32 * px + gap + px * 2.0;
    let cx = x + (cell.w / 2.0).floor();
    let top = y + ((cell.h - total) / 2.0).round();
    for k in 0..2 {
        let base = top + k as f32 * gap;
        for i in 0..=half {
            // 上向きは頂点が上、下向きは頂点が下
            let dy = if up { i } else { half - i } as f32 * px;
            p.out.push(Instance::fill(cx - i as f32 * px - px, base + dy, px * 2.0, px * 2.0, c));
            p.out.push(Instance::fill(cx + i as f32 * px - px, base + dy, px * 2.0, px * 2.0, c));
        }
    }
}

/// 左の欄の 1 行目: タブ（プロジェクト・コミット）。出しているタブは明るい字と下線
pub(crate) fn draw_left_tabs(p: &mut Painter, r: Rect, active: crate::tree_panel::LeftTab, focused: bool, extra: &str) {
    use crate::tree_panel::LeftTab;
    let tabs: Vec<(TabIcon, &'static str, bool)> = crate::tree_panel::TABS
        .iter()
        .map(|(l, t)| (if *t == LeftTab::Project { TabIcon::Project } else { TabIcon::Commit }, *l, *t == active))
        .collect();
    draw_icon_tabs(p, r, &tabs, focused, extra);
}

/// 左右の欄のタブのアイコン
#[derive(Clone, Copy)]
pub(crate) enum TabIcon {
    Project,
    Commit,
    Database,
    Maven,
    Preview,
    Git,
    Search,
    Problems,
    Output,
    Terminal,
    Result,
}

/// 左右の欄の 1 行目: タブをアイコンで並べ（出しているタブは明るく下線）、その横に出しているタブの名前
pub(crate) fn draw_icon_tabs(p: &mut Painter, r: Rect, tabs: &[(TabIcon, &'static str, bool)], focused: bool, extra: &str) {
    let cell = p.fonts.cell();
    let y = r.y as f32 * cell.h;
    let tw = crate::tree_panel::TAB_W;
    let mut x = r.x;
    let mut name = "";
    for &(icon, label, on) in tabs {
        if x + tw + 3 > r.x + r.w {
            break;
        }
        let color = if on && focused { FG } else if on { rgb(0xc8c8c8) } else { STATUS_FG };
        draw_tab_icon(p, icon, (x as f32 + tw as f32 / 2.0) * cell.w, y, color);
        if on {
            p.out.push(Instance::fill(x as f32 * cell.w + cell.w * 0.5, y + cell.h - 2.0, (tw - 1) as f32 * cell.w, 2.0, if focused { GIT_MOD } else { STATUS_FG }));
        }
        // 名前は（設定で「いつも」でなければ）出さず、マウスを載せたときに出す
        p.tab_hits.push((r.y, x, x + tw, label));
        if on {
            name = label;
        }
        x += tw;
    }
    let saved = p.cols;
    p.cols = (r.x + r.w).saturating_sub(3);
    if TAB_NAMES.load(Ordering::Relaxed) && !name.is_empty() {
        x = p.text(&format!(" {name}"), x, y, STATUS_FG);
    }
    if !extra.is_empty() {
        p.text(&format!(" {extra}"), x, y, LINE_NO);
    }
    p.cols = saved;
    p.out.push(Instance::fill(r.x as f32 * cell.w, y + cell.h - 1.0, r.w as f32 * cell.w, 1.0, LINE_NO));
}

/// タブのアイコンを、中心の x・行の y に 1 色で描く（IntelliJ の新しい UI のような線のアイコン）
pub(crate) fn draw_tab_icon(p: &mut Painter, icon: TabIcon, cx: f32, y: f32, c: [f32; 4]) {
    let cell = p.fonts.cell();
    let s = (cell.h * 0.7).round().clamp(10.0, 30.0);
    let x = (cx - s / 2.0).round();
    // 下の線（下線 2px）と重ならないよう、その上の範囲の真ん中に
    let y = y + ((cell.h - 3.0 - s) / 2.0).round().max(0.0);
    let px = (s / 16.0).max(1.0).round();
    let mut f = |x: f32, y: f32, w: f32, h: f32| p.out.push(Instance::fill(x, y, w, h, c));
    // 枠（線の太さ px）
    let frame = |f: &mut dyn FnMut(f32, f32, f32, f32), x: f32, y: f32, w: f32, h: f32| {
        f(x, y, w, px);
        f(x, y + h - px, w, px);
        f(x, y, px, h);
        f(x + w - px, y, px, h);
    };
    match icon {
        TabIcon::Project => {
            // フォルダ: つまみと枠
            f(x, y + px, s * 0.45, px);
            f(x, y + px, px, px * 2.0);
            f(x + s * 0.45 - px, y + px, px, px * 2.0);
            frame(&mut f, x, y + px * 3.0, s, s - px * 4.0);
        }
        TabIcon::Commit => {
            // コミット: 横線と、真ん中の輪
            let r = (s * 0.5).round();
            let my = y + (s / 2.0).round();
            f(x, my - px / 2.0, (s - r) / 2.0, px);
            f(x + (s + r) / 2.0, my - px / 2.0, (s - r) / 2.0, px);
            frame(&mut f, x + (s - r) / 2.0, my - r / 2.0, r, r);
        }
        TabIcon::Database => {
            // データベース: 積み重ねた円筒（3 段の枠）
            let h = ((s - px * 2.0) / 3.0).floor();
            for k in 0..3 {
                frame(&mut f, x + px, y + k as f32 * (h + px), s - px * 2.0, h);
            }
        }
        TabIcon::Maven => {
            // Maven: m の文字
            p.small_text("m", x + (s - cell.w * 1.1) / 2.0, y + (s - cell.h * 1.1) / 2.0, 1.1, c);
        }
        TabIcon::Git => {
            // Git: 幹の縦線と、枝の先の輪（右上）、輪から幹へ戻る線
            let lx = x + (s * 0.28).round();
            f(lx, y, px, s);
            let r = (s * 0.34).round();
            let (rx, ry) = (x + s - r - px, y + (s * 0.12).round());
            frame(&mut f, rx, ry, r, r);
            let bx = rx + (r / 2.0).floor();
            let by = y + (s * 0.66).round();
            f(bx, ry + r, px, by - ry - r);
            f(lx, by, bx - lx + px, px);
        }
        TabIcon::Search => {
            // 検索: 虫めがね（輪と、右下への柄）
            let r = (s * 0.62).round();
            frame(&mut f, x, y, r, r);
            let mut k = 0.0;
            while x + r - px + k < x + s {
                f(x + r - px + k, y + r - px + k, px * 2.0, px * 2.0);
                k += px;
            }
        }
        TabIcon::Problems => {
            // 問題: 枠の中に「!」
            frame(&mut f, x + px, y, s - px * 2.0, s);
            let cx = x + (s / 2.0).floor();
            f(cx - px / 2.0, y + px * 3.0, px * 1.5, s * 0.45);
            f(cx - px / 2.0, y + s - px * 4.0, px * 1.5, px * 1.5);
        }
        TabIcon::Output => {
            // ログ: 長さの違う行
            for (k, w) in [1.0, 0.7, 0.9, 0.5].iter().enumerate() {
                f(x, y + px + k as f32 * (s / 4.0).floor(), (s * w).round(), px);
            }
        }
        TabIcon::Terminal => {
            // ターミナル: 枠の中に › と _
            frame(&mut f, x, y + px, s, s - px * 2.0);
            let (cx, cy) = (x + px * 3.0, y + (s / 2.0).round());
            let h = (s * 0.18).round();
            let mut k = 0.0;
            while k <= h {
                f(cx + k, cy - h + k, px, px);
                f(cx + k, cy + h - k, px, px);
                k += px;
            }
            f(cx + h + px * 2.0, cy + h, (s * 0.25).round(), px);
        }
        TabIcon::Result => {
            // クエリの結果: 表（枠と、見出しの行と列の区切り）
            frame(&mut f, x, y + px, s, s - px * 2.0);
            f(x, y + px + (s * 0.3).round(), s, px);
            f(x + (s * 0.4).round(), y + px, px, s - px * 2.0);
        }
        TabIcon::Preview => {
            // プレビュー: 紙と行
            let w = s * 0.8;
            let x = x + (s - w) / 2.0;
            frame(&mut f, x, y, w, s);
            for k in 0..3 {
                f(x + px * 3.0, y + px * 4.0 + k as f32 * px * 3.0, w - px * 6.0, px);
            }
        }
    }
}

/// 右の欄の下に枠を出す（`hl` 行目を強調）。枠の左端の列と上端の y を返す
pub(crate) fn draw_panel_box(p: &mut Painter, r: Rect, lines: &[String], hl: usize) -> (usize, f32) {
    let cell = p.fonts.cell();
    let h = lines.len();
    let top = (r.y + r.h).saturating_sub(h + 1);
    let (x, y, w) = (r.x as f32 * cell.w + 4.0, top as f32 * cell.h, (r.w as f32 - 0.5) * cell.w - 8.0);
    p.out.push(Instance::fill(x - 1.0, y - 1.0, w + 2.0, h as f32 * cell.h + 2.0, STATUS_FG));
    p.out.push(Instance::fill(x, y, w, h as f32 * cell.h, rgb(0x141414)));
    for (k, l) in lines.iter().enumerate() {
        let yy = y + k as f32 * cell.h;
        if k == hl && hl > 0 {
            p.out.push(Instance::fill(x, yy, w, cell.h, MATCH_BG));
        }
        p.text(l, r.x, yy, if k == 0 { FG } else if k + 1 == lines.len() { STATUS_FG } else { FG });
    }
    (r.x, y)
}

/// 表（結果のタブとテーブルの編集で共通）。左に行番号、上に列名と型の 2 行
pub(crate) struct GridView<'a> {
    pub(crate) cols: &'a [String],
    pub(crate) types: &'a [String],
    pub(crate) widths: &'a [usize],
    /// 列名の前の印（K / F）
    pub(crate) marks: Vec<char>,
    /// 並べ替えている列と向き
    pub(crate) sort: Option<(String, bool)>,
    pub(crate) rows: usize,
    pub(crate) value: &'a dyn Fn(usize, usize) -> Option<String>,
    /// 行の印（行番号や ＋）と、行の地の色
    pub(crate) row_style: &'a dyn Fn(usize) -> (String, Option<[f32; 4]>),
    pub(crate) cell_bg: &'a dyn Fn(usize, usize) -> Option<[f32; 4]>,
    pub(crate) row: usize,
    pub(crate) col: usize,
    pub(crate) scroll: usize,
    pub(crate) hscroll: usize,
    pub(crate) focused: bool,
}

pub(crate) fn draw_grid(p: &mut Painter, g: &GridView, r: Rect) {
    let cell = p.fonts.cell();
    let (x0, wpx) = (r.x as f32 * cell.w, r.w as f32 * cell.w);
    let saved = p.cols;
    p.cols = r.x + r.w;
    let rn = 6;
    let mut xs = Vec::new();
    let mut x = r.x + rn + 1;
    for c in g.hscroll..g.cols.len() {
        if x >= r.x + r.w {
            break;
        }
        xs.push((c, x));
        x += g.widths[c] + 3;
    }
    let hy = r.y as f32 * cell.h;
    let ty = hy + cell.h;
    for &(c, x) in &xs {
        let mut name = String::new();
        if g.marks.get(c).is_some_and(|m| *m != ' ') {
            name.push(g.marks[c]);
            name.push(' ');
        }
        name.push_str(&g.cols[c]);
        if let Some((s, asc)) = &g.sort {
            if s.trim_matches('"').eq_ignore_ascii_case(&g.cols[c]) {
                name.push_str(if *asc { " ↑" } else { " ↓" });
            }
        }
        let lim = p.cols;
        p.cols = (x + g.widths[c] + 1).min(r.x + r.w);
        p.text(&name, x, hy, if c == g.col && g.focused { FG } else { FINDER_MATCH });
        p.text(g.types.get(c).map_or("", |s| s.as_str()), x, ty, LINE_NO);
        p.cols = lim;
    }
    p.out.push(Instance::fill(x0, ty + cell.h - 1.0, wpx, 1.0, LINE_NO));
    let body = r.h.saturating_sub(2);
    for k in g.scroll..(g.scroll + body).min(g.rows) {
        let y = (r.y + 2 + k - g.scroll) as f32 * cell.h;
        let (label, bg) = (g.row_style)(k);
        if let Some(bg) = bg {
            p.out.push(Instance::fill(x0, y, wpx, cell.h, bg));
        } else if k == g.row {
            p.out.push(Instance::fill(x0, y, wpx, cell.h, rgb(0x262626)));
        }
        let lw = p.width(&label);
        p.text(&label, (r.x + rn).saturating_sub(lw + 1), y, if label.starts_with('＋') { GIT_ADD } else { LINE_NO });
        for &(c, x) in &xs {
            let v = (g.value)(k, c);
            let text = crate::result_grid::show(&v);
            let sel = k == g.row && c == g.col && g.focused;
            if sel {
                p.out.push(Instance::fill((x - 1) as f32 * cell.w, y, (g.widths[c] + 2) as f32 * cell.w, cell.h, FG));
            } else if let Some(bg) = (g.cell_bg)(k, c) {
                p.out.push(Instance::fill((x - 1) as f32 * cell.w, y, (g.widths[c] + 2) as f32 * cell.w, cell.h, bg));
            }
            let lim = p.cols;
            p.cols = (x + g.widths[c]).min(r.x + r.w);
            let fg = if sel { BG } else if v.is_none() { LINE_NO } else if bg == Some(DIFF_DEL_BG) { rgb(0x8a6a6a) } else { FG };
            p.text(&text, x, y, fg);
            p.cols = lim;
            let sx = (x + g.widths[c] + 1) as f32 * cell.w + cell.w * 0.5;
            p.out.push(Instance::fill(sx, y, 1.0, cell.h, rgb(0x2a2a2a)));
        }
    }
    p.cols = saved;
}

/// 並べ替えの指定（「COL ASC」）を読む
pub(crate) fn sort_of(order: &str) -> Option<(String, bool)> {
    let mut it = order.split_whitespace();
    let c = it.next()?.to_string();
    let asc = !it.next().is_some_and(|d| d.eq_ignore_ascii_case("desc"));
    (it.next().is_none()).then_some((c, asc))
}

/// 絞り込みの行（WHERE と ORDER BY）。入力中ならカーソル
pub(crate) fn draw_filter_row(p: &mut Painter, r: Rect, y: f32, where_: &str, order: &str, editing: Option<&(crate::result_grid::Field, crate::vim_field::VimField)>, right: &str, preedit: &str) {
    use crate::result_grid::Field;
    let cell = p.fonts.cell();
    let mut c = p.text(" WHERE ", r.x, y, FINDER_MATCH);
    let w_box = 36;
    p.out.push(Instance::fill(c as f32 * cell.w, y + 1.0, w_box as f32 * cell.w, cell.h - 2.0, rgb(0x141414)));
    match editing.filter(|e| e.0 == Field::Where) {
        Some((_, f)) => {
            draw_vim_field(p, f, c + 1, y, preedit);
        }
        None => {
            p.text(where_, c + 1, y, FG);
        }
    }
    c += w_box + 2;
    let c2 = p.text("ORDER BY ", c, y, FINDER_MATCH);
    p.out.push(Instance::fill(c2 as f32 * cell.w, y + 1.0, 28.0 * cell.w, cell.h - 2.0, rgb(0x141414)));
    match editing.filter(|e| e.0 == Field::Order) {
        Some((_, f)) => {
            draw_vim_field(p, f, c2 + 1, y, preedit);
        }
        None => {
            p.text(order, c2 + 1, y, FG);
        }
    }
    let rw = p.width(right);
    p.text(right, (r.x + r.w).saturating_sub(rw + 3), y, STATUS_FG);
}

/// 差分の 1 行（先頭の印と、変わった部分の地）
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_diff_line(p: &mut Painter, sign: &str, text: &str, span: Option<std::ops::Range<usize>>, word_bg: [f32; 4], sign_color: [f32; 4], col: usize, y: f32) {
    let lead = format!(" {sign} ");
    let mut items = p.items(&lead, sign_color);
    for (b, g) in text.grapheme_indices(true) {
        let width = p.cell_width(g);
        let hit = span.as_ref().is_some_and(|s| b >= s.start && b < s.end);
        items.push(Item { g: Cow::Borrowed(g), width, fg: FG, bg: if hit { Some(word_bg) } else { None }, underline: false });
    }
    p.draw(&items, col, y);
}

/// 診断の波線を引く範囲（行の中の byte）。列の文字が語の一部ならその語、記号ならその字から空白の手前まで。
/// 行頭の空白（列の情報の無い診断は列 0）なら、行の字のある部分全体
pub(crate) fn squiggle_range(line: &str, col: usize) -> (usize, usize) {
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let at = line.char_indices().nth(col).map_or(line.len(), |x| x.0);
    let ch = line[at..].chars().next();
    let whole = || {
        let start = line.len() - line.trim_start().len();
        (start, line.trim_end().len().max(start))
    };
    match ch {
        // 行末（; が足りないなど）: 最後の字に
        None if col > 0 => {
            let end = line.trim_end().len();
            (line.trim_end().char_indices().last().map_or(0, |x| x.0), end)
        }
        None => whole(),
        Some(' ' | '\t') if col == 0 => whole(),
        Some(c) if word(c) => {
            let s = line[..at].char_indices().rev().take_while(|(_, c)| word(*c)).last().map_or(at, |x| x.0);
            let e = line[at..].char_indices().find(|(_, c)| !word(*c)).map_or(line.len(), |x| at + x.0);
            (s, e)
        }
        // 文字列: 閉じる引用符まで
        Some(q @ ('"' | '\'' | '`')) => {
            let e = line[at + 1..].find(q).map_or(line.len(), |i| at + 1 + i + 1);
            (at, e)
        }
        Some(_) => {
            let e = line[at..].char_indices().skip(1).find(|(_, c)| c.is_whitespace()).map_or(line.len(), |x| at + x.0);
            (at, e)
        }
    }
}

/// 波線（x0〜x1、y はセルの下端）。小さな四角を三角波の形に並べる
pub(crate) fn draw_squiggle(p: &mut Painter, x0: f32, x1: f32, bottom: f32, color: [f32; 4]) {
    let cell = p.fonts.cell();
    let px = (cell.h / 16.0).max(1.0).round();
    let amp = px * 2.0;
    let top = bottom - amp - px * 1.5;
    let mut x = x0;
    let mut k = 0usize;
    while x < x1 {
        // 0, 1, 2, 1, 0, … の高さ
        let phase = k % 4;
        let dy = [0.0, 1.0, 2.0, 1.0][phase] * px;
        p.out.push(Instance::fill(x, top + dy, px.min(x1 - x), px, color));
        x += px;
        k += 1;
    }
}

/// ダイアログの地と枠（画面の真ん中に幅 `w`・高さ `h`）。左上の (列, 行)
pub(crate) fn dialog_frame(p: &mut Painter, cols: usize, rows: usize, w: usize, h: usize) -> (usize, usize) {
    let cell = p.fonts.cell();
    let (x0, y0) = ((cols - w) / 2, (rows - h) / 2);
    let (x, y) = ((x0 as f32 - 0.5) * cell.w, (y0 as f32 - 0.5) * cell.h);
    let (fw, fh) = ((w as f32 + 1.0) * cell.w, (h as f32 + 1.0) * cell.h);
    p.out.push(Instance::fill(x, y, fw, fh, STATUS_FG));
    p.out.push(Instance::fill(x + 1.0, y + 1.0, fw - 2.0, fh - 2.0, rgb(0x141414)));
    (x0, y0)
}

/// ダイアログの下のボタンを右から並べる（`right` 列の左まで、画面の `row` 行目）。`primary` は強調する。当たり判定を返す
pub(crate) fn dialog_buttons<T: Copy + PartialEq>(p: &mut Painter, right: usize, row: usize, buttons: &[(&str, T)], primary: T) -> Vec<(usize, usize, usize, T)> {
    let cell = p.fonts.cell();
    let y = row as f32 * cell.h;
    let mut bx = right;
    let mut hits = Vec::new();
    for &(label, b) in buttons {
        let bw = p.width(label);
        bx = bx.saturating_sub(bw + 1);
        let main = b == primary;
        p.out.push(Instance::fill(bx as f32 * cell.w, y, bw as f32 * cell.w, cell.h, if main { ACCENT_FG } else { MATCH_BG }));
        p.text(label, bx, y, if main { BG } else { FG });
        hits.push((row, bx, bx + bw, b));
    }
    hits
}

/// 表示の幅 `w` に収まるように切る（切ったら末尾に …）
pub(crate) fn clip_width(p: &Painter, s: &str, w: usize) -> String {
    if p.width(s) <= w {
        return s.to_string();
    }
    let mut out = String::new();
    for g in s.graphemes(true) {
        if p.width(&out) + p.width(g) + 1 > w {
            break;
        }
        out.push_str(g);
    }
    out.push('…');
    out
}

/// 当たった文字の色（ハイライトのキーワードと同じ控えめな青）
pub(crate) const FINDER_MATCH: [f32; 4] = ACCENT_FG;

/// 最下行の入力欄（`:` `/` `?` `→`）。`active` なら縦棒カーソルと IME の変換中の文字列を出す
/// 入力欄の文字（カーソルの位置に IME の変換中の文字を下線付きで入れ、縦棒を描く）。描いた最後の列を返す
pub(crate) fn field(p: &mut Painter, text: &str, cursor: usize, preedit: &str, col: usize, y: f32, active: bool) -> usize {
    let cursor = cursor.min(text.len());
    let mut items = p.items(&text[..cursor], FG);
    let caret = col + items.iter().map(|i| i.width).sum::<usize>();
    let mut pre_w = 0;
    if active && !preedit.is_empty() {
        let mut pre = p.items(preedit, FG);
        pre.iter_mut().for_each(|i| i.underline = true);
        pre_w = pre.iter().map(|i| i.width).sum();
        items.extend(pre);
    }
    items.extend(p.items(&text[cursor..], FG));
    let end = col + items.iter().map(|i| i.width).sum::<usize>();
    p.draw(&items, col, y);
    if active {
        p.caret(caret + pre_w, y);
    }
    end
}

/// Vim 風の入力欄の 1 行（Insert は縦棒と IME の変換中の文字、ノーマルは四角のカーソル、Visual は選んだ所の地）
pub(crate) fn draw_vim_line(p: &mut Painter, f: &crate::vim_field::VimField, l: usize, col: usize, y: f32, preedit: &str, active: bool) -> usize {
    let cell = p.fonts.cell();
    let text = f.ed.buf.line(l);
    let (cl, cb) = f.cursor();
    let cb = cb.min(text.len());
    if active && l == cl && f.is_insert() {
        return field(p, &text, cb, preedit, col, y, true);
    }
    // Visual: 選んだ所は反転（エディタの Visual と同じ見え方）
    let ls = f.line_start(l);
    let sel = f.visual().filter(|_| active).and_then(|(a, b)| {
        let (s, e) = (a.max(ls) - ls, b.saturating_sub(ls).min(text.len()));
        (a <= ls + text.len() && b > ls && s <= e).then_some((s.min(text.len()), e))
    });
    let end = col + p.width(&text);
    match sel {
        Some((s, e)) => {
            let x1 = col + p.width(&text[..s]);
            let x2 = x1 + p.width(&text[s..e]);
            p.text(&text[..s], col, y, FG);
            // 行末まで選んでいれば 1 セル余分に塗る
            let w = (x2 - x1).max(1);
            p.out.push(Instance::fill(x1 as f32 * cell.w, y, w as f32 * cell.w, cell.h, FG));
            p.text(&text[s..e], x1, y, BG);
            p.text(&text[e..], x2, y, FG);
        }
        None => {
            p.text(&text, col, y, FG);
        }
    }
    if active && l == cl {
        // 四角のカーソル。選んだ中ではもう一度反転して元の色に
        let x = col + p.width(&text[..cb]);
        let ch = text[cb..].graphemes(true).next().unwrap_or(" ");
        let w = p.width(ch).max(1);
        let in_sel = sel.is_some_and(|(s, e)| cb >= s && cb < e.max(s + 1));
        let (bg, fg) = if in_sel { (BG, FG) } else { (CURSOR, BG) };
        p.out.push(Instance::fill(x as f32 * cell.w, y, w as f32 * cell.w, cell.h, bg));
        p.text(ch, x, y, fg);
    }
    end
}

/// 1 行の Vim 風の入力欄
pub(crate) fn draw_vim_field(p: &mut Painter, f: &crate::vim_field::VimField, col: usize, y: f32, preedit: &str) -> usize {
    draw_vim_line(p, f, 0, col, y, preedit, true)
}

pub(crate) fn prompt_line(p: &mut Painter, lead: &str, text: &str, cursor: usize, preedit: &str, y: f32, active: bool) {
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

/// ターミナルの色（16 色は Windows Terminal の Campbell、16〜255 は xterm の 256 色）
pub(crate) fn term_color(c: vt100::Color, default: [f32; 4]) -> [f32; 4] {
    const BASE: [u32; 16] = [0x0c0c0c, 0xc50f1f, 0x13a10e, 0xc19c00, 0x0037da, 0x881798, 0x3a96dd, 0xcccccc, 0x767676, 0xe74856, 0x16c60c, 0xf9f1a5, 0x3b78ff, 0xb4009e, 0x61d6d6, 0xf2f2f2];
    match c {
        vt100::Color::Default => default,
        vt100::Color::Idx(i) if i < 16 => rgb(BASE[i as usize]),
        vt100::Color::Idx(i) if i < 232 => {
            let i = i - 16;
            let v = |x: u8| if x == 0 { 0 } else { 55 + x as u32 * 40 };
            rgb(v(i / 36) << 16 | v((i / 6) % 6) << 8 | v(i % 6))
        }
        vt100::Color::Idx(i) => {
            let g = 8 + (i - 232) as u32 * 10;
            rgb(g << 16 | g << 8 | g)
        }
        vt100::Color::Rgb(r, g, b) => rgb((r as u32) << 16 | (g as u32) << 8 | b as u32),
    }
}

#[cfg(test)]
mod squiggle_tests {
    use super::*;

    #[test]
    pub(crate) fn squiggle_ranges() {
        let l = "    String name = foo.bar(1);";
        assert_eq!(&l[squiggle_range(l, 11).0..squiggle_range(l, 11).1], "name", "語の途中でも語全体");
        assert_eq!(&l[squiggle_range(l, 0).0..squiggle_range(l, 0).1], "String name = foo.bar(1);", "列 0 で空白なら行全体");
        assert_eq!(&l[squiggle_range(l, 25).0..squiggle_range(l, 25).1], "(1);", "記号から空白の手前まで");
        let s = r#"    let name: u32 = "text";"#;
        assert_eq!(&s[squiggle_range(s, 20).0..squiggle_range(s, 20).1], r#""text""#, "文字列は閉じる引用符まで");
        let j = "  名前 = 1";
        assert_eq!(&j[squiggle_range(j, 2).0..squiggle_range(j, 2).1], "名前");
    }
}
