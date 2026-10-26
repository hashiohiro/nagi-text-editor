//! 利用者への知らせ（ADR 0016 の 2）。
//!
//! 状態の行に出す文は `self.notify.info(…)`・`warn`・`error` で出す。出したものは履歴に残り（`:messages`）、
//! warn と error はログ（nagi.log）にも書く。Notify は Editor の 1 つのフィールドなので、パネルを借りている途中でも呼べる。

use std::collections::VecDeque;

use nagi_core::log::{self, Level};

use crate::editor::Editor;

/// 履歴に残す数
const KEEP: usize = 200;

#[derive(Default)]
pub struct Notify {
    /// 今、状態の行に出している文
    pub text: String,
    /// その重さ（描くときの色）
    pub level: Option<Level>,
    /// 今の文は入力欄のモードの表示か（`-- VISUAL --` など。欄を離れたら消す）
    mode: bool,
    /// (時刻, 重さ, 文)。古いものから
    history: VecDeque<(String, Level, String)>,
    /// 今の文は吹き出しに出したもの（状態の行には出さない）
    balloon: bool,
    /// 右下に出している吹き出し（IntelliJ の通知と同じ。古いものから）
    pub balloons: Vec<Balloon>,
}

/// 右下の吹き出し 1 つ
pub struct Balloon {
    pub level: Level,
    pub text: String,
    /// 消える時刻
    pub until: std::time::Instant,
}

/// 同時に出す吹き出しの数
const BALLOONS: usize = 4;

impl Notify {
    /// 重さを選んで知らせる（結果に応じて重さが変わるとき）
    pub fn put(&mut self, level: Level, text: String) {
        if !text.is_empty() {
            if self.history.len() >= KEEP {
                self.history.pop_front();
            }
            let time = log::now().get(11..19).unwrap_or("").to_string();
            self.history.push_back((time, level, text.clone()));
            if level >= Level::Warn {
                log::write(level, &text);
            }
        }
        // 吹き出しに出す（同じ文は出し直すだけ）。エラーほど長く残す
        if !text.is_empty() {
            let secs = match level {
                Level::Error => 15,
                Level::Warn => 8,
                _ => 4,
            };
            self.balloons.retain(|b| b.text != text);
            self.balloons.push(Balloon { level, text: text.clone(), until: std::time::Instant::now() + std::time::Duration::from_secs(secs) });
            if self.balloons.len() > BALLOONS {
                self.balloons.remove(0);
            }
        }
        self.level = Some(level);
        self.text = text;
        self.mode = false;
        self.balloon = true;
    }

    /// 状態の行に出す文（吹き出しに出したものは出さない。進み具合・入力欄のモードだけ）
    pub fn status_text(&self) -> &str {
        if self.balloon { "" } else { &self.text }
    }

    /// 次に吹き出しが消える時刻
    pub fn balloon_deadline(&self) -> Option<std::time::Instant> {
        self.balloons.iter().map(|b| b.until).min()
    }

    /// 時刻を過ぎた吹き出しを消す。消したら true
    pub fn expire(&mut self, now: std::time::Instant) -> bool {
        let n = self.balloons.len();
        self.balloons.retain(|b| b.until > now);
        n != self.balloons.len()
    }

    /// 知らせる（ふつう）
    pub fn info(&mut self, text: impl Into<String>) {
        self.put(Level::Info, text.into());
    }

    /// 知らせる（気をつけてほしいこと。ログにも書く）
    pub fn warn(&mut self, text: impl Into<String>) {
        self.put(Level::Warn, text.into());
    }

    /// 知らせる（できなかったこと。ログにも書く）
    pub fn error(&mut self, text: impl Into<String>) {
        self.put(Level::Error, text.into());
    }

    /// 状態の行を消す（履歴は残す）
    pub fn clear(&mut self) {
        self.text.clear();
        self.level = None;
        self.mode = false;
        self.balloon = false;
    }

    /// 履歴に残さずに出す（途中の進み具合など、すぐ書き換わるもの）
    pub fn progress(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.level = Some(Level::Info);
        self.mode = false;
        self.balloon = false;
    }

    /// 入力欄のモードを出す（履歴に残さない。押して別の所へ移ったら clear_mode で消す）
    pub fn mode(&mut self, text: impl Into<String>) {
        self.progress(text);
        self.mode = true;
    }

    /// 出しているのが入力欄のモードなら消す
    pub fn clear_mode(&mut self) {
        if self.mode {
            self.clear();
        }
    }

    pub fn history(&self) -> impl Iterator<Item = &(String, Level, String)> {
        self.history.iter()
    }
}

/// 吹き出しの位置（吹き出しの番号, 矩形, 折り返した行）。右下から上へ積む（新しいものが下）
pub fn balloon_layout(ed: &Editor, rows: usize) -> Vec<(usize, crate::panes::Rect, Vec<String>)> {
    let a = ed.area();
    let w = (a.w / 3).clamp(30, 60).min(a.w);
    let inner = w.saturating_sub(5);
    let mut out = Vec::new();
    let mut bottom = rows;
    for (i, b) in ed.notify.balloons.iter().enumerate().rev() {
        let lines = wrap(&b.text, inner, 4);
        let h = lines.len() + 2;
        if bottom < a.y + h + 1 {
            break;
        }
        let r = crate::panes::Rect { x: a.x + a.w - w - 1, y: bottom - h, w, h };
        bottom = r.y.saturating_sub(1);
        out.push((i, r, lines));
    }
    out
}

/// 表示の幅 `w` で折り返す（`max` 行まで。あふれたら最後に …）
fn wrap(text: &str, w: usize, max: usize) -> Vec<String> {
    let mut lines = vec![String::new()];
    let mut cur = 0;
    for ch in text.chars() {
        let cw = nagi_core::display_col(ch.encode_utf8(&mut [0; 4]), ch.len_utf8(), false);
        if cur + cw > w.max(1) {
            if lines.len() == max {
                let last = lines.last_mut().unwrap();
                last.pop();
                last.push('…');
                return lines;
            }
            lines.push(String::new());
            cur = 0;
        }
        lines.last_mut().unwrap().push(ch);
        cur += cw;
    }
    lines
}

/// 右下の吹き出し（layout.rs の View）
pub struct BalloonView;

impl crate::layout::View for BalloonView {
    fn rect(&self, ed: &Editor, s: crate::layout::Screen) -> Option<crate::panes::Rect> {
        let v = balloon_layout(ed, s.rows);
        let top = v.iter().map(|b| b.1.y).min()?;
        let (x, w) = (v[0].1.x, v[0].1.w);
        Some(crate::panes::Rect { x, y: top, w, h: s.rows - top })
    }

    fn draw(&self, ed: &Editor, p: &mut crate::view::Painter, r: crate::panes::Rect, s: crate::layout::Screen) {
        let _ = r;
        draw::draw_balloons(p, ed, s.rows);
    }

    /// 押した吹き出しを閉じる（吹き出しの間の行は下へ通す）
    fn click(&self, ed: &mut Editor, r: crate::panes::Rect, row: usize, col: usize, clicks: u32) -> crate::layout::Click {
        let _ = (r, clicks);
        match balloon_layout(ed, ed.screen.1).into_iter().find(|b| b.1.contains(row, col)) {
            Some((i, ..)) => {
                ed.notify.balloons.remove(i);
                crate::layout::Click::Done
            }
            None => crate::layout::Click::Pass,
        }
    }
}

pub(crate) mod draw {
    use super::*;
    use crate::view::*;

    pub(crate) fn draw_balloons(p: &mut Painter, ed: &Editor, rows: usize) {
        let cell = p.fonts.cell();
        let saved = p.cols;
        for (i, r, lines) in balloon_layout(ed, rows) {
            let b = &ed.notify.balloons[i];
            let (x0, y0, w, h) = (r.x as f32 * cell.w, r.y as f32 * cell.h, r.w as f32 * cell.w, r.h as f32 * cell.h);
            p.out.push(Instance::fill(x0 - 1.0, y0 - 1.0, w + 2.0, h + 2.0, SEPARATOR));
            p.out.push(Instance::fill(x0, y0, w, h, POPUP_BG));
            let icon = match b.level {
                Level::Error => "g_errorDialog",
                Level::Warn => "g_warningDialog",
                _ => "g_informationDialog",
            };
            let px = crate::icons::small(cell.h);
            crate::icons::draw(p, icon, x0 + cell.w, crate::icons::centered(y0 + cell.h, cell.h, px), px, None);
            p.cols = r.x + r.w - 1;
            for (k, l) in lines.iter().enumerate() {
                p.text(l, r.x + 3, (r.y + 1 + k) as f32 * cell.h, FG);
            }
        }
        p.cols = saved;
    }
}

impl Editor {
    /// `:messages`: 知らせの履歴を結果の欄に出す（新しいものが下）
    pub(crate) fn show_messages(&mut self) {
        let lines: Vec<String> = self
            .notify
            .history()
            .map(|(t, l, m)| {
                let mark = match l {
                    Level::Info => "  ",
                    Level::Warn => "W ",
                    Level::Error => "E ",
                };
                format!("{t} {mark}{m}")
            })
            .collect();
        let n = lines.len();
        self.open_result_panel("知らせ（:messages）");
        if let Some(g) = &mut self.grep {
            for text in lines {
                g.hits.push(nagi_workers::grep::Hit { path: String::new(), line: 0, range: (0, 0), text });
            }
            g.running = false;
            g.selected = n.saturating_sub(1);
        }
        self.sync_output_view();
        let log = log::default_path().map_or(String::new(), |p| p.display().to_string());
        self.notify.progress(format!("知らせ {n} 件（ログ: {log}）"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    #[test]
    fn levels_and_history() {
        let mut e = Editor::new(Buffer::from_str(""), None);
        e.set_screen(120, 40);
        e.notify.info("ひとつめ");
        e.notify.error("ふたつめはできません");
        assert_eq!(e.notify.text, "ふたつめはできません");
        assert_eq!(e.notify.level, Some(Level::Error));
        e.notify.progress("進み具合");
        e.notify.clear();
        assert!(e.notify.text.is_empty());
        e.show_messages();
        let g = e.grep.as_ref().unwrap();
        assert_eq!(g.hits.len(), 2, "進み具合は履歴に残さない");
        assert!(g.hits[1].text.ends_with("E ふたつめはできません"), "{}", g.hits[1].text);
    }

    /// 失敗（Err の腕・エラーの値）を info で出していない（重さの誤りを見つける。ADR 0016）
    #[test]
    fn errors_are_not_info() {
        let re = regex::Regex::new(r"Err\([a-z_]*\) => [^\n]*notify\.info\(|notify\.info\((e|err)\)").unwrap();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut bad = Vec::new();
        for f in std::fs::read_dir(&dir).unwrap().flatten() {
            let text = std::fs::read_to_string(f.path()).unwrap_or_default();
            for (i, l) in text.lines().enumerate() {
                if re.is_match(l) {
                    bad.push(format!("{}:{}", f.file_name().to_string_lossy(), i + 1));
                }
            }
        }
        assert!(bad.is_empty(), "失敗を info で出している: {bad:?}");
    }
}
