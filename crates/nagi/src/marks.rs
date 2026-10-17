//! マーク（`m{a-zA-Z}` と `'` `` ` ``）とジャンプリスト（`Ctrl-o` / `Ctrl-i`）。
//!
//! - 小文字のマークはバッファごと、大文字はファイルをまたぐ（Vim と同じ）。
//! - 特別なマーク: `''`（直前に跳んだ元）、`'.`（最後の変更）、`'<` `'>`（最後の Visual の両端）。
//! - 行を足したり消したりしたら、それより後ろのマークとジャンプの行をずらす（消した行の中のものはその行の頭へ）。
//! - ジャンプ（`G` `gg` `%` `{` `}` `n` `N` `*` `#`、検索の確定、`:{行}`、マーク、別のファイルへの切り替え）の
//!   元の位置を記録し、`Ctrl-o` で戻り `Ctrl-i` で進む。同じバッファの同じ行の古い記録は消す。

use std::collections::HashMap;
use std::path::PathBuf;

use nagi_core::Cursor;

use crate::editor::Editor;

/// ジャンプリストの長さの上限（Vim と同じ 100）
const MAX_JUMPS: usize = 100;

/// バッファごとのマーク（作業中のバッファのものは Editor の欄、脇のバッファのものは Doc にある）
#[derive(Default, Clone)]
pub struct Marks {
    pub named: HashMap<char, Cursor>,
    /// `''` の行き先（最後に跳んだ元）
    pub pc: Option<Cursor>,
}

/// 編集で行がずれたときのマークの位置。`sl..=el` の行を `nl` 本の改行を含む文字列に置き換えた
pub(crate) fn shift_line(line: usize, sl: usize, el: usize, nl: usize) -> usize {
    if line > el {
        line + nl - (el - sl)
    } else if line > sl + nl {
        sl + nl
    } else {
        line
    }
}

impl Editor {
    /// 編集の前に呼ぶ: [start, end) を `text` に置き換えるとき、後ろのマークとジャンプの行をずらす
    pub(crate) fn shift_marks(&mut self, start: usize, end: usize, text: &str) {
        let (sl, sb) = self.buf.pos_of(start);
        let (el, eb) = self.buf.pos_of(end);
        let nl = text.matches('\n').count();
        if el == sl && nl == 0 {
            return;
        }
        self.shift_anchors(sl, el, nl, sb == 0 && eb == 0);
        let f = |c: &mut Cursor| c.line = shift_line(c.line, sl, el, nl);
        self.marks.named.values_mut().for_each(f);
        if let Some(c) = &mut self.marks.pc {
            f(c);
        }
        let doc = self.cur_doc;
        for (d, c) in self.jumps.iter_mut() {
            if *d == doc {
                f(c);
            }
        }
        let path = self.buf.path.clone();
        for (p, c) in self.global_marks.values_mut() {
            if path.as_ref().is_some_and(|me| same_path(me, p)) {
                f(c);
            }
        }
    }

    /// `m{c}`
    pub(crate) fn set_mark(&mut self, c: char) {
        if c.is_ascii_uppercase() {
            match self.buf.path.clone() {
                Some(p) => {
                    self.global_marks.insert(c, (p, self.cursor));
                }
                None => self.message = "E20: 名前の無いバッファには大文字のマークを付けられません".into(),
            }
        } else {
            self.marks.named.insert(c, self.cursor);
        }
    }

    /// マーク `c` の位置（作業中のバッファの中）。大文字で別のファイルなら、そのファイルを開いてから返す
    pub(crate) fn mark_pos(&mut self, c: char) -> Option<Cursor> {
        let pos = match c {
            'a'..='z' => self.marks.named.get(&c).copied(),
            'A'..='Z' => {
                let (p, cur) = self.global_marks.get(&c).cloned()?;
                if !self.buf.path.as_ref().is_some_and(|me| same_path(me, &p)) {
                    self.push_jump();
                    self.jump_lock = true;
                    self.edit_file(p);
                    self.jump_lock = false;
                }
                Some(cur)
            }
            '\'' | '`' => self.marks.pc,
            '.' => self.last_change.map(|a| {
                let (l, b) = self.buf.pos_of(a.min(self.buf.len_bytes()));
                Cursor { line: l, byte: b, want_col: 0 }
            }),
            '<' | '>' => self.marks.named.get(&c).copied(),
            _ => None,
        };
        if pos.is_none() {
            self.message = format!("E20: マークは設定されていません: {c}");
        }
        let mut pos = pos?;
        let last = self.buf.line_count().saturating_sub(1);
        pos.line = pos.line.min(last);
        pos.byte = pos.byte.min(self.buf.line_len(pos.line));
        Some(pos)
    }

    /// 今の位置をジャンプの元として記録する（`''` の行き先にもなる）
    pub(crate) fn push_jump(&mut self) {
        self.push_jump_from(self.cursor);
    }

    pub(crate) fn push_jump_from(&mut self, from: Cursor) {
        if self.jump_lock {
            return;
        }
        self.marks.pc = Some(from);
        let doc = self.cur_doc;
        // 同じバッファの同じ行の古い記録は消す（Vim と同じ）
        self.jumps.retain(|(d, c)| !(*d == doc && c.line == from.line));
        self.jumps.push((doc, from));
        if self.jumps.len() > MAX_JUMPS {
            self.jumps.remove(0);
        }
        self.jump_idx = self.jumps.len();
    }

    /// `Ctrl-o` / `Ctrl-i`
    pub(crate) fn jump(&mut self, older: bool, count: usize) {
        if older && self.jump_idx == self.jumps.len() {
            // 一番新しいところから戻るときは、今の位置も記録しておく（Ctrl-i で戻れるように）
            let doc = self.cur_doc;
            let here = self.cursor;
            self.jumps.retain(|(d, c)| !(*d == doc && c.line == here.line));
            self.jumps.push((doc, here));
            self.jump_idx = self.jumps.len() - 1;
        }
        let target = if older { self.jump_idx.checked_sub(count) } else { Some(self.jump_idx + count).filter(|&t| t < self.jumps.len()) };
        let Some(t) = target else { return };
        let (doc, c) = self.jumps[t];
        self.jump_idx = t;
        if doc != self.cur_doc {
            if !matches!(self.docs.get(doc), Some(crate::buffers::Slot::Parked(_))) {
                // 消したバッファへのジャンプは飛ばす
                self.jumps.remove(t);
                self.jump_idx = self.jump_idx.min(self.jumps.len());
                return;
            }
            self.jump_lock = true;
            self.switch_doc(doc);
            self.jump_lock = false;
        }
        let last = self.buf.line_count().saturating_sub(1);
        self.cursor.line = c.line.min(last);
        self.cursor.byte = c.byte.min(self.buf.line_len(self.cursor.line));
        self.clamp_normal();
    }

    /// `:marks`（最下行に 1 行で）
    pub(crate) fn list_marks(&mut self) {
        let mut items: Vec<String> = self.marks.named.iter().filter(|(c, _)| c.is_ascii_lowercase()).map(|(c, p)| format!("{c}:{}", p.line + 1)).collect();
        items.sort();
        let mut g: Vec<String> = self
            .global_marks
            .iter()
            .map(|(c, (p, cur))| format!("{c}:{}:{}", p.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned()), cur.line + 1))
            .collect();
        g.sort();
        items.extend(g);
        self.message = if items.is_empty() { "マークはありません".into() } else { items.join("  ") };
    }

    /// `:delm {文字}` / `:delm!`
    pub(crate) fn delete_marks(&mut self, arg: &str, all: bool) {
        if all {
            self.marks.named.retain(|c, _| !c.is_ascii_lowercase());
            return;
        }
        for c in arg.chars().filter(|c| !c.is_whitespace()) {
            self.marks.named.remove(&c);
            self.global_marks.remove(&c);
        }
    }
}

fn same_path(a: &std::path::Path, b: &std::path::Path) -> bool {
    let n = |p: &std::path::Path| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().to_lowercase();
    n(a) == n(b)
}

/// 大文字のマーク（ファイルと位置）
pub type GlobalMarks = HashMap<char, (PathBuf, Cursor)>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_lines() {
        // 3 行目（0 始まり 2）の後ろに 2 行足す
        assert_eq!(shift_line(5, 2, 2, 2), 7);
        assert_eq!(shift_line(2, 2, 2, 2), 2);
        assert_eq!(shift_line(1, 2, 2, 2), 1);
        // 2〜4 行目を消して 1 行に
        assert_eq!(shift_line(6, 2, 5, 0), 3);
        assert_eq!(shift_line(4, 2, 5, 0), 2);
    }

    fn keys(e: &mut Editor, k: &str) {
        for key in nagi_vim::parse_keys(k, " ") {
            e.key(key);
        }
    }

    /// 別のファイルへ移っても Ctrl-o で戻れる。大文字のマークはファイルをまたぐ
    #[test]
    fn jumps_and_global_marks_across_files() {
        let dir = std::env::temp_dir().join(format!("nagi-marks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.txt"), dir.join("b.txt"));
        std::fs::write(&a, "a1\na2\na3\n").unwrap();
        std::fs::write(&b, "b1\nb2\n").unwrap();
        let mut e = Editor::new(nagi_core::Buffer::open(&a).unwrap(), None);
        keys(&mut e, "jmAG");
        keys(&mut e, &format!(":e {}<CR>j", b.display()));
        assert_eq!(e.buf.line(0), "b1");
        keys(&mut e, "<C-o>");
        assert_eq!((e.buf.line(0).as_str(), e.cursor.line), ("a1", 2), "G の後の位置へ戻る");
        keys(&mut e, "<C-i>");
        assert_eq!((e.buf.line(0).as_str(), e.cursor.line), ("b1", 1), "進むと b の続きへ");
        keys(&mut e, "'A");
        assert_eq!((e.buf.line(0).as_str(), e.cursor.line), ("a1", 1), "大文字のマークで a へ");
        keys(&mut e, ":marks<CR>");
        assert!(e.message.contains("A:a.txt:2"), "{}", e.message);
    }
}
