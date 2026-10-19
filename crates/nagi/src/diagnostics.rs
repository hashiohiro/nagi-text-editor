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

use crate::editor::{Editor, State};

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
    /// 複数行で出る診断の前の行（rustc の error: …、Python の File "…", line N）
    pending: Pending,
}

/// 複数行で出る診断の途中
#[derive(Default)]
pub struct Pending {
    /// rustc・clang などの見出し（エラーか, 本文）。次の --> の行で位置が分かる
    header: Option<(bool, String)>,
    /// Python の File "x.py", line N（次の XxxError: の行で本文が分かる）
    py: Option<PyAt>,
}

/// Python の File の行の後: 元の行（先頭の空白を削って字下げして出る）と、その下の ^ の行
#[derive(Default)]
struct PyAt {
    file: String,
    line: usize,
    /// 出力の中の元の行の字下げ
    src_indent: Option<usize>,
    /// 元の行の中の ^ の位置（削った空白は含まない）
    col: Option<usize>,
}

/// 複数行で出る診断を読む（rustc の見出しと --> の行、Python の File の行と XxxError の行）
pub fn parse_multi(p: &mut Pending, line: &str, root: &Path) -> Option<Diag> {
    static RS_HEAD: OnceLock<Regex> = OnceLock::new();
    static RS_AT: OnceLock<Regex> = OnceLock::new();
    static PY_AT: OnceLock<Regex> = OnceLock::new();
    static PY_ERR: OnceLock<Regex> = OnceLock::new();
    let head = RS_HEAD.get_or_init(|| Regex::new(r"^(error|warning)(\[\w+\])?: (.+)$").unwrap());
    let at = RS_AT.get_or_init(|| Regex::new(r"^\s*--> (.+?):(\d+):(\d+)\s*$").unwrap());
    let py_at = PY_AT.get_or_init(|| Regex::new(r#"^\s*File "(.+?)", line (\d+)"#).unwrap());
    let py_err = PY_ERR.get_or_init(|| Regex::new(r"^(\w+(?:Error|Exception|Warning)): (.*)$").unwrap());
    let line = line.trim_end();
    if let Some(c) = head.captures(line) {
        p.header = Some((&c[1] == "error", c[3].to_string()));
        return None;
    }
    if let Some(c) = at.captures(line) {
        let (error, msg) = p.header.take()?;
        let (l, col) = (c[2].parse::<usize>().ok()?, c[3].parse::<usize>().ok()?);
        return Some(Diag { path: diag_path(&c[1], root), line: l.saturating_sub(1), col: col.saturating_sub(1), error, msg });
    }
    if let Some(c) = py_at.captures(line) {
        p.py = Some(PyAt { file: c[1].to_string(), line: c[2].parse().unwrap_or(1), ..Default::default() });
        return None;
    }
    if let Some(c) = py_err.captures(line) {
        let at = p.py.take()?;
        let path = diag_path(&at.file, root);
        let line = at.line.saturating_sub(1);
        // ^ の位置に、ファイルの行の削られた字下げを足す（読めなければ ^ の位置のまま）
        let col = at.col.map_or(0, |c| {
            let indent = std::fs::read_to_string(&path).ok().and_then(|t| t.lines().nth(line).map(|l| l.chars().take_while(|c| c.is_whitespace()).count()));
            c + indent.unwrap_or(0)
        });
        return Some(Diag { path, line, col, error: !c[1].ends_with("Warning"), msg: format!("{}: {}", &c[1], &c[2]) });
    }
    // File の行の後の元の行と ^ の行（3.11 からは ~~~^^^ の形も）
    if let Some(at) = p.py.as_mut().filter(|a| a.col.is_none()) {
        let indent = line.chars().take_while(|c| *c == ' ').count();
        let body = line.trim();
        match at.src_indent {
            None if !body.is_empty() => at.src_indent = Some(indent),
            Some(si) if !body.is_empty() && body.chars().all(|c| matches!(c, '^' | '~')) => {
                let mark = line.chars().position(|c| c == '^').unwrap_or(indent);
                at.col = Some(mark.saturating_sub(si));
            }
            _ => {}
        }
    }
    None
}

/// 出力の中のパス（相対なら根から、WSL の中で動かしたコマンドの /home/... はそのディストリビューションの UNC に）
fn diag_path(f: &str, root: &Path) -> PathBuf {
    let p = PathBuf::from(f.trim().replace('/', std::path::MAIN_SEPARATOR_STR));
    match nagi_workers::wsl::split_unc(root).map(|(d, _)| d) {
        Some(d) if f.starts_with('/') => nagi_workers::wsl::to_unc(&d, f),
        _ if p.is_absolute() => p,
        _ => root.join(p),
    }
}

/// 診断を指すとみなす拡張子（`http://host:80:` のような行を拾わないため）
const SOURCE_EXT: &[&str] = &[
    "ts", "tsx", "js", "jsx", "mjs", "cjs", "vue", "java", "kt", "kts", "rs", "py", "go", "c", "cc", "cpp", "cxx", "c++", "h", "hpp", "hh", "hxx", "inl", "ipp", "m", "mm", "cs", "fs", "vb", "scala", "sbt", "groovy", "gradle", "xml",
    "json", "yml", "yaml", "properties", "sql", "php", "rb", "lua", "swift", "dart", "zig", "hs", "lhs", "ex", "exs", "r", "pl", "toml", "cmake", "txt",
];

fn patterns() -> &'static [Regex] {
    static P: OnceLock<Vec<Regex>> = OnceLock::new();
    P.get_or_init(|| {
        [
            // tsc: src/a.ts(3,5): error TS2304: …
            r"^(?P<f>[^\s(][^(]*)\((?P<l>\d+),(?P<c>\d+)\): (?P<s>error|warning) (?P<m>.*)$",
            // MSVC（cl）・dotnet: C:\x\a.cpp(12): error C2065: … / a.cs(12,5): warning CS0168: …
            r"^\s*(?P<f>(?:[A-Za-z]:)?[^(:]+)\((?P<l>\d+)(?:,(?P<c>\d+))?\)\s*:\s*(?:fatal )?(?P<s>error|warning) (?P<m>.*)$",
            // sbt（Scala）: [error] /x/A.scala:3:5: …
            r"^\[(?P<s>error|warn)\] (?P<f>.+?):(?P<l>\d+):(?P<c>\d+): (?P<m>.*)$",
            // dart analyze --format machine: ERROR|COMPILE_TIME_ERROR|UNDEFINED_IDENTIFIER|/x/a.dart|3|5|1|…
            r"^(?P<s>ERROR|WARNING|INFO)\|[A-Z_]+\|[A-Z_]+\|(?P<f>[^|]+)\|(?P<l>\d+)\|(?P<c>\d+)\|\d+\|(?P<m>.*)$",
            // PHP: PHP Parse error: syntax error, … in x.php on line 3
            r"^(?:PHP )?(?:Parse|Fatal) error:\s+(?P<m>.*) in (?P<f>.+?) on line (?P<l>\d+)",
            // Elixir: ** (CompileError) lib/a.ex:3: …
            r"^\*\* \(\w+Error\) (?P<f>\S+?):(?P<l>\d+):(?:(?P<c>\d+):)? (?P<m>.*)$",
            // Maven: [ERROR] /x/A.java:[12,5] …
            r"^\[(?P<s>ERROR|WARNING)\] (?P<f>.+?):\[(?P<l>\d+),(?P<c>\d+)\] (?P<m>.*)$",
            // Kotlin（Gradle）: e: file:///C:/x/A.kt:12:5 …
            r"^(?P<s>e|w): (?:file:///)?(?P<f>.+?):(?P<l>\d+):(?P<c>\d+) (?P<m>.*)$",
            // javac（Gradle）: C:\x\A.java:12: error: …
            r"^(?P<f>(?:[A-Za-z]:)?[^:]+):(?P<l>\d+): (?P<s>error|warning): (?P<m>.*)$",
            // gcc 形式（cargo の short、eslint の unix、go vet、clang、zig、swift、ghc）: src\a.rs:3:5: error[E0425]: …
            r"^(?P<f>(?:[A-Za-z]:)?[^:\s][^:]*):(?P<l>\d+):(?P<c>\d+):? (?:(?P<s>error|warning|note)\S*?:)?\s*(?P<m>.*)$",
            // 列の無い形（ruby -wc、luac）: a.rb:3: syntax error, … / luac: a.lua:3: …
            r"^(?:luac: )?(?P<f>(?:[A-Za-z]:)?[^:\s][^:]*):(?P<l>\d+): (?:(?P<s>warning|error):\s*)?(?P<m>.*)$",
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
        let ext = std::path::Path::new(f).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        if !SOURCE_EXT.contains(&ext.as_str()) {
            continue;
        }
        let path = diag_path(f, root);
        let l: usize = c.name("l")?.as_str().parse().ok()?;
        let col: usize = c.name("c").and_then(|m| m.as_str().parse().ok()).unwrap_or(1);
        let msg = c.name("m").map_or("", |m| m.as_str()).trim().to_string();
        // gcc・clang の note は前の診断の補足（数えない）
        if c.name("s").is_some_and(|s| s.as_str() == "note") {
            return None;
        }
        let error = match c.name("s").map(|s| s.as_str().to_ascii_lowercase()) {
            Some(s) => s == "error" || s == "e",
            // 種類の書かれていない行（eslint の unix）は本文の [Warning/…] で見分ける
            None => !msg.contains("[Warning"),
        };
        return Some(Diag { path, line: l.saturating_sub(1), col: col.saturating_sub(1), error, msg });
    }
    None
}

/// プロジェクトの根のファイルから、型や文法を確かめるコマンドを決める。プロジェクトとしてのビルドが無ければ、
/// 開いているファイル（`file`）を言語ごとに検査する（Python・PHP・Ruby・Lua）
fn detect_command(root: &Path, file: Option<&Path>) -> Option<String> {
    let has = |f: &str| root.join(f).exists();
    let has_ext = |e: &str| std::fs::read_dir(root).into_iter().flatten().flatten().any(|x| x.path().extension().is_some_and(|x| x.eq_ignore_ascii_case(e)));
    let ex = nagi_workers::exec::Exec::in_dir(root);
    if let Some(c) = project_command(&has, &has_ext, ex.is_wsl()) {
        return Some(c);
    }
    // 開いているファイルの検査（WSL なら Linux のパスと Linux のコマンド）
    let f = file?;
    let q = format!("\"{}\"", ex.path(f));
    let ext = f.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "py" | "pyw" => format!("{} -m py_compile {q}", ex.pick("python", "python3")),
        "php" => format!("php -l {q}"),
        "rb" => format!("ruby -wc {q}"),
        "lua" => format!("luac -p {q}"),
        "c" => format!("{} -fsyntax-only -Wall {q}", ex.pick("clang", "gcc")),
        "cpp" | "cc" | "cxx" => format!("{} -fsyntax-only -Wall {q}", ex.pick("clang++", "g++")),
        "go" => format!("go vet {q}"),
        "rs" => format!("rustc --edition 2021 --error-format short --emit metadata -o {} {q}", ex.pick("NUL", "/dev/null")),
        "swift" => format!("swiftc -parse {q}"),
        "dart" => format!("dart analyze --format machine {q}"),
        "zig" => format!("zig ast-check {q}"),
        "hs" => format!("ghc -fno-code {q}"),
        "exs" | "ex" => format!("elixirc --ignore-module-conflict -o {} {q}", ex.pick("%TEMP%", "/tmp")),
        _ => return None,
    })
}

/// プロジェクトのファイルから決まるコマンド
fn project_command(has: &dyn Fn(&str) -> bool, has_ext: &dyn Fn(&str) -> bool, wsl: bool) -> Option<String> {
    Some(if has("go.mod") {
        "go vet ./...".into()
    } else if has_ext("sln") || has_ext("csproj") || has_ext("fsproj") {
        "dotnet build -nologo -v q -clp:NoSummary".into()
    } else if has("build.zig") {
        "zig build".into()
    } else if has("Package.swift") {
        "swift build".into()
    } else if has("pubspec.yaml") {
        "dart analyze --format machine".into()
    } else if has("mix.exs") {
        "mix compile --force".into()
    } else if has("stack.yaml") {
        "stack build --fast".into()
    } else if has_ext("cabal") {
        "cabal build".into()
    } else if has("build.sbt") {
        "sbt -batch compile".into()
    } else if has("CMakeLists.txt") {
        "cmake -S . -B build && cmake --build build".into()
    } else if has("Makefile") || has("makefile") || has("GNUmakefile") {
        "make -k".into()
    } else if has("tsconfig.json") {
        "npx tsc --noEmit --pretty false".into()
    } else if has("gradlew") && wsl {
        // WSL のプロジェクトは WSL の中で動かす
        "./gradlew compileJava compileTestJava -q --console=plain".into()
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
            let file = self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok());
            match detect_command(&root, file.as_deref()) {
                Some(c) => c,
                None => {
                    self.notify.warn("コマンドが決められません（:Make の後にコマンドを書いてください）");
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
        let waker = self.waker();
        // Maven・Gradle で JAVA_HOME が使えなければ（古い JRE を指したままなど）、見つけた JDK をこのコマンドのときだけ使う
        let mut ex = nagi_workers::exec::Exec::in_dir(root);
        // Maven・JDK が無ければ nagi 用に入れてから動かす
        let then = crate::settings_dialog::AfterTools::Job { cmd: cmd.to_string(), root: root.to_path_buf(), title: title.to_string(), output };
        if !self.fetch_missing_tools(ex.missing_tools(cmd), then) {
            return;
        }
        let fix = if nagi_workers::exec::needs_jdk(cmd) { ex.fix_jdk() } else { None };
        match ex.stream(cmd, waker) {
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
                self.make = Some(MakeState { job, cmd: cmd.to_string(), root: root.to_path_buf(), output, status: None, pending: Pending::default() });
            }
            Err(e) => self.notify.error(format!("コマンドを起動できません: {e}")),
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
            // 1 行の形 → だめなら複数行の形（rustc・Python）
            let multi = match &mut self.make {
                Some(m) => parse_multi(&mut m.pending, &l, &root),
                None => None,
            };
            match multi.or_else(|| parse_line(&l, &root)) {
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
        if changed {
            self.sync_output_view();
        }
        if let Some(code) = done {
            let (cmd, status) = self.make.take().map(|m| (m.cmd, m.status)).unwrap_or_default();
            let (e, w) = (self.diags.iter().filter(|d| d.error).count(), self.diags.iter().filter(|d| !d.error).count());
            if let Some(g) = self.output_panel_mut() {
                g.running = false;
            }
            // 重さは見つけたものに合わせる（エラーがあれば error、警告だけなら warn）
            let level = if e > 0 { nagi_core::log::Level::Error } else if w > 0 { nagi_core::log::Level::Warn } else { nagi_core::log::Level::Info };
            self.notify.put(level, match (e + w, code) {
                _ if status.is_some() => format!("{}（エラー {e} 件、警告 {w} 件）", status.unwrap_or_default()),
                (0, Some(0)) => format!("診断: 問題はありません（{cmd}）"),
                (0, c) => format!("診断: 診断の形の出力はありませんでした（終了コード {}）", c.map_or("-".into(), |c| c.to_string())),
                _ => format!("診断: エラー {e} 件、警告 {w} 件"),
            });
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
                None => self.notify.error("構文エラーはありません"),
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
            self.notify.info(format!("{}: {}", if d.error { "error" } else { "warning" }, d.msg));
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

    /// `path` のファイルの診断の位置（行, 列（文字）, エラーか）。本文に波線を引く
    pub fn diag_spans(&self, path: Option<&Path>) -> Vec<(usize, usize, bool)> {
        if self.diags.is_empty() {
            return Vec::new();
        }
        let Some(p) = path.and_then(|p| std::path::absolute(p).ok()) else { return Vec::new() };
        self.diags.iter().filter(|d| d.path == p).map(|d| (d.line, d.col, d.error)).collect()
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
            g.view = None;
            g.query.set("");
        }
        self.bottom_kind = crate::result_grid::BottomKind::Output;
        self.set_state(State::Normal);
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
        assert!(e.notify.text.contains("エラー 1 件、警告 1 件"), "{}", e.notify.text);
        // ]e: 次の診断（2 行目の警告）、もう一度で 3 行目のエラー
        e.next_error(true);
        assert_eq!((e.cursor.line, e.cursor.byte), (1, 0));
        assert!(e.notify.text.starts_with("warning"));
        e.next_error(true);
        assert_eq!((e.cursor.line, e.cursor.byte), (2, 3));
        e.notify.clear();
        assert_eq!(e.diag_at_cursor().map(|d| d.msg.as_str()), Some("boom"));
        assert_eq!(e.diag_lines(e.buf.path.as_deref()).len(), 2);
    }

    /// 言語ごとの出力の形（1 行・複数行）
    #[test]
    fn more_formats() {
        let root = Path::new(r"C:\work\p");
        let one = |l: &str| parse_line(l, root).map(|d| (d.path.file_name().unwrap().to_string_lossy().into_owned(), d.line + 1, d.col + 1, d.error));
        assert_eq!(one(r"C:\work\p\a.cpp(12): error C2065: 'x': undeclared identifier"), Some(("a.cpp".into(), 12, 1, true)));
        assert_eq!(one(r"Program.cs(7,13): warning CS0168: The variable 'e' is declared but never used [C:\work\p\p.csproj]"), Some(("Program.cs".into(), 7, 13, false)));
        assert_eq!(one("./main.go:5:2: undefined: foo"), Some(("main.go".into(), 5, 2, true)));
        assert_eq!(one("[error] /work/p/src/A.scala:3:5: not found: value x"), Some(("A.scala".into(), 3, 5, true)));
        assert_eq!(one("ERROR|COMPILE_TIME_ERROR|UNDEFINED_IDENTIFIER|C:\\work\\p\\a.dart|3|5|1|Undefined name 'x'."), Some(("a.dart".into(), 3, 5, true)));
        assert_eq!(one("PHP Parse error:  syntax error, unexpected end of file in C:\\work\\p\\a.php on line 4"), Some(("a.php".into(), 4, 1, true)));
        assert_eq!(one("a.rb:3: syntax error, unexpected end-of-input"), Some(("a.rb".into(), 3, 1, true)));
        assert_eq!(one("src/main.zig:3:5: error: use of undeclared identifier 'x'"), Some(("main.zig".into(), 3, 5, true)));
        assert_eq!(one("bad.c:5:12: note: each undeclared identifier is reported only once"), None, "note は数えない");
        assert_eq!(one("  --> src/main.rs:3:21"), None, "rustc の位置の行は 1 行の形としては読まない");
        // 複数行: rustc
        let mut p = Pending::default();
        assert!(parse_multi(&mut p, "error[E0308]: mismatched types", root).is_none());
        let d = parse_multi(&mut p, "  --> src/main.rs:3:21", root).unwrap();
        assert_eq!((d.line + 1, d.col + 1, d.error, d.msg.as_str()), (3, 21, true, "mismatched types"));
        // 複数行: Python
        assert!(parse_multi(&mut p, r#"  File "C:\work\p\a.py", line 2"#, root).is_none());
        let d = parse_multi(&mut p, "SyntaxError: invalid syntax", root).unwrap();
        assert_eq!((d.path.file_name().unwrap().to_str().unwrap(), d.line + 1, d.col, d.msg.as_str()), ("a.py", 2, 0, "SyntaxError: invalid syntax"));
        // ^ の位置から列（ファイルの行の字下げを足す）
        let py = std::env::temp_dir().join(format!("nagi-py-{}.py", std::process::id()));
        std::fs::write(&py, "def f():\n    x = (1 +\n").unwrap();
        assert!(parse_multi(&mut p, &format!(r#"  File "{}", line 2"#, py.display()), root).is_none());
        assert!(parse_multi(&mut p, "    x = (1 +", root).is_none());
        assert!(parse_multi(&mut p, "        ^", root).is_none());
        let d = parse_multi(&mut p, "SyntaxError: '(' was never closed", root).unwrap();
        assert_eq!((d.line + 1, d.col + 1), (2, 9));
        // 3.11 からの ~~~^^^ の形は ^ の始まりを指す
        assert!(parse_multi(&mut p, &format!(r#"  File "{}", line 2"#, py.display()), root).is_none());
        assert!(parse_multi(&mut p, "    x = (1 +", root).is_none());
        assert!(parse_multi(&mut p, "    ~~~~^^^", root).is_none());
        assert_eq!(parse_multi(&mut p, "ZeroDivisionError: division by zero", root).unwrap().col + 1, 9);
        let _ = std::fs::remove_file(&py);
        // 見分け方
        let dir = std::env::temp_dir().join(format!("nagi-detect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("go.mod"), "module x\n").unwrap();
        assert_eq!(detect_command(&dir, None).as_deref(), Some("go vet ./..."));
        std::fs::remove_file(dir.join("go.mod")).unwrap();
        std::fs::write(dir.join("app.csproj"), "<Project/>").unwrap();
        assert!(detect_command(&dir, None).unwrap().starts_with("dotnet build"));
        std::fs::remove_file(dir.join("app.csproj")).unwrap();
        assert_eq!(detect_command(&dir, Some(&dir.join("a.rb"))).as_deref(), Some(format!("ruby -wc \"{}\"", dir.join("a.rb").display()).as_str()));
    }
}
