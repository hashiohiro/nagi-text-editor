//! 波線にマウスを載せたときの説明（IntelliJ と同じ）。
//!
//! マウスを本文の上で止めて少したつと、その位置の診断（エラー・警告）の文を小窓で出す。
//! マウスが別のセルへ動く・キーを打つ・送ると消す。

use std::time::{Duration, Instant};

use crate::editor::Editor;
use crate::panes::Rect;

/// 止めてから出すまで（IntelliJ の既定の 0.5 秒くらい）
const HOVER_DELAY: Duration = Duration::from_millis(500);
/// 小窓の 1 行の幅（これより長い文は折り返す）
const TIP_WIDTH: usize = 72;

/// 出している説明
#[derive(Clone, Debug, PartialEq)]
pub struct HoverTip {
    /// 載せたセル（画面の行・列）
    pub row: usize,
    pub col: usize,
    pub msg: String,
    pub error: bool,
}

impl Editor {
    /// マウスが動いた（画面のセル。ボタンを押していないとき）。出していた説明を消した・行番号の欄に出入りしたら true（描き直す）
    pub(crate) fn hover_move(&mut self, row: usize, col: usize, now: Instant) -> bool {
        if self.hover_cell == Some((row, col)) {
            return false;
        }
        let was = self.hover_cell.is_some_and(|(r, c)| self.over_gutter(r, c));
        self.hover_cell = Some((row, col));
        let had = self.hover_tip.take().is_some();
        self.hover_due = self.over_text(row, col).then_some(now + HOVER_DELAY);
        had || was != self.over_gutter(row, col)
    }

    /// どれかの窓の行番号の欄の上か（開いた折りたたみの印を出す）
    fn over_gutter(&self, row: usize, col: usize) -> bool {
        self.editor_shown() && self.window_rects().iter().any(|(_, r)| row >= r.y && row < r.y + r.h && col >= r.x && col < r.x + self.gutter())
    }

    /// 説明を出す時刻（App の待ちに入れる）
    pub fn hover_deadline(&self) -> Option<Instant> {
        self.hover_due
    }

    /// 消す（キー・送ったとき）
    pub(crate) fn hover_clear(&mut self) {
        self.hover_tip = None;
        self.hover_due = None;
    }

    /// 時刻になったら、載せているセルの診断を探して出す。出したら true
    pub(crate) fn fire_hover(&mut self, now: Instant) -> bool {
        if self.hover_due.is_none_or(|d| now < d) {
            return false;
        }
        self.hover_due = None;
        let Some((row, col)) = self.hover_cell.filter(|&(r, c)| self.over_text(r, c)) else { return false };
        let rect = self.active_rect();
        let pos = self.pos_at(row - rect.y, col - rect.x);
        let line = self.core.buf.line(pos.line);
        // 行の終わりより右（pos_at は行末に寄せる）では出さない
        if pos.byte >= line.trim_end_matches(['\n', '\r']).len() {
            return false;
        }
        let Some(here) = self.core.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok()) else { return false };
        let hit = self
            .diags
            .iter()
            .filter(|d| d.path == here && d.line == pos.line)
            .filter(|d| {
                let (a, b) = crate::view::squiggle_range(&line, d.col);
                (a..b.max(a + 1)).contains(&pos.byte)
            })
            .min_by_key(|d| !d.error);
        let Some(d) = hit else { return false };
        self.hover_tip = Some(HoverTip { row, col, msg: d.msg.clone(), error: d.error });
        true
    }

    /// 作業中の窓の本文（行番号の欄・右端のスクロールバーを除く）の上か
    fn over_text(&self, row: usize, col: usize) -> bool {
        if !self.editor_shown() || self.menu.is_some() {
            return false;
        }
        let r = self.active_rect();
        let h = self.text_rows(r);
        row >= r.y && row < r.y + h && col >= r.x + self.gutter() && col + 1 < r.x + r.w
    }

    /// 説明の小窓の場所と、折り返した行
    pub fn hover_box(&self, rows: usize, cols: usize) -> Option<(Rect, Vec<String>)> {
        let t = self.hover_tip.as_ref()?;
        let lines = wrap(&t.msg, TIP_WIDTH);
        let w = lines.iter().map(|l| nagi_core::display_col(l, l.len(), false)).max().unwrap_or(0) + 4;
        let h = lines.len();
        let x = t.col.min(cols.saturating_sub(w));
        // 載せた行の下に。入らなければ上に
        let y = if t.row + 1 + h <= rows { t.row + 1 } else { t.row.saturating_sub(h) };
        Some((Rect { x, y, w, h }, lines))
    }
}

/// 文を `w` 列で折り返す（改行はそのまま）
fn wrap(s: &str, w: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in s.lines() {
        let mut cur = String::new();
        let mut n = 0;
        for ch in para.chars() {
            let cw = nagi_core::display_col(&ch.to_string(), ch.len_utf8(), false);
            if n + cw > w {
                out.push(std::mem::take(&mut cur));
                n = 0;
            }
            cur.push(ch);
            n += cw;
        }
        out.push(cur);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// 説明の小窓（layout.rs の View）
pub struct HoverView;

impl crate::layout::View for HoverView {
    fn rect(&self, ed: &Editor, s: crate::layout::Screen) -> Option<Rect> {
        ed.hover_box(s.rows, s.cols).map(|b| b.0)
    }

    fn draw(&self, ed: &Editor, p: &mut crate::view::Painter, r: Rect, s: crate::layout::Screen) {
        let _ = r;
        let (Some((b, lines)), Some(t)) = (ed.hover_box(s.rows, s.cols), ed.hover_tip.as_ref()) else { return };
        use crate::view::*;
        let cell = p.fonts.cell();
        popup_frame(p, b.x as f32 * cell.w, b.y as f32 * cell.h, b.w as f32 * cell.w, b.h as f32 * cell.h);
        let saved = p.cols;
        p.cols = b.x + b.w;
        for (k, l) in lines.iter().enumerate() {
            let y = (b.y + k) as f32 * cell.h;
            if k == 0 {
                p.text("●", b.x + 1, y, if t.error { DIAG_ERROR } else { DIAG_WARN });
            }
            p.text(l, b.x + 3, y, FG);
        }
        p.cols = saved;
    }

    /// 押したら消して、下（本文）へ渡す
    fn click(&self, ed: &mut Editor, r: Rect, at: crate::layout::At, clicks: u32) -> crate::layout::Click {
        let _ = (r, at, clicks);
        ed.hover_clear();
        crate::layout::Click::Pass
    }

    fn mono(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    /// 波線の語の上で止めると説明を出し、ほかのセルへ動く・キーを打つと消す。語の外・行の終わりより右では出さない
    #[test]
    fn tip_on_squiggle() {
        let dir = std::env::temp_dir().join(format!("nagi-hover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.rs"), "let x = foo(1);\nlet y = 2;\n").unwrap();
        let mut buf = Buffer::open(&dir.join("a.rs")).unwrap();
        buf.path = Some(dir.join("a.rs"));
        let mut e = Editor::new(buf, None);
        e.set_screen(100, 30);
        let path = std::path::absolute(dir.join("a.rs")).unwrap();
        e.diags = vec![crate::diagnostics::Diag { path, line: 0, col: 8, error: true, msg: "cannot find function `foo`".into() }];
        let r = e.active_rect();
        let g = r.x + e.gutter();
        let at = |c: usize| g + c;
        let t0 = Instant::now();
        // foo の 2 文字目
        assert!(!e.hover_move(r.y, at(9), t0));
        assert!(!e.fire_hover(t0), "すぐには出さない");
        assert!(e.fire_hover(t0 + HOVER_DELAY));
        assert_eq!(e.hover_tip.as_ref().map(|t| t.msg.as_str()), Some("cannot find function `foo`"));
        let (b, lines) = e.hover_box(30, 100).unwrap();
        assert_eq!((b.y, lines.len()), (r.y + 1, 1), "載せた行の下");
        // ほかのセルへ動くと消す。語の外では出さない
        assert!(e.hover_move(r.y, at(2), t0));
        assert!(!e.fire_hover(t0 + HOVER_DELAY));
        // 行の終わりより右でも出さない
        e.hover_move(r.y, at(40), t0);
        assert!(!e.fire_hover(t0 + HOVER_DELAY));
        // キーで消す
        e.hover_move(r.y, at(8), t0);
        assert!(e.fire_hover(t0 + HOVER_DELAY));
        e.key(nagi_vim::Key::Right);
        assert!(e.hover_tip.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
