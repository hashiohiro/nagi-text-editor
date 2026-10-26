//! プロジェクトの全文検索。`ignore` で `.gitignore` に従ってファイルを並べて歩き、`grep-searcher` で探す。
//! 1 ファイル分の一致がそろうたびに送るので、最初の結果はすぐ届く。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use grep_matcher::Matcher;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::sinks::UTF8;
use grep_searcher::{BinaryDetection, SearcherBuilder};

use crate::Waker;

/// これより多い一致は集めない（メモリと描画を抑える）
pub const MAX_HITS: usize = 20_000;
/// 1 行の本文として持つ最大の byte 数（長い行は切る）
const MAX_TEXT: usize = 400;

/// 1 つの一致
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// 根からの相対パス（区切りは `/`）
    pub path: String,
    /// 1 始まりの行番号
    pub line: usize,
    /// 行の中の一致の範囲（byte。`text` の中の位置）
    pub range: (usize, usize),
    /// 行の本文（行頭の空白を除き、長ければ切る）
    pub text: String,
}

/// 探している途中の検索。落とすと探すのをやめる
pub struct GrepJob {
    /// 見つけたもの（送り口が閉じたら探し終えた）。捨てると探すのをやめる
    pub rx: nagi_core::task::Task<Vec<Hit>>,
}

/// どこで一致したものを数えるか（IntelliJ の Find in Files の「文脈」）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Context {
    #[default]
    Anywhere,
    InComments,
    InStrings,
    ExceptComments,
    ExceptStrings,
    ExceptBoth,
}

impl Context {
    pub const ALL: [Context; 6] = [Context::Anywhere, Context::InComments, Context::InStrings, Context::ExceptComments, Context::ExceptStrings, Context::ExceptBoth];

    pub fn label(self) -> &'static str {
        match self {
            Context::Anywhere => "どこでも",
            Context::InComments => "コメントの中",
            Context::InStrings => "文字列の中",
            Context::ExceptComments => "コメントを除く",
            Context::ExceptStrings => "文字列を除く",
            Context::ExceptBoth => "コメント・文字列を除く",
        }
    }

    /// 次のもの（切り替え）
    pub fn next(self) -> Context {
        let i = Context::ALL.iter().position(|c| *c == self).unwrap_or(0);
        Context::ALL[(i + 1) % Context::ALL.len()]
    }

    fn keeps(self, k: Lex) -> bool {
        match self {
            Context::Anywhere => true,
            Context::InComments => k == Lex::Comment,
            Context::InStrings => k == Lex::Str,
            Context::ExceptComments => k != Lex::Comment,
            Context::ExceptStrings => k != Lex::Str,
            Context::ExceptBoth => k == Lex::Code,
        }
    }
}

/// 検索の条件（IntelliJ の Find in Files と同じもの）
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Opts {
    /// 大文字小文字を区別する（Match case）
    pub case: bool,
    /// 単語として（Words）
    pub words: bool,
    /// 正規表現（Regex）
    pub regex: bool,
    /// ファイルマスク（`*.java, !*Test.java`。空ならすべて。`/` を含むものは根からのパスと比べる）
    pub mask: String,
    /// この中だけを探す（Directory。None ならプロジェクト全体）
    pub dir: Option<PathBuf>,
    pub context: Context,
}

/// `*` と `?` の glob（大文字小文字を区別しない）
fn glob(pat: &str, s: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pat.to_lowercase().chars().collect(), s.to_lowercase().chars().collect());
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(sp) = star {
            pi = sp + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// ファイルマスクに合うか（`rel` は根からの相対パス）
pub fn mask_matches(mask: &str, rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    let test = |p: &str| if p.contains('/') { glob(p.trim_start_matches('/'), rel) || glob(&format!("*/{}", p.trim_start_matches('/')), rel) } else { glob(p, name) };
    let pats: Vec<&str> = mask.split(',').map(str::trim).filter(|p| !p.is_empty()).collect();
    let (neg, pos): (Vec<&str>, Vec<&str>) = pats.iter().partition(|p| p.starts_with('!'));
    if neg.iter().any(|p| test(&p[1..])) {
        return false;
    }
    pos.is_empty() || pos.iter().any(|p| test(p))
}

/// 字句の種類（文脈で絞るため）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lex {
    Code,
    Comment,
    Str,
}

/// 拡張子ごとのコメント・文字列の書き方
struct Syntax {
    line: &'static [&'static str],
    block: &'static [(&'static str, &'static str)],
    quotes: &'static [char],
    triple: bool,
}

fn syntax_for(rel: &str) -> Option<Syntax> {
    let name = rel.rsplit('/').next().unwrap_or(rel).to_ascii_lowercase();
    let ext = name.rsplit_once('.').map_or(name.as_str(), |x| x.1);
    Some(match ext {
        "java" | "kt" | "kts" | "scala" | "groovy" | "gradle" | "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" | "c" | "h" | "cpp" | "hpp" | "cc" | "cs" | "go" | "swift" | "dart" | "php" | "scss" | "less" | "jsonc" | "json5" | "apex" | "cls" | "trigger" => Syntax { line: &["//"], block: &[("/*", "*/")], quotes: &['"', '\'', '`'], triple: ext == "kt" || ext == "java" },
        // Rust の ' は寿命のことがあるので文字列にしない
        "rs" => Syntax { line: &["//"], block: &[("/*", "*/")], quotes: &['"'], triple: false },
        "css" => Syntax { line: &[], block: &[("/*", "*/")], quotes: &['"', '\''], triple: false },
        "py" | "pyi" => Syntax { line: &["#"], block: &[], quotes: &['"', '\''], triple: true },
        "sh" | "bash" | "zsh" | "yml" | "yaml" | "toml" | "rb" | "pl" | "r" | "properties" | "conf" | "cfg" | "dockerfile" | "mk" | "ps1" => Syntax { line: &["#"], block: &[], quotes: &['"', '\''], triple: false },
        "sql" | "soql" | "pks" | "pkb" => Syntax { line: &["--"], block: &[("/*", "*/")], quotes: &['\'', '"'], triple: false },
        "xml" | "html" | "htm" | "vue" | "svelte" | "jsp" | "xhtml" | "fxml" | "pom" => Syntax { line: &[], block: &[("<!--", "-->")], quotes: &[], triple: false },
        _ if name == "makefile" || name == "dockerfile" => Syntax { line: &["#"], block: &[], quotes: &['"', '\''], triple: false },
        _ => return None,
    })
}

/// ファイルの中のコメント・文字列の範囲（byte。始めの順）
fn lex_ranges(text: &str, syn: &Syntax) -> Vec<(usize, usize, Lex)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    'outer: while i < b.len() {
        let rest = &text[i..];
        for l in syn.line {
            if rest.starts_with(l) {
                let end = rest.find('\n').map_or(text.len(), |k| i + k);
                out.push((i, end, Lex::Comment));
                i = end;
                continue 'outer;
            }
        }
        for (open, close) in syn.block {
            if rest.starts_with(open) {
                let end = rest[open.len()..].find(close).map_or(text.len(), |k| i + open.len() + k + close.len());
                out.push((i, end, Lex::Comment));
                i = end;
                continue 'outer;
            }
        }
        let c = b[i] as char;
        if syn.quotes.contains(&c) {
            let q3: String = std::iter::repeat_n(c, 3).collect();
            let end = if syn.triple && c != '`' && rest.starts_with(&q3) {
                rest[3..].find(&q3).map_or(text.len(), |k| i + 3 + k + 3)
            } else {
                // \ の後は飛ばす。` 以外は行の終わりまで
                let mut j = i + 1;
                loop {
                    match b.get(j) {
                        None => break text.len(),
                        Some(b'\\') => j += 2,
                        Some(&x) if x as char == c => break j + 1,
                        Some(b'\n') if c != '`' => break j,
                        _ => j += 1,
                    }
                }
            };
            let mut end = end.min(text.len());
            while !text.is_char_boundary(end) {
                end += 1;
            }
            out.push((i, end, Lex::Str));
            i = end.max(i + 1);
            continue;
        }
        i += rest.chars().next().map_or(1, char::len_utf8);
    }
    out
}

fn lex_at(ranges: &[(usize, usize, Lex)], at: usize) -> Lex {
    let k = ranges.partition_point(|r| r.0 <= at);
    match k.checked_sub(1).map(|k| ranges[k]) {
        Some((s, e, kind)) if s <= at && at < e => kind,
        _ => Lex::Code,
    }
}

/// `root` 以下で `pattern` を探す。大文字小文字は区別しない。`regex` でなければ文字どおりに探す。
/// パターンが正しくなければ Err（正規表現の誤り）
pub fn search(root: &Path, pattern: &str, regex: bool, waker: Waker) -> nagi_core::Result<GrepJob> {
    search_with(root, pattern, &Opts { regex, ..Default::default() }, waker)
}

/// 条件を付けて探す（パスは `root` からの相対。`opts.dir` があればその中だけ）
pub fn search_with(root: &Path, pattern: &str, opts: &Opts, waker: Waker) -> nagi_core::Result<GrepJob> {
    let matcher = RegexMatcherBuilder::new().case_insensitive(!opts.case).word(opts.words).fixed_strings(!opts.regex).build(pattern).map_err(|e| e.to_string())?;
    let root: PathBuf = root.to_path_buf();
    let base = opts.dir.clone().unwrap_or_else(|| root.clone());
    let (mask, context) = (opts.mask.clone(), opts.context);
    let rx = nagi_core::task::spawn("grep", waker, move |ctx| {
        let (tx, waker, stop) = (ctx.sender(), ctx.waker(), ctx.cancel_flag());
        let count = Arc::new(AtomicUsize::new(0));
        let walker = crate::files::walker(&base).build_parallel();
        walker.run(|| {
            let matcher = matcher.clone();
            let tx = tx.clone();
            let waker = waker.clone();
            let stop = stop.clone();
            let count = count.clone();
            let root = root.clone();
            let mask = mask.clone();
            let mut searcher = SearcherBuilder::new().binary_detection(BinaryDetection::quit(0)).line_number(true).build();
            Box::new(move |entry| {
                if stop.load(Ordering::Relaxed) || count.load(Ordering::Relaxed) >= MAX_HITS {
                    return ignore::WalkState::Quit;
                }
                let Ok(e) = entry else { return ignore::WalkState::Continue };
                if !e.file_type().is_some_and(|t| t.is_file()) {
                    return ignore::WalkState::Continue;
                }
                let rel = e.path().strip_prefix(&root).unwrap_or(e.path()).to_string_lossy().replace('\\', "/");
                if !mask.is_empty() && !mask_matches(&mask, &rel) {
                    return ignore::WalkState::Continue;
                }
                if context != Context::Anywhere {
                    let mut hits = Vec::new();
                    // 文脈で絞る: ファイル全体のコメント・文字列を見て、その行で条件に合う最初の一致
                    let Some(syn) = syntax_for(&rel) else {
                        // 書き方の分からないファイルはすべて本文として扱う
                        if !context.keeps(Lex::Code) {
                            return ignore::WalkState::Continue;
                        }
                        return search_plain(&mut searcher, &matcher, e.path(), &rel, &stop, &count, &tx, &waker);
                    };
                    let Ok(text) = std::fs::read_to_string(e.path()) else { return ignore::WalkState::Continue };
                    if matcher.find(text.as_bytes()).ok().flatten().is_none() {
                        return ignore::WalkState::Continue;
                    }
                    let ranges = lex_ranges(&text, &syn);
                    let mut start = 0;
                    for (no, raw) in text.split_inclusive('\n').enumerate() {
                        let line = raw.trim_end_matches(['\r', '\n']);
                        let lead = line.len() - line.trim_start().len();
                        let body = &line[lead..];
                        let mut first = None;
                        let _ = matcher.find_iter(body.as_bytes(), |m| {
                            if context.keeps(lex_at(&ranges, start + lead + m.start())) {
                                first = Some(m);
                                return false;
                            }
                            true
                        });
                        if let Some(m) = first {
                            let mut cut = body.len().min(MAX_TEXT);
                            while !body.is_char_boundary(cut) {
                                cut -= 1;
                            }
                            hits.push(Hit { path: rel.clone(), line: no + 1, range: (m.start().min(cut), m.end().min(cut)), text: body[..cut].to_string() });
                        }
                        start += raw.len();
                    }
                    return send_hits(hits, &count, &tx, &waker);
                }
                search_plain(&mut searcher, &matcher, e.path(), &rel, &stop, &count, &tx, &waker)
            })
        });
        // 送り口を閉じて起こすのは task が行う（受け取る側が「終わった」と分かる）
        drop(tx);
    });
    Ok(GrepJob { rx })
}

fn send_hits(hits: Vec<Hit>, count: &AtomicUsize, tx: &std::sync::mpsc::Sender<Vec<Hit>>, waker: &Waker) -> ignore::WalkState {
    if !hits.is_empty() {
        count.fetch_add(hits.len(), Ordering::Relaxed);
        if tx.send(hits).is_err() {
            return ignore::WalkState::Quit;
        }
        waker();
    }
    ignore::WalkState::Continue
}

/// 1 ファイルを行ごとに探す（文脈で絞らないとき）
#[allow(clippy::too_many_arguments)]
fn search_plain(searcher: &mut grep_searcher::Searcher, matcher: &grep_regex::RegexMatcher, path: &Path, rel: &str, stop: &std::sync::atomic::AtomicBool, count: &AtomicUsize, tx: &std::sync::mpsc::Sender<Vec<Hit>>, waker: &Waker) -> ignore::WalkState {
    let mut hits = Vec::new();
    let _ = searcher.search_path(
        matcher,
        path,
        UTF8(|line_no, line| {
            let line = line.trim_end_matches(['\r', '\n']);
            let lead = line.len() - line.trim_start().len();
            let body = &line[lead..];
            let Ok(Some(m)) = matcher.find(body.as_bytes()) else { return Ok(true) };
            let mut cut = body.len().min(MAX_TEXT);
            while !body.is_char_boundary(cut) {
                cut -= 1;
            }
            let range = (m.start().min(cut), m.end().min(cut));
            hits.push(Hit { path: rel.to_string(), line: line_no as usize, range, text: body[..cut].to_string() });
            Ok(!stop.load(Ordering::Relaxed))
        }),
    );
    send_hits(hits, count, tx, waker)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_case_insensitive_and_literal() {
        let dir = std::env::temp_dir().join(format!("nagi-grep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join(".gitignore"), "out/\n").unwrap();
        std::fs::create_dir_all(dir.join("out")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn main() {\n    let Foo = 1;\n}\n").unwrap();
        std::fs::write(dir.join("src/b.txt"), "日本語の foo.bar\n").unwrap();
        std::fs::write(dir.join("out/c.rs"), "foo\n").unwrap();
        let job = search(&dir, "foo", false, Arc::new(|| {})).unwrap();
        let mut hits: Vec<Hit> = job.rx.iter().flatten().collect();
        hits.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(hits.len(), 2);
        assert_eq!((hits[0].path.as_str(), hits[0].line, hits[0].text.as_str(), hits[0].range), ("src/a.rs", 2, "let Foo = 1;", (4, 7)));
        assert_eq!(hits[1].text, "日本語の foo.bar");
        // 文字どおり: `.` は任意の 1 文字ではない
        let job = search(&dir, "o.b", false, Arc::new(|| {})).unwrap();
        assert_eq!(job.rx.iter().flatten().count(), 1);
        let job = search(&dir, "f.o", true, Arc::new(|| {})).unwrap();
        assert_eq!(job.rx.iter().flatten().count(), 2);
        assert!(search(&dir, "(", true, Arc::new(|| {})).is_err());
    }

    #[test]
    fn options_like_intellij() {
        let dir = std::env::temp_dir().join(format!("nagi-grep-opts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/sub")).unwrap();
        std::fs::write(dir.join("src/A.java"), "class A {\n  // Foo in comment\n  String s = \"Foo in string\";\n  Foo foo = new Foo(); /* Foo */\n  int Foobar;\n}\n").unwrap();
        std::fs::write(dir.join("src/ATest.java"), "Foo\n").unwrap();
        std::fs::write(dir.join("src/sub/b.txt"), "foo\n").unwrap();
        let run = |pat: &str, o: Opts| -> Vec<(String, usize, String)> {
            let job = search_with(&dir, pat, &o, Arc::new(|| {})).unwrap();
            let mut v: Vec<(String, usize, String)> = job.rx.iter().flatten().map(|h| (h.path.clone(), h.line, h.text[h.range.0..h.range.1].to_string())).collect();
            v.sort();
            v
        };
        let lines = |v: Vec<(String, usize, String)>| v.into_iter().map(|x| format!("{}:{}", x.0, x.1)).collect::<Vec<_>>();
        // 大文字小文字・単語
        assert_eq!(run("foo", Opts { case: true, ..Default::default() }).len(), 2, "foo（A.java の foo と b.txt）");
        assert_eq!(lines(run("Foo", Opts { case: true, words: true, ..Default::default() })), ["src/A.java:2", "src/A.java:3", "src/A.java:4", "src/ATest.java:1"]);
        // ファイルマスク・ディレクトリ
        assert_eq!(lines(run("foo", Opts { mask: "*.java, !*Test.java".into(), ..Default::default() })).len(), 4);
        assert_eq!(lines(run("foo", Opts { dir: Some(dir.join("src/sub")), ..Default::default() })), ["src/sub/b.txt:1"]);
        // 文脈
        assert_eq!(lines(run("Foo", Opts { case: true, words: true, mask: "A.java".into(), context: Context::InComments, ..Default::default() })), ["src/A.java:2", "src/A.java:4"]);
        assert_eq!(lines(run("Foo", Opts { case: true, words: true, mask: "A.java".into(), context: Context::InStrings, ..Default::default() })), ["src/A.java:3"]);
        assert_eq!(lines(run("Foo", Opts { case: true, words: true, mask: "A.java".into(), context: Context::ExceptBoth, ..Default::default() })), ["src/A.java:4"]);
        // 4 行目はコメントの前の Foo が一致
        assert_eq!(run("Foo", Opts { case: true, words: true, mask: "A.java".into(), context: Context::InComments, ..Default::default() })[1].2, "Foo");
        assert!(mask_matches("src/*.java", "src/A.java") && !mask_matches("*.ts", "a.java") && mask_matches("", "x"));
    }
}
