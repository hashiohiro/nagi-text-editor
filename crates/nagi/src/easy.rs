//! EasyMotion（IdeaVim の easymotion）: 画面に見えている跳び先に印（a・s・d…）を出し、打った印へ跳ぶ。
//!
//! 単語の頭（カーソルより後）、下・上の行の頭、打った 1 文字、打った文字列（Enter で決める）の 5 つ。
//! 印は 26 個までは 1 文字、それより多ければ 2 文字。Esc でやめる。

use nagi_vim::Key;

use crate::editor::Editor;

/// 印に使う文字（ホームポジションから）
const LETTERS: &str = "asdfghjklqwertyuiopzxcvbnm";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EasyKind {
    WordForward,
    LineForward,
    LineBackward,
    /// 1 文字を打ってから
    FindChar,
    /// 文字列を打って Enter で
    Search,
}

pub struct Easy {
    pub kind: EasyKind,
    /// 跳び先（行, byte）と印
    pub targets: Vec<(usize, usize, String)>,
    /// 打った印の頭（2 文字の印の 1 文字目）
    pub typed: String,
    /// FindChar・Search で打っている文字（印を出す前）
    pub query: String,
    /// 印を出している
    pub picking: bool,
}

/// `n` 個の印（26 個までは 1 文字、多ければ 2 文字）
fn labels(n: usize) -> Vec<String> {
    let l: Vec<char> = LETTERS.chars().collect();
    if n <= l.len() {
        return l.iter().take(n).map(|c| c.to_string()).collect();
    }
    l.iter().flat_map(|a| l.iter().map(move |b| format!("{a}{b}"))).take(n).collect()
}

impl Editor {
    pub fn easy_active(&self) -> bool {
        self.easy.is_some()
    }

    /// 始める（FindChar・Search は文字を待つ）
    pub(crate) fn start_easy(&mut self, kind: EasyKind) {
        let mut e = Easy { kind, targets: Vec::new(), typed: String::new(), query: String::new(), picking: false };
        match kind {
            EasyKind::FindChar => self.notify.info("EasyMotion: 跳ぶ先の文字を打つ（Esc でやめる）"),
            EasyKind::Search => self.notify.info("EasyMotion: 検索する文字列を打って Enter"),
            _ => {
                e.targets = self.easy_targets(kind, "");
                e.picking = true;
            }
        }
        self.easy = Some(e);
        self.easy_label_or_finish();
    }

    /// 見えている行の範囲
    fn easy_rows(&self) -> std::ops::Range<usize> {
        let end = (self.top + self.rows).min(self.buf.line_count());
        self.top.min(end)..end
    }

    fn easy_targets(&self, kind: EasyKind, q: &str) -> Vec<(usize, usize, String)> {
        let (cl, cb) = (self.cursor.line, self.cursor.byte);
        let mut at: Vec<(usize, usize)> = Vec::new();
        for l in self.easy_rows() {
            let line = self.buf.line(l);
            match kind {
                EasyKind::WordForward => {
                    let mut prev_word = false;
                    for (i, c) in line.char_indices() {
                        let w = c.is_alphanumeric() || c == '_';
                        if w && !prev_word && (l > cl || l == cl && i > cb) {
                            at.push((l, i));
                        }
                        prev_word = w;
                    }
                }
                EasyKind::LineForward | EasyKind::LineBackward => {
                    let ok = if kind == EasyKind::LineForward { l > cl } else { l < cl };
                    if ok {
                        at.push((l, line.len() - line.trim_start().len()));
                    }
                }
                EasyKind::FindChar | EasyKind::Search => {
                    if q.is_empty() {
                        continue;
                    }
                    let (hay, needle) = if q.chars().any(char::is_uppercase) { (line.clone(), q.to_string()) } else { (line.to_lowercase(), q.to_lowercase()) };
                    // 小文字にして長さが変わる行（ß など）は、そのままで探す
                    let (hay, needle) = if hay.len() == line.len() { (hay, needle) } else { (line.clone(), q.to_string()) };
                    let mut from = 0;
                    while let Some(i) = hay[from..].find(&needle) {
                        if (l, from + i) != (cl, cb) {
                            at.push((l, from + i));
                        }
                        from += i + needle.len().max(1);
                    }
                }
            }
        }
        // 上へ跳ぶものは近い順に印を付ける
        if kind == EasyKind::LineBackward {
            at.reverse();
        }
        let names = labels(at.len());
        at.into_iter().zip(names).map(|((l, b), s)| (l, b, s)).collect()
    }

    /// 跳び先が無ければやめる、1 つだけならそこへ跳ぶ
    fn easy_label_or_finish(&mut self) {
        let Some(e) = &self.easy else { return };
        if !e.picking {
            return;
        }
        match e.targets.len() {
            0 => {
                self.easy = None;
                self.notify.info("EasyMotion: 跳び先がありません");
            }
            1 => {
                let (l, b, _) = e.targets[0].clone();
                self.easy_jump(l, b);
            }
            n => self.notify.info(format!("EasyMotion: 印を打って跳ぶ（{n} か所。Esc でやめる）")),
        }
    }

    fn easy_jump(&mut self, line: usize, byte: usize) {
        self.easy = None;
        self.notify.clear();
        self.push_jump();
        self.cursor.line = line;
        self.cursor.byte = byte;
        let l = self.buf.line(line);
        self.cursor.want_col = nagi_core::display_col(&l, byte.min(l.len()), false);
    }

    /// 印を出している間・文字を待っている間のキー
    pub(crate) fn easy_key(&mut self, key: Key) {
        let Some(e) = &mut self.easy else { return };
        if matches!(key, Key::Esc | Key::Ctrl('c')) {
            self.easy = None;
            self.notify.clear();
            return;
        }
        if !e.picking {
            match (e.kind, key) {
                (EasyKind::FindChar, Key::Char(c)) => e.query.push(c),
                (EasyKind::Search, Key::Char(c)) => {
                    e.query.push(c);
                    let q = e.query.clone();
                    self.notify.info(format!("EasyMotion: {q}"));
                    return;
                }
                (EasyKind::Search, Key::Backspace) => {
                    e.query.pop();
                    return;
                }
                (EasyKind::Search, Key::Enter) => {}
                _ => return,
            }
            let (kind, q) = (e.kind, e.query.clone());
            let targets = self.easy_targets(kind, &q);
            if let Some(e) = &mut self.easy {
                e.targets = targets;
                e.picking = true;
            }
            self.easy_label_or_finish();
            return;
        }
        let Key::Char(c) = key else { return };
        e.typed.push(c);
        let typed = e.typed.clone();
        if let Some((l, b, _)) = e.targets.iter().find(|t| t.2 == typed).cloned() {
            self.easy_jump(l, b);
        } else if !e.targets.iter().any(|t| t.2.starts_with(&typed)) {
            // どの印でもない: やり直し
            e.typed.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    #[test]
    fn jumps() {
        let mut e = Editor::new(Buffer::from_str("alpha beta gamma\n  delta\nepsilon beta\n"), None);
        e.set_screen(80, 20);
        e.start_easy(EasyKind::WordForward);
        let t: Vec<(usize, usize, String)> = e.easy.as_ref().unwrap().targets.clone();
        assert_eq!(t.iter().map(|x| (x.0, x.1)).collect::<Vec<_>>(), [(0, 6), (0, 11), (1, 2), (2, 0), (2, 8)]);
        e.key(Key::Char('f'));
        assert!(e.easy.is_none());
        assert_eq!((e.cursor.line, e.cursor.byte), (2, 0), "4 つ目の印 f");
        e.start_easy(EasyKind::FindChar);
        e.key(Key::Char('b'));
        e.key(Key::Char('s'));
        assert_eq!((e.cursor.line, e.cursor.byte), (2, 8), "b の 2 つ目");
        e.start_easy(EasyKind::LineBackward);
        e.key(Key::Char('a'));
        assert_eq!(e.cursor.line, 1, "上の近い行から a");
        assert_eq!((labels(26)[25].as_str(), labels(30)[27].as_str()), ("m", "ss"), "27 か所からは 2 文字");
    }
}
