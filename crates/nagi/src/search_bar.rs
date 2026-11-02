//! ファイルの中の検索・置換の帯（IntelliJ の Ctrl+F・Ctrl+R と同じく本文の上）。
//!
//! Find・Replace のコマンド（Ctrl+F・Ctrl+R）で開いたときだけ。Vim の `/` は今までどおり最下行で打つ。
//! 1 行目は虫眼鏡と検索語・件数・Cc（大文字小文字）・W（単語）・.*（正規表現）・↑ ↓・×、置換なら 2 行目に置換後の語と「置換」「すべて置換」。
//! 帯の分だけ窓を下げる（panes.rs の windows_top）。

use nagi_vim::Key;

use crate::editor::{Editor, State};
use crate::panes::Rect;

/// 帯の押せる所
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BarHit {
    Query,
    Repl,
    Case,
    Words,
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
        let field_end = right.saturating_sub(32);
        let mut v = vec![
            (BarHit::Query, r.y, r.x, field_end),
            (BarHit::Case, r.y, field_end + 8, field_end + 12),
            (BarHit::Words, r.y, field_end + 12, field_end + 15),
            (BarHit::Regex, r.y, field_end + 15, field_end + 19),
            (BarHit::Prev, r.y, field_end + 21, field_end + 23),
            (BarHit::Next, r.y, field_end + 24, field_end + 26),
            (BarHit::Close, r.y, field_end + 28, field_end + 30),
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
            BarHit::Case => {
                self.key(Key::Alt('c'));
            }
            BarHit::Words => {
                self.key(Key::Alt('o'));
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

    fn click(&self, ed: &mut Editor, r: Rect, at: crate::layout::At, clicks: u32) -> crate::layout::Click {
        let (row, col) = at.screen;
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
        let px = crate::metrics::icon(crate::metrics::Icon::Small);
        crate::icons::draw(p, "g_search", x0 + cell.w * 0.5, crate::metrics::centered(y0, px), px, None);
        if let Some((_, row, _, c1)) = at(BarHit::Query) {
            p.cols = c1;
            field(p, &ed.query.text, ed.query.cursor, if ed.replace_field == 0 { &ed.preedit } else { "" }, r.x + 1 + crate::metrics::icon_cols(crate::metrics::Icon::Small), row as f32 * cell.h, ed.replace_field == 0);
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
        // IntelliJ と同じ 3 つの切り替え: Cc（大文字小文字）・W（単語）・.*（正規表現）。入っていれば地を付ける
        for (b, label, on) in [(BarHit::Case, "Cc", ed.search.case), (BarHit::Words, "W", ed.search.words), (BarHit::Regex, ".*", ed.search.regex)] {
            if let Some((_, row, c0, c1)) = at(b) {
                if on {
                    p.out.push(Instance::rounded(c0 as f32 * cell.w, row as f32 * cell.h + crate::metrics::pad(), (c1 - c0) as f32 * cell.w, cell.h - 2.0 * crate::metrics::pad(), crate::metrics::radius(crate::metrics::Radius::Control), LIST_SEL_BG));
                }
                let lw = p.width(label);
                p.text(label, c0 + (c1 - c0).saturating_sub(lw) / 2, row as f32 * cell.h, if on { FG_BRIGHT } else { STATUS_FG });
            }
        }
        for (b, s) in [(BarHit::Prev, "↑"), (BarHit::Next, "↓"), (BarHit::Close, "×")] {
            if let Some((_, row, c0, _)) = at(b) {
                p.text(s, c0, row as f32 * cell.h, FG);
            }
        }
        if let Some((_, row, _, c1)) = at(BarHit::Repl) {
            p.cols = c1;
            p.text("→", r.x + 1, row as f32 * cell.h, STATUS_FG);
            field(p, &ed.repl.text, ed.repl.cursor, if ed.replace_field == 1 { &ed.preedit } else { "" }, r.x + 1 + crate::metrics::icon_cols(crate::metrics::Icon::Small), row as f32 * cell.h, ed.replace_field == 1);
            p.cols = r.x + r.w;
            for (b, s) in [(BarHit::ReplaceOne, "置換"), (BarHit::ReplaceAll, "すべて置換")] {
                if let Some((_, row, c0, c1)) = at(b) {
                    p.out.push(Instance::rounded(c0 as f32 * cell.w, row as f32 * cell.h + crate::metrics::pad(), (c1 - c0) as f32 * cell.w, cell.h - 2.0 * crate::metrics::pad(), crate::metrics::radius(crate::metrics::Radius::Control), RAISED_BG));
                    let lw = p.width(s);
                    p.text(s, c0 + (c1 - c0).saturating_sub(lw) / 2, row as f32 * cell.h, FG);
                }
            }
        }
        p.cols = saved;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    fn count(e: &mut Editor) -> usize {
        e.refresh_matches();
        e.match_count().map_or(0, |c| c.1)
    }

    /// IntelliJ と同じ 3 つの切り替え: Cc（Alt+C）・W（Alt+O）・.*（Alt+X）。帯のアイコンを押しても同じ
    #[test]
    fn case_and_words_toggles() {
        let mut e = Editor::new(Buffer::from_str("Foo foo food\n"), None);
        e.set_screen(120, 30);
        e.shortcut("Find");
        for c in "foo".chars() {
            e.key(Key::Char(c));
        }
        assert_eq!(count(&mut e), 3, "既定は区別しない・部分一致");
        e.key(Key::Alt('c'));
        assert_eq!(count(&mut e), 2, "大文字小文字を区別");
        e.key(Key::Alt('o'));
        assert_eq!(count(&mut e), 1, "単語として");
        // 帯のアイコン
        let (_, row, c0, _) = e.search_bar_hits().into_iter().find(|h| h.0 == BarHit::Case).unwrap();
        e.search_bar_click(row, c0);
        assert!(!e.search.case);
        assert_eq!(count(&mut e), 2, "Foo・foo（food は単語でない）");
    }
}
