//! 名前の変更（IntelliJ の Rename、Shift+F6・`:Rename 新しい名前`）。LSP は使わない。
//!
//! - ローカル変数・引数（ファイルの中の束縛）: 今のファイルの中で、同じ束縛を指す所だけを変える（u で戻せる）。
//! - それ以外（型・メソッド・フィールドなど）: 参照の検索（Alt+F7）と同じ求め方で、定義が同じものを指す所を集め、
//!   下の欄に一覧を出して確かめてから、全部のファイルを書き換える。Java・Kotlin の型で、ファイルの名前が型の名前なら
//!   ファイルの名前も変える。開いているファイルはバッファを書き換え（今のバッファは u で戻せる）、ほかはファイルに書く。
//!   定義が 1 か所に決まらないとき・保存していないファイルがあるときは変えない。

use std::path::PathBuf;

use crate::editor::{Editor, Prompt, State};

/// 名前に使える文字だけか（Java・Kotlin・TS・Python の識別子）
fn valid_name(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$') && cs.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

/// 変える 1 か所（根からの相対パス, 行（0 始まり）, 行の中の byte）
type Site = (String, usize, usize);

impl Editor {
    /// `:Rename [新しい名前]`。名前を書かなければ、今の名前を入れたコマンドの行を開く（Shift+F6）
    pub(crate) fn rename_command(&mut self, arg: &str) {
        let Some((name, word_at)) = self.word_at_cursor() else {
            self.notify.error("E348: カーソルの下に識別子がありません");
            return;
        };
        let new = arg.trim();
        if new.is_empty() {
            self.set_state(State::Command);
            self.cmdline = Prompt::default();
            self.cmdline.set(&format!("Rename {name}"));
            return;
        }
        if !valid_name(new) {
            self.notify.error(format!("名前に使えない文字があります: {new}"));
            return;
        }
        if new == name {
            return;
        }
        self.ensure_syntax();
        // ファイルの中の束縛のうち、プロジェクトの索引に無いもの（ローカル変数・引数）。メソッド・フィールドは索引にある
        if let Some(def) = self.syntax.as_ref().and_then(|s| s.local_definition(&self.buf, word_at)) {
            let Some(idx) = &self.nav.index else {
                self.notify.warn("索引を作っているところです。しばらくしてからもう一度");
                return;
            };
            let here = self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok());
            let rel = here.as_ref().and_then(|h| h.strip_prefix(&idx.root).ok()).map(|r| r.to_string_lossy().replace('\\', "/"));
            let at = self.buf.pos_of(def);
            let member = rel.is_some_and(|rel| idx.lookup(&name).iter().any(|x| *x.path == *rel && (x.line as usize, x.col as usize) == at));
            if !member {
                self.rename_local(&name, new, def);
                return;
            }
        }
        if !self.dirty_docs().is_empty() {
            self.notify.warn("保存していないファイルがあります。:wa で保存してから名前を変えてください");
            return;
        }
        let Some(target) = self.definition_target(&name, word_at) else {
            self.notify.warn(format!("{name} の定義が 1 か所に決まらないので、名前を変えられません（索引ができていないか、同じ名前の定義がいくつもある）"));
            return;
        };
        self.open_grep_regex(&format!(r"\b{}\b", regex::escape(&name)));
        if let Some(g) = &mut self.grep {
            g.refs = Some(crate::grep_panel::RefsFilter { name, target, rename: Some(new.to_string()) });
        }
        self.notify.info("名前を変える所を探しています…");
    }

    /// 今のファイルの中で、`def` の束縛を指す `name` を `new` に（1 回の編集で戻せる）
    fn rename_local(&mut self, name: &str, new: &str, def: usize) {
        let text = self.buf.text();
        let re = regex::Regex::new(&format!(r"\b{}\b", regex::escape(name))).unwrap();
        let syn = self.syntax.as_ref();
        let at: Vec<usize> = re
            .find_iter(&text)
            .map(|m| m.start())
            // x.name・this.name（メンバー）は除く
            .filter(|&s| !text[..s].trim_end().ends_with('.'))
            .filter(|&s| s == def || syn.and_then(|y| y.local_definition(&self.buf, s)) == Some(def))
            .collect();
        let (line, byte) = (self.cursor.line, self.cursor.byte);
        self.hist.begin();
        for &s in at.iter().rev() {
            self.edit(s, s + name.len(), new);
        }
        self.hist.close();
        self.cursor.line = line;
        let delta = at.iter().filter(|&&s| s < self.buf.line_start(line) + byte && s >= self.buf.line_start(line)).count() as isize * (new.len() as isize - name.len() as isize);
        self.cursor.byte = ((byte as isize + delta).max(0) as usize).min(self.buf.line(line).len());
        self.clamp_normal();
        self.notify.info(format!("{name} を {new} に変えました（{} か所。u で戻せる）", at.len()));
    }

    /// 全文検索は 1 行に 1 件なので、同じ行のほかの一致もそれぞれ 1 件に分ける（1 つずつ定義を確かめて変えるため）
    pub(crate) fn split_rename_hits(&mut self, name: &str) {
        let Some(g) = &mut self.grep else { return };
        let re = regex::Regex::new(&format!(r"\b{}\b", regex::escape(name))).unwrap();
        let mut file: Option<(String, Vec<String>)> = None;
        let mut out = Vec::with_capacity(g.hits.len());
        for h in std::mem::take(&mut g.hits) {
            if file.as_ref().is_none_or(|f| f.0 != h.path) {
                let text = std::fs::read_to_string(g.root.join(&h.path)).unwrap_or_default();
                file = Some((h.path.clone(), text.lines().map(String::from).collect()));
            }
            let line = file.as_ref().and_then(|f| f.1.get(h.line.saturating_sub(1))).map(|l| l.trim_start().to_string()).unwrap_or_default();
            let ms: Vec<(usize, usize)> = re.find_iter(&line).map(|m| (m.start(), m.end())).collect();
            if ms.len() <= 1 {
                out.push(h);
                continue;
            }
            for r in ms {
                out.push(nagi_workers::grep::Hit { range: r, ..h.clone() });
            }
        }
        g.hits = out;
    }

    /// 参照の検索の絞り込みが終わった: 一覧（下の欄）を見せて、確かめてから変える
    pub(crate) fn ask_rename(&mut self, name: String, new: String, target: (String, usize, usize)) {
        let Some(g) = &self.grep else { return };
        let sites: Vec<Site> = g.hits.iter().map(|h| (h.path.clone(), h.line.saturating_sub(1), h.range.0)).collect();
        let files = {
            let mut v: Vec<&str> = sites.iter().map(|s| s.0.as_str()).collect();
            v.dedup();
            v.len()
        };
        if sites.is_empty() {
            self.notify.warn(format!("{name} の使われている所が見つかりません"));
            return;
        }
        let root = g.root.clone();
        let q = format!("{name} を {new} に変えます（{} か所・{files} ファイル。下の欄の一覧のとおり）", sites.len());
        self.ask(q, move |e| e.apply_rename(root, &name, &new, sites, target));
    }

    fn apply_rename(&mut self, root: PathBuf, name: &str, new: &str, sites: Vec<Site>, target: (String, usize, usize)) {
        let (mut done, mut skipped, mut failed) = (0usize, 0usize, Vec::new());
        let mut by_file: Vec<(String, Vec<(usize, usize)>)> = Vec::new();
        for (p, l, c) in sites {
            match by_file.last_mut() {
                Some((q, v)) if *q == p => v.push((l, c)),
                _ => by_file.push((p, vec![(l, c)])),
            }
        }
        // どのバッファの u でも、ほかのファイルを一緒に戻せるように、変えたものに同じ札を付ける
        let tag = self.multi_edits.last().map_or(1, |m| m.tag + 1);
        let mut multi = MultiEdit { tag, open: Vec::new(), disk: Vec::new(), moved: None };
        let cur = self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok());
        let (line, byte) = (self.cursor.line, self.cursor.byte);
        for (rel, places) in &by_file {
            let path = root.join(rel);
            // 行の中の一致の位置（ヒットの本文は行頭の空白を除いたもの）→ ファイルの中の byte
            let starts = |buf: &nagi_core::Buffer| -> Vec<usize> {
                let mut v: Vec<usize> = places
                    .iter()
                    .filter_map(|&(l, c)| {
                        if l >= buf.line_count() {
                            return None;
                        }
                        let text = buf.line(l);
                        let col = text.len() - text.trim_start().len() + c;
                        text.get(col..)?.starts_with(name).then(|| buf.line_start(l) + col)
                    })
                    .collect();
                v.sort_unstable();
                v.dedup();
                v
            };
            if cur.as_ref().is_some_and(|c| crate::buffers::same_path(c, &path)) {
                let at = starts(&self.buf);
                skipped += places.len() - at.len();
                self.hist.begin();
                for &s in at.iter().rev() {
                    self.edit(s, s + name.len(), new);
                }
                self.hist.tag_current(tag);
                done += at.len();
                if let Err(e) = self.buf.save() {
                    failed.push(format!("{rel}: {e}"));
                }
                multi.open.push(path);
                continue;
            }
            // 脇にあるバッファ（その履歴に残す）
            if let Some(d) = self.find_doc(&path).and_then(|id| self.parked_doc_mut(id)) {
                let at = starts(&d.buf);
                skipped += places.len() - at.len();
                d.hist.begin();
                for &s in at.iter().rev() {
                    let c = d.buf.edit(s, s + name.len(), new);
                    d.hist.record(c);
                }
                d.hist.tag_current(tag);
                done += at.len();
                if let Err(e) = d.buf.save() {
                    failed.push(format!("{rel}: {e}"));
                }
                multi.open.push(path);
                continue;
            }
            // 開いていないファイル（前の中身を覚えておく）
            let mut buf = match nagi_core::Buffer::open(&path) {
                Ok(b) => b,
                Err(e) => {
                    failed.push(format!("{rel}: {e}"));
                    continue;
                }
            };
            let before = buf.text();
            let at = starts(&buf);
            skipped += places.len() - at.len();
            for &s in at.iter().rev() {
                buf.edit(s, s + name.len(), new);
            }
            done += at.len();
            match buf.save() {
                Ok(_) => multi.disk.push((path, before, buf.text())),
                Err(e) => failed.push(format!("{rel}: {e}")),
            }
        }
        // Java・Kotlin の型で、ファイルの名前が型の名前なら、ファイルの名前も
        let mut moved = None;
        let tpath = root.join(&target.0);
        let is_type = self.nav.index.as_ref().is_some_and(|i| i.lookup(name).iter().any(|l| *l.path == *target.0 && l.kind.is_type()));
        let ext = tpath.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if is_type && matches!(ext.as_str(), "java" | "kt") && tpath.file_stem().is_some_and(|s| s == name) {
            let to = tpath.with_file_name(format!("{new}.{ext}"));
            if to.exists() {
                failed.push(format!("{} があるので、ファイルの名前は変えません", to.display()));
            } else {
                match std::fs::rename(&tpath, &to) {
                    Ok(()) => {
                        self.repath_doc(&tpath, &to);
                        for p in multi.open.iter_mut().chain(multi.disk.iter_mut().map(|d| &mut d.0)) {
                            if crate::buffers::same_path(p, &tpath) || *p == tpath {
                                *p = to.clone();
                            }
                        }
                        multi.moved = Some((tpath.clone(), to.clone()));
                        moved = Some(to);
                    }
                    Err(e) => failed.push(format!("{}: {e}", tpath.display())),
                }
            }
        }
        self.multi_edits.push(multi);
        if self.multi_edits.len() > 20 {
            self.multi_edits.remove(0);
        }
        if cur.is_some() {
            self.cursor.line = line.min(self.buf.line_count().saturating_sub(1));
            self.cursor.byte = byte.min(self.buf.line(self.cursor.line).len());
            self.clamp_normal();
        }
        self.close_grep();
        // 索引を作り直す（変えた名前で引けるように）
        self.start_index();
        for f in &failed {
            self.notify.error(f.clone());
        }
        let mut msg = format!("{name} を {new} に変えました（{done} か所・{} ファイル。u で全部のファイルを戻せる）", by_file.len());
        if let Some(to) = moved {
            msg.push_str(&format!("。ファイルの名前も {}", to.file_name().unwrap_or_default().to_string_lossy()));
        }
        if skipped > 0 {
            msg.push_str(&format!("。中身がずれていた {skipped} か所は変えていません"));
        }
        if failed.is_empty() {
            self.notify.info(msg);
        }
    }

    /// u・Ctrl-r の 1 回。名前の変更で一緒に変えたもの（同じ札）なら、ほかのファイルも一緒に戻す・進める
    pub(crate) fn undo_step(&mut self, redo: bool) -> Option<usize> {
        let tag = if redo { self.hist.redo_tag() } else { self.hist.undo_tag() };
        let at = if redo { self.hist.redo(&mut self.buf) } else { self.hist.undo(&mut self.buf) };
        if let (Some(_), Some(t)) = (at, tag) {
            self.follow_multi(t, redo);
        }
        at
    }

    fn follow_multi(&mut self, tag: u64, redo: bool) {
        let Some(m) = self.multi_edits.iter().find(|m| m.tag == tag).cloned() else { return };
        let (mut n, mut stale, mut failed) = (0usize, Vec::new(), Vec::new());
        let cur = self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok());
        // 戻すときは、先にファイルの名前を戻す（バッファの名前も）
        if let (Some((from, to)), false) = (&m.moved, redo) {
            if to.exists() && !from.exists() && std::fs::rename(to, from).is_ok() {
                self.repath_doc(to, from);
            }
        }
        // 名前を変えたファイルは、この間は前の名前にある（戻すときは先に戻した、進めるときは最後に変える）
        let place = |p: &PathBuf| -> PathBuf {
            match &m.moved {
                Some((from, to)) if p == to => from.clone(),
                _ => p.clone(),
            }
        };
        if let Err(e) = self.buf.save() {
            failed.push(e.to_string());
        }
        for p in &m.open {
            let p = place(p);
            if cur.as_ref().is_some_and(|c| crate::buffers::same_path(c, &p)) {
                n += 1;
                continue;
            }
            match self.find_doc(&p).and_then(|id| self.parked_doc_mut(id)) {
                Some(d) if (if redo { d.hist.redo_tag() } else { d.hist.undo_tag() }) == Some(tag) => {
                    if redo { d.hist.redo(&mut d.buf) } else { d.hist.undo(&mut d.buf) };
                    if let Err(e) = d.buf.save() {
                        failed.push(format!("{}: {e}", p.display()));
                    }
                    n += 1;
                }
                // 後から変えた・閉じたバッファは触らない
                _ => stale.push(p),
            }
        }
        for (p, before, after) in &m.disk {
            let p = place(p);
            let (want, put) = if redo { (before, after) } else { (after, before) };
            match nagi_core::Buffer::open(&p) {
                Ok(mut b) if b.text() == *want => {
                    let len = b.len_bytes();
                    b.edit(0, len, put);
                    match b.save() {
                        Ok(_) => n += 1,
                        Err(e) => failed.push(format!("{}: {e}", p.display())),
                    }
                }
                _ => stale.push(p),
            }
        }
        // 進めるときは、最後にファイルの名前を変える
        if let (Some((from, to)), true) = (&m.moved, redo) {
            if from.exists() && !to.exists() && std::fs::rename(from, to).is_ok() {
                self.repath_doc(from, to);
            }
        }
        for f in failed {
            self.notify.error(f);
        }
        let mut msg = format!("名前の変更を{}（{n} ファイル）", if redo { "やり直しました" } else { "戻しました" });
        if !stale.is_empty() {
            msg.push_str(&format!("。後から変えた {} ファイルは触っていません: {}", stale.len(), stale.iter().filter_map(|p| p.file_name()).map(|f| f.to_string_lossy().into_owned()).collect::<Vec<_>>().join(", ")));
        }
        self.notify.info(msg);
        self.start_index();
    }
}

/// 一度に変えたファイルたち（名前の変更 1 回）
#[derive(Clone, Debug)]
pub struct MultiEdit {
    /// 履歴の節の札（History::tag_current）
    pub tag: u64,
    /// バッファで変えたファイル（その履歴に札がある）
    pub open: Vec<PathBuf>,
    /// 開いていなかったファイル（パス, 前の中身, 後の中身）
    pub disk: Vec<(PathBuf, String, String)>,
    /// ファイルの名前を変えた（前, 後）
    pub moved: Option<(PathBuf, PathBuf)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;
    use std::path::Path;

    fn open(path: &Path) -> Editor {
        let mut buf = Buffer::open(path).unwrap();
        buf.path = Some(path.to_path_buf());
        let lang = nagi_syntax::Lang::detect(path).unwrap();
        let mut syn = nagi_syntax::Syntax::new(lang).unwrap();
        syn.parse(&mut buf);
        let mut e = Editor::new(buf, Some(syn));
        e.rows = 20;
        e
    }

    fn keys(e: &mut Editor, k: &str) {
        for key in nagi_vim::parse_keys(k, " ") {
            e.key(key);
        }
    }

    #[test]
    fn names() {
        assert!(valid_name("fooBar_1") && valid_name("$x") && valid_name("名前"));
        assert!(!valid_name("1a") && !valid_name("a-b") && !valid_name(""));
    }

    /// ローカル変数: 同じ束縛だけ（同じ名前のフィールド・別のメソッドの変数は変えない）。u で戻せる
    #[test]
    fn renames_local() {
        let dir = std::env::temp_dir().join(format!("nagi-rename-local-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("A.java");
        std::fs::write(&f, "class A {\n  int n;\n  void a() {\n    int n = 1;\n    n = n + this.n;\n  }\n  void b() {\n    int n = 2;\n  }\n}\n").unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        let mut e = open(&f);
        e.start_index();
        for _ in 0..1000 {
            e.poll_index();
            if e.nav.index.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        keys(&mut e, "4G0ww");
        e.rename_command("count");
        assert_eq!(e.buf.text(), "class A {\n  int n;\n  void a() {\n    int count = 1;\n    count = count + this.n;\n  }\n  void b() {\n    int n = 2;\n  }\n}\n", "{}", e.notify.text);
        keys(&mut e, "u");
        assert!(e.buf.text().contains("    int n = 1;\n    n = n + this.n;"));
        // 名前を書かなければ、コマンドの行に今の名前
        e.rename_command("");
        assert_eq!(e.state(), State::Command);
        assert_eq!(e.cmdline.text, "Rename n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 型: 参照の検索で集めた所を、確かめてから全部のファイルで変え、ファイルの名前も変える
    #[test]
    fn renames_type_across_files() {
        let dir = std::env::temp_dir().join(format!("nagi-rename-type-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("p")).unwrap();
        std::fs::write(dir.join("p/Foo.java"), "package p;\n\npublic class Foo {\n  public Foo() {}\n}\n").unwrap();
        std::fs::write(dir.join("p/Use.java"), "package p;\n\nclass Use {\n  Foo f = new Foo();\n  // Foo\n  String s = \"Foo\";\n}\n").unwrap();
        let mut e = open(&dir.join("p/Use.java"));
        e.start_index();
        for _ in 0..1000 {
            e.poll_index();
            if e.nav.index.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        keys(&mut e, "4G0w");
        e.rename_command("Bar");
        for _ in 0..2000 {
            e.poll_grep();
            if e.state() == State::Menu {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(e.state(), State::Menu, "確かめる: {}", e.notify.text);
        // はい
        keys(&mut e, "<CR>");
        assert_eq!(std::fs::read_to_string(dir.join("p/Use.java")).unwrap(), "package p;\n\nclass Use {\n  Bar f = new Bar();\n  // Foo\n  String s = \"Foo\";\n}\n", "{}", e.notify.text);
        assert!(!dir.join("p/Foo.java").exists());
        assert_eq!(std::fs::read_to_string(dir.join("p/Bar.java")).unwrap(), "package p;\n\npublic class Bar {\n  public Bar() {}\n}\n");
        assert_eq!(e.buf.text(), std::fs::read_to_string(dir.join("p/Use.java")).unwrap(), "開いているバッファも");
        assert!(e.notify.text.contains("ファイルの名前も Bar.java"), "{}", e.notify.text);
        // u で全部のファイルとファイルの名前を戻し、Ctrl-r でやり直す
        let use0 = "package p;\n\nclass Use {\n  Foo f = new Foo();\n  // Foo\n  String s = \"Foo\";\n}\n";
        let foo0 = "package p;\n\npublic class Foo {\n  public Foo() {}\n}\n";
        keys(&mut e, "u");
        assert_eq!(e.buf.text(), use0);
        assert_eq!(std::fs::read_to_string(dir.join("p/Use.java")).unwrap(), use0);
        assert!(!dir.join("p/Bar.java").exists());
        assert_eq!(std::fs::read_to_string(dir.join("p/Foo.java")).unwrap(), foo0, "{}", e.notify.text);
        assert!(e.notify.text.contains("名前の変更を戻しました（2 ファイル）"), "{}", e.notify.text);
        keys(&mut e, "<C-r>");
        assert!(e.buf.text().contains("Bar f = new Bar();"));
        assert!(std::fs::read_to_string(dir.join("p/Bar.java")).unwrap().contains("public class Bar"), "{}", e.notify.text);
        assert!(!dir.join("p/Foo.java").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// メソッド: 同じ名前の別のクラスのメソッド・コメント・文字列は変えない。開いていない（脇の）バッファも
    #[test]
    fn renames_method_only_where_it_points() {
        let dir = std::env::temp_dir().join(format!("nagi-rename-method-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join("A.java"), "class A {\n  void run() {}\n  void use() {\n    run(); run();\n  }\n}\n").unwrap();
        std::fs::write(dir.join("B.java"), "class B {\n  void run() {}\n  void go(A a) {\n    a.run();\n    run();\n    // run\n    log(\"A.run\");\n  }\n}\n").unwrap();
        let mut e = open(&dir.join("B.java"));
        e.edit_file(dir.join("A.java"));
        e.start_index();
        for _ in 0..1000 {
            e.poll_index();
            if e.nav.index.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        // A の run の定義の上で
        keys(&mut e, "2G0fr");
        e.rename_command("start");
        for _ in 0..2000 {
            e.poll_grep();
            if e.state() == State::Menu {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        keys(&mut e, "<CR>");
        assert_eq!(e.buf.text(), "class A {\n  void start() {}\n  void use() {\n    start(); start();\n  }\n}\n", "{}", e.notify.text);
        assert_eq!(std::fs::read_to_string(dir.join("B.java")).unwrap(), "class B {\n  void run() {}\n  void go(A a) {\n    a.start();\n    run();\n    // run\n    log(\"A.run\");\n  }\n}\n");
        // 脇のバッファも書き換えている
        let id = e.find_doc(&dir.join("B.java")).unwrap();
        assert!(e.parked_doc_mut(id).unwrap().buf.text().contains("a.start();"));
        // 今のバッファの u で、脇のバッファも戻す（ファイルも）
        keys(&mut e, "u");
        assert!(e.buf.text().contains("void run() {}"));
        assert!(e.parked_doc_mut(id).unwrap().buf.text().contains("a.run();"));
        assert!(std::fs::read_to_string(dir.join("B.java")).unwrap().contains("a.run();"));
        // 脇のバッファを後から変えていれば、そこは触らない
        keys(&mut e, "<C-r>");
        assert!(e.parked_doc_mut(id).unwrap().buf.text().contains("a.start();"));
        let d = e.parked_doc_mut(id).unwrap();
        let c = d.buf.edit(0, 0, "// x\n");
        d.hist.record(c);
        keys(&mut e, "u");
        assert!(e.parked_doc_mut(id).unwrap().buf.text().contains("a.start();"));
        assert!(e.notify.text.contains("後から変えた 1 ファイル"), "{}", e.notify.text);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 本物のプロジェクトの写しで名前を変える（NAGI_RENAME_FILE=ファイル NAGI_RENAME_AT=行:列（1 始まり） NAGI_RENAME_TO=新しい名前。写しを書き換える）
    #[test]
    #[ignore]
    fn rename_in_copy_of_project() {
        let (Some(file), Ok(at), Ok(to)) = (std::env::var_os("NAGI_RENAME_FILE").map(std::path::PathBuf::from), std::env::var("NAGI_RENAME_AT"), std::env::var("NAGI_RENAME_TO")) else { return };
        let (l, c) = at.split_once(':').unwrap();
        let mut e = open(&file);
        e.start_index();
        for _ in 0..30000 {
            e.poll_index();
            if e.nav.index.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        e.cursor.line = l.parse::<usize>().unwrap() - 1;
        e.cursor.byte = c.parse::<usize>().unwrap() - 1;
        e.rename_command(&to);
        for _ in 0..30000 {
            e.poll_grep();
            if e.state() == State::Menu {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        eprintln!("ASK {}", e.notify.text);
        if e.state() == State::Menu {
            keys(&mut e, "<CR>");
        }
        eprintln!("DONE {}", e.notify.text);
    }
}
