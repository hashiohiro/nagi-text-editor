//! 複数のバッファ。作業中のバッファは Editor の欄（buf・hist・syntax など）にそのまま置き、
//! それ以外を `Doc` として脇に置く。切り替えるときに入れ替えるので、Vim 操作のコードは作業中の 1 本だけを見ればよい。
//!
//! バッファの番号は開いた順の 1 から（Vim と同じ）。消しても番号は詰めない。
//! 隠れたバッファの変更は保ったまま切り替えられる（Vim の `set hidden` と同じ）。

use std::path::{Path, PathBuf};

use nagi_core::{Buffer, Cursor, History};

use crate::editor::{Editor, State};

/// 脇に置いたバッファと、その表示状態
pub(crate) struct Doc {
    pub buf: Buffer,
    pub hist: History,
    pub syntax: Option<nagi_syntax::Syntax>,
    pub pending_syntax: Option<nagi_core::task::Task<nagi_syntax::Syntax>>,
    pub last_change: Option<usize>,
    pub marks: crate::marks::Marks,
    pub folds: crate::folds::Folds,
    /// 最後に見ていたときのカーソルと表示位置（戻ってきたときにそこから続ける）
    pub cursor: Cursor,
    pub top: usize,
}

pub(crate) enum Slot {
    /// Editor の欄に出ている（作業中）
    Active,
    Parked(Box<Doc>),
    /// `:bd` で消した（番号は詰めない）
    Deleted,
}

/// 同じファイルかどうかの比較用。相対パスや大文字小文字の違いを吸収する
fn same_file(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()));
    norm(a).to_string_lossy().eq_ignore_ascii_case(&norm(b).to_string_lossy())
}

impl Editor {
    /// 開いているバッファの (番号, パス, 変更あり, 作業中か)
    pub fn buffer_list(&self) -> Vec<(usize, Option<PathBuf>, bool, bool)> {
        self.docs
            .iter()
            .enumerate()
            .filter_map(|(i, s)| match s {
                Slot::Active => Some((i, self.buf.path.clone(), self.buf.dirty, true)),
                Slot::Parked(d) => Some((i, d.buf.path.clone(), d.buf.dirty, false)),
                Slot::Deleted => None,
            })
            .collect()
    }

    /// 作業中のバッファを脇へ置き、`id` を作業中にする
    pub(crate) fn switch_doc(&mut self, id: usize) {
        if id == self.cur_doc || !matches!(self.docs.get(id), Some(Slot::Parked(_))) {
            return;
        }
        // 別のファイルへ移るのはジャンプ（Ctrl-o で戻れる）
        self.push_jump();
        self.finish_load();
        let Slot::Parked(next) = std::mem::replace(&mut self.docs[id], Slot::Active) else { unreachable!() };
        let prev = self.swap_in(*next);
        self.docs[self.cur_doc] = Slot::Parked(Box::new(prev));
        self.alt_doc = Some(self.cur_doc);
        self.cur_doc = id;
        // 脇にある間に外部の変更で読み直したバッファは、構文木を作り直す
        if self.syntax.is_none() && self.pending_syntax.is_none() {
            self.start_syntax();
        }
    }

    /// `d` を Editor の欄へ入れ、出ていたものを返す
    fn swap_in(&mut self, d: Doc) -> Doc {
        let prev = Doc {
            buf: std::mem::replace(&mut self.buf, d.buf),
            hist: std::mem::replace(&mut self.hist, d.hist),
            syntax: std::mem::replace(&mut self.syntax, d.syntax),
            pending_syntax: std::mem::replace(&mut self.pending_syntax, d.pending_syntax),
            last_change: std::mem::replace(&mut self.last_change, d.last_change),
            marks: std::mem::replace(&mut self.marks, d.marks),
            folds: std::mem::replace(&mut self.folds, d.folds),
            cursor: std::mem::replace(&mut self.cursor, d.cursor),
            top: std::mem::replace(&mut self.top, d.top),
        };
        self.frac = 0.0;
        if !matches!(self.state(), State::Normal) {
            self.set_state(State::Normal);
        }
        self.pending_keys.clear();
        self.reset_search_cache();
        self.poll_syntax();
        let last = self.buf.line_count().saturating_sub(1);
        self.cursor.line = self.cursor.line.min(last);
        self.clamp_normal();
        prev
    }

    /// 新しいバッファを足して作業中にする
    fn push_doc(&mut self, buf: Buffer) {
        let id = self.docs.len();
        self.docs.push(Slot::Active);
        self.push_jump();
        let d = Doc { buf, hist: History::default(), syntax: None, pending_syntax: None, last_change: None, marks: Default::default(), folds: Default::default(), cursor: Cursor::default(), top: 0 };
        self.finish_load();
        let prev = self.swap_in(d);
        self.docs[self.cur_doc] = Slot::Parked(Box::new(prev));
        self.alt_doc = Some(self.cur_doc);
        self.cur_doc = id;
        self.start_syntax();
    }

    /// `path` のバッファを探す
    pub(crate) fn find_doc(&self, path: &Path) -> Option<usize> {
        self.docs.iter().enumerate().find_map(|(i, s)| {
            let p = match s {
                Slot::Active => self.buf.path.as_deref(),
                Slot::Parked(d) => d.buf.path.as_deref(),
                Slot::Deleted => None,
            }?;
            same_file(p, path).then_some(i)
        })
    }

    /// `:e path`。開いていればそのバッファへ、無ければ読み込んで足す
    pub(crate) fn edit_file(&mut self, path: PathBuf) {
        if let Some(id) = self.find_doc(&path) {
            self.switch_doc(id);
            return;
        }
        let mut message = String::new();
        let buf = match Buffer::open(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut b = Buffer::from_str("");
                b.path = Some(path.clone());
                self.new_file_defaults(&mut b);
                message = format!("\"{}\" [新規]", path.display());
                b
            }
            Err(e) => {
                self.notify.error(format!("{}: {e}", path.display()));
                return;
            }
        };
        // 最初の名前の無い空のバッファは、使わずに置き換える（Vim と同じ）
        if self.buf.path.is_none() && !self.buf.dirty && self.buf.len_bytes() == 0 && self.docs.len() == 1 {
            self.docs.clear();
            self.docs.push(Slot::Active);
            self.cur_doc = 0;
            let d = Doc { buf, hist: History::default(), syntax: None, pending_syntax: None, last_change: None, marks: Default::default(), folds: Default::default(), cursor: Cursor::default(), top: 0 };
            self.swap_in(d);
            self.start_syntax();
        } else {
            self.push_doc(buf);
        }
        crate::finder::remember_recent(&path);
        self.watch_file(&path);
        self.notify.info(message);
    }

    /// `:new`。空のバッファを足して作業中にする
    pub(crate) fn new_empty_buffer(&mut self) {
        self.push_doc(Buffer::from_str(""));
    }

    /// `:e!`。作業中のバッファを読み直す（変更は捨てる）
    pub(crate) fn reload(&mut self) {
        let Some(path) = self.buf.path.clone() else { return };
        match Buffer::open(&path) {
            Ok(b) => {
                let d = Doc { buf: b, hist: History::default(), syntax: None, pending_syntax: None, last_change: None, marks: std::mem::take(&mut self.marks), folds: Default::default(), cursor: self.cursor, top: self.top };
                self.swap_in(d);
                self.start_syntax();
            }
            Err(e) => self.notify.error(format!("{}: {e}", path.display())),
        }
    }

    /// `:b {N|名前|#}`
    pub(crate) fn goto_buffer(&mut self, arg: &str) {
        let arg = arg.trim();
        let id = if arg == "#" {
            self.alt_doc.filter(|&i| matches!(self.docs.get(i), Some(Slot::Parked(_))))
        } else if let Ok(n) = arg.parse::<usize>() {
            n.checked_sub(1).filter(|&i| !matches!(self.docs.get(i), None | Some(Slot::Deleted)))
        } else {
            // 名前の一部（大文字小文字を区別しない）。1 つに決まらなければエラー
            let low = arg.to_lowercase();
            let hits: Vec<usize> = self
                .buffer_list()
                .into_iter()
                .filter(|(_, p, _, _)| p.as_ref().is_some_and(|p| p.to_string_lossy().to_lowercase().contains(&low)))
                .map(|(i, ..)| i)
                .collect();
            match hits.as_slice() {
                [i] => Some(*i),
                [] => None,
                _ => {
                    self.notify.error(format!("E93: {arg} に当てはまるバッファが複数あります"));
                    return;
                }
            }
        };
        match id {
            Some(i) => self.switch_doc(i),
            None => self.notify.error(format!("E86: バッファ {arg} はありません")),
        }
    }

    /// `:bn` / `:bp`（端では反対側へ回る）
    pub(crate) fn cycle_buffer(&mut self, forward: bool, count: usize) {
        let list: Vec<usize> = self.buffer_list().into_iter().map(|(i, ..)| i).collect();
        let Some(pos) = list.iter().position(|&i| i == self.cur_doc) else { return };
        let n = list.len();
        let to = if forward { (pos + count) % n } else { (pos + n - count % n) % n };
        self.switch_doc(list[to]);
    }

    /// `:bd[!] [N]`。作業中のバッファを消したら、別のバッファへ移る（最後の 1 本なら空のバッファにする）
    pub(crate) fn delete_buffer(&mut self, arg: &str, force: bool) {
        let id = match arg.trim() {
            "" => self.cur_doc,
            a => match a.parse::<usize>().ok().and_then(|n| n.checked_sub(1)) {
                Some(i) if !matches!(self.docs.get(i), None | Some(Slot::Deleted)) => i,
                _ => {
                    self.notify.error(format!("E516: バッファ {a} はありません"));
                    return;
                }
            },
        };
        let dirty = match &self.docs[id] {
            Slot::Active => self.buf.dirty,
            Slot::Parked(d) => d.buf.dirty,
            Slot::Deleted => false,
        };
        if dirty && !force {
            self.notify.error(format!("E89: バッファ {} の変更が保存されていません（:bd! で破棄）", id + 1));
            return;
        }
        // 消すバッファを出しているほかの窓は閉じる（Vim と同じ）
        self.close_windows_of(id);
        if id != self.cur_doc {
            self.docs[id] = Slot::Deleted;
            return;
        }
        // 移る先: 直前のバッファ、無ければ次のバッファ
        let next = self
            .alt_doc
            .filter(|&i| matches!(self.docs.get(i), Some(Slot::Parked(_))))
            .or_else(|| self.docs.iter().position(|s| matches!(s, Slot::Parked(_))));
        match next {
            Some(n) => {
                self.switch_doc(n);
                self.docs[id] = Slot::Deleted;
            }
            None => {
                self.push_doc(Buffer::from_str(""));
                self.docs[id] = Slot::Deleted;
            }
        }
        self.alt_doc = None;
    }

    /// `:ls`。最下行に 1 行で出す（`%` 作業中、`#` 直前、`+` 変更あり）
    pub(crate) fn list_buffers(&mut self) {
        let items: Vec<String> = self
            .buffer_list()
            .into_iter()
            .map(|(i, p, dirty, active)| {
                let mark = if active { "%" } else if Some(i) == self.alt_doc { "#" } else { " " };
                let name = p.map_or("[No Name]".into(), |p| p.file_name().map_or(p.display().to_string(), |n| n.to_string_lossy().into_owned()));
                format!("{}{mark}{} {name}", i + 1, if dirty { "+" } else { "" })
            })
            .collect();
        self.notify.info(items.join("  "));
    }

    /// 変更が保存されていないバッファ（作業中を含む）の番号
    pub(crate) fn dirty_docs(&self) -> Vec<usize> {
        self.buffer_list().into_iter().filter(|(_, _, d, _)| *d).map(|(i, ..)| i).collect()
    }

    /// `:wa`。名前のあるバッファの変更をすべて保存する
    pub(crate) fn write_all(&mut self) -> bool {
        let mut ok = true;
        let mut n = 0;
        let mut failed = Vec::new();
        for slot in self.docs.iter_mut() {
            if let Slot::Parked(d) = slot {
                if d.buf.dirty && d.buf.path.is_some() {
                    // 保存時の処理（裏のバッファは取り消しの履歴には残さない）
                    for (s, e, t) in crate::editor_settings::tidy(&d.buf, self.settings.bool("editor.trim_trailing"), self.settings.bool("editor.final_newline")) {
                        d.buf.edit(s, e, &t);
                    }
                    match d.buf.save() {
                        Ok(_) => n += 1,
                        Err(e) => {
                            failed.push(format!("E212: 書き込めません: {e}"));
                            ok = false;
                        }
                    }
                }
            }
        }
        for m in failed {
            self.notify.error(m);
        }
        if self.buf.dirty && self.buf.path.is_some() {
            self.tidy_before_save();
            match self.buf.save() {
                Ok(_) => n += 1,
                Err(e) => {
                    self.notify.error(format!("E212: 書き込めません: {e}"));
                    ok = false;
                }
            }
        }
        if ok {
            self.notify.info(format!("{n} 個のバッファを書き込み"));
        }
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Effect;

    fn keys(e: &mut Editor, k: &str) -> Option<Effect> {
        let mut last = None;
        for key in nagi_vim::parse_keys(k, " ") {
            last = Some(e.key(key));
        }
        last
    }

    fn files() -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("nagi-buffers-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.txt"), dir.join("b.txt"));
        std::fs::write(&a, "a1\na2\na3\n").unwrap();
        std::fs::write(&b, "b1\nb2\n").unwrap();
        (a, b)
    }

    /// 切り替えても変更とカーソルは残り、`Ctrl-^` `:bn` `:b 名前` で行き来できる
    #[test]
    fn switch_keeps_changes_and_cursor() {
        let (a, b) = files();
        let mut e = Editor::new(Buffer::from_str(""), None);
        e.rows = 5;
        keys(&mut e, &format!(":e {}<CR>", a.display()));
        assert_eq!(e.docs.len(), 1, "最初の空のバッファは置き換える");
        keys(&mut e, "jx");
        assert_eq!(e.buf.line(1), "2");
        keys(&mut e, &format!(":e {}<CR>", b.display()));
        assert_eq!(e.buf.line(0), "b1");
        assert_eq!((e.cursor.line, e.cursor.byte), (0, 0));
        keys(&mut e, "<C-^>");
        assert_eq!(e.buf.line(1), "2", "変更は保たれる");
        assert_eq!(e.cursor.line, 1, "カーソルも戻る");
        assert!(e.buf.dirty);
        keys(&mut e, ":bn<CR>");
        assert_eq!(e.buf.line(0), "b1");
        keys(&mut e, ":b a.t<CR>");
        assert_eq!(e.buf.line(0), "a1");
        keys(&mut e, &format!(":e {}<CR>", b.display()));
        assert_eq!(e.docs.len(), 2, "開いているファイルは開き直さない");
        keys(&mut e, ":ls<CR>");
        assert!(e.notify.text.contains("1#+ a.txt") && e.notify.text.contains("2% b.txt"), "{}", e.notify.text);
    }

    /// 隠れたバッファに変更があれば `:q` `:bd` は止め、`!` なら進む
    #[test]
    fn quit_and_delete_check_hidden_changes() {
        let (a, b) = files();
        let mut e = Editor::new(Buffer::open(&a).unwrap(), None);
        e.rows = 5;
        keys(&mut e, "x");
        keys(&mut e, &format!(":e {}<CR>", b.display()));
        assert!(matches!(keys(&mut e, ":q<CR>"), Some(Effect::None)));
        assert!(e.notify.text.starts_with("E162"), "{}", e.notify.text);
        keys(&mut e, ":bd 1<CR>");
        assert!(e.notify.text.starts_with("E89"), "{}", e.notify.text);
        keys(&mut e, ":bd! 1<CR>");
        assert_eq!(e.buffer_list().len(), 1);
        assert!(matches!(keys(&mut e, ":q<CR>"), Some(Effect::Quit)));
        // 最後の 1 本を消すと空のバッファになる
        keys(&mut e, ":bd<CR>");
        assert_eq!(e.buffer_list().len(), 1);
        assert_eq!(e.buf.len_bytes(), 0);
    }
}

#[cfg(test)]
mod write_as_tests {
    use super::*;

    fn keys(e: &mut Editor, k: &str) {
        for key in nagi_vim::parse_keys(k, " ") {
            e.key(key);
        }
    }

    #[test]
    fn write_with_name() {
        let dir = std::env::temp_dir().join(format!("nagi-writeas-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.txt");
        // 名前の無いバッファに :w 名前 → その名前になって保存される
        let mut e = Editor::new(Buffer::from_str(""), None);
        keys(&mut e, &format!("ihello<Esc>:w {}<CR>", a.display()));
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "hello");
        assert_eq!(e.buf.path.as_deref(), Some(a.as_path()));
        assert!(!e.buf.dirty);
        // 名前のあるバッファの :w 別名 → 書き出すだけ（名前は a のまま）。既にあるファイルは ! が要る
        let b = dir.join("b.txt");
        keys(&mut e, &format!("A!<Esc>:w {}<CR>", b.display()));
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "hello!");
        assert_eq!(e.buf.path.as_deref(), Some(a.as_path()));
        assert!(e.buf.dirty, "a にはまだ書いていない");
        keys(&mut e, &format!(":w {}<CR>", b.display()));
        assert!(e.notify.text.starts_with("E13"), "{}", e.notify.text);
        keys(&mut e, &format!(":w! {}<CR>", b.display()));
        assert!(!e.notify.text.starts_with("E13"));
        // :saveas は名前ごと変える
        let c = dir.join("c.txt");
        keys(&mut e, &format!(":saveas {}<CR>", c.display()));
        assert_eq!(e.buf.path.as_deref(), Some(c.as_path()));
        assert_eq!(std::fs::read_to_string(&c).unwrap(), "hello!");
    }

    #[test]
    fn unnamed_write_opens_dialog() {
        let mut e = Editor::new(Buffer::from_str(""), None);
        let mut last = crate::editor::Effect::None;
        for k in nagi_vim::parse_keys("ihi<Esc>:w<CR>", " ") {
            last = e.key(k);
        }
        assert!(matches!(last, crate::editor::Effect::SaveDialog));
        let mut last = crate::editor::Effect::None;
        for k in nagi_vim::parse_keys(":saveas<CR>", " ") {
            last = e.key(k);
        }
        assert!(matches!(last, crate::editor::Effect::SaveDialog));
    }
}
