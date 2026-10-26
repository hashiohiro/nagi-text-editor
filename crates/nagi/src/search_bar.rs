//! ファイルの中の検索・置換の帯（IntelliJ の Ctrl+F・Ctrl+R と同じく本文の上）。
//!
//! Find・Replace のコマンド（Ctrl+F・Ctrl+R）で開いたときだけ。Vim の `/` は今までどおり最下行で打つ。
//! 1 行目は虫眼鏡と検索語・件数・正規表現の札・↑ ↓・×、置換なら 2 行目に置換後の語と「置換」「すべて置換」。
//! 帯の分だけ窓を下げる（panes.rs の windows_top）。

use nagi_vim::Key;

use crate::editor::{Editor, State};
use crate::panes::Rect;

/// 帯の押せる所
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BarHit {
    Query,
    Repl,
    Regex,
    Prev,
    Next,
    Close,
    ReplaceOne,
    ReplaceAll,
}

impl Editor {
    /// 帯の行の数（出していなければ 0）
    pub fn search_bar_rows(&self) -> usize {
        match self.state() {
            State::Search if self.search_bar => 1,
            State::Replace if self.search_bar => 2,
            _ => 0,
        }
    }

    /// 帯の矩形（窓の領域の上、タブと SQL の帯の下）
    pub fn search_bar_rect(&self) -> Option<Rect> {
        let h = self.search_bar_rows();
        if h == 0 {
            return None;
        }
        let (x, w) = self.windows_x();
        Some(Rect { x, y: self.area().y + self.tabs_row() + self.console_row(), w, h })
    }

    /// 帯の部品の位置（部品, 行, 始めの列, 終わりの列）。右から ×・↓・↑・件数・[.*]、置換の行は右に 2 つのボタン
    pub(crate) fn search_bar_hits(&self) -> Vec<(BarHit, usize, usize, usize)> {
        let Some(r) = self.search_bar_rect() else { return Vec::new() };
        let right = r.x + r.w;
        let field_end = right.saturating_sub(24);
        let mut v = vec![
            (BarHit::Query, r.y, r.x, field_end),
            (BarHit::Regex, r.y, field_end + 10, field_end + 14),
            (BarHit::Prev, r.y, field_end + 15, field_end + 17),
            (BarHit::Next, r.y, field_end + 18, field_end + 20),
            (BarHit::Close, r.y, field_end + 21, field_end + 23),
        ];
        if r.h > 1 {
            v.push((BarHit::Repl, r.y + 1, r.x, field_end));
            v.push((BarHit::ReplaceOne, r.y + 1, field_end + 1, field_end + 7));
            v.push((BarHit::ReplaceAll, r.y + 1, field_end + 8, field_end + 20));
        }
        v
    }

    pub(crate) fn search_bar_click(&mut self, row: usize, col: usize) {
        let Some(hit) = self.search_bar_hits().into_iter().find(|h| h.1 == row && col >= h.2 && col < h.3).map(|h| h.0) else { return };
        match hit {
            BarHit::Query => self.replace_field = 0,
            BarHit::Repl => self.replace_field = 1,
            BarHit::Regex => {
                self.key(Key::Alt('r'));
            }
            BarHit::Prev => self.bar_step(false),
            BarHit::Next => self.bar_step(true),
            BarHit::Close => {
                self.key(Key::Esc);
            }
            BarHit::ReplaceOne => {
                self.key(Key::Enter);
            }
            BarHit::ReplaceAll => {
                self.key(Key::Alt('a'));
            }
        }
    }
}

/// 帯（layout.rs の View）
pub struct SearchBarView;

impl crate::layout::View for SearchBarView {
    fn rect(&self, ed: &Editor, s: crate::layout::Screen) -> Option<Rect> {
        let _ = s;
        ed.search_bar_rect()
    }

    fn draw(&self, ed: &Editor, p: &mut crate::view::Painter, r: Rect, s: crate::layout::Screen) {
        let _ = s;
        draw::draw_search_bar(p, ed, r);
    }

    fn click(&self, ed: &mut Editor, r: Rect, row: usize, col: usize, clicks: u32) -> crate::layout::Click {
        let _ = (r, clicks);
        ed.search_bar_click(row, col);
        crate::layout::Click::Done
    }
}

pub(crate) mod draw {
    use super::*;
    use crate::view::*;

    pub(crate) fn draw_search_bar(p: &mut Painter, ed: &Editor, r: Rect) {
        let cell = p.fonts.cell();
        let (x0, y0, w, h) = (r.x as f32 * cell.w, r.y as f32 * cell.h, r.w as f32 * cell.w, r.h as f32 * cell.h);
        p.out.push(Instance::fill(x0, y0, w, h, PANEL_BG));
        p.out.push(Instance::fill(x0, y0 + h - 1.0, w, 1.0, SEPARATOR));
        let saved = p.cols;
        let hits = ed.search_bar_hits();
        let at = |b: BarHit| hits.iter().find(|h| h.0 == b).copied();
        // 検索語（と置換後の語）
        let px = crate::icons::small(cell.h);
        crate::icons::draw(p, "g_search", x0 + cell.w * 0.5, crate::icons::centered(y0, cell.h, px), px, None);
        if let Some((_, row, _, c1)) = at(BarHit::Query) {
            p.cols = c1;
            field(p, &ed.query.text, ed.query.cursor, if ed.replace_field == 0 { &ed.preedit } else { "" }, r.x + 3, row as f32 * cell.h, ed.replace_field == 0);
            // 件数（3/12）。見つからなければ赤く
            p.cols = r.x + r.w;
            let (count, found) = match (ed.search.current, ed.match_count()) {
                _ if ed.query.text.is_empty() => (String::new(), true),
                (Some(_), Some((Some(i), n))) => (format!("{}/{n}", i + 1), true),
                (Some(_), _) => ("?/…".into(), true),
                _ => ("0 件".into(), false),
            };
            p.text(&count, c1 + 1, row as f32 * cell.h, if found { STATUS_FG } else { DIAG_ERROR });
        }
        if let Some((_, row, c0, c1)) = at(BarHit::Regex) {
            if ed.search.regex {
                p.out.push(Instance::rounded(c0 as f32 * cell.w, row as f32 * cell.h + 2.0, (c1 - c0) as f32 * cell.w, cell.h - 4.0, 5.0, LIST_SEL_BG));
            }
            p.text("[.*]", c0, row as f32 * cell.h, if ed.search.regex { FG_BRIGHT } else { STATUS_FG });
        }
        for (b, s) in [(BarHit::Prev, "↑"), (BarHit::Next, "↓"), (BarHit::Close, "×")] {
            if let Some((_, row, c0, _)) = at(b) {
                p.text(s, c0, row as f32 * cell.h, FG);
            }
        }
        if let Some((_, row, _, c1)) = at(BarHit::Repl) {
            p.cols = c1;
            p.text("→", r.x + 1, row as f32 * cell.h, STATUS_FG);
            field(p, &ed.repl.text, ed.repl.cursor, if ed.replace_field == 1 { &ed.preedit } else { "" }, r.x + 3, row as f32 * cell.h, ed.replace_field == 1);
            p.cols = r.x + r.w;
            for (b, s) in [(BarHit::ReplaceOne, "置換"), (BarHit::ReplaceAll, "すべて置換")] {
                if let Some((_, row, c0, c1)) = at(b) {
                    p.out.push(Instance::rounded(c0 as f32 * cell.w, row as f32 * cell.h + 2.0, (c1 - c0) as f32 * cell.w, cell.h - 4.0, 5.0, RAISED_BG));
                    let lw = p.width(s);
                    p.text(s, c0 + (c1 - c0).saturating_sub(lw) / 2, row as f32 * cell.h, FG);
                }
            }
        }
        p.cols = saved;
    }
}
