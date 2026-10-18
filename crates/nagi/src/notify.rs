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
    /// (時刻, 重さ, 文)。古いものから
    history: VecDeque<(String, Level, String)>,
}

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
        self.level = Some(level);
        self.text = text;
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
    }

    /// 履歴に残さずに出す（途中の進み具合など、すぐ書き換わるもの）
    pub fn progress(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.level = Some(Level::Info);
    }

    pub fn history(&self) -> impl Iterator<Item = &(String, Level, String)> {
        self.history.iter()
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
}
