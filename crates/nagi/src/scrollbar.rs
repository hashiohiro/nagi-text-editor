//! エディタの窓の右端のスクロールバー（IntelliJ のエラーストライプと同じ）。
//!
//! 窓ごとに右端の 1 列に重ねて描く: 見えている範囲のつまみ、エラー・警告の印（赤・黄）、git の変更の印（緑・青・赤）。
//! 印を押すとその行へ跳び、つまみはドラッグで送り、ほかの所を押すとその位置を真ん中に出す。

use crate::editor::Editor;
use crate::panes::Rect;

/// 1 つの窓のスクロールバーに出すもの
pub struct Stripe {
    /// 窓の番号と、本文の矩形（右端の 1 列がスクロールバー）
    pub win: usize,
    pub rect: Rect,
    /// バッファの行の数と、先頭に見せている行
    pub total: usize,
    pub top: usize,
    /// 印（行, 色の種類）
    pub marks: Vec<(usize, Mark)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Error,
    Warning,
    Added,
    Modified,
    Deleted,
}

impl Stripe {
    /// スクロールバーの列（画面の列）
    pub fn col(&self) -> usize {
        self.rect.x + self.rect.w - 1
    }

    /// 行 → スクロールバーの中の位置（0.0〜1.0）
    pub fn frac(&self, line: usize) -> f32 {
        line as f32 / self.total.max(1) as f32
    }

    /// つまみ（0.0〜1.0 の始まりと長さ）。全部見えていれば None
    pub fn thumb(&self) -> Option<(f32, f32)> {
        (self.total > self.rect.h).then(|| (self.frac(self.top), (self.rect.h as f32 / self.total as f32).max(0.03)))
    }

    /// スクロールバーの `row`（画面の行）に当たる行
    pub fn line_at(&self, row: usize) -> usize {
        let k = row.saturating_sub(self.rect.y) as f32 + 0.5;
        ((k / self.rect.h.max(1) as f32) * self.total as f32) as usize
    }

    /// `row` の近く（スクロールバーの 1 行の中）にある印の行。エラーを先に
    pub fn mark_near(&self, row: usize) -> Option<usize> {
        let k = row.saturating_sub(self.rect.y);
        let near = |l: usize| (self.frac(l) * self.rect.h as f32) as usize == k;
        let mut hits: Vec<&(usize, Mark)> = self.marks.iter().filter(|(l, _)| near(*l)).collect();
        hits.sort_by_key(|(l, m)| (*m as u8, *l));
        hits.first().map(|(l, _)| *l)
    }
}

impl Editor {
    /// 見えている窓のスクロールバー（エディタを出していないとき・本文の無い窓では空）
    pub fn stripes(&self) -> Vec<Stripe> {
        if !self.editor_shown() {
            return Vec::new();
        }
        self.window_rects()
            .into_iter()
            .filter_map(|(win, rect)| {
                let st = self.win_state(win)?;
                let buf = if st.doc == self.core.cur_doc { &self.core.buf } else { &self.parked_doc(st.doc)?.buf };
                let rect = Rect { h: self.text_rows(rect), ..rect };
                let path = buf.path.as_deref();
                let mut marks: Vec<(usize, Mark)> = self.diag_lines(path).into_iter().map(|(l, e)| (l, if e { Mark::Error } else { Mark::Warning })).collect();
                marks.extend(self.git_markers(path).into_iter().map(|(l, m)| {
                    (l, match m {
                        crate::git_ui::Marker::Add => Mark::Added,
                        crate::git_ui::Marker::Mod => Mark::Modified,
                        crate::git_ui::Marker::DelAbove => Mark::Deleted,
                    })
                }));
                let top = if win == self.core.cur_win { self.core.top } else { st.top };
                Some(Stripe { win, rect, total: buf.line_count(), top, marks })
            })
            .collect()
    }

    /// 画面の (行, 列) がスクロールバーの上か
    pub fn stripe_at(&self, row: usize, col: usize) -> Option<Stripe> {
        self.stripes().into_iter().find(|s| col == s.col() && row >= s.rect.y && row < s.rect.y + s.rect.h)
    }

    /// スクロールバーを押した: 印の近くならその行へ跳ぶ、つまみならドラッグを始める（true）、ほかはその位置を真ん中に
    pub(crate) fn stripe_press(&mut self, row: usize, col: usize) -> bool {
        let Some(s) = self.stripe_at(row, col) else { return false };
        if s.win != self.core.cur_win {
            self.focus_window(s.win);
        }
        self.leave_panel();
        if let Some(line) = s.mark_near(row) {
            self.push_jump();
            self.core.cursor.line = line.min(self.core.buf.line_count().saturating_sub(1));
            self.with_motion(|m, c| m.first_non_blank(c));
            self.core.top = line.saturating_sub(self.rows / 3);
            self.stripe_grab = None;
            return true;
        }
        let k = row - s.rect.y;
        let on_thumb = s.thumb().is_some_and(|(a, len)| {
            let (a, b) = ((a * s.rect.h as f32) as usize, ((a + len) * s.rect.h as f32).ceil() as usize);
            k >= a && k < b.max(a + 1)
        });
        if on_thumb {
            // つかんだ所とつまみの頭の差を保って送る
            self.stripe_grab = Some(s.line_at(row) as isize - s.top as isize);
        } else {
            let line = s.line_at(row);
            self.scroll(line as isize - (s.rect.h / 2) as isize - self.core.top as isize);
            self.stripe_grab = None;
        }
        true
    }

    /// つまみのドラッグ（`row` は画面の行）
    pub(crate) fn stripe_drag(&mut self, row: usize) {
        let Some(grab) = self.stripe_grab else { return };
        let Some(s) = self.stripes().into_iter().find(|s| s.win == self.core.cur_win) else { return };
        let row = row.clamp(s.rect.y, s.rect.y + s.rect.h.saturating_sub(1));
        let top = (s.line_at(row) as isize - grab).max(0);
        self.scroll(top - self.core.top as isize);
    }
}

/// スクロールバー（layout.rs の View）。窓の並ぶ所を持ち、右端の列のほかは下（本文）へ渡す
pub struct StripeView;

impl crate::layout::View for StripeView {
    fn rect(&self, ed: &Editor, s: crate::layout::Screen) -> Option<Rect> {
        let _ = s;
        if !ed.editor_shown() {
            return None;
        }
        let (x, w) = ed.windows_x();
        let y = ed.windows_top();
        Some(Rect { x, y, w, h: ed.windows_rows().saturating_sub(y) })
    }

    fn draw(&self, ed: &Editor, p: &mut crate::view::Painter, r: Rect, s: crate::layout::Screen) {
        let _ = (r, s);
        draw::draw_stripes(p, ed);
    }

    fn click(&self, ed: &mut Editor, r: Rect, at: crate::layout::At, clicks: u32) -> crate::layout::Click {
        let _ = (r, clicks);
        let (row, col) = at.screen;
        if !ed.stripe_press(row, col) {
            return crate::layout::Click::Pass;
        }
        if ed.stripe_grab.is_some() { crate::layout::Click::DragStripe } else { crate::layout::Click::Done }
    }

    fn mono(&self) -> bool {
        true
    }
}

pub(crate) mod draw {
    use super::*;
    use crate::view::*;

    /// 窓ごとのスクロールバー（右端の列の右寄りに細く。つまみは薄い灰、印は色の短い横線）
    pub(crate) fn draw_stripes(p: &mut Painter, ed: &Editor) {
        let cell = p.fonts.cell();
        for s in ed.stripes() {
            if s.thumb().is_none() && s.marks.is_empty() {
                continue;
            }
            let (y0, h) = (s.rect.y as f32 * cell.h, s.rect.h as f32 * cell.h);
            let right = (s.rect.x + s.rect.w) as f32 * cell.w;
            let w = (cell.w * 0.8).round().max(6.0);
            let x = right - w - 1.0;
            if let Some((a, len)) = s.thumb() {
                p.out.push(Instance::rounded(x + 1.0, y0 + a * h, w - 2.0, (len * h).max(8.0), 3.0, [1.0, 1.0, 1.0, 0.13]));
            }
            // git の印は左半分、エラー・警告は全体の幅で上に重ねる
            let mut marks = s.marks.clone();
            marks.sort_by_key(|(_, m)| std::cmp::Reverse(*m as u8));
            for (line, m) in marks {
                let y = (y0 + s.frac(line) * h).min(y0 + h - 3.0);
                let (mx, mw, color) = match m {
                    Mark::Error => (x, w, DIAG_ERROR),
                    Mark::Warning => (x, w, DIAG_WARN),
                    Mark::Added => (x, w / 2.0, GIT_ADD),
                    Mark::Modified => (x, w / 2.0, GIT_MOD),
                    Mark::Deleted => (x, w / 2.0, GIT_DEL),
                };
                p.out.push(Instance::fill(mx, y, mw, 3.0, color));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    /// 押した所: 印ならその行へ、つまみならドラッグで送る、ほかはその位置を真ん中に
    #[test]
    fn press_marks_thumb_and_track() {
        let text: String = (0..400).map(|i| format!("line {i}\n")).collect();
        let dir = std::env::temp_dir().join(format!("nagi-stripe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), &text).unwrap();
        let mut buf = Buffer::open(&dir.join("a.txt")).unwrap();
        buf.path = Some(dir.join("a.txt"));
        let mut e = Editor::new(buf, None);
        e.set_screen(100, 40);
        let path = std::path::absolute(dir.join("a.txt")).unwrap();
        e.diags = vec![crate::diagnostics::Diag { path, line: 300, col: 0, error: true, msg: "boom".into() }];
        let s = e.stripes().pop().unwrap();
        assert_eq!((s.total, s.top), (400, 0));
        assert!(s.marks.contains(&(300, Mark::Error)));
        // 印の行のスクロールバーを押すと、その行へ
        let row = s.rect.y + (s.frac(300) * s.rect.h as f32) as usize;
        let screen = crate::layout::Screen { rows: 40, cols: 100 };
        crate::layout::press(&mut e, screen, row, s.col(), |_| 1);
        assert_eq!(e.core.cursor.line, 300);
        assert!(e.core.top <= 300 && 300 < e.core.top + e.rows, "見える所へ送る");
        // つまみをつかんで下へ: 送る（カーソルは見える所に留まる）
        e.core.cursor.line = 0;
        e.scroll(-1000);
        let s = e.stripes().pop().unwrap();
        let hit = crate::layout::press(&mut e, screen, s.rect.y, s.col(), |_| 1).map(|h| h.1);
        assert_eq!(hit, Some(crate::layout::Click::DragStripe));
        e.stripe_drag(s.rect.y + s.rect.h / 2);
        assert!(e.core.top > 150, "{}", e.core.top);
        // 印の無い所を押すと、その位置を真ん中に（つかまない）
        e.stripe_grab = None;
        e.scroll(-1000);
        let s = e.stripes().pop().unwrap();
        let hit = crate::layout::press(&mut e, screen, s.rect.y + s.rect.h - 2, s.col(), |_| 1).map(|h| h.1);
        assert_eq!(hit, Some(crate::layout::Click::Done));
        assert!(e.core.top > 300, "{}", e.core.top);
        // 右端の列でなければ本文へ
        assert!(crate::layout::press(&mut e, screen, s.rect.y + 1, s.col() - 3, |_| 1).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
