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
}

const ALL: [Lang; 14] = [
    Lang::Rust,
    Lang::TypeScript,
    Lang::Tsx,
    Lang::JavaScript,
    Lang::Java,
    Lang::Kotlin,
    Lang::Python,
    Lang::Bash,
    Lang::Batch,
    Lang::PowerShell,
    Lang::Ini,
    Lang::Json,
    Lang::Yaml,
    Lang::Vim,
];

// tree-sitter-vim 0.4.0 は古い形式のバインディング（Language を値で返す宣言）なので使わず、
// 同じクレートがビルドする C の関数を新しい形式（LanguageFn）で呼ぶ
unsafe extern "C" {
    fn tree_sitter_vim() -> *const ();
}
const VIM_LANGUAGE: tree_sitter_language::LanguageFn = unsafe { tree_sitter_language::LanguageFn::from_raw(tree_sitter_vim) };

impl Lang {
    pub fn detect(path: &Path) -> Option<Lang> {
        // 拡張子ではなくファイル名で決まるもの
        let name = path.file_name()?.to_str()?.to_ascii_lowercase();
        match name.as_str() {
            ".vimrc" | "_vimrc" | ".ideavimrc" | "_ideavimrc" | ".gvimrc" | "nagi.vim" => return Some(Lang::Vim),
            ".bashrc" | ".bash_profile" | ".profile" | ".zshrc" => return Some(Lang::Bash),
            ".gitconfig" | ".editorconfig" | ".npmrc" => return Some(Lang::Ini),
            _ => {}
        }
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "py" | "pyw" | "pyi" => Lang::Python,
            "sh" | "bash" => Lang::Bash,
            "bat" | "cmd" => Lang::Batch,
            "ps1" | "psm1" | "psd1" => Lang::PowerShell,
            "ini" | "cfg" | "conf" | "properties" => Lang::Ini,
            "json" | "jsonc" | "json5" => Lang::Json,
            "yml" | "yaml" => Lang::Yaml,
            "vim" => Lang::Vim,
            "ts" | "mts" | "cts" => Lang::TypeScript,
            "tsx" => Lang::Tsx,
            "js" | "mjs" | "cjs" | "jsx" => Lang::JavaScript,
            "java" => Lang::Java,
            "kt" | "kts" => Lang::Kotlin,
            "rs" => Lang::Rust,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Lang::Rust => "Rust",
            Lang::TypeScript => "TypeScript",
            Lang::Tsx => "TSX",
            Lang::JavaScript => "JavaScript",
            Lang::Java => "Java",
            Lang::Kotlin => "Kotlin",
            Lang::Python => "Python",
            Lang::Bash => "Bash",
            Lang::Batch => "Batch",
            Lang::PowerShell => "PowerShell",
            Lang::Ini => "INI",
            Lang::Json => "JSON",
            Lang::Yaml => "YAML",
            Lang::Vim => "Vim",
        }
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

    /// シェル（$変数 を色分けする）
    fn is_shell(self) -> bool {
        matches!(self, Lang::Bash | Lang::Batch | Lang::PowerShell)
    }

    fn language(self) -> Language {
        match self {
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Lang::Java => tree_sitter_java::LANGUAGE.into(),
            Lang::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::Bash => tree_sitter_bash::LANGUAGE.into(),
            Lang::Batch => tree_sitter_batch::LANGUAGE.into(),
            Lang::PowerShell => tree_sitter_powershell::LANGUAGE.into(),
            Lang::Ini => tree_sitter_ini::LANGUAGE.into(),
            Lang::Json => tree_sitter_json::LANGUAGE.into(),
            Lang::Yaml => tree_sitter_yaml::LANGUAGE.into(),
            Lang::Vim => VIM_LANGUAGE.into(),
        }
    }

    /// ハイライトのクエリ。TypeScript は JavaScript のクエリに TS 固有分を足して使う（grammar の想定どおり）
    fn highlights_source(self) -> String {
        match self {
            Lang::TypeScript => format!("{}\n{}", tree_sitter_javascript::HIGHLIGHT_QUERY, tree_sitter_typescript::HIGHLIGHTS_QUERY),
            // TSX は JSX の要素（タグ・属性）の規則も足す
            Lang::Tsx => format!("{}\n{}\n{}", tree_sitter_javascript::HIGHLIGHT_QUERY, tree_sitter_javascript::JSX_HIGHLIGHT_QUERY, tree_sitter_typescript::HIGHLIGHTS_QUERY),
            Lang::JavaScript => format!("{}\n{}", tree_sitter_javascript::HIGHLIGHT_QUERY, tree_sitter_javascript::JSX_HIGHLIGHT_QUERY),
            Lang::Java => tree_sitter_java::HIGHLIGHTS_QUERY.to_string(),
            Lang::Rust => tree_sitter_rust::HIGHLIGHTS_QUERY.to_string(),
            Lang::Kotlin => include_str!("../queries/kotlin/highlights.scm").to_string(),
            Lang::Python => tree_sitter_python::HIGHLIGHTS_QUERY.to_string(),
            Lang::Bash => tree_sitter_bash::HIGHLIGHT_QUERY.to_string(),
            Lang::Batch => tree_sitter_batch::HIGHLIGHTS_QUERY.to_string(),
            Lang::PowerShell => tree_sitter_powershell::HIGHLIGHTS_QUERY.to_string(),
            Lang::Ini => tree_sitter_ini::HIGHLIGHTS_QUERY.to_string(),
            Lang::Json => tree_sitter_json::HIGHLIGHTS_QUERY.to_string(),
            Lang::Yaml => tree_sitter_yaml::HIGHLIGHTS_QUERY.to_string(),
            Lang::Vim => tree_sitter_vim::HIGHLIGHTS_QUERY.to_string(),
        }
    }

    /// 言語ごとに 1 回だけコンパイルしたクエリ
    pub fn highlights(self) -> &'static Result<Query, String> {
        static Q: [OnceLock<Result<Query, String>>; ALL.len()] = [const { OnceLock::new() }; ALL.len()];
        Q[self as usize].get_or_init(|| Query::new(&self.language(), &self.highlights_source()).map_err(|e| format!("{e}")))
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
}

fn classify(capture: &str, lang: Lang) -> Option<Hl> {
    let head = capture.split('.').next().unwrap_or(capture);
    Some(match head {
        // 設定・データの言語ではキーを、シェルでは $変数 を色分けする（プログラミング言語では地の色のまま）
        "property" | "field" | "tag" if lang.is_data() => Hl::Keyword,
        // JSON はキーを string.special.key として取り出す
        _ if lang.is_data() && capture.split('.').any(|p| p == "key") => Hl::Keyword,
        "variable" if lang.is_shell() && !capture.contains("builtin") => Hl::Type,
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
        parser.set_language(&lang.language()).ok()?;
        Some(Self { lang, parser: Some(parser), tree: None, stale: false, job: None, last_parse: Duration::ZERO, waker: None })
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
                if self.lang.last_wins() {
                    last.1 = hl;
                }
                continue;
            }
            out.push((r, hl));
        }
        out
    }

    /// ERROR ノードの数（構文エラーの有無の目安。[e ]e で使う）
    pub fn has_error(&self) -> bool {
        self.tree.as_ref().is_some_and(|t| t.root_node().has_error())
    }
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
        for lang in ALL {
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
}
