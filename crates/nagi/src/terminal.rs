//! 下の欄の「ターミナル」（IntelliJ の Terminal、Alt+F12）。ConPTY でシェル（PowerShell）を動かす。
//!
//! 欄に移ると、キーをそのままシェルへ送る（Vim の割り当ては通さない）。本文へ戻るのは `Ctrl-\ n`（Vim の
//! ターミナルと同じ。シェルが Esc を使うため）、本文のクリック、Alt+F12（欄を隠す）。日本語は IME で打てる。
//! 作業フォルダはプロジェクトの根。シェルが終わったら Enter で起動し直す。マウスのホイールで遡って見られる。

use nagi_vim::Key;
use nagi_workers::term::Term;

use crate::editor::{Editor, Mode};
use crate::result_grid::BottomKind;

pub struct TermView {
    pub term: Term,
    /// Ctrl-\ を押した（次の n で本文へ）
    pending_escape: bool,
    pub exited: bool,
}

/// キーを端末に送る並びに直す（`app_cursor` は矢印を ESC O の形にする）
pub fn key_bytes(key: Key, app_cursor: bool) -> Option<Vec<u8>> {
    let s = |x: &str| Some(x.as_bytes().to_vec());
    let arrow = |c: char| Some(if app_cursor { format!("\x1bO{c}") } else { format!("\x1b[{c}") }.into_bytes());
    match key {
        Key::Char(c) => Some(c.to_string().into_bytes()),
        Key::Enter | Key::Ctrl('m') => s("\r"),
        Key::Tab => s("\t"),
        Key::Backspace => s("\x7f"),
        Key::Esc => s("\x1b"),
        Key::Delete => s("\x1b[3~"),
        Key::Up => arrow('A'),
        Key::Down => arrow('B'),
        Key::Right => arrow('C'),
        Key::Left => arrow('D'),
        Key::Home => s("\x1b[H"),
        Key::End => s("\x1b[F"),
        Key::PageUp => s("\x1b[5~"),
        Key::PageDown => s("\x1b[6~"),
        Key::Ctrl(c) => {
            let b = match c {
                'a'..='z' => c as u8 - b'a' + 1,
                '[' => 0x1b,
                '\\' => 0x1c,
                ']' => 0x1d,
                '^' => 0x1e,
                '_' => 0x1f,
                ' ' | '@' => 0,
                _ => return None,
            };
            Some(vec![b])
        }
        Key::Alt(c) => Some(format!("\x1b{c}").into_bytes()),
        Key::F(n @ 1..=4) => Some(format!("\x1bO{}", (b'P' + n - 1) as char).into_bytes()),
        Key::F(n @ 5..=12) => {
            let code = [15, 17, 18, 19, 20, 21, 23, 24][(n - 5) as usize];
            Some(format!("\x1b[{code}~").into_bytes())
        }
        _ => None,
    }
}

impl Editor {
    /// ターミナルのタブを出して移る（まだならシェルを起動する）
    pub(crate) fn open_terminal(&mut self) {
        self.bottom_hidden = false;
        self.bottom_kind = BottomKind::Terminal;
        self.mode = Mode::Terminal;
        self.pending_keys.clear();
        self.rows = self.text_rows(self.active_rect());
        if self.terminal.as_ref().is_none_or(|t| t.exited) {
            self.start_terminal();
        }
    }

    fn start_terminal(&mut self) {
        let r = self.bottom_content_rect();
        let (rows, cols) = r.map_or((10, 80), |r| (r.h as u16, r.w.saturating_sub(1) as u16));
        let cwd = self.nav_root();
        let waker = self.waker.clone().unwrap_or_else(|| std::sync::Arc::new(|| {}));
        let shell = nagi_workers::term::default_shell();
        match Term::spawn(&shell, &cwd, rows, cols, waker) {
            Ok(term) => self.terminal = Some(TermView { term, pending_escape: false, exited: false }),
            Err(e) => {
                self.message = format!("シェルを起動できません（{shell}）: {e}");
                self.mode = Mode::Normal;
            }
        }
    }

    /// 欄の大きさに合わせ、シェルが終わったかを見る（描く前に呼ぶ）
    pub(crate) fn sync_terminal(&mut self) {
        let r = (self.bottom_kind == BottomKind::Terminal).then(|| self.bottom_content_rect()).flatten();
        let Some(t) = &mut self.terminal else { return };
        if let Some(r) = r {
            t.term.resize(r.h as u16, r.w.saturating_sub(1) as u16);
        }
        if !t.exited && t.term.exited() {
            t.exited = true;
        }
    }

    pub(crate) fn terminal_key(&mut self, key: Key) {
        let Some(t) = &mut self.terminal else {
            if key == Key::Enter {
                self.start_terminal();
            } else {
                self.mode = Mode::Normal;
            }
            return;
        };
        // Ctrl-\ n（と Ctrl-\ Ctrl-n）で本文へ。Ctrl-\ を 2 回で Ctrl-\ そのものを送る
        if std::mem::take(&mut t.pending_escape) {
            match key {
                Key::Char('n') | Key::Ctrl('n') | Key::Char('k') | Key::Ctrl('k') => {
                    self.mode = Mode::Normal;
                    return;
                }
                Key::Ctrl('\\') => {
                    t.term.write(&[0x1c]);
                    return;
                }
                _ => {}
            }
        } else if key == Key::Ctrl('\\') {
            t.pending_escape = true;
            return;
        }
        if t.exited {
            if key == Key::Enter {
                self.terminal = None;
                self.start_terminal();
            }
            return;
        }
        // 打ったら一番下へ
        if t.term.scrollback() > 0 {
            t.term.set_scrollback(0);
        }
        if key == Key::Paste {
            let text = match &self.clipboard {
                Some(cb) => (cb.get)(),
                None => crate::clipboard::get(),
            };
            if let Some(text) = text {
                self.terminal_input(&text);
            }
            return;
        }
        let app = t.term.with_screen(|s| s.application_cursor());
        if let Some(b) = key_bytes(key, app) {
            t.term.write(&b);
        }
    }

    /// IME の確定・貼り付け（貼り付けは括弧付きの形に対応）
    pub(crate) fn terminal_input(&mut self, text: &str) {
        let Some(t) = &mut self.terminal else { return };
        if t.exited {
            return;
        }
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        if text.chars().count() > 1 && t.term.with_screen(|s| s.bracketed_paste()) {
            t.term.write(format!("\x1b[200~{text}\x1b[201~").as_bytes());
        } else {
            t.term.write(text.as_bytes());
        }
    }

    pub(crate) fn terminal_click(&mut self) {
        self.mode = Mode::Terminal;
        if self.terminal.is_none() {
            self.start_terminal();
        }
    }

    /// ホイール: さかのぼる（正で上へ）
    pub(crate) fn terminal_scroll(&mut self, lines: isize) {
        if let Some(t) = &self.terminal {
            let now = t.term.scrollback() as isize;
            t.term.set_scrollback((now + lines).max(0) as usize);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    #[test]
    fn keys_to_bytes() {
        assert_eq!(key_bytes(Key::Char('あ'), false).unwrap(), "あ".as_bytes());
        assert_eq!(key_bytes(Key::Ctrl('c'), false).unwrap(), vec![3]);
        assert_eq!(key_bytes(Key::Up, false).unwrap(), b"\x1b[A");
        assert_eq!(key_bytes(Key::Up, true).unwrap(), b"\x1bOA");
        assert_eq!(key_bytes(Key::Enter, false).unwrap(), b"\r");
        assert_eq!(key_bytes(Key::F(5), false).unwrap(), b"\x1b[15~");
        assert_eq!(key_bytes(Key::Alt('b'), false).unwrap(), b"\x1bb");
    }

    /// シェルを起動して打ち、Ctrl-\ n で本文へ戻る
    #[test]
    fn terminal_tab_runs_shell() {
        unsafe { std::env::set_var("NAGI_SHELL", "cmd.exe /Q /K prompt $G") };
        let mut e = Editor::new(Buffer::from_str("x\n"), None);
        e.set_screen(120, 40);
        e.toggle_bottom_tab(BottomKind::Terminal);
        assert_eq!(e.mode, Mode::Terminal);
        assert!(e.terminal.is_some(), "{}", e.message);
        for k in nagi_vim::parse_keys("echo nagi-tab-ok<CR>", " ") {
            e.key(k);
        }
        let start = std::time::Instant::now();
        loop {
            e.sync_terminal();
            let text = e.terminal.as_ref().unwrap().term.with_screen(|s| s.contents());
            if text.matches("nagi-tab-ok").count() >= 2 {
                break;
            }
            assert!(start.elapsed().as_secs() < 15, "{text:?}");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // 欄の大きさに合わせている
        let r = e.bottom_content_rect().unwrap();
        assert_eq!(e.terminal.as_ref().unwrap().term.with_screen(|s| s.size()), (r.h as u16, (r.w - 1) as u16));
        e.key(Key::Ctrl('\\'));
        e.key(Key::Char('n'));
        assert_eq!(e.mode, Mode::Normal);
        // 本文のキーは本文へ
        e.key(Key::Char('x'));
        assert_eq!(e.buf.line(0), "");
    }
}
