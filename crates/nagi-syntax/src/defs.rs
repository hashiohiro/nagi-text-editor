//! 定義の一覧（シンボル検索・ファイルの構造・プロジェクトの索引）と、ファイルの中の定義ジャンプ（`gd`）。
//!
//! 言語ごとのクエリは書かず、構文木をたどって「名前（`name` の欄）を持つ節」を、節の種類の名前で見分ける。
//! 関数の中の変数は一覧に入れない（索引が細かい名前で埋まらないように）。ファイルの中の定義ジャンプでは、
//! 引数やローカル変数も含めた束縛から、カーソルから見える一番内側のものを選ぶ。

use std::ops::Range;

use tree_sitter::{Node, Parser, Tree};

use crate::{FUNCTION_KINDS, Lang};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum DefKind {
    Function,
    Method,
    Class,
    Interface,
    Type,
    Enum,
    Constant,
    Variable,
    Field,
    Module,
}

impl DefKind {
    pub fn label(self) -> &'static str {
        match self {
            DefKind::Function => "関数",
            DefKind::Method => "メソッド",
            DefKind::Class => "クラス",
            DefKind::Interface => "インターフェース",
            DefKind::Type => "型",
            DefKind::Enum => "enum",
            DefKind::Constant => "定数",
            DefKind::Variable => "変数",
            DefKind::Field => "フィールド",
            DefKind::Module => "モジュール",
        }
    }

    /// 型の仲間か（GotoClass で出す）
    pub fn is_type(self) -> bool {
        matches!(self, DefKind::Class | DefKind::Interface | DefKind::Type | DefKind::Enum)
    }

    pub fn code(self) -> u8 {
        self as u8
    }

    pub fn from_code(c: u8) -> Option<DefKind> {
        use DefKind::*;
        [Function, Method, Class, Interface, Type, Enum, Constant, Variable, Field, Module].get(c as usize).copied()
    }
}

/// 節の種類から定義の種類（言語をまたいで名前で見分ける）
fn def_kind(kind: &str) -> Option<DefKind> {
    Some(match kind {
        "function_declaration" | "function_item" | "function_definition" | "generator_function_declaration" | "function_signature" | "function_signature_item" | "macro_definition" => DefKind::Function,
        "method_declaration" | "method_definition" | "method_signature" | "abstract_method_signature" | "constructor_declaration" | "compact_constructor_declaration" | "annotation_type_element_declaration" => DefKind::Method,
        "class_declaration" | "abstract_class_declaration" | "class_definition" | "struct_item" | "record_declaration" | "object_declaration" | "companion_object" | "union_item" | "class" => DefKind::Class,
        "interface_declaration" | "trait_item" | "annotation_type_declaration" => DefKind::Interface,
        "type_alias_declaration" | "type_item" | "associated_type" => DefKind::Type,
        "enum_declaration" | "enum_item" => DefKind::Enum,
        "enum_constant" | "enum_variant" | "const_item" | "static_item" => DefKind::Constant,
        "field_declaration" | "public_field_definition" | "property_signature" => DefKind::Field,
        "mod_item" | "module" | "internal_module" | "module_declaration" => DefKind::Module,
        "variable_declarator" => DefKind::Variable,
        _ => return None,
    })
}

/// 引数・ローカルの束縛を作る節（この節の名前・パターンの識別子が束縛）
const BINDING_PARENTS: &[&str] = &[
    "formal_parameter",
    "catch_formal_parameter",
    "spread_parameter",
    "required_parameter",
    "optional_parameter",
    "rest_pattern",
    "parameter",
    "self_parameter",
    "closure_parameters",
    "let_declaration",
    "for_expression",
    "for_in_statement",
    "for_statement",
    "enhanced_for_statement",
    "catch_clause",
    "parameters",
    "lambda_parameters",
    "typed_parameter",
    "default_parameter",
    "typed_default_parameter",
    "for_in_clause",
    "variable_declaration",
    "function_value_parameters",
    "inferred_parameters",
    "resource",
    "shorthand_property_identifier_pattern",
    "object_pattern",
    "array_pattern",
    "tuple_pattern",
    "assignment",
];

const SCOPE_KINDS: &[&str] = &[
    "block",
    "statement_block",
    "compound_statement",
    "class_body",
    "declaration_list",
    "program",
    "source_file",
    "module",
    "for_statement",
    "for_in_statement",
    "enhanced_for_statement",
    "for_expression",
    "catch_clause",
    "match_arm",
    "if_let_expression",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Def {
    pub name: String,
    pub kind: DefKind,
    /// 名前の位置（行は 0 始まり、列は行の中の byte）
    pub line: usize,
    pub col: usize,
    /// 名前の絶対 byte 位置
    pub at: usize,
}

fn inside_function(mut n: Node) -> bool {
    while let Some(p) = n.parent() {
        if FUNCTION_KINDS.contains(&p.kind()) {
            return true;
        }
        n = p;
    }
    false
}

/// 構文木から定義の一覧（関数の中の変数は除く）。`text` は byte 範囲の文字列を返す
pub fn collect_defs(tree: &Tree, text: &dyn Fn(Range<usize>) -> String) -> Vec<Def> {
    let mut out = Vec::new();
    let mut c = tree.walk();
    'walk: loop {
        let n = c.node();
        push_def(n, text, &mut out);
        if c.goto_first_child() {
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
    out
}

fn def_from(name: Node, kind: DefKind, text: &dyn Fn(Range<usize>) -> String) -> Def {
    let p = name.start_position();
    Def { name: text(name.byte_range()), kind, line: p.row, col: p.column, at: name.start_byte() }
}

fn push_def(n: Node, text: &dyn Fn(Range<usize>) -> String, out: &mut Vec<Def>) {
    let k = n.kind();
    // Kotlin のプロパティ（val / var）
    if k == "property_declaration" {
        if !inside_function(n) {
            let mut c = n.walk();
            for ch in n.named_children(&mut c) {
                if ch.kind() == "variable_declaration" {
                    if let Some(id) = ch.named_child(0) {
                        out.push(def_from(id, DefKind::Variable, text));
                    }
                }
            }
        }
        return;
    }
    // Python のモジュール直下の代入（NAME = ...）
    if k == "assignment" && n.parent().is_some_and(|p| p.kind() == "expression_statement" && p.parent().is_some_and(|g| g.kind() == "module")) {
        if let Some(left) = n.child_by_field_name("left").filter(|l| l.kind() == "identifier") {
            out.push(def_from(left, DefKind::Variable, text));
        }
        return;
    }
    let Some(mut kind) = def_kind(k) else { return };
    // Rust の field_declaration と Java の field_declaration は形が違う（Java は declarator の中に名前）
    if k == "field_declaration" && n.child_by_field_name("name").is_none() {
        return;
    }
    if k == "variable_declarator" {
        if inside_function(n) {
            return;
        }
        // Java のフィールド、TS の const f = () => …
        if n.parent().is_some_and(|p| p.kind() == "field_declaration") {
            kind = DefKind::Field;
        } else if n.child_by_field_name("value").is_some_and(|v| matches!(v.kind(), "arrow_function" | "function_expression" | "function")) {
            kind = DefKind::Function;
        } else if n.parent().is_some_and(|p| text(p.start_byte()..(p.start_byte() + 5).min(p.end_byte())) == "const") {
            kind = DefKind::Constant;
        }
    }
    let Some(name) = n.child_by_field_name("name") else { return };
    // 名前が識別子でないもの（分割代入など）は入れない
    if name.child_count() > 0 && !matches!(name.kind(), "type_identifier" | "identifier") {
        return;
    }
    out.push(def_from(name, kind, text));
}

/// import / use の中か（取り込んだ名前は束縛として扱わず、定義のあるファイルへ跳ぶ）
fn in_import(mut n: Node) -> bool {
    while let Some(p) = n.parent() {
        let k = p.kind();
        if k.starts_with("import") || matches!(k, "use_declaration" | "named_imports" | "namespace_import") {
            return true;
        }
        if SCOPE_KINDS.contains(&k) {
            return false;
        }
        n = p;
    }
    false
}

/// 束縛の識別子か（引数・ローカル変数・定義の名前）
fn is_binding(id: Node) -> bool {
    if in_import(id) {
        return false;
    }
    let Some(p) = id.parent() else { return false };
    if p.child_by_field_name("name") == Some(id) || p.child_by_field_name("pattern") == Some(id) {
        return true;
    }
    // for (x of …) / Python の for x in / 代入の左辺
    if p.child_by_field_name("left") == Some(id) && matches!(p.kind(), "for_in_statement" | "for_statement" | "for_in_clause" | "assignment") {
        return true;
    }
    BINDING_PARENTS.contains(&p.kind()) && p.child_by_field_name("type") != Some(id) && p.child_by_field_name("value") != Some(id) && p.child_by_field_name("body") != Some(id)
}

/// 束縛が見える範囲（引数は関数全体、ほかは一番内側のブロック）
fn scope_of(id: Node) -> Range<usize> {
    let mut n = id;
    while let Some(p) = n.parent() {
        let k = p.kind();
        // 引数は関数全体（本体を含む）
        if FUNCTION_KINDS.contains(&k) && n.kind() != "statement_block" && n.kind() != "block" {
            // 関数の名前そのものは、関数の外（親のスコープ）で見える
            if p.child_by_field_name("name") != Some(id) {
                return p.byte_range();
            }
        }
        if SCOPE_KINDS.contains(&k) {
            return p.byte_range();
        }
        n = p;
    }
    n.byte_range()
}

/// `at` にある識別子の定義の位置（ファイルの中で見つかれば）。見える束縛のうち一番内側、同じなら手前で一番近いもの
pub fn find_local_definition(tree: &Tree, at: usize, text: &dyn Fn(Range<usize>) -> String) -> Option<usize> {
    let root = tree.root_node();
    let target = root.descendant_for_byte_range(at, at)?;
    if target.child_count() > 0 {
        return None;
    }
    let name = text(target.byte_range());
    if name.trim().is_empty() || !name.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$') {
        return None;
    }
    let mut best: Option<(usize, usize)> = None; // (スコープの大きさ, 位置)
    let mut c = tree.walk();
    'walk: loop {
        let n = c.node();
        if n.child_count() == 0 && n.end_byte() - n.start_byte() == name.len() && is_binding(n) && text(n.byte_range()) == name {
            let scope = scope_of(n);
            let visible = scope.start <= at && at <= scope.end;
            if visible {
                let size = scope.end - scope.start;
                let pos = n.start_byte();
                // 内側のスコープ優先。同じスコープなら、カーソルより手前で一番近いもの（無ければ最初）
                let better = match best {
                    None => true,
                    Some((bs, bp)) => size < bs || (size == bs && pos <= at && (bp > at || pos > bp)),
                };
                if better {
                    best = Some((size, pos));
                }
            }
        }
        if c.goto_first_child() {
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
    best.map(|(_, p)| p)
}

/// 索引用の解析器（スレッドごとに 1 つ持つ）
pub struct DefParser {
    parser: Parser,
}

impl Default for DefParser {
    fn default() -> Self {
        Self { parser: Parser::new() }
    }
}

impl DefParser {
    pub fn defs(&mut self, lang: Lang, src: &str) -> Vec<Def> {
        defs_of_source(lang, src, &mut self.parser)
    }
}

/// ファイルの中身から定義の一覧（索引用。構文木を作って捨てる）
pub fn defs_of_source(lang: Lang, src: &str, parser: &mut Parser) -> Vec<Def> {
    if parser.set_language(&lang.language()).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(src, None) else { return Vec::new() };
    collect_defs(&tree, &|r: Range<usize>| src.get(r).unwrap_or("").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defs(lang: Lang, src: &str) -> Vec<(String, DefKind)> {
        let mut p = Parser::new();
        defs_of_source(lang, src, &mut p).into_iter().map(|d| (d.name, d.kind)).collect()
    }

    fn local(lang: Lang, src: &str, at_text: &str, nth: usize) -> Option<String> {
        let mut p = Parser::new();
        p.set_language(&lang.language()).unwrap();
        let tree = p.parse(src, None).unwrap();
        let at = src.match_indices(at_text).nth(nth).unwrap().0;
        let d = find_local_definition(&tree, at, &|r: Range<usize>| src[r].to_string())?;
        // 定義の行を返す
        Some(src[..d].rsplit('\n').next().unwrap_or("").to_string() + &src[d..].split('\n').next().unwrap_or("").to_string())
    }

    #[test]
    fn definitions_per_language() {
        use DefKind::*;
        let java = "package a;\nclass Foo implements Bar {\n  private int count;\n  Foo() {}\n  void run(int x) { int local = 1; }\n}\ninterface Bar {}\nenum E { A, B }\n";
        assert_eq!(
            defs(Lang::Java, java),
            vec![("Foo".into(), Class), ("count".into(), Field), ("Foo".into(), Method), ("run".into(), Method), ("Bar".into(), Interface), ("E".into(), Enum), ("A".into(), Constant), ("B".into(), Constant)]
        );
        let ts = "export const LIMIT = 3;\nexport const f = (a: number) => a;\nfunction g() { const inner = 1; }\nclass C { m() {} x = 1; }\ninterface I { p: string }\ntype T = number;\nenum K { A }\n";
        let d = defs(Lang::TypeScript, ts);
        for want in [("LIMIT", Constant), ("f", Function), ("g", Function), ("C", Class), ("m", Method), ("x", Field), ("I", Interface), ("p", Field), ("T", Type), ("K", Enum)] {
            assert!(d.contains(&(want.0.into(), want.1)), "{want:?} が無い: {d:?}");
        }
        assert!(!d.iter().any(|(n, _)| n == "inner"), "関数の中の変数は入れない");
        let rs = "struct S { f: u8 }\nenum E { V }\ntrait T {}\nfn main() { let x = 1; }\nconst C: u8 = 1;\nmod m {}\n";
        let d = defs(Lang::Rust, rs);
        for want in [("S", Class), ("f", Field), ("E", Enum), ("V", Constant), ("T", Interface), ("main", Function), ("C", Constant), ("m", Module)] {
            assert!(d.contains(&(want.0.into(), want.1)), "{want:?} が無い: {d:?}");
        }
        let kt = "class A { fun f() {} }\nobject O\nval top = 1\nfun g() { val inner = 2 }\n";
        let d = defs(Lang::Kotlin, kt);
        for want in [("A", Class), ("f", Function), ("O", Class), ("top", Variable), ("g", Function)] {
            assert!(d.contains(&(want.0.into(), want.1)), "{want:?} が無い: {d:?}");
        }
        assert!(!d.iter().any(|(n, _)| n == "inner"));
        let py = "X = 1\nclass P:\n    def m(self): pass\ndef f(a):\n    y = 2\n";
        let d = defs(Lang::Python, py);
        for want in [("X", Variable), ("P", Class), ("m", Function), ("f", Function)] {
            assert!(d.contains(&(want.0.into(), want.1)), "{want:?} が無い: {d:?}");
        }
    }

    #[test]
    fn local_definitions() {
        // 引数と、内側のスコープの同じ名前
        let ts = "const x = 1;\nfunction f(x: number) {\n  { const x = 2; use(x); }\n  return x;\n}\nuse(x);\n";
        let at = |n| local(Lang::TypeScript, ts, "x", n);
        // 3 行目の use(x) の x（4 つ目の x）→ 同じブロックの const x = 2
        assert_eq!(at(3).as_deref(), Some("  { const x = 2; use(x); }"));
        // 4 行目の return x → 引数
        assert_eq!(at(4).as_deref(), Some("function f(x: number) {"));
        // 最後の use(x) → ファイルの const x
        assert_eq!(at(5).as_deref(), Some("const x = 1;"));
        // Java のフィールドとローカル
        let java = "class A {\n  int n;\n  void f(int m) { int k = m + n; k++; }\n}\n";
        assert_eq!(local(Lang::Java, java, "m + n", 0).as_deref(), Some("  void f(int m) { int k = m + n; k++; }"));
        assert_eq!(local(Lang::Java, java, "k++", 0).as_deref(), Some("  void f(int m) { int k = m + n; k++; }"));
        assert_eq!(local(Lang::Java, java, "n;", 1).as_deref(), Some("  int n;"));
        // Rust の let と引数
        let rs = "fn f(a: u8) -> u8 {\n    let b = a;\n    b\n}\n";
        assert_eq!(local(Lang::Rust, rs, "a;", 0).as_deref(), Some("fn f(a: u8) -> u8 {"));
        assert_eq!(local(Lang::Rust, rs, "b\n}", 0).as_deref(), Some("    let b = a;"));
    }
}
