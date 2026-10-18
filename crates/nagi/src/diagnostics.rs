//! 外部コマンドの診断（`:Make` `:eslint`。architecture.md §10）。
//!
//! コマンドを裏で動かし、出力を 1 行ずつ「ファイル・行・列・エラーか警告か・本文」に分けて、下のパネル
//! （全文検索と同じもの。リストモードと `:cn` `:cp` が使える）に並べる。行番号はエラーの行を赤、警告の行を黄色にし、
//! カーソルの行の本文は最下行に出す。`]e` `[e` で次・前の診断へ（診断が無ければ構文エラーへ）。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use nagi_workers::grep::Hit;
use nagi_workers::jobs::{Job, JobMsg};
use regex::Regex;

use crate::editor::{Editor, Mode};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diag {
    pub path: PathBuf,
    /// 0 始まり
    pub line: usize,
    pub col: usize,
    pub error: bool,
    pub msg: String,
}

/// 動いているコマンド
pub struct MakeState {
    job: Job,
    pub cmd: String,
    root: PathBuf,
    /// 診断の形でない行もパネルに出す（Maven の実行）
    output: bool,
    /// 出力の中の「BUILD SUCCESS」「BUILD FAILURE」
    status: Option<String>,
}

/// 診断を指すとみなす拡張子（`http://host:80:` のような行を拾わないため）
const SOURCE_EXT: &[&str] = &[
    "ts", "tsx", "js", "jsx", "mjs", "cjs", "vue", "java", "kt", "kts", "rs", "py", "go", "c", "cc", "cpp", "h", "hpp", "cs", "scala", "groovy", "gradle", "xml", "json", "yml", "yaml", "properties", "sql",
];

fn patterns() -> &'static [Regex] {
    static P: OnceLock<Vec<Regex>> = OnceLock::new();
    P.get_or_init(|| {
        [
            // tsc: src/a.ts(3,5): error TS2304: …
            r"^(?P<f>[^\s(][^(]*)\((?P<l>\d+),(?P<c>\d+)\): (?P<s>error|warning) (?P<m>.*)$",
            // Maven: [ERROR] /x/A.java:[12,5] …
            r"^\[(?P<s>ERROR|WARNING)\] (?P<f>.+?):\[(?P<l>\d+),(?P<c>\d+)\] (?P<m>.*)$",
            // Kotlin（Gradle）: e: file:///C:/x/A.kt:12:5 …
            r"^(?P<s>e|w): (?:file:///)?(?P<f>.+?):(?P<l>\d+):(?P<c>\d+) (?P<m>.*)$",
            // javac（Gradle）: C:\x\A.java:12: error: …
            r"^(?P<f>(?:[A-Za-z]:)?[^:]+):(?P<l>\d+): (?P<s>error|warning): (?P<m>.*)$",
            // gcc 形式（cargo の short、eslint の unix）: src\a.rs:3:5: error[E0425]: …
            r"^(?P<f>(?:[A-Za-z]:)?[^:]+):(?P<l>\d+):(?P<c>\d+):? (?:(?P<s>error|warning|note)\S*?:)?\s*(?P<m>.*)$",
        ]
        .iter()
        .map(|p| Regex::new(p).unwrap())
        .collect()
    })
}

/// 出力の 1 行を診断にする（`root` は相対パスの基準）
pub fn parse_line(line: &str, root: &Path) -> Option<Diag> {
    let line = line.trim_end();
    for re in patterns() {
        let Some(c) = re.captures(line) else { continue };
        let f = c.name("f")?.as_str().trim();
        let p = PathBuf::from(f.replace('/', std::path::MAIN_SEPARATOR_STR));
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        if !SOURCE_EXT.contains(&ext.as_str()) {
            continue;
        }
        let path = if p.is_absolute() { p } else { root.join(p) };
        let l: usize = c.name("l")?.as_str().parse().ok()?;
        let col: usize = c.name("c").and_then(|m| m.as_str().parse().ok()).unwrap_or(1);
        let msg = c.name("m").map_or("", |m| m.as_str()).trim().to_string();
        let error = match c.name("s").map(|s| s.as_str().to_ascii_lowercase()) {
            Some(s) => s == "error" || s == "e",
            // 種類の書かれていない行（eslint の unix）は本文の [Warning/…] で見分ける
            None => !msg.contains("[Warning"),
        };
        return Some(Diag { path, line: l.saturating_sub(1), col: col.saturating_sub(1), error, msg });
    }
    None
}

/// プロジェクトの根のファイルから、型や文法を確かめるコマンドを決める
fn detect_command(root: &Path) -> Option<String> {
    let has = |f: &str| root.join(f).exists();
    Some(if has("tsconfig.json") {
        "npx tsc --noEmit --pretty false".into()
    } else if has("gradlew.bat") {
        "gradlew.bat compileJava compileTestJava -q --console=plain".into()
    } else if has("build.gradle") || has("build.gradle.kts") {
        "gradle compileJava compileTestJava -q --console=plain".into()
    } else if has("pom.xml") {
        "mvn -q -B compile".into()
    } else if has("Cargo.toml") {
        "cargo check --message-format short".into()
    } else {
        return None;
    })
}

fn rel(p: &Path, root: &Path) -> String {
    p.strip_prefix(root).map_or_else(|_| p.display().to_string(), |r| r.to_string_lossy().replace('\\', "/"))
}

impl Editor {
    /// `:Make [コマンド]`
    pub(crate) fn make(&mut self, arg: &str) {
        let root = self.nav_root();
        let cmd = if arg.trim().is_empty() {
            match detect_command(&root) {
                Some(c) => c,
                None => {
                    self.message = "コマンドが決められません（:Make の後にコマンドを書いてください）".into();
                    return;
                }
            }
        } else {
            arg.trim().to_string()
        };
        self.run_job(&cmd, &root, &format!("診断: {cmd}"), false);
    }

    /// コマンドを `root` で動かし、診断（`output` なら出力の全行）を下のパネルに出す
    pub(crate) fn run_job(&mut self, cmd: &str, root: &Path, title: &str, output: bool) {
        let waker = self.waker.clone().unwrap_or_else(|| std::sync::Arc::new(|| {}));
        // Maven・Gradle で JAVA_HOME が使えなければ（古い JRE を指したままなど）、見つけた JDK をこのコマンドのときだけ使う
        let java = ["mvn", "gradle"].iter().any(|t| cmd.split_whitespace().next().is_some_and(|w| w.to_ascii_lowercase().starts_with(t)));
        let fix = if java { nagi_workers::jobs::java_home_fix() } else { None };
        let env: Vec<(String, String)> = fix.iter().map(|j| ("JAVA_HOME".to_string(), j.display().to_string())).collect();
        match nagi_workers::jobs::run_env(cmd, root, &env, waker) {
            Ok(job) => {
                self.diags.clear();
                self.open_result_panel(title);
                if let (Some(j), Some(g)) = (&fix, &mut self.grep) {
                    let text = format!("JAVA_HOME が使えないので、このコマンドでは {} を使います", j.display());
                    g.hits.push(nagi_workers::grep::Hit { path: String::new(), line: 0, range: (0, 0), text });
                }
                if let Some(g) = &mut self.grep {
                    g.root = root.to_path_buf();
                }
                self.make = Some(MakeState { job, cmd: cmd.to_string(), root: root.to_path_buf(), output, status: None });
            }
            Err(e) => self.message = format!("コマンドを起動できません: {e}"),
        }
    }

    /// 出力を受け取る。変わったら true
    pub fn poll_make(&mut self) -> bool {
        let Some(m) = &self.make else { return false };
        let mut changed = false;
        let mut done = None;
        let mut lines = Vec::new();
        while let Ok(msg) = m.job.rx.try_recv() {
            match msg {
                JobMsg::Line(l) => lines.push(l),
                JobMsg::Done(code) => done = Some(code),
            }
            changed = true;
        }
        let (root, output) = (m.root.clone(), m.output);
        for l in lines {
            match parse_line(&l, &root) {
                Some(d) => {
                    if let Some(g) = self.output_panel_mut() {
                        let head = if d.error { "error" } else { "warning" };
                        let text = format!("{head}: {}", d.msg);
                        g.hits.push(Hit { path: rel(&d.path, &root), line: d.line + 1, range: (0, head.len()), text });
                    }
                    self.diags.push(d);
                }
                None if output => {
                    if l.contains("BUILD SUCCESS") || l.contains("BUILD FAILURE") {
                        if let Some(m) = &mut self.make {
                            m.status = Some(if l.contains("SUCCESS") { "BUILD SUCCESS" } else { "BUILD FAILURE" }.into());
                        }
                    }
                    // 診断の形でない行は、ファイルを持たない行として出す
                    if let Some(g) = self.output_panel_mut() {
                        g.hits.push(Hit { path: String::new(), line: 0, range: (0, 0), text: l });
                    }
                }
                None => {}
            }
        }
        if let Some(code) = done {
            let (cmd, status) = self.make.take().map(|m| (m.cmd, m.status)).unwrap_or_default();
            let (e, w) = (self.diags.iter().filter(|d| d.error).count(), self.diags.iter().filter(|d| !d.error).count());
            if let Some(g) = self.output_panel_mut() {
                g.running = false;
            }
            self.message = match (e + w, code) {
                _ if status.is_some() => format!("{}（エラー {e} 件、警告 {w} 件）", status.unwrap_or_default()),
                (0, Some(0)) => format!("診断: 問題はありません（{cmd}）"),
                (0, c) => format!("診断: 診断の形の出力はありませんでした（終了コード {}）", c.map_or("-".into(), |c| c.to_string())),
                _ => format!("診断: エラー {e} 件、警告 {w} 件"),
            };
        }
        changed
    }

    /// `]e` / `[e`: 次・前の診断へ。診断が無ければ構文エラー（tree-sitter の ERROR の節）へ
    pub(crate) fn next_error(&mut self, forward: bool) {
        if self.diags.is_empty() {
            self.ensure_syntax();
            let Some(syn) = &self.syntax else { return };
            let errs = syn.error_positions();
            let at = self.abs();
            let to = if forward { errs.iter().copied().find(|&p| p > at) } else { errs.iter().rev().copied().find(|&p| p < at) };
            match to {
                Some(p) => {
                    self.push_jump();
                    self.set_abs(p);
                    self.clamp_normal();
                }
                None => self.message = "構文エラーはありません".into(),
            }
            return;
        }
        let here = self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok());
        let cur = (self.cursor.line, self.cursor.byte);
        let mut list: Vec<&Diag> = self.diags.iter().collect();
        list.sort_by(|a, b| (&a.path, a.line, a.col).cmp(&(&b.path, b.line, b.col)));
        // 今のファイルの中で次・前、無ければファイルをまたいで
        let in_file = |d: &&Diag| Some(&d.path) == here.as_ref();
        let pick = if forward {
            list.iter().find(|d| in_file(d) && (d.line, d.col) > cur).or_else(|| list.iter().find(|d| here.as_ref().is_none_or(|h| d.path > *h))).or(list.first())
        } else {
            list.iter().rev().find(|d| in_file(d) && (d.line, d.col) < cur).or_else(|| list.iter().rev().find(|d| here.as_ref().is_none_or(|h| d.path < *h))).or(list.last())
        };
        if let Some(d) = pick.map(|d| (*d).clone()) {
            self.jump_to(&d.path, d.line, d.col);
            self.message = format!("{}: {}", if d.error { "error" } else { "warning" }, d.msg);
        }
    }

    /// カーソルの行の診断（最下行に出す）
    pub fn diag_at_cursor(&self) -> Option<&Diag> {
        if self.diags.is_empty() {
            return None;
        }
        let here = self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok())?;
        self.diags.iter().filter(|d| d.path == here && d.line == self.cursor.line).min_by_key(|d| !d.error)
    }

    /// `path` のファイルの診断のある行（行 → エラーか）
    pub fn diag_lines(&self, path: Option<&Path>) -> std::collections::HashMap<usize, bool> {
        let mut out = std::collections::HashMap::new();
        if self.diags.is_empty() {
            return out;
        }
        let Some(p) = path.and_then(|p| std::path::absolute(p).ok()) else { return out };
        for d in self.diags.iter().filter(|d| d.path == p) {
            let e = out.entry(d.line).or_insert(false);
            *e |= d.error;
        }
        out
    }

    pub(crate) fn nav_root(&self) -> PathBuf {
        let root = self
            .buf
            .path
            .as_deref()
            .and_then(|p| std::path::absolute(p).ok())
            .and_then(|p| nagi_workers::files::project_root(&p))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        std::path::absolute(&root).unwrap_or(root)
    }

    /// 結果のパネルを空にして開く（見出し付き。フォーカスは編集画面のまま）
    pub(crate) fn open_result_panel(&mut self, title: &str) {
        // 検索の結果は脇に残して、ログのタブに出す
        self.stash_grep_unless(true);
        self.open_grep_raw("");
        if let Some(g) = &mut self.grep {
            g.title = Some(title.to_string());
            g.running = true;
            g.hits.clear();
            g.query.set("");
        }
        self.bottom_kind = crate::result_grid::BottomKind::Output;
        self.mode = Mode::Normal;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_formats() {
        let root = PathBuf::from(r"C:\p");
        let d = parse_line("src/a.ts(3,5): error TS2304: Cannot find name 'x'.", &root).unwrap();
        assert_eq!((d.path.clone(), d.line, d.col, d.error, d.msg.as_str()), (root.join(r"src\a.ts"), 2, 4, true, "TS2304: Cannot find name 'x'."));
        let d = parse_line(r"C:\p\src\A.java:12: error: cannot find symbol", &root).unwrap();
        assert_eq!((d.line, d.col, d.error, d.msg.as_str()), (11, 0, true, "cannot find symbol"));
        let d = parse_line(r"[WARNING] C:\p\src\A.java:[7,3] unchecked call", &root).unwrap();
        assert_eq!((d.line, d.col, d.error), (6, 2, false));
        let d = parse_line("e: file:///C:/p/src/B.kt:4:9 Unresolved reference: foo", &root).unwrap();
        assert_eq!((d.line, d.col, d.error, d.msg.as_str()), (3, 8, true, "Unresolved reference: foo"));
        let d = parse_line(r"src\main.rs:3:5: error[E0425]: cannot find value `y` in this scope", &root).unwrap();
        assert_eq!((d.line, d.col, d.error, d.msg.as_str()), (2, 4, true, "cannot find value `y` in this scope"));
        let d = parse_line(r"C:\p\a.js:1:10: 'x' is defined but never used. [Warning/no-unused-vars]", &root).unwrap();
        assert!(!d.error);
        // ソースでないものは拾わない
        assert!(parse_line("Listening on http://localhost:8080:1: x", &root).is_none());
        assert!(parse_line("BUILD SUCCESSFUL in 3s", &root).is_none());
    }

    #[test]
    fn make_panel_and_jump() {
        let dir = std::env::temp_dir().join(format!("nagi-make-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join("a.rs"), "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
        let mut buf = nagi_core::Buffer::open(&dir.join("a.rs")).unwrap();
        buf.path = Some(dir.join("a.rs"));
        let mut e = Editor::new(buf, None);
        e.rows = 20;
        e.make("echo a.rs:3:4: error: boom&& echo a.rs:2:1: warning: careful&& echo done");
        for _ in 0..500 {
            e.poll_make();
            if e.make.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(e.make.is_none(), "終わらない");
        assert_eq!(e.diags.len(), 2);
        assert_eq!(e.grep.as_ref().unwrap().hits.len(), 2);
        assert!(e.message.contains("エラー 1 件、警告 1 件"), "{}", e.message);
        // ]e: 次の診断（2 行目の警告）、もう一度で 3 行目のエラー
        e.next_error(true);
        assert_eq!((e.cursor.line, e.cursor.byte), (1, 0));
        assert!(e.message.starts_with("warning"));
        e.next_error(true);
        assert_eq!((e.cursor.line, e.cursor.byte), (2, 3));
        e.message.clear();
        assert_eq!(e.diag_at_cursor().map(|d| d.msg.as_str()), Some("boom"));
        assert_eq!(e.diag_lines(e.buf.path.as_deref()).len(), 2);
    }
}
