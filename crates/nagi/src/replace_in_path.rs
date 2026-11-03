//! 複数ファイル内置換（IntelliJ の Replace in Path。Ctrl+Shift+R）。
//!
//! 全文検索の小窓に置換の欄を足して開く。Tab で検索語と置換の欄を行き来し、Enter（「置換」）で選んでいる一致の行を、
//! Alt+A（「すべて置換」）で確かめてから結果のすべての行を置き換える。正規表現なら `$1` で組を使える。
//! 書き換えは名前の変更（rename.rs）と同じく、開いているファイルはバッファで（その履歴に残す）、開いていないファイルは
//! 文字コードを保って書き、どのバッファの u でも全部のファイルを一緒に戻せる。

use std::path::PathBuf;

use crate::editor::{Editor, State};

impl Editor {
    /// Ctrl+Shift+R: 置換の欄のある全文検索の小窓を開く
    pub(crate) fn open_replace(&mut self) {
        self.open_grep("");
        if let Some(g) = &mut self.panels.grep {
            g.replacing = true;
            g.editing_replace = false;
        }
    }

    /// 検索の条件から、行の中で置き換える正規表現（grep.rs の探し方と同じ: 大小・単語・正規表現）
    fn replace_regex(&self) -> Result<regex::Regex, String> {
        let g = self.panels.grep.as_ref().ok_or("検索していません")?;
        let o = g.forced.as_ref().unwrap_or(&g.opts);
        let pat = if o.regex { g.query.text.clone() } else { regex::escape(&g.query.text) };
        let pat = if o.words { format!(r"\b(?:{pat})\b") } else { pat };
        regex::RegexBuilder::new(&pat).case_insensitive(!o.case).build().map_err(|e| e.to_string())
    }

    /// 置き換える（`all` なら結果のすべての行、でなければ選んでいる一致の行）
    pub(crate) fn grep_replace(&mut self, all: bool) {
        let Some(g) = &self.panels.grep else { return };
        if g.hits.is_empty() || g.query.text.is_empty() {
            self.svc.notify.info("置き換えるものがありません");
            return;
        }
        let re = match self.replace_regex() {
            Ok(r) => r,
            Err(e) => {
                self.svc.notify.error(format!("正規表現の誤り: {e}"));
                return;
            }
        };
        let regex_mode = g.forced.as_ref().unwrap_or(&g.opts).regex;
        let (query, rep) = (g.query.text.clone(), g.replace.text.clone());
        // ファイルごとの行（0 始まり）
        let mut by_file: Vec<(String, Vec<usize>)> = Vec::new();
        let picked: Vec<&nagi_workers::grep::Hit> = if all { g.hits.iter().collect() } else { g.hits.get(g.selected).into_iter().collect() };
        for h in picked {
            match by_file.last_mut() {
                Some((p, v)) if *p == h.path => v.push(h.line - 1),
                _ => by_file.push((h.path.clone(), vec![h.line - 1])),
            }
        }
        let root = g.root.clone();
        if !all {
            self.replace_lines(root, by_file, re, rep, regex_mode);
            return;
        }
        let (n, files) = (g.hits.len(), by_file.len());
        self.ask_or(
            format!("{files} 件のファイルに {n} 回出現する '{query}' を '{rep}' に置換しますか？"),
            move |e| e.replace_lines(root, by_file, re, rep, regex_mode),
            |e| e.svc.notify.info("置換をやめました"),
        );
    }

    /// ファイルの行の、正規表現に合う所をすべて置き換えて保存し、探し直す
    fn replace_lines(&mut self, root: PathBuf, by_file: Vec<(String, Vec<usize>)>, re: regex::Regex, rep: String, regex_mode: bool) {
        // 行の中の置き換え（始め, 終わり, 新しい文字）。正規表現でなければ置換の文字はそのまま（$ を組と読まない）
        let plan = |buf: &nagi_core::Buffer, lines: &[usize]| -> Vec<(usize, usize, String)> {
            let mut v = Vec::new();
            for &l in lines {
                if l >= buf.line_count() {
                    continue;
                }
                let text = buf.line(l);
                let body = text.trim_end_matches(['\n', '\r']);
                let base = buf.line_start(l);
                for c in re.captures_iter(body) {
                    let m = c.get(0).unwrap();
                    if m.is_empty() {
                        continue;
                    }
                    let new = if regex_mode {
                        let mut s = String::new();
                        c.expand(&rep, &mut s);
                        s
                    } else {
                        rep.clone()
                    };
                    v.push((base + m.start(), base + m.end(), new));
                }
            }
            v.sort_by_key(|x| x.0);
            v.dedup_by_key(|x| x.0);
            v
        };
        let (done, files, failed) = self.edit_files(&root, &by_file, &plan);
        for f in &failed {
            self.svc.notify.error(f.clone());
        }
        if done == 0 && failed.is_empty() {
            self.svc.notify.info("置き換えるものがありませんでした（ファイルが変わっている）");
        } else if failed.is_empty() {
            self.svc.notify.info(format!("{done} か所を置き換えました（{files} ファイル。u で全部のファイルを戻せる）"));
        }
        // 結果を探し直す（置き換えた所は出なくなる）
        let keep = self.state();
        if let Some(g) = &mut self.panels.grep {
            g.query.cursor = g.query.text.len();
        }
        self.restart_grep();
        if keep == State::GrepPrompt {
            self.set_state(State::GrepPrompt);
        }
    }

    /// ファイルたちを書き換える（`plan` はバッファから置き換えの範囲を作る）。開いているファイルはバッファで（その履歴に残し）、
    /// 開いていないファイルは文字コードを保って書く。どのバッファの u でも全部を戻せるように同じ札を付ける。
    /// (置き換えた数, 変えたファイルの数, 誤り)
    pub(crate) fn edit_files(&mut self, root: &std::path::Path, by_file: &[(String, Vec<usize>)], plan: &dyn Fn(&nagi_core::Buffer, &[usize]) -> Vec<(usize, usize, String)>) -> (usize, usize, Vec<String>) {
        let (mut done, mut files, mut failed) = (0usize, 0usize, Vec::new());
        let tag = self.multi_edits.last().map_or(1, |m| m.tag + 1);
        let mut multi = crate::rename::MultiEdit { tag, open: Vec::new(), disk: Vec::new(), moved: None };
        let cur = self.core.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok());
        let (line, byte) = (self.core.cursor.line, self.core.cursor.byte);
        for (rel, lines) in by_file {
            let path = root.join(rel);
            if cur.as_ref().is_some_and(|c| crate::buffers::same_path(c, &path)) {
                let edits = plan(&self.core.buf, lines);
                if edits.is_empty() {
                    continue;
                }
                self.core.hist.begin();
                for (s, e, t) in edits.iter().rev() {
                    self.edit(*s, *e, t);
                }
                self.core.hist.tag_current(tag);
                done += edits.len();
                files += 1;
                if let Err(e) = self.core.buf.save() {
                    failed.push(format!("{rel}: {e}"));
                }
                multi.open.push(path);
                continue;
            }
            if let Some(d) = self.find_doc(&path).and_then(|id| self.parked_doc_mut(id)) {
                let edits = plan(&d.buf, lines);
                if edits.is_empty() {
                    continue;
                }
                d.hist.begin();
                for (s, e, t) in edits.iter().rev() {
                    let c = d.buf.edit(*s, *e, t);
                    d.hist.record(c);
                }
                d.hist.tag_current(tag);
                done += edits.len();
                files += 1;
                if let Err(e) = d.buf.save() {
                    failed.push(format!("{rel}: {e}"));
                }
                multi.open.push(path);
                continue;
            }
            let mut buf = match nagi_core::Buffer::open(&path) {
                Ok(b) => b,
                Err(e) => {
                    failed.push(format!("{rel}: {e}"));
                    continue;
                }
            };
            let edits = plan(&buf, lines);
            if edits.is_empty() {
                continue;
            }
            let before = buf.text();
            for (s, e, t) in edits.iter().rev() {
                buf.edit(*s, *e, t);
            }
            done += edits.len();
            files += 1;
            match buf.save() {
                Ok(_) => multi.disk.push((path, before, buf.text())),
                Err(e) => failed.push(format!("{rel}: {e}")),
            }
        }
        if files > 0 {
            self.multi_edits.push(multi);
            if self.multi_edits.len() > 20 {
                self.multi_edits.remove(0);
            }
        }
        if cur.is_some() {
            self.core.cursor.line = line.min(self.core.buf.line_count().saturating_sub(1));
            self.core.cursor.byte = byte.min(self.core.buf.line(self.core.cursor.line).len());
            self.clamp_normal();
        }
        (done, files, failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;
    use nagi_vim::Key;

    fn wait(e: &mut Editor) {
        for _ in 0..200 {
            e.poll_grep();
            if !e.panels.grep.as_ref().is_some_and(|g| g.searching()) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// 複数ファイル内置換: 選んだ一致の行だけ・すべて（確かめてから）を置き換える。開いているファイルはバッファで、
    /// 開いていない Shift_JIS のファイルは文字コードを保つ。u で全部のファイルを戻せる。正規表現なら $1
    #[test]
    fn replaces_in_files() {
        let dir = std::env::temp_dir().join(format!("nagi-repl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join("a.txt"), "old one\nkeep\nold two old\n").unwrap();
        // Shift_JIS の「古い old」
        let (sjis, _, _) = encoding_rs::SHIFT_JIS.encode("古い old\n");
        std::fs::write(dir.join("b.txt"), &sjis).unwrap();
        let mut buf = Buffer::open(&dir.join("a.txt")).unwrap();
        buf.path = Some(dir.join("a.txt"));
        let mut e = Editor::new(buf, None);
        e.cwd = dir.clone();
        e.set_screen(120, 40);
        e.open_replace();
        assert_eq!(e.state(), State::GrepPrompt);
        e.grep_insert("old");
        wait(&mut e);
        assert_eq!(e.panels.grep.as_ref().unwrap().hits.len(), 3, "a.txt の 2 行と b.txt: {:?}", e.panels.grep.as_ref().unwrap().hits);
        // 置換の欄（Tab）
        e.grep_prompt_key(Key::Tab);
        assert!(e.panels.grep.as_ref().unwrap().editing_replace);
        e.grep_insert("new");
        // Enter: 選んでいる一致（a.txt の 1 行目）の行だけ
        e.grep_prompt_key(Key::Enter);
        assert_eq!(e.core.buf.line(0), "new one");
        assert_eq!(e.core.buf.line(2), "old two old");
        wait(&mut e);
        assert_eq!(e.panels.grep.as_ref().unwrap().hits.len(), 2);
        // Alt+A: すべて（確かめる）
        e.grep_prompt_key(Key::Alt('a'));
        assert!(e.menu.is_some(), "確かめる");
        e.menu_run(0);
        assert_eq!(e.core.buf.line(2), "new two new");
        let raw = std::fs::read(dir.join("b.txt")).unwrap();
        let (text, _, bad) = encoding_rs::SHIFT_JIS.decode(&raw);
        assert!(!bad && text == "古い new\n", "Shift_JIS のまま: {text:?}");
        // u で全部のファイルを戻す
        e.set_state(State::Normal);
        e.undo_step(false);
        assert_eq!(e.core.buf.line(2), "old two old");
        let raw = std::fs::read(dir.join("b.txt")).unwrap();
        let (text, _, _) = encoding_rs::SHIFT_JIS.decode(&raw);
        assert_eq!(text, "古い old\n");
        // 正規表現の組
        let mut e2 = Editor::new(Buffer::from_str(""), None);
        e2.cwd = dir.clone();
        std::fs::write(dir.join("c.txt"), "get_name(x)\n").unwrap();
        e2.open_replace();
        if let Some(g) = &mut e2.panels.grep {
            g.opts.regex = true;
            g.opts.mask = "c.txt".into();
        }
        e2.grep_insert(r"get_(\w+)\(");
        wait(&mut e2);
        if let Some(g) = &mut e2.panels.grep {
            g.replace.set("fetch_$1(");
        }
        e2.grep_replace(false);
        assert_eq!(std::fs::read_to_string(dir.join("c.txt")).unwrap(), "fetch_name(x)\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
