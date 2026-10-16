//! 画面の組み立て: 行番号・本文・カーソル・ステータス行をインスタンス列にする。

use nagi_render::{Instance, Renderer};
use nagi_text::Fonts;

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

/// 文字列を (col, row) から描く。戻り値は描き終えた列。
fn text(out: &mut Vec<Instance>, fonts: &mut Fonts, r: &mut Renderer, s: &str, col: usize, y: f32, color: [f32; 4], max_cols: usize) -> usize {
    let cell = fonts.cell();
    let mut c = col;
    let items: Vec<(String, usize)> = nagi_text::graphemes(fonts, s).map(|(g, _, w)| (g.to_string(), w)).collect();
    for (g, w) in items {
        if c + w > max_cols {
            break;
        }
        if let Some(key) = fonts.glyph_for(&g, w) {
            if let Some(a) = r.glyph(fonts, key) {
                let x = c as f32 * cell.w;
                out.push(Instance {
                    rect: [x + a.left, y + a.top, a.uv[2], a.uv[3]],
                    uv: a.uv,
                    color,
                    kind: if a.color { 2 } else { 1 },
                });
            }
        }
        c += w;
    }
    c
}

/// 画面に出るグリフを先にまとめてアトラスへ載せる（並列ラスタライズ）
pub fn prefetch(ed: &Editor, fonts: &mut Fonts, r: &mut Renderer) {
    let cell = fonts.cell();
    let (_, h) = r.size();
    let rows = ((h as f32 / cell.h) as usize).saturating_sub(1).max(1);
    let total = ed.buf.line_count();
    let mut keys = Vec::new();
    let mut lines: Vec<String> = (ed.top..(ed.top + rows).min(total)).map(|l| ed.buf.line(l)).collect();
    lines.push("~ 0123456789,[No Name]-- INSERT VISUAL".into());
    for line in &lines {
        let items: Vec<(String, usize)> = nagi_text::graphemes(fonts, line).map(|(g, _, w)| (g.to_string(), w)).collect();
        for (g, w) in items {
            if let Some(k) = fonts.glyph_for(&g, w) {
                keys.push(k);
            }
        }
    }
    r.prefetch(fonts, &keys);
}

pub fn build(ed: &mut Editor, fonts: &mut Fonts, r: &mut Renderer) -> Vec<Instance> {
    let cell = fonts.cell();
    let (w, h) = r.size();
    let cols = (w as f32 / cell.w) as usize;
    let rows = ((h as f32 / cell.h) as usize).saturating_sub(1).max(1);
    let gutter = GUTTER;
    let mut out = Vec::with_capacity(cols * rows + 64);
    let total = ed.buf.line_count();
    // ターミナルにはできない細さ: 縦棒カーソルと下線は 2px / 1px で描く
    let bar = (cell.w / 6.0).round().max(2.0);
    let underline = (cell.h / 16.0).round().max(1.0);

    for row in 0..rows {
        let ln = ed.top + row;
        let y = row as f32 * cell.h;
        if ln >= total {
            // ファイル末尾より後ろは Vim と同じく ~
            text(&mut out, fonts, r, "~", 0, y, TILDE, cols);
            continue;
        }
        let no = format!("{:>5} ", ln + 1);
        text(&mut out, fonts, r, &no, 0, y, LINE_NO, cols);
        let line = ed.buf.line(ln);
        if ln != ed.cursor.line || ed.mode == Mode::Command {
            text(&mut out, fonts, r, &line, gutter, y, FG, cols);
            continue;
        }
        let at = ed.cursor.byte.min(line.len());
        let ccol = nagi_core::display_col(&line, at, false);
        match ed.mode {
            Mode::Insert => {
                // 本文 → 変換中の文字列（下線）→ 残りの本文。カーソルは縦棒
                let c = text(&mut out, fonts, r, &line[..at], gutter, y, FG, cols);
                let pc = text(&mut out, fonts, r, &ed.preedit, c, y, FG, cols);
                if !ed.preedit.is_empty() {
                    out.push(Instance::fill(c as f32 * cell.w, y + cell.h - underline, (pc - c) as f32 * cell.w, underline, FG));
                }
                text(&mut out, fonts, r, &line[at..], pc, y, FG, cols);
                let caret = match ed.preedit_cursor {
                    Some((a, _)) if !ed.preedit.is_empty() => c + nagi_core::display_col(&ed.preedit, a, false),
                    _ if !ed.preedit.is_empty() => pc,
                    _ => c,
                };
                out.push(Instance::fill(caret as f32 * cell.w, y, bar, cell.h, CURSOR));
            }
            _ => {
                // ブロックカーソル（文字の下に敷き、文字は背景色で描く）
                let g = unicode_first_grapheme(&line[at..]);
                let cw = if g.is_empty() { 1 } else { nagi_core::grapheme_width(g, false) };
                out.push(Instance::fill((gutter + ccol) as f32 * cell.w, y, cw as f32 * cell.w, cell.h, CURSOR));
                let c = text(&mut out, fonts, r, &line[..at], gutter, y, FG, cols);
                let c2 = text(&mut out, fonts, r, g, c, y, BG, cols);
                text(&mut out, fonts, r, &line[at + g.len()..], c2, y, FG, cols);
            }
        }
    }

    // 最下行: 左にモード・メッセージ・コマンドライン、右に位置（Vim の showmode と ruler と同じ形）
    let y = rows as f32 * cell.h;
    match ed.mode {
        Mode::Command => {
            text(&mut out, fonts, r, &format!(":{}", ed.cmdline), 0, y, FG, cols);
            let c = 1 + nagi_core::display_col(&ed.cmdline, ed.cmd_cursor, false);
            out.push(Instance::fill(c as f32 * cell.w, y, bar, cell.h, CURSOR));
        }
        _ => {
            let left = if !ed.message.is_empty() {
                ed.message.clone()
            } else if ed.mode == Mode::Insert {
                "-- INSERT --".into()
            } else {
                String::new()
            };
            text(&mut out, fonts, r, &left, 0, y, FG, cols);
        }
    }
    let name = ed.buf.path.as_ref().map_or("[No Name]".to_string(), |p| p.display().to_string());
    let pos = format!(
        "{}{}{}   {}{}   {},{} ",
        if ed.count > 0 { ed.count.to_string() } else { String::new() },
        ed.pending.map(String::from).unwrap_or_default(),
        ed.showcmd,
        name,
        if ed.buf.dirty { " [+]" } else { "" },
        ed.cursor.line + 1,
        nagi_core::display_col(&ed.buf.line(ed.cursor.line), ed.cursor.byte, false) + 1
    );
    let pc = cols.saturating_sub(nagi_text::graphemes(fonts, &pos).map(|(_, _, w)| w).sum::<usize>());
    text(&mut out, fonts, r, &pos, pc, y, STATUS_FG, cols);
    out
}

fn unicode_first_grapheme(s: &str) -> &str {
    use unicode_segmentation::UnicodeSegmentation;
    s.graphemes(true).next().unwrap_or("")
}
