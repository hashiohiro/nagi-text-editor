//! 識別子の補完（Insert モードの `Ctrl-n` / `Ctrl-p`。architecture.md §10。自動では出さない）。
//!
//! Vim の `i_CTRL-N` と同じ動き:
//! - 最初の `Ctrl-n` でカーソルの前の語で始まる単語を探し、1 つ目をその場に入れて候補を出す
//!   （今のバッファのカーソルより後ろ → 先頭からカーソルまで → ほかのバッファ → 索引の名前の順。`Ctrl-p` は後ろ向き）
//! - 続けて `Ctrl-n` / `Ctrl-p` で次・前へ。端まで行くと打った語に戻る
//! - ほかのキーはそのまま確定して処理する。`Ctrl-e` は取り消して打った語に戻す、`Ctrl-y` は確定
//! - 大文字小文字は区別せずに探し、候補はそのままの形で入れる（ignorecase と同じ）

use std::collections::HashSet;

use nagi_vim::Key;

use crate::buffers::Slot;
use crate::editor::Editor;

/// 候補の数の上限（大きなファイルでも一覧を作る時間を抑える）
const MAX_ITEMS: usize = 500;
/// 今のバッファで単語を探す範囲（カーソルの前後）
const SCAN_BYTES: usize = 1 << 20;

pub struct Completion {
    /// 打った語の始まり（絶対 byte）
    pub start: usize,
    /// 打った語
    pub original: String,
    pub items: Vec<String>,
    /// 選んでいる候補（None は打った語のまま）
    pub sel: Option<usize>,
    /// 候補ごとの印と補足（SQL の補完の型と表の名前。ふつうの補完では空）
    pub notes: Vec<(char, String)>,
    /// IntelliJ のような補完（Ctrl+Space。打つたびに絞り込む）なら、その中身（smart_complete.rs）
    pub smart: Option<crate::smart_complete::SmartState>,
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// `text` の中の単語を、`from` から先へ（`forward`）または手前へ並べる
fn words_in(text: &str, from: usize, forward: bool) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices() {
        match (word_char(c), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                out.push((s, &text[s..i]));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push((s, &text[s..]));
    }
    let (mut after, mut before): (Vec<_>, Vec<_>) = out.into_iter().partition(|(s, _)| *s >= from);
    if forward {
        after.extend(before);
        after
    } else {
        before.reverse();
        after.reverse();
        before.extend(after);
        before
    }
}

impl Editor {
    /// Insert モードで補完中のキー。扱ったら true（ほかのキーは確定してから普通に処理する）
    pub(crate) fn completion_key(&mut self, key: Key) -> bool {
        match key {
            Key::Ctrl('n') | Key::Ctrl('p') => {
                let forward = key == Key::Ctrl('n');
                if self.completion.is_none() {
                    self.start_completion(forward);
                } else {
                    self.step_completion(forward);
                }
                true
            }
            Key::Ctrl('e') if self.completion.is_some() => {
                if let Some(c) = &mut self.completion {
                    c.sel = None;
                }
                self.apply_completion();
                self.completion = None;
                true
            }
            Key::Ctrl('y') if self.completion.is_some() => {
                self.completion = None;
                true
            }
            _ => {
                // 確定して、キーは普通に処理する
                self.completion = None;
                false
            }
        }
    }

    /// SQL コンソールの補完（`auto` は `.` を打ったときに一覧だけ出す）
    pub(crate) fn start_sql_completion(&mut self, auto: bool) {
        let line = self.buf.line(self.cursor.line);
        let b = self.cursor.byte.min(line.len());
        let ws = line[..b].char_indices().rev().take_while(|(_, c)| word_char(*c) || *c == '#').last().map_or(b, |(i, _)| i);
        let prefix = line[ws..b].to_string();
        let start = self.buf.line_start(self.cursor.line) + ws;
        // 別名.
        let qualifier = line[..ws].strip_suffix('.').map(|q| q.chars().rev().take_while(|c| word_char(*c)).collect::<String>().chars().rev().collect::<String>()).filter(|q| !q.is_empty());
        let cands = self.sql_candidates(&prefix, qualifier.as_deref());
        if cands.is_empty() {
            if !auto {
                self.notify.info("-- 補完の候補はありません（列を読んでいるところなら、もう一度 Ctrl+Space）");
            }
            return;
        }
        let items = cands.iter().map(|c| c.0.clone()).collect();
        let notes = cands.into_iter().map(|c| (c.1, c.2)).collect();
        self.completion = Some(Completion { start, original: prefix, items, sel: if auto { None } else { Some(0) }, notes, smart: None });
        if !auto {
            self.apply_completion();
        }
    }

    fn start_completion(&mut self, forward: bool) {
        if self.console_source().is_some() {
            self.start_sql_completion(false);
            return;
        }
        let line = self.buf.line(self.cursor.line);
        let b = self.cursor.byte.min(line.len());
        let ws = line[..b].char_indices().rev().take_while(|(_, c)| word_char(*c)).last().map_or(b, |(i, _)| i);
        let prefix = line[ws..b].to_string();
        let start = self.buf.line_start(self.cursor.line) + ws;
        let low = prefix.to_lowercase();
        let mut seen: HashSet<String> = HashSet::new();
        seen.insert(prefix.clone());
        let mut items = Vec::new();
        let mut push = |w: &str, items: &mut Vec<String>| {
            if items.len() < MAX_ITEMS && w.len() > prefix.len() && w.to_lowercase().starts_with(&low) && seen.insert(w.to_string()) {
                items.push(w.to_string());
            }
        };
        // 今のバッファ（カーソルの前後 1MB）
        let lo = start.saturating_sub(SCAN_BYTES);
        let hi = (start + SCAN_BYTES).min(self.buf.len_bytes());
        let lo = self.buf.line_start(self.buf.pos_of(lo).0);
        let text = self.buf.slice_string(lo, hi);
        for (s, w) in words_in(&text, start - lo, forward) {
            // 打っている語そのものは除く
            if lo + s != start {
                push(w, &mut items);
            }
        }
        // ほかのバッファ
        for slot in &self.docs {
            if let Slot::Parked(d) = slot {
                let n = d.buf.len_bytes().min(SCAN_BYTES);
                let t = d.buf.slice_string(0, n);
                for (_, w) in words_in(&t, 0, true) {
                    push(w, &mut items);
                }
            }
        }
        // 索引の名前（ほかのファイルの定義）
        if let Some(idx) = &self.nav.index {
            let mut names: Vec<&str> = idx.names().filter(|n| n.to_lowercase().starts_with(&low)).collect();
            names.sort_unstable();
            for n in names {
                push(n, &mut items);
            }
        }
        if items.is_empty() {
            self.notify.info("-- 補完の候補はありません");
            return;
        }
        self.completion = Some(Completion { start, original: prefix, items, sel: Some(0), notes: Vec::new(), smart: None });
        self.apply_completion();
    }

    fn step_completion(&mut self, forward: bool) {
        let Some(c) = &mut self.completion else { return };
        let n = c.items.len();
        // 候補の並びの両端の先は「打った語」（None）
        c.sel = match (c.sel, forward) {
            (None, true) => Some(0),
            (None, false) => Some(n - 1),
            (Some(i), true) if i + 1 < n => Some(i + 1),
            (Some(i), false) if i > 0 => Some(i - 1),
            _ => None,
        };
        self.apply_completion();
    }

    /// 選んでいる候補（無ければ打った語）を本文に入れる
    fn apply_completion(&mut self) {
        let Some(c) = &self.completion else { return };
        let text = match c.sel {
            Some(i) => c.items[i].clone(),
            None => c.original.clone(),
        };
        let start = c.start;
        let end = self.abs();
        self.edit(start, end, &text);
        self.set_abs(start + text.len());
    }
}


/// 描き方（view.rs の Painter で描く）
pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    /// 補完の候補（カーソルの下に最大 10 件。下に余白が無ければ上に）。ターミナルの Vim の補完メニューと同じ形
    pub(crate) fn draw_completion(p: &mut Painter, ed: &Editor, c: &crate::complete::Completion, rows: usize, cols: usize) {
        const MAX: usize = 10;
        let cell = p.fonts.cell();
        let rect = ed.active_rect();
        let (sl, sb) = ed.buf.pos_of(c.start);
        if sl < ed.top {
            return;
        }
        // 折り返していれば、その行の中の画面の行と列
        let Some((r, c0)) = ed.screen_pos(sl, sb) else { return };
        let row = rect.y + r;
        let col = (rect.x + ed.gutter() + c0).saturating_sub(1);
        let n = c.items.len().min(MAX);
        // 選んでいる候補が見える範囲
        let first = c.sel.map_or(0, |s| s.saturating_sub(MAX - 1));
        let notew = c.notes.iter().skip(first).take(n).map(|x| p.width(&x.1)).max().unwrap_or(0);
        let width = c.items.iter().skip(first).take(n).map(|i| p.width(i)).max().unwrap_or(1) + 2 + if c.notes.is_empty() { 0 } else { notew + 5 };
        let col = col.min(cols.saturating_sub(width));
        let below = row + 1 + n <= rows;
        let top = if below { row + 1 } else { row.saturating_sub(n) };
        for (k, item) in c.items.iter().skip(first).take(n).enumerate() {
            let y = (top + k) as f32 * cell.h;
            let selected = c.sel == Some(first + k);
            let (bg, fg) = if selected { (FG, BG) } else { (MATCH_BG, FG) };
            p.out.push(Instance::fill(col as f32 * cell.w, y, width as f32 * cell.w, cell.h, bg));
            match c.notes.get(first + k) {
                Some((mark, note)) => {
                    let mut b = [0u8; 4];
                    let mc = match mark {
                        'K' => rgb(0xd7af5f),
                        'F' | 'T' | 'c' | 'I' | 'e' => FINDER_MATCH,
                        'V' | 'f' => rgb(0xaf87d7),
                        'm' => rgb(0xd7af87),
                        'v' => STATUS_FG,
                        _ => fg,
                    };
                    let x = p.text(" ", col, y, fg);
                    let x = p.text(mark.encode_utf8(&mut b), x, y, if selected { BG } else { mc });
                    p.text(&format!(" {item}"), x, y, fg);
                    let nw = p.width(note);
                    p.text(note, (col + width).saturating_sub(nw + 1), y, if selected { BG } else { STATUS_FG });
                }
                None => {
                    p.text(&format!(" {item}"), col, y, fg);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    fn keys(e: &mut Editor, k: &str) {
        for key in nagi_vim::parse_keys(k, " ") {
            e.key(key);
        }
    }

    #[test]
    fn ctrl_n_and_p() {
        let mut e = Editor::new(Buffer::from_str("alpha beta\nAlphabet\nalpine\n"), None);
        // 3 行目の後ろで al → 前向き: 後ろには無いので先頭から alpha, Alphabet, alpine の順
        keys(&mut e, "Go al<C-n>");
        assert_eq!(e.buf.line(3), " alpha");
        assert_eq!(e.completion.as_ref().unwrap().items, vec!["alpha", "Alphabet", "alpine"]);
        keys(&mut e, "<C-n><C-n>");
        assert_eq!(e.buf.line(3), " alpine");
        // 端の先は打った語
        keys(&mut e, "<C-n>");
        assert_eq!(e.buf.line(3), " al");
        // Ctrl-p で後ろから
        keys(&mut e, "<C-p>");
        assert_eq!(e.buf.line(3), " alpine");
        // ほかのキーで確定して続けて打つ
        keys(&mut e, "!<Esc>");
        assert_eq!(e.buf.line(3), " alpine!");
        assert!(e.completion.is_none());
        // Ctrl-e で取り消す
        keys(&mut e, "o be<C-n><C-e><Esc>");
        assert_eq!(e.buf.line(4), "  be", "前の行の字下げを引き継ぐ");
        // Ctrl-y で確定して続けて打つ
        keys(&mut e, "o be<C-n><C-y>X<Esc>");
        assert_eq!(e.buf.line(5), "   betaX");
    }
}
