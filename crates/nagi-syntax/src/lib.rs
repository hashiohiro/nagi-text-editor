//! tree-sitter による構文解析とハイライト（architecture.md §5.3、ADR 0008）。
//!
//! - 構文木はバッファごとに 1 つ持ち、編集はインクリメンタルに反映する（`Tree::edit` → 再パース）。
//! - ハイライトは表示範囲だけにクエリをかける（`QueryCursor::set_byte_range`）。
//! - クエリは言語ごとに 1 回だけコンパイルして使い回す。

use std::ops::Range;
use std::path::Path;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

pub mod defs;
pub mod grammar;
pub mod markdown;
pub mod sass;

use nagi_core::{Buffer, EditLog};
use tree_sitter::{InputEdit, Language, Node, Parser, Point, Query, QueryCursor, StreamingIterator, Tree};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Rust,
    TypeScript,
    Tsx,
    JavaScript,
    Java,
    Kotlin,
    Python,
    Bash,
    Batch,
    PowerShell,
    Ini,
    Json,
    Yaml,
    Vim,
    Css,
    Scss,
    Html,
    C,
    Cpp,
    Go,
    CSharp,
    Php,
    Ruby,
    Lua,
    Scala,
    Swift,
    Dart,
    Zig,
    Haskell,
    Elixir,
    R,
    Sql,
    Toml,
    Xml,
    Make,
    CMake,
    Dockerfile,
    Groovy,
}

/// 言語の表（1 行に 1 言語。並びは enum Lang と同じ）。言語を足すときはここと enum に 1 行ずつ
pub(crate) const LANGS: [Spec; 38] = [
    Spec { lang: Lang::Rust, name: "Rust", exts: &["rs"], files: &[], source: Source::Download("rust"), query: include_str!("../queries/rust/highlights.scm"), fences: &["rust", "rs"] },
    Spec { lang: Lang::TypeScript, name: "TypeScript", exts: &["ts", "mts", "cts"], files: &[], source: Source::Download("typescript"), query: include_str!("../queries/typescript/highlights.scm"), fences: &["ts", "typescript"] },
    Spec { lang: Lang::Tsx, name: "TSX", exts: &["tsx"], files: &[], source: Source::Download("tsx"), query: include_str!("../queries/tsx/highlights.scm"), fences: &["tsx"] },
    Spec { lang: Lang::JavaScript, name: "JavaScript", exts: &["js", "mjs", "cjs", "jsx"], files: &[], source: Source::Download("javascript"), query: include_str!("../queries/javascript/highlights.scm"), fences: &["js", "javascript", "jsx", "mjs"] },
    Spec { lang: Lang::Java, name: "Java", exts: &["java"], files: &[], source: Source::Download("java"), query: include_str!("../queries/java/highlights.scm"), fences: &["java"] },
    Spec { lang: Lang::Kotlin, name: "Kotlin", exts: &["kt", "kts"], files: &[], source: Source::Download("kotlin"), query: include_str!("../queries/kotlin/highlights.scm"), fences: &["kotlin", "kt", "kts"] },
    Spec { lang: Lang::Python, name: "Python", exts: &["py", "pyw", "pyi"], files: &[], source: Source::Download("python"), query: include_str!("../queries/python/highlights.scm"), fences: &["python", "py"] },
    Spec { lang: Lang::Bash, name: "Bash", exts: &["sh", "bash"], files: &[".bashrc", ".bash_profile", ".profile", ".zshrc"], source: Source::Builtin(tree_sitter_bash::LANGUAGE), query: tree_sitter_bash::HIGHLIGHT_QUERY, fences: &["sh", "bash", "shell", "zsh", "console"] },
    Spec { lang: Lang::Batch, name: "Batch", exts: &["bat", "cmd"], files: &[], source: Source::Builtin(tree_sitter_batch::LANGUAGE), query: tree_sitter_batch::HIGHLIGHTS_QUERY, fences: &["bat", "cmd", "batch"] },
    Spec { lang: Lang::PowerShell, name: "PowerShell", exts: &["ps1", "psm1", "psd1"], files: &[], source: Source::Builtin(tree_sitter_powershell::LANGUAGE), query: tree_sitter_powershell::HIGHLIGHTS_QUERY, fences: &["powershell", "ps1", "pwsh"] },
    Spec { lang: Lang::Ini, name: "INI", exts: &["ini", "cfg", "conf", "properties"], files: &[".gitconfig", ".editorconfig", ".npmrc"], source: Source::Builtin(tree_sitter_ini::LANGUAGE), query: tree_sitter_ini::HIGHLIGHTS_QUERY, fences: &["ini", "properties"] },
    Spec { lang: Lang::Json, name: "JSON", exts: &["json", "jsonc", "json5"], files: &[], source: Source::Builtin(tree_sitter_json::LANGUAGE), query: tree_sitter_json::HIGHLIGHTS_QUERY, fences: &["json", "jsonc"] },
    Spec { lang: Lang::Yaml, name: "YAML", exts: &["yml", "yaml"], files: &[], source: Source::Builtin(tree_sitter_yaml::LANGUAGE), query: tree_sitter_yaml::HIGHLIGHTS_QUERY, fences: &["yaml", "yml"] },
    Spec { lang: Lang::Vim, name: "Vim", exts: &["vim"], files: &[".vimrc", "_vimrc", ".ideavimrc", "_ideavimrc", ".gvimrc", "nagi.vim"], source: Source::Download("vim"), query: include_str!("../queries/vim/highlights.scm"), fences: &["vim"] },
    Spec { lang: Lang::Css, name: "CSS", exts: &["css"], files: &[], source: Source::Download("css"), query: include_str!("../queries/css/highlights.scm"), fences: &["css"] },
    Spec { lang: Lang::Scss, name: "SCSS", exts: &["scss"], files: &[], source: Source::Download("scss"), query: include_str!("../queries/scss/highlights.scm"), fences: &["scss"] },
    Spec { lang: Lang::Html, name: "HTML", exts: &["html", "htm", "xhtml"], files: &[], source: Source::Download("html"), query: include_str!("../queries/html/highlights.scm"), fences: &["html"] },
    Spec { lang: Lang::C, name: "C", exts: &["c"], files: &[], source: Source::Download("c"), query: include_str!("../queries/c/highlights.scm"), fences: &["c", "h"] },
    Spec { lang: Lang::Cpp, name: "C++", exts: &["h", "cpp", "cc", "cxx", "c++", "hpp", "hh", "hxx", "h++", "ino", "ipp", "tpp"], files: &[], source: Source::Download("cpp"), query: include_str!("../queries/cpp/highlights.scm"), fences: &["cpp", "c++", "cc", "cxx", "hpp"] },
    Spec { lang: Lang::Go, name: "Go", exts: &["go"], files: &[], source: Source::Download("go"), query: include_str!("../queries/go/highlights.scm"), fences: &["go", "golang"] },
    Spec { lang: Lang::CSharp, name: "C#", exts: &["cs", "csx"], files: &[], source: Source::Download("csharp"), query: include_str!("../queries/csharp/highlights.scm"), fences: &["cs", "csharp", "c#"] },
    Spec { lang: Lang::Php, name: "PHP", exts: &["php", "phtml", "php8"], files: &[], source: Source::Download("php"), query: include_str!("../queries/php/highlights.scm"), fences: &["php"] },
    Spec { lang: Lang::Ruby, name: "Ruby", exts: &["rb", "rake", "gemspec", "ru"], files: &["gemfile", "rakefile", "podfile", "vagrantfile"], source: Source::Download("ruby"), query: include_str!("../queries/ruby/highlights.scm"), fences: &["ruby", "rb"] },
    Spec { lang: Lang::Lua, name: "Lua", exts: &["lua"], files: &[], source: Source::Download("lua"), query: include_str!("../queries/lua/highlights.scm"), fences: &["lua"] },
    Spec { lang: Lang::Scala, name: "Scala", exts: &["scala", "sc", "sbt"], files: &[], source: Source::Download("scala"), query: include_str!("../queries/scala/highlights.scm"), fences: &["scala"] },
    Spec { lang: Lang::Swift, name: "Swift", exts: &["swift"], files: &[], source: Source::Download("swift"), query: include_str!("../queries/swift/highlights.scm"), fences: &["swift"] },
    Spec { lang: Lang::Dart, name: "Dart", exts: &["dart"], files: &[], source: Source::Download("dart"), query: include_str!("../queries/dart/highlights.scm"), fences: &["dart"] },
    Spec { lang: Lang::Zig, name: "Zig", exts: &["zig", "zon"], files: &[], source: Source::Download("zig"), query: include_str!("../queries/zig/highlights.scm"), fences: &["zig"] },
    Spec { lang: Lang::Haskell, name: "Haskell", exts: &["hs", "lhs"], files: &[], source: Source::Download("haskell"), query: include_str!("../queries/haskell/highlights.scm"), fences: &["haskell", "hs"] },
    Spec { lang: Lang::Elixir, name: "Elixir", exts: &["ex", "exs", "heex"], files: &[], source: Source::Download("elixir"), query: include_str!("../queries/elixir/highlights.scm"), fences: &["elixir", "ex", "exs"] },
    Spec { lang: Lang::R, name: "R", exts: &["r"], files: &[], source: Source::Download("r"), query: include_str!("../queries/r/highlights.scm"), fences: &["r"] },
    Spec { lang: Lang::Sql, name: "SQL", exts: &["sql", "ddl", "dml"], files: &[], source: Source::Download("sql"), query: include_str!("../queries/sql/highlights.scm"), fences: &["sql", "mysql", "postgresql", "postgres", "plsql"] },
    Spec { lang: Lang::Toml, name: "TOML", exts: &["toml"], files: &[], source: Source::Download("toml"), query: include_str!("../queries/toml/highlights.scm"), fences: &["toml"] },
    Spec { lang: Lang::Xml, name: "XML", exts: &["xml", "xsd", "xsl", "xslt", "svg", "csproj", "fsproj", "vbproj", "props", "targets", "fxml", "plist", "xaml", "wsdl"], files: &[], source: Source::Builtin(tree_sitter_xml::LANGUAGE_XML), query: include_str!("../queries/xml/highlights.scm"), fences: &["xml", "svg", "xaml"] },
    Spec { lang: Lang::Make, name: "Makefile", exts: &["mk", "mak"], files: &["makefile", "gnumakefile"], source: Source::Download("make"), query: include_str!("../queries/make/highlights.scm"), fences: &["make", "makefile"] },
    Spec { lang: Lang::CMake, name: "CMake", exts: &["cmake"], files: &["cmakelists.txt"], source: Source::Download("cmake"), query: include_str!("../queries/cmake/highlights.scm"), fences: &["cmake"] },
    Spec { lang: Lang::Dockerfile, name: "Dockerfile", exts: &["dockerfile"], files: &["dockerfile", "containerfile"], source: Source::Download("dockerfile"), query: include_str!("../queries/dockerfile/highlights.scm"), fences: &["dockerfile", "docker"] },
    Spec { lang: Lang::Groovy, name: "Groovy", exts: &["groovy", "gradle", "gvy", "gy"], files: &["jenkinsfile"], source: Source::Download("groovy"), query: include_str!("../queries/groovy/highlights.scm"), fences: &["groovy", "gradle"] },
];

/// 1 つの言語
pub(crate) struct Spec {
    pub lang: Lang,
    /// 画面に出す名前
    pub name: &'static str,
    /// 拡張子（小文字）
    pub exts: &'static [&'static str],
    /// 拡張子でなくファイル名で決まるもの（小文字）
    pub files: &'static [&'static str],
    pub source: Source,
    /// 色分けの規則（tree-sitter のクエリ）
    pub query: &'static str,
    /// Markdown のコードブロックの言語名（小文字）
    pub fences: &'static [&'static str],
}

/// 文法の在りか
pub(crate) enum Source {
    /// exe に入っている
    Builtin(tree_sitter_language::LanguageFn),
    /// 使うときにダウンロードする（grammar.rs の名前）
    Download(&'static str),
}




impl Lang {
    pub fn detect(path: &Path) -> Option<Lang> {
        // 拡張子ではなくファイル名で決まるもの
        let name = path.file_name()?.to_str()?.to_ascii_lowercase();
        if let Some(s) = LANGS.iter().find(|s| s.files.contains(&name.as_str())) {
            return Some(s.lang);
        }
        if name.starts_with("dockerfile.") {
            return Some(Lang::Dockerfile);
        }
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        LANGS.iter().find(|s| s.exts.contains(&ext.as_str())).map(|s| s.lang)
    }

    /// 表の行
    fn spec(self) -> &'static Spec {
        &LANGS[self as usize]
    }

    /// Markdown のコードブロックの言語名から
    pub fn from_fence(name: &str) -> Option<Lang> {
        let n = name.to_ascii_lowercase();
        LANGS.iter().find(|s| s.fences.contains(&n.as_str())).map(|s| s.lang)
    }

    pub fn name(self) -> &'static str {
        self.spec().name
    }

    /// 設定・データの言語（キーを色分けする）
    fn is_data(self) -> bool {
        matches!(self, Lang::Ini | Lang::Json | Lang::Yaml | Lang::Vim)
    }

    /// 同じノードに複数のキャプチャがあるとき、後に書かれたものを優先する（Neovim 流のクエリ）。
    /// tree-sitter 本家の grammar は先に書かれたものを優先する
    fn last_wins(self) -> bool {
        matches!(self, Lang::Yaml | Lang::Vim | Lang::Ini)
    }

    fn is_css(self) -> bool {
        matches!(self, Lang::Css | Lang::Scss)
    }

    /// シェル（$変数 を色分けする）
    fn is_shell(self) -> bool {
        matches!(self, Lang::Bash | Lang::Batch | Lang::PowerShell)
    }

    /// 対応している言語すべて
    pub fn all() -> Vec<Lang> {
        LANGS.iter().map(|s| s.lang).collect()
    }

    /// 文法をダウンロードする言語の名前（grammar.rs。exe に入っている言語は None）
    pub fn grammar_id(self) -> Option<&'static str> {
        match self.spec().source {
            Source::Download(id) => Some(id),
            Source::Builtin(_) => None,
        }
    }

    /// 文法を使える（exe に入っているか、ダウンロード済み）
    pub fn available(self) -> bool {
        self.grammar_id().is_none_or(grammar::installed)
    }

    /// tree-sitter の言語。ダウンロードする言語で、まだ入っていなければ None
    pub fn language(self) -> Option<Language> {
        match self.spec().source {
            Source::Builtin(f) => Some(f.into()),
            Source::Download(id) => grammar::load(id),
        }
    }

    /// 言語ごとに 1 回だけコンパイルしたクエリ
    pub fn highlights(self) -> &'static Result<Query, String> {
        static Q: [OnceLock<Result<Query, String>>; LANGS.len()] = [const { OnceLock::new() }; LANGS.len()];
        static MISSING: OnceLock<Result<Query, String>> = OnceLock::new();
        // 文法がまだ無ければ覚えない（ダウンロードした後に作る）
        let Some(language) = self.language() else {
            return MISSING.get_or_init(|| Err("文法がまだダウンロードされていません".into()));
        };
        Q[self as usize].get_or_init(|| Query::new(&language, self.spec().query).map_err(|e| format!("{e}")))
    }
}

/// ハイライトの種類。色数を絞るため、キャプチャ名をこの数種類にまとめる
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hl {
    Keyword,
    String,
    Comment,
    Type,
    Number,
    Function,
    Attribute,
    Escape,
    /// 変数・引数
    Variable,
    /// プロパティ・フィールド・定数
    Property,
}

fn classify(capture: &str, lang: Lang) -> Option<Hl> {
    let head = capture.split('.').next().unwrap_or(capture);
    Some(match head {
        // 設定・データの言語ではキーを、シェルでは $変数 を色分けする（プログラミング言語では地の色のまま）
        "property" | "field" | "tag" if lang.is_data() => Hl::Keyword,
        // CSS: プロパティ（color: など）は属性の色、タグ（div）は青、$変数・--変数は型の色、@規則はキーワード
        "property" if lang.is_css() => Hl::Attribute,
        "tag" | "mixin" | "include" | "extend" | "use" | "forward" | "for" | "while" | "each" | "if" | "else" | "return" | "warn" | "error" | "debug" if lang.is_css() => Hl::Keyword,
        "variable" if lang.is_css() => Hl::Type,
        // HTML: タグは青、属性の名前は属性の色、<!DOCTYPE> は控えめに
        "tag" if lang == Lang::Html => Hl::Keyword,
        "constant" if lang == Lang::Html => Hl::Comment,
        // JSON はキーを string.special.key として取り出す
        _ if lang.is_data() && capture.split('.').any(|p| p == "key") => Hl::Keyword,
        "variable" if lang.is_shell() && !capture.contains("builtin") => Hl::Type,
        // this・super・self は予約語の色、ほかの変数・引数は変数の色
        "variable" if capture.contains("builtin") => Hl::Keyword,
        "variable" | "parameter" => Hl::Variable,
        "property" | "field" => Hl::Property,
        "constant" if !capture.contains("builtin") => Hl::Property,
        "keyword" | "conditional" | "repeat" | "include" | "exception" => Hl::Keyword,
        "string" | "character" => {
            if capture.contains("escape") {
                Hl::Escape
            } else {
                Hl::String
            }
        }
        "escape" => Hl::Escape,
        "comment" => Hl::Comment,
        "type" | "constructor" => Hl::Type,
        "number" | "float" | "boolean" => Hl::Number,
        "constant" if capture.contains("builtin") => Hl::Number,
        "function" | "method" => Hl::Function,
        "attribute" | "decorator" | "annotation" | "label" => Hl::Attribute,
        // JSX / HTML のタグ（<div> など。部品の <MyButton> は型の色になる）
        "tag" => Hl::Keyword,
        _ => return None,
    })
}

pub struct Syntax {
    pub lang: Lang,
    /// 裏で再解析している間は、そのスレッドへ渡している
    parser: Option<Parser>,
    tree: Option<Tree>,
    /// 編集を反映したが、まだ再解析していない（木の位置だけずらしてある）
    stale: bool,
    /// 裏で走っている再解析と、その解析を始めた後の編集（届いた木にも反映する）
    job: Option<(Receiver<(Parser, Option<Tree>, Duration)>, Vec<InputEdit>)>,
    /// 直前の解析にかかった時間。短ければその場で、長ければ裏で再解析する
    last_parse: Duration,
    /// 裏の再解析が終わったときに UI スレッドを起こす
    pub waker: Option<Arc<dyn Fn() + Send + Sync>>,
    /// HTML の <script> <style> の中のハイライトの覚え（中身 → 中身の中の範囲）
    injected: std::sync::Mutex<Vec<(Lang, String, Vec<(Range<usize>, Hl)>)>>,
}

// Parser と Tree は別スレッドへ移せる（初回のパースを読み込みスレッドで行う）
unsafe impl Send for Syntax {}

/// これより速く解析できるファイルは、編集のたびにその場で再解析する（色がずれる瞬間を作らない）
const SYNC_PARSE_LIMIT: Duration = Duration::from_millis(3);

fn point((row, column): (usize, usize)) -> Point {
    Point { row, column }
}

fn parse_with(parser: &mut Parser, buf: &Buffer, old: Option<&Tree>) -> Option<Tree> {
    parser.parse_with_options(&mut |byte, _| buf.chunk_from(byte), old, None)
}

impl Syntax {
    pub fn new(lang: Lang) -> Option<Self> {
        let mut parser = Parser::new();
        parser.set_language(&lang.language()?).ok()?;
        Some(Self { lang, parser: Some(parser), tree: None, stale: false, job: None, last_parse: Duration::ZERO, waker: None, injected: Default::default() })
    }

    /// バッファ全体をその場でパースする（初回）
    pub fn parse(&mut self, buf: &mut Buffer) {
        for e in buf.take_edits() {
            if let Some(t) = &mut self.tree {
                t.edit(&input_edit(e));
            }
        }
        let Some(parser) = &mut self.parser else { return };
        let t = Instant::now();
        self.tree = parse_with(parser, buf, self.tree.as_ref());
        self.last_parse = t.elapsed();
        self.stale = false;
    }

    /// 編集を構文木に反映する。戻り値は色が変わりうるか（描き直す）。
    ///
    /// 解析が速いファイルはその場で再解析する。遅いファイル（数十〜数百 ms。同じ階層に要素が非常に多いと、
    /// 差分解析でも全体に比例して時間がかかる）は、木の位置だけずらして裏で再解析し、届いたら差し替える。
    pub fn sync(&mut self, buf: &mut Buffer) -> bool {
        let mut changed = self.receive();
        let edits = buf.take_edits();
        if !edits.is_empty() {
            for e in edits {
                let ie = input_edit(e);
                if let Some(t) = &mut self.tree {
                    t.edit(&ie);
                }
                if let Some((_, since)) = &mut self.job {
                    since.push(ie);
                }
            }
            self.stale = true;
            changed = true;
        }
        if self.stale && self.job.is_none() {
            if self.last_parse < SYNC_PARSE_LIMIT || self.waker.is_none() {
                self.parse(buf);
            } else {
                self.spawn_parse(buf);
            }
        }
        changed
    }

    fn spawn_parse(&mut self, buf: &Buffer) {
        let Some(mut parser) = self.parser.take() else { return };
        let (old, snap, waker) = (self.tree.clone(), buf.snapshot(), self.waker.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        self.stale = false;
        self.job = Some((rx, Vec::new()));
        std::thread::spawn(move || {
            let t = Instant::now();
            let tree = parse_with(&mut parser, &snap, old.as_ref());
            if tx.send((parser, tree, t.elapsed())).is_ok() {
                if let Some(w) = waker {
                    w();
                }
            }
        });
    }

    /// 裏の再解析が終わっていれば受け取る
    fn receive(&mut self) -> bool {
        let Some((rx, _)) = &self.job else { return false };
        let Ok((parser, tree, took)) = rx.try_recv() else { return false };
        let (_, since) = self.job.take().unwrap();
        self.parser = Some(parser);
        self.last_parse = took;
        if let Some(mut t) = tree {
            for ie in &since {
                t.edit(ie);
            }
            self.tree = Some(t);
        }
        // 解析中に入った編集は、届いた木ではまだ位置をずらしただけ
        self.stale |= !since.is_empty();
        true
    }

    /// 裏で再解析中か（テスト用）
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    /// `range`（絶対 byte）にかかるハイライト。返す範囲は重ならないよう、後から来た内側のキャプチャで上書きした結果
    pub fn highlight(&self, buf: &Buffer, range: Range<usize>) -> Vec<(Range<usize>, Hl)> {
        let (Some(tree), Ok(query)) = (&self.tree, self.lang.highlights()) else { return Vec::new() };
        let names = query.capture_names();
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(range.clone());
        let text = |node: Node| buf.chunks_in(node.start_byte(), node.end_byte());
        let mut caps = cursor.captures(query, tree.root_node(), text);
        let mut out: Vec<(Range<usize>, Hl)> = Vec::new();
        while let Some((m, i)) = caps.next() {
            let c = m.captures()[*i];
            let Some(hl) = classify(names[c.index as usize], self.lang) else { continue };
            let r = c.node.start_byte()..c.node.end_byte();
            // 同じノードに複数のキャプチャがあるとき、どちらを残すかは grammar のクエリの書き方による
            if let Some(last) = out.last_mut().filter(|(lr, _)| *lr == r) {
                // 変数・プロパティは汎用の規則の色なので、関数・型などの具体的な色が当たればそちらを使う
                let weak = |h: Hl| matches!(h, Hl::Variable | Hl::Property);
                if self.lang.last_wins() || (weak(last.1) && !weak(hl)) {
                    last.1 = hl;
                }
                continue;
            }
            out.push((r, hl));
        }
        if self.lang == Lang::Html {
            self.html_injections(tree, buf, &range, &mut out);
            out.sort_by_key(|(r, _)| r.start);
        }
        out
    }

    /// HTML の <script> は JavaScript、<style> は CSS でハイライトする（見えている範囲にかかるものだけ。中身ごとに覚える）
    fn html_injections(&self, tree: &Tree, buf: &Buffer, range: &Range<usize>, out: &mut Vec<(Range<usize>, Hl)>) {
        let mut stack = vec![tree.root_node()];
        let mut seen = Vec::new();
        while let Some(n) = stack.pop() {
            if n.end_byte() <= range.start || n.start_byte() >= range.end {
                continue;
            }
            // Angular: 文字の中の {{ 式 }} と @if (…) など、属性の [x] (x) *ngX の値は TypeScript として
            if n.kind() == "text" {
                let t = buf.slice_string(n.start_byte(), n.end_byte());
                for (a, b) in angular_text_exprs(&t) {
                    self.inject(Lang::TypeScript, n.start_byte() + a, &t[a..b], range, &mut seen, out);
                }
                for (a, b) in angular_blocks(&t) {
                    out.push((n.start_byte() + a.0..n.start_byte() + a.1, Hl::Keyword));
                    if let Some((c, d)) = b {
                        self.inject(Lang::TypeScript, n.start_byte() + c, &t[c..d], range, &mut seen, out);
                    }
                }
                continue;
            }
            if n.kind() == "attribute" {
                let mut c = n.walk();
                let kids: Vec<Node> = n.children(&mut c).collect();
                let name = kids.iter().find(|k| k.kind() == "attribute_name").map(|k| buf.slice_string(k.start_byte(), k.end_byte())).unwrap_or_default();
                let val = kids.iter().find_map(|k| match k.kind() {
                    "quoted_attribute_value" => {
                        let mut c2 = k.walk();
                        k.children(&mut c2).find(|x| x.kind() == "attribute_value")
                    }
                    "attribute_value" => Some(*k),
                    _ => None,
                });
                // ふつうの属性の値の中の {{ 式 }}: 式の外は文字列の色のまま、式は TypeScript
                if !name.starts_with(['[', '(', '*']) {
                    if let Some(v) = val {
                        let t = buf.slice_string(v.start_byte(), v.end_byte());
                        let exprs = angular_text_exprs(&t);
                        if !exprs.is_empty() {
                            let (vs, ve) = (v.start_byte(), v.end_byte());
                            out.retain(|(r, h)| !(*h == Hl::String && r.start <= vs && ve <= r.end));
                            let mut last = 0;
                            for &(a, b) in &exprs {
                                out.push((vs + last..vs + a, Hl::String));
                                self.inject(Lang::TypeScript, vs + a, &t[a..b], range, &mut seen, out);
                                last = b;
                            }
                            out.push((vs + last..ve, Hl::String));
                            if let Some(q) = v.parent().filter(|p| p.kind() == "quoted_attribute_value") {
                                out.push((q.start_byte()..vs, Hl::String));
                                out.push((ve..q.end_byte(), Hl::String));
                            }
                        }
                    }
                }
                if name.starts_with(['[', '(', '*']) {
                    if let Some(v) = val {
                        let t = buf.slice_string(v.start_byte(), v.end_byte());
                        // 値は文字列ではなく式なので、外側の文字列の色を外す（引用符だけ残す）
                        let (vs, ve) = (v.start_byte(), v.end_byte());
                        out.retain(|(r, h)| !(*h == Hl::String && r.start <= vs && ve <= r.end));
                        if let Some(q) = v.parent().filter(|p| p.kind() == "quoted_attribute_value") {
                            out.push((q.start_byte()..vs, Hl::String));
                            out.push((ve..q.end_byte(), Hl::String));
                        }
                        self.inject(Lang::TypeScript, v.start_byte(), &t, range, &mut seen, out);
                    }
                }
                continue;
            }
            let lang = match n.kind() {
                "script_element" => {
                    // type が JavaScript 以外（テンプレート・JSON など）は JSON だけ色を付ける
                    let start = n.child(0).map(|t| buf.slice_string(t.start_byte(), t.end_byte()).to_ascii_lowercase()).unwrap_or_default();
                    if start.contains("json") {
                        Some(Lang::Json)
                    } else if start.contains("type=") && !start.contains("javascript") && !start.contains("module") {
                        None
                    } else {
                        Some(Lang::JavaScript)
                    }
                }
                "style_element" => Some(Lang::Css),
                _ => {
                    let mut c = n.walk();
                    stack.extend(n.children(&mut c));
                    continue;
                }
            };
            let Some(lang) = lang else { continue };
            let mut c = n.walk();
            let Some(raw) = n.children(&mut c).find(|x| x.kind() == "raw_text") else { continue };
            let (s, e) = (raw.start_byte(), raw.end_byte());
            let text = buf.slice_string(s, e);
            self.inject(lang, s, &text, range, &mut seen, out);
        }
        // 今の文書に無くなった中身の覚えは捨てる（見えていないものは残す。数は抑える）
        let mut cache = self.injected.lock().unwrap_or_else(|p| p.into_inner());
        if cache.len() > 512 {
            cache.retain(|(_, t, _)| seen.contains(t));
        }
    }

    /// `text`（文書の `s` から）を `lang` でハイライトして足す（中身ごとに覚える）
    fn inject(&self, lang: Lang, s: usize, text: &str, range: &Range<usize>, seen: &mut Vec<String>, out: &mut Vec<(Range<usize>, Hl)>) {
        if s + text.len() <= range.start || s >= range.end || text.trim().is_empty() {
            return;
        }
        let mut cache = self.injected.lock().unwrap_or_else(|p| p.into_inner());
        let spans = match cache.iter().find(|(l, t, _)| *l == lang && t == text) {
            Some((_, _, sp)) => sp.clone(),
            None => {
                let sp = Syntax::new(lang)
                    .map(|mut syn| {
                        let mut b = Buffer::from_str(text);
                        syn.parse(&mut b);
                        syn.highlight(&b, 0..text.len())
                    })
                    .unwrap_or_default();
                cache.push((lang, text.to_string(), sp.clone()));
                sp
            }
        };
        drop(cache);
        seen.push(text.to_string());
        out.extend(spans.into_iter().map(|(r, h)| (r.start + s..r.end + s, h)).filter(|(r, _)| r.end > range.start && r.start < range.end));
    }

    /// ERROR ノードの数（構文エラーの有無の目安。[e ]e で使う）
    pub fn has_error(&self) -> bool {
        self.tree.as_ref().is_some_and(|t| t.root_node().has_error())
    }
}

/// HTML の中のカーソルの場所（Angular のテンプレートから定義へ跳ぶため）
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HtmlCtx {
    /// 要素の名前（<app-button>）
    Tag(String),
    /// 属性の名前（[width] (click) *ngIf の [ ] ( ) * を除いたもの）と、その要素の名前
    Attr { tag: String, name: String },
    /// それ以外（{{ 式 }}・属性の値・@if (式) の中など）
    Other,
}

/// `at` の場所の種類
pub fn html_context(tree: &Tree, at: usize, text: &dyn Fn(Range<usize>) -> String) -> Option<HtmlCtx> {
    let n = tree.root_node().descendant_for_byte_range(at, at)?;
    let tag_of = |mut x: Node| -> String {
        while let Some(p) = x.parent() {
            if matches!(p.kind(), "start_tag" | "self_closing_tag" | "end_tag") {
                let mut c = p.walk();
                return p.children(&mut c).find(|k| k.kind() == "tag_name").map(|k| text(k.byte_range())).unwrap_or_default();
            }
            x = p;
        }
        String::new()
    };
    Some(match n.kind() {
        "tag_name" => HtmlCtx::Tag(text(n.byte_range())),
        "attribute_name" => {
            let raw = text(n.byte_range());
            let name = raw.trim_start_matches(['[', '(', '*']).trim_end_matches([']', ')']).split('.').next().unwrap_or("").to_string();
            HtmlCtx::Attr { tag: tag_of(n), name }
        }
        _ => HtmlCtx::Other,
    })
}

/// Angular: 文字の中の {{ 式 }} の式の範囲
pub fn angular_text_exprs(t: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = t[from..].find("{{").map(|i| i + from) {
        let Some(j) = t[i + 2..].find("}}").map(|j| j + i + 2) else { break };
        out.push((i + 2, j));
        from = j + 2;
    }
    out
}

/// Angular 17 の制御フロー（@if (式) {、@for、@switch、@case、@else、@defer など）: (@語 の範囲, ( ) の中の範囲)
pub fn angular_blocks(t: &str) -> Vec<((usize, usize), Option<(usize, usize)>)> {
    const KW: [&str; 11] = ["@if", "@else if", "@else", "@for", "@switch", "@case", "@default", "@defer", "@placeholder", "@loading", "@empty"];
    let mut out = Vec::new();
    let b = t.as_bytes();
    let mut i = 0;
    while let Some(k) = t[i..].find('@').map(|k| k + i) {
        let kw = KW.iter().find(|w| t[k..].starts_with(**w) && !t[k + w.len()..].starts_with(|c: char| c.is_alphanumeric()));
        let Some(kw) = kw else {
            i = k + 1;
            continue;
        };
        let e = k + kw.len();
        let mut j = e;
        while j < b.len() && b[j] == b' ' {
            j += 1;
        }
        let mut paren = None;
        if j < b.len() && b[j] == b'(' {
            let mut depth = 0;
            for (q, c) in t[j..].char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            paren = Some((j + 1, j + q));
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
        out.push(((k, e), paren));
        i = paren.map_or(e, |p| p.1);
    }
    out
}

fn input_edit(e: EditLog) -> InputEdit {
    InputEdit {
        start_byte: e.start,
        old_end_byte: e.old_end,
        new_end_byte: e.new_end,
        start_position: point(e.start_pos),
        old_end_position: point(e.old_end_pos),
        new_end_position: point(e.new_end_pos),
    }
}

/// tree-sitter の text object の種類（`if/af` `ic/ac` `ia/aa`）
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ObjKind {
    Function,
    Class,
    Argument,
}

/// 関数とみなす節（言語をまたいで種類の名前で見分ける）
const FUNCTION_KINDS: &[&str] = &[
    "function_item",
    "function_declaration",
    "function_definition",
    "function_expression",
    "function",
    "generator_function_declaration",
    "method_declaration",
    "method_definition",
    "constructor_declaration",
    "arrow_function",
    "closure_expression",
    "lambda",
    "lambda_expression",
    "lambda_literal",
    "anonymous_function",
    "secondary_constructor",
];

const CLASS_KINDS: &[&str] = &[
    "class_declaration",
    "class_definition",
    "class",
    "abstract_class_declaration",
    "struct_item",
    "enum_item",
    "union_item",
    "impl_item",
    "trait_item",
    "mod_item",
    "interface_declaration",
    "enum_declaration",
    "record_declaration",
    "annotation_type_declaration",
    "object_declaration",
    "companion_object",
];

/// 引数・仮引数の並び（この直下の名前付きの子が 1 つの引数）
const ARG_LIST_KINDS: &[&str] = &[
    "arguments",
    "argument_list",
    "parameters",
    "formal_parameters",
    "parameter_list",
    "type_arguments",
    "type_parameters",
    "value_arguments",
    "function_value_parameters",
    "closure_parameters",
    "lambda_parameters",
];

impl Syntax {
    /// `at` を含む text object の範囲（byte）。`inner` は中身だけ（関数とクラスは本体の内側、引数は区切りを含めない）
    pub fn text_object(&self, at: usize, kind: ObjKind, inner: bool, buf: &Buffer) -> Option<Range<usize>> {
        let tree = self.tree.as_ref()?;
        let mut node = tree.root_node().descendant_for_byte_range(at, at)?;
        loop {
            let found = match kind {
                ObjKind::Function => FUNCTION_KINDS.contains(&node.kind()),
                ObjKind::Class => CLASS_KINDS.contains(&node.kind()),
                ObjKind::Argument => node.is_named() && node.parent().is_some_and(|p| ARG_LIST_KINDS.contains(&p.kind())),
            };
            if found {
                break;
            }
            node = node.parent()?;
        }
        if kind == ObjKind::Argument {
            return Some(argument_range(node, inner, buf));
        }
        if !inner {
            return Some(node.start_byte()..node.end_byte());
        }
        // 中身: 本体（body）の括弧の内側。括弧が無い言語（Python）は本体そのもの
        let body = node.child_by_field_name("body").or_else(|| {
            let mut c = node.walk();
            let last = node.named_children(&mut c).last();
            last
        })?;
        let (s, e) = (body.start_byte(), body.end_byte());
        if e < s + 2 {
            return Some(s..e);
        }
        let (first, last) = (buf.slice_string(s, s + 1), buf.slice_string(e - 1, e));
        if (first == "{" && last == "}") || (first == "(" && last == ")") {
            Some(s + 1..e - 1)
        } else {
            Some(s..e)
        }
    }
}

/// 折りたためる節（関数・クラスに加えて、ブロックや大きな値）
const FOLD_KINDS: &[&str] = &[
    "block",
    "statement_block",
    "compound_statement",
    "class_body",
    "interface_body",
    "enum_body",
    "declaration_list",
    "field_declaration_list",
    "enum_variant_list",
    "match_block",
    "switch_body",
    "switch_block",
    "object",
    "array",
    "array_expression",
    "field_initializer_list",
    "token_tree",
    "tuple_expression",
    "object_type",
    "jsx_element",
    "template_string",
    "array_initializer",
    "element_value_array_initializer",
    "collection_literal",
    "lambda_literal",
    "parenthesized_expression",
    "dictionary",
    "list",
    "arguments",
    "argument_list",
    "use_list",
    "import_statement",
    "mapping",
    "block_mapping",
    "block_sequence",
    "comment",
    "block_comment",
];

fn foldable(kind: &str) -> bool {
    FOLD_KINDS.contains(&kind) || FUNCTION_KINDS.contains(&kind) || CLASS_KINDS.contains(&kind)
}

impl Syntax {
    /// 行 `line` を含む折りたためる範囲（行）を、内側から順に。同じ行の範囲は 1 つにまとめる
    pub fn folds_at(&self, line: usize) -> Vec<(usize, usize)> {
        let Some(tree) = &self.tree else { return Vec::new() };
        let p = Point { row: line, column: 0 };
        let end = Point { row: line, column: usize::MAX / 2 };
        let Some(mut node) = tree.root_node().descendant_for_point_range(p, end) else { return Vec::new() };
        let mut out: Vec<(usize, usize)> = Vec::new();
        loop {
            let (s, e) = (node.start_position().row, node.end_position().row);
            if e > s && foldable(node.kind()) && s <= line && !out.contains(&(s, e)) {
                out.push((s, e));
            }
            match node.parent() {
                Some(p) => node = p,
                None => break,
            }
        }
        out
    }

    /// ファイルの中のすべての折りたためる範囲（行）
    pub fn all_folds(&self) -> Vec<(usize, usize)> {
        let Some(tree) = &self.tree else { return Vec::new() };
        let mut out = Vec::new();
        let mut c = tree.walk();
        'walk: loop {
            let n = c.node();
            let (s, e) = (n.start_position().row, n.end_position().row);
            if e > s && foldable(n.kind()) {
                out.push((s, e));
            }
            // 1 行に収まる節の中には入らない
            if e > s && c.goto_first_child() {
                continue;
            }
            loop {
                if c.goto_next_sibling() {
                    continue 'walk;
                }
                if !c.goto_parent() {
                    break 'walk;
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

impl Syntax {
    /// 構文エラー（ERROR の節と、足りない節）の位置（byte。前から順に）
    pub fn error_positions(&self) -> Vec<usize> {
        let Some(tree) = &self.tree else { return Vec::new() };
        let mut out = Vec::new();
        let mut c = tree.walk();
        'walk: loop {
            let n = c.node();
            if n.is_error() || n.is_missing() {
                out.push(n.start_byte());
            }
            // エラーを含まない節の中には入らない
            if n.has_error() && c.goto_first_child() {
                continue;
            }
            loop {
                if c.goto_next_sibling() {
                    continue 'walk;
                }
                if !c.goto_parent() {
                    break 'walk;
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// このファイルの定義の一覧（ファイルの構造）
    pub fn defs(&self, buf: &Buffer) -> Vec<defs::Def> {
        let Some(tree) = &self.tree else { return Vec::new() };
        defs::collect_defs(tree, &|r: Range<usize>| buf.slice_string(r.start, r.end))
    }

    /// HTML の中のカーソルの場所の種類
    pub fn html_context(&self, buf: &Buffer, at: usize) -> Option<HtmlCtx> {
        if self.lang != Lang::Html {
            return None;
        }
        html_context(self.tree.as_ref()?, at, &|r: Range<usize>| buf.slice_string(r.start, r.end))
    }

    /// `at`（呼び出しの名前）の引数の数
    pub fn arg_count(&self, at: usize) -> Option<usize> {
        defs::call_arg_count(self.tree.as_ref()?, at)
    }

    /// `at`（呼び出しの名前）の引数の型
    pub fn call_args(&self, buf: &Buffer, at: usize) -> Option<Vec<defs::Arg>> {
        let tree = self.tree.as_ref()?;
        let text = |r: Range<usize>| buf.slice_string(r.start, r.end);
        let binds = defs::local_bindings(tree, &text);
        defs::call_args(tree, at, &text, &binds)
    }

    /// `at`（呼び出しの名前）の受け手の型
    pub fn receiver(&self, buf: &Buffer, at: usize) -> Option<defs::Receiver> {
        let tree = self.tree.as_ref()?;
        let text = |r: Range<usize>| buf.slice_string(r.start, r.end);
        let binds = defs::local_bindings(tree, &text);
        defs::receiver_at(tree, at, &text, &binds)
    }

    /// `at` の識別子の、このファイルの中の定義の位置（引数・ローカル変数を含む）
    pub fn local_definition(&self, buf: &Buffer, at: usize) -> Option<usize> {
        let tree = self.tree.as_ref()?;
        defs::find_local_definition(tree, at, &|r: Range<usize>| buf.slice_string(r.start, r.end))
    }
}

/// 引数の範囲。外側（aa）は後ろの区切り（`, ` まで）を含め、最後の引数なら前の区切りを含める
fn argument_range(node: Node, inner: bool, buf: &Buffer) -> Range<usize> {
    let (s, e) = (node.start_byte(), node.end_byte());
    if inner {
        return s..e;
    }
    let mut next = node.next_sibling();
    while let Some(n) = next {
        if n.kind() == "," {
            // 区切りの後ろの空白も含める
            let mut end = n.end_byte();
            while matches!(buf.slice_string(end, (end + 1).min(buf.len_bytes())).as_str(), " " | "\t") {
                end += 1;
            }
            return s..end;
        }
        if n.is_named() {
            break;
        }
        next = n.next_sibling();
    }
    // 最後の引数: 前の区切りから
    let mut prev = node.prev_sibling();
    while let Some(p) = prev {
        if p.kind() == "," {
            return p.start_byte()..e;
        }
        if p.is_named() {
            break;
        }
        prev = p.prev_sibling();
    }
    s..e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_compile() {
        for lang in Lang::all() {
            if let Err(e) = lang.highlights() {
                panic!("{}: {e}", lang.name());
            }
        }
    }

    fn kinds(lang: Lang, src: &str) -> Vec<(String, Hl)> {
        let mut buf = Buffer::from_str(src);
        let mut s = Syntax::new(lang).unwrap();
        s.parse(&mut buf);
        s.highlight(&buf, 0..src.len()).into_iter().map(|(r, h)| (src[r].to_string(), h)).collect()
    }

    /// 裏で再解析している間に編集が入っても、最後は最初から解析し直したのと同じ木になる
    #[test]
    fn background_reparse_catches_up() {
        let src = "function a() { return 1; }\n".repeat(200);
        let mut buf = Buffer::from_str(&src);
        let mut s = Syntax::new(Lang::TypeScript).unwrap();
        s.parse(&mut buf);
        s.last_parse = Duration::from_secs(1); // 遅いファイルとして扱わせる
        s.waker = Some(Arc::new(|| {}));
        buf.edit(9, 10, "bb");
        s.sync(&mut buf);
        assert!(s.busy());
        buf.edit(0, 0, "const z = 2;\n");
        let end = buf.len_bytes();
        buf.edit(end, end, "let q = 'x';\n");
        s.sync(&mut buf);
        let t = Instant::now();
        while s.busy() || s.stale {
            s.last_parse = Duration::from_secs(1);
            s.sync(&mut buf);
            assert!(t.elapsed() < Duration::from_secs(10));
            std::thread::yield_now();
        }
        let mut fresh = Syntax::new(Lang::TypeScript).unwrap();
        let mut copy = Buffer::from_str(&buf.text());
        fresh.parse(&mut copy);
        assert_eq!(s.tree.as_ref().unwrap().root_node().to_sexp(), fresh.tree.as_ref().unwrap().root_node().to_sexp());
        let hl = s.highlight(&buf, 0..buf.len_bytes());
        assert!(hl.iter().any(|(r, h)| *h == Hl::Keyword && buf.slice_string(r.start, r.end) == "const"), "{hl:?}");
    }

    #[test]
    fn rust() {
        let k = kinds(Lang::Rust, "// c\n/// 文書\n#[derive(Debug)]\nfn f(x: u32) -> String { let s = \"a\\n\"; println!(\"{}\", 1); s.to_string() }\n");
        assert!(k.contains(&("// c\n".into(), Hl::Comment)) || k.iter().any(|(t, h)| t.starts_with("// c") && *h == Hl::Comment), "{k:?}");
        assert!(k.iter().any(|(t, h)| t.starts_with("/// 文書") && *h == Hl::Comment), "{k:?}");
        assert!(k.contains(&("fn".into(), Hl::Keyword)), "{k:?}");
        assert!(k.contains(&("let".into(), Hl::Keyword)), "{k:?}");
        assert!(k.contains(&("u32".into(), Hl::Type)), "{k:?}");
        assert!(k.contains(&("String".into(), Hl::Type)), "{k:?}");
        assert!(k.iter().any(|(t, h)| t == "\\n" && *h == Hl::Escape) || k.iter().any(|(t, h)| t.starts_with('"') && *h == Hl::String), "{k:?}");
        assert!(k.contains(&("1".into(), Hl::Number)), "{k:?}");
        assert!(Lang::detect(Path::new("src/main.rs")) == Some(Lang::Rust));
    }

    /// JSX / TSX: タグはキーワードの色、属性は属性の色、部品（大文字で始まる）は型の色、{ } の中は TS / JS のまま
    #[test]
    fn jsx_and_tsx() {
        let src = "const App = (p: Props) => <div className=\"box\" onClick={() => go(1)}><MyButton label=\"ok\" /></div>;\n";
        let k = kinds(Lang::Tsx, src);
        assert!(k.contains(&("div".into(), Hl::Keyword)), "{k:?}");
        assert!(k.contains(&("className".into(), Hl::Attribute)), "{k:?}");
        assert!(k.contains(&("onClick".into(), Hl::Attribute)), "{k:?}");
        assert!(k.contains(&("MyButton".into(), Hl::Type)), "{k:?}");
        assert!(k.contains(&("\"box\"".into(), Hl::String)), "{k:?}");
        assert!(k.contains(&("Props".into(), Hl::Type)), "{k:?}");
        assert!(k.contains(&("go".into(), Hl::Function)), "{k:?}");
        assert!(k.contains(&("1".into(), Hl::Number)), "{k:?}");
        let src = "export default function App() { return <section id=\"a\"><Item key={1} /></section>; }\n";
        let k = kinds(Lang::JavaScript, src);
        assert!(k.contains(&("section".into(), Hl::Keyword)), "{k:?}");
        assert!(k.contains(&("id".into(), Hl::Attribute)), "{k:?}");
        assert!(k.contains(&("Item".into(), Hl::Type)), "{k:?}");
        assert_eq!(Lang::detect(Path::new("a/App.tsx")), Some(Lang::Tsx));
        assert_eq!(Lang::detect(Path::new("a/App.jsx")), Some(Lang::JavaScript));
    }

    #[test]
    fn html() {
        let src = "<!DOCTYPE html>\n<!-- c -->\n<div class=\"box\" id=\"a\"><p>本文</p></div>\n<style>\n.box { color: red; }\n</style>\n<script>\nconst n = 1;\n</script>\n";
        let k = kinds(Lang::Html, src);
        assert!(k.contains(&("div".into(), Hl::Keyword)), "{k:?}");
        assert!(k.contains(&("class".into(), Hl::Attribute)), "{k:?}");
        assert!(k.iter().any(|(t, h)| t.contains("box") && *h == Hl::String), "{k:?}");
        assert!(k.iter().any(|(t, h)| t.contains("<!-- c") && *h == Hl::Comment), "{k:?}");
        // <style> の中は CSS、<script> の中は JavaScript
        assert!(k.contains(&("color".into(), Hl::Attribute)), "{k:?}");
        assert!(k.contains(&("const".into(), Hl::Keyword)), "{k:?}");
        assert!(k.contains(&("1".into(), Hl::Number)), "{k:?}");
        assert_eq!(Lang::detect(Path::new("app/page.component.html")), Some(Lang::Html));
        // Angular: {{ 式 }}、[x] (x) *ngIf の値、@if (式) は TypeScript
        let ng = "<p>{{ title | uppercase }}</p>\n<app-button [width]=\"300\" (click)=\"onGo(1)\" *ngIf=\"isShown && count > 0\"></app-button>\n@if (user.admin) {\n  <b>admin</b>\n} @else {\n}\n";
        let k = kinds(Lang::Html, ng);
        assert!(k.contains(&("300".into(), Hl::Number)), "{k:?}");
        assert!(k.contains(&("onGo".into(), Hl::Function)) || k.iter().any(|(t, h)| t == "onGo" && *h != Hl::String), "{k:?}");
        assert!(k.contains(&("1".into(), Hl::Number)), "{k:?}");
        assert!(k.iter().any(|(t, h)| t == "@if" && *h == Hl::Keyword), "{k:?}");
        assert!(k.iter().any(|(t, h)| t == "@else" && *h == Hl::Keyword), "{k:?}");
        assert!(!k.iter().any(|(t, h)| t.contains("onGo(1)") && *h == Hl::String), "値は文字列の色のままにしない: {k:?}");
        assert_eq!(angular_text_exprs("a {{ x }} b {{y}}"), vec![(4, 7), (14, 15)]);
        let k = kinds(Lang::Html, "<a title=\"no {{ n + 1 }}\"></a>\n");
        assert!(k.contains(&("1".into(), Hl::Number)), "{k:?}");
        assert!(!k.iter().any(|(t, h)| t.contains("n + 1") && *h == Hl::String), "{k:?}");
    }

    #[test]
    fn css_and_scss() {
        let k = kinds(Lang::Css, "/* c */\n@media (max-width: 600px) {\n  .box > a:hover { color: #fff; margin: 0 4px; font-family: \"Noto\"; }\n}\n:root { --gap: 8px; }\n");
        assert!(k.iter().any(|(t, h)| t.starts_with("/* c") && *h == Hl::Comment), "{k:?}");
        assert!(k.contains(&("color".into(), Hl::Attribute)), "{k:?}");
        assert!(k.contains(&("a".into(), Hl::Keyword)), "{k:?}");
        assert!(k.iter().any(|(t, h)| t.contains("4") && *h == Hl::Number), "{k:?}");
        assert!(k.iter().any(|(t, h)| t.contains("Noto") && *h == Hl::String), "{k:?}");
        assert!(k.iter().any(|(t, h)| t.contains("@media") && *h == Hl::Keyword), "{k:?}");
        let k = kinds(Lang::Scss, "$main: #333;\n@mixin round($r) { border-radius: $r; }\n.card {\n  @include round(4px);\n  &:hover { color: darken($main, 10%); }\n}\n");
        assert!(k.iter().any(|(t, h)| t == "$main" && *h == Hl::Type), "{k:?}");
        assert!(k.iter().any(|(t, h)| t.contains("@mixin") && *h == Hl::Keyword), "{k:?}");
        assert!(k.iter().any(|(t, h)| t.contains("@include") && *h == Hl::Keyword), "{k:?}");
        assert!(k.contains(&("border-radius".into(), Hl::Attribute)), "{k:?}");
        assert_eq!(Lang::detect(Path::new("a/site.scss")), Some(Lang::Scss));
        assert_eq!(Lang::detect(Path::new("a/site.css")), Some(Lang::Css));
    }

    /// this・return は予約語、関数は関数の色、変数・引数は変数の色、プロパティは紫（全部大文字の名前は TS の規則どおり型の色）
    #[test]
    fn ts_identifiers() {
        let k = kinds(Lang::TypeScript, "class A {\n  private x = 1;\n  go(n: number) {\n    const y = this.x + n;\n    return foo(y, MAX);\n  }\n}\n");
        assert!(k.contains(&("this".into(), Hl::Keyword)), "{k:?}");
        assert!(k.contains(&("return".into(), Hl::Keyword)), "{k:?}");
        assert!(k.contains(&("go".into(), Hl::Function)), "{k:?}");
        assert!(k.contains(&("foo".into(), Hl::Function)), "{k:?}");
        assert!(k.contains(&("y".into(), Hl::Variable)), "{k:?}");
        assert!(k.contains(&("n".into(), Hl::Variable)), "{k:?}");
        assert!(k.contains(&("x".into(), Hl::Property)), "{k:?}");
    }

    #[test]
    fn typescript() {
        let k = kinds(Lang::TypeScript, "// c\nconst x: string = \"a\"; function f() { return 1; }\n");
        assert!(k.contains(&("// c".into(), Hl::Comment)), "{k:?}");
        assert!(k.contains(&("const".into(), Hl::Keyword)), "{k:?}");
        assert!(k.contains(&("\"a\"".into(), Hl::String)), "{k:?}");
        assert!(k.contains(&("string".into(), Hl::Type)), "{k:?}");
        assert!(k.contains(&("1".into(), Hl::Number)), "{k:?}");
    }

    fn obj(lang: Lang, src: &str, at_text: &str, kind: ObjKind, inner: bool) -> String {
        let mut buf = Buffer::from_str(src);
        let mut syn = Syntax::new(lang).unwrap();
        syn.parse(&mut buf);
        let at = src.find(at_text).unwrap();
        let r = syn.text_object(at, kind, inner, &buf).unwrap();
        src[r].to_string()
    }

    #[test]
    fn folds() {
        let rs = "fn a() {\n    let v = [\n        1,\n    ];\n}\nfn b() {}\n";
        let mut buf = Buffer::from_str(rs);
        let mut syn = Syntax::new(Lang::Rust).unwrap();
        syn.parse(&mut buf);
        // 2 行目（let v = [）: 配列 1〜3 行、関数 0〜4 行
        assert_eq!(syn.folds_at(2), vec![(1, 3), (0, 4)]);
        assert_eq!(syn.all_folds(), vec![(0, 4), (1, 3)]);
    }

    #[test]
    fn text_objects() {
        let rs = "fn add(a: i32, b: i32) -> i32 {\n    let c = a + b;\n    c\n}\nstruct P { x: i32 }\n";
        assert_eq!(obj(Lang::Rust, rs, "let c", ObjKind::Function, false), &rs[..rs.find("\nstruct").unwrap()]);
        assert_eq!(obj(Lang::Rust, rs, "let c", ObjKind::Function, true), "\n    let c = a + b;\n    c\n");
        assert_eq!(obj(Lang::Rust, rs, "b: i32", ObjKind::Argument, true), "b: i32");
        assert_eq!(obj(Lang::Rust, rs, "a: i32", ObjKind::Argument, false), "a: i32, ");
        assert_eq!(obj(Lang::Rust, rs, "b: i32", ObjKind::Argument, false), ", b: i32");
        assert_eq!(obj(Lang::Rust, rs, "x: i32", ObjKind::Class, true), " x: i32 ");
        let ts = "class A {\n  m(x, y) { return f(x, g(y)); }\n}\n";
        assert_eq!(obj(Lang::TypeScript, ts, "return", ObjKind::Function, false), "m(x, y) { return f(x, g(y)); }");
        assert_eq!(obj(Lang::TypeScript, ts, "g(y)", ObjKind::Argument, false), ", g(y)");
        assert_eq!(obj(Lang::TypeScript, ts, "return", ObjKind::Class, true), "\n  m(x, y) { return f(x, g(y)); }\n");
    }

    #[test]
    fn kotlin() {
        let k = kinds(Lang::Kotlin, "// c\nfun main() { val s = \"a\"; println(1) }\n");
        assert!(k.contains(&("fun".into(), Hl::Keyword)), "{k:?}");
        assert!(k.contains(&("\"a\"".into(), Hl::String)), "{k:?}");
        assert!(k.contains(&("main".into(), Hl::Function)), "{k:?}");
    }

    #[test]
    fn config_and_scripts() {
        let k = kinds(Lang::Json, "{\"a\": 1, \"b\": \"x\"}");
        assert!(k.contains(&("\"a\"".into(), Hl::Keyword)), "{k:?}");
        assert!(k.contains(&("1".into(), Hl::Number)), "{k:?}");
        let k = kinds(Lang::Vim, "\" c\nset clipboard+=unnamedplus\nnnoremap ew :w<CR>\n");
        assert!(k.iter().any(|(_, h)| *h == Hl::Comment), "{k:?}");
        assert!(k.iter().any(|(t, h)| t == "set" && *h == Hl::Keyword), "{k:?}");
        let k = kinds(Lang::Python, "# c\ndef f(x):\n    return \"s\"\n");
        assert!(k.contains(&("def".into(), Hl::Keyword)), "{k:?}");
        let k = kinds(Lang::PowerShell, "# c\n$x = \"a\"\nWrite-Output $x\n");
        assert!(k.iter().any(|(_, h)| *h == Hl::Comment), "{k:?}");
        let k = kinds(Lang::Yaml, "a: 1\nb: [x, y]\n");
        assert!(k.iter().any(|(t, h)| t == "a" && *h == Hl::Keyword), "{k:?}");
        assert_eq!(Lang::detect(Path::new("C:/Users/x/.ideavimrc")), Some(Lang::Vim));
        assert_eq!(Lang::detect(Path::new("run.BAT")), Some(Lang::Batch));
    }

    #[test]
    fn incremental_edit() {
        let mut buf = Buffer::from_str("const a = 1;\n");
        let mut s = Syntax::new(Lang::TypeScript).unwrap();
        s.parse(&mut buf);
        buf.edit(0, 0, "// x\n");
        assert!(s.sync(&mut buf));
        let k: Vec<_> = s.highlight(&buf, 0..buf.len_bytes()).into_iter().map(|(_, h)| h).collect();
        assert_eq!(k.first(), Some(&Hl::Comment));
        assert!(!s.has_error());
    }

    /// どの言語も、文法と色分けの規則が組み合わさる（規則の誤りで色が付かなくならないように）
    #[test]
    fn all_highlight_queries_compile() {
        let bad: Vec<String> = Lang::all().iter().filter_map(|l| l.highlights().as_ref().err().map(|e| format!("{}: {e}", l.name()))).collect();
        assert!(bad.is_empty(), "{bad:#?}");
    }

    /// 表の並びが enum と同じ（Lang を表の添え字に使う）。拡張子・ファイル名・コードブロックの名前は 1 つの言語にだけ
    #[test]
    fn table_is_consistent() {
        for (i, s) in LANGS.iter().enumerate() {
            assert_eq!(s.lang as usize, i, "{} の並び", s.name);
        }
        for key in [|s: &Spec| s.exts, |s: &Spec| s.files, |s: &Spec| s.fences] {
            let mut seen = std::collections::HashMap::new();
            for s in &LANGS {
                for k in key(s) {
                    assert!(seen.insert(*k, s.name).is_none(), "{k} が {} と {} の両方にある", seen[k], s.name);
                }
            }
        }
        assert_eq!(Lang::detect(Path::new("x/Dockerfile.dev")), Some(Lang::Dockerfile));
        assert_eq!(Lang::detect(Path::new("CMakeLists.txt")), Some(Lang::CMake));
        assert_eq!(Lang::from_fence("C#"), Some(Lang::CSharp));
    }
}
