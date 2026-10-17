//! 定義の一覧（シンボル検索・ファイルの構造・プロジェクトの索引）と、ファイルの中の定義ジャンプ（`gd`）。
//!
//! 言語ごとのクエリは書かず、構文木をたどって「名前（`name` の欄）を持つ節」を、節の種類の名前で見分ける。
//! 関数の中の変数は一覧に入れない（索引が細かい名前で埋まらないように）。ファイルの中の定義ジャンプでは、
//! 引数やローカル変数も含めた束縛から、カーソルから見える一番内側のものを選ぶ。

use std::ops::Range;

use tree_sitter::{Node, Parser, Tree};

use std::collections::HashMap;

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
    // 名前・パターンの欄は、定義や束縛を作る節のときだけ（method_invocation の name などは使う側）
    let defining = def_kind(p.kind()).is_some() || BINDING_PARENTS.contains(&p.kind()) || matches!(p.kind(), "property_declaration" | "variable_declaration");
    if defining && (p.child_by_field_name("name") == Some(id) || p.child_by_field_name("pattern") == Some(id)) {
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

/// ファイルの中の束縛（名前 → (見える範囲, 位置)）。構文木を 1 回たどって集める
pub fn local_bindings(tree: &Tree, text: &dyn Fn(Range<usize>) -> String) -> std::collections::HashMap<String, Vec<(Range<usize>, usize)>> {
    let mut out: std::collections::HashMap<String, Vec<(Range<usize>, usize)>> = std::collections::HashMap::new();
    let mut c = tree.walk();
    'walk: loop {
        let n = c.node();
        if n.child_count() == 0 && is_binding(n) {
            let name = text(n.byte_range());
            if name.chars().next().is_some_and(|ch| ch.is_alphabetic() || ch == '_' || ch == '$') {
                out.entry(name).or_default().push((scope_of(n), n.start_byte()));
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
    out
}

/// 束縛の候補から、`at` で見えるもののうち一番内側、同じなら手前で一番近いもの（無ければ最初）
fn pick_binding(cands: &[(Range<usize>, usize)], at: usize) -> Option<usize> {
    let mut best: Option<(usize, usize)> = None; // (スコープの大きさ, 位置)
    for (scope, pos) in cands {
        if !(scope.start <= at && at <= scope.end) {
            continue;
        }
        let (size, pos) = (scope.end - scope.start, *pos);
        let better = match best {
            None => true,
            Some((bs, bp)) => size < bs || (size == bs && pos <= at && (bp > at || pos > bp)),
        };
        if better {
            best = Some((size, pos));
        }
    }
    best.map(|(_, p)| p)
}

/// `at` にある識別子の名前（定義を探せる識別子なら）
fn ident_at(tree: &Tree, at: usize, text: &dyn Fn(Range<usize>) -> String) -> Option<String> {
    let target = tree.root_node().descendant_for_byte_range(at, at)?;
    if target.child_count() > 0 {
        return None;
    }
    let name = text(target.byte_range());
    (!name.trim().is_empty() && name.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')).then_some(name)
}

/// `at` にある識別子の定義の位置（ファイルの中で見つかれば）。見える束縛のうち一番内側、同じなら手前で一番近いもの
pub fn find_local_definition(tree: &Tree, at: usize, text: &dyn Fn(Range<usize>) -> String) -> Option<usize> {
    let name = ident_at(tree, at, text)?;
    let binds = local_bindings(tree, text);
    pick_binding(binds.get(&name)?, at)
}

/// メソッド呼び出しの受け手（`x.foo()` の `x`）。定義の候補を、その型のファイルに絞るのに使う
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Receiver {
    /// this.foo() / Java の受け手なしの foo()（今のクラス）
    This,
    /// 型が分かった（宣言の型・new の型・静的な呼び出しのクラス名）
    Type(String),
    /// 受け手のフィールド（x.f の f。宣言の型は索引で調べる）
    Member(Box<Receiver>, String),
    /// 受け手のメソッドの戻り値（x.m() の m）
    Call(Box<Receiver>, String),
}

/// `at`（呼び出しの名前の位置）の呼び出しの引数の数
pub fn call_arg_count(tree: &Tree, at: usize) -> Option<usize> {
    let n = tree.root_node().descendant_for_byte_range(at, at)?;
    let mut call = n.parent()?;
    if call.kind() == "member_expression" {
        call = call.parent()?;
    }
    if !matches!(call.kind(), "method_invocation" | "call_expression") {
        return None;
    }
    let args = call.child_by_field_name("arguments")?;
    let mut c = args.walk();
    Some(args.named_children(&mut c).filter(|a| a.kind() != "comment").count())
}

/// 呼び出しの引数の型（分かる範囲で）
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Arg {
    /// リテラル（"String" "int" "double" "boolean" "char" "null"）
    Lit(&'static str),
    /// 変数・new・キャストなど（型は索引でたどる）
    Expr(Receiver),
    Unknown,
}

/// `at`（呼び出しの名前の位置）の呼び出しの引数
pub fn call_args(tree: &Tree, at: usize, text: &dyn Fn(Range<usize>) -> String, binds: &HashMap<String, Vec<(Range<usize>, usize)>>) -> Option<Vec<Arg>> {
    let n = tree.root_node().descendant_for_byte_range(at, at)?;
    let mut call = n.parent()?;
    if call.kind() == "member_expression" {
        call = call.parent()?;
    }
    if !matches!(call.kind(), "method_invocation" | "call_expression") {
        return None;
    }
    let args = call.child_by_field_name("arguments")?;
    let mut c = args.walk();
    let out = args
        .named_children(&mut c)
        .filter(|a| a.kind() != "comment")
        .map(|a| match a.kind() {
            "string_literal" | "string" | "template_string" | "text_block" => Arg::Lit("String"),
            "decimal_integer_literal" | "hex_integer_literal" | "octal_integer_literal" | "binary_integer_literal" => {
                Arg::Lit(if text(a.byte_range()).ends_with(['l', 'L']) { "long" } else { "int" })
            }
            "decimal_floating_point_literal" => Arg::Lit("double"),
            "number" => Arg::Lit("number"),
            "true" | "false" => Arg::Lit("boolean"),
            "character_literal" => Arg::Lit("char"),
            "null_literal" | "null" | "undefined" => Arg::Lit("null"),
            _ => expr_type(tree, a, text, binds, 0).map_or(Arg::Unknown, Arg::Expr),
        })
        .collect();
    Some(out)
}

/// `at`（呼び出しの名前の位置）の受け手の型
pub fn receiver_at(tree: &Tree, at: usize, text: &dyn Fn(Range<usize>) -> String, binds: &HashMap<String, Vec<(Range<usize>, usize)>>) -> Option<Receiver> {
    let n = tree.root_node().descendant_for_byte_range(at, at)?;
    let p = n.parent()?;
    let obj = match p.kind() {
        "method_invocation" if p.child_by_field_name("name") == Some(n) => match p.child_by_field_name("object") {
            Some(o) => o,
            None => return Some(Receiver::This),
        },
        "member_expression" if p.child_by_field_name("property") == Some(n) => p.child_by_field_name("object")?,
        _ => return None,
    };
    expr_type(tree, obj, text, binds, 0)
}

fn expr_type(tree: &Tree, e: Node, text: &dyn Fn(Range<usize>) -> String, binds: &HashMap<String, Vec<(Range<usize>, usize)>>, depth: usize) -> Option<Receiver> {
    if depth > 6 {
        return None;
    }
    match e.kind() {
        "this" => Some(Receiver::This),
        "identifier" => {
            let s = text(e.byte_range());
            if let Some(d) = binds.get(&s).and_then(|c| pick_binding(c, e.start_byte())) {
                return declared_type(tree, d, text, binds, depth);
            }
            // 束縛でない大文字の名前は、クラスの静的な呼び出し（Foo.bar()）
            s.chars().next().is_some_and(char::is_uppercase).then_some(Receiver::Type(s))
        }
        // this.x.foo(): フィールド x の型（TS のコンストラクタ引数のフィールドは関数の中にしか見えないので、範囲を問わず探す）。
        // このファイルに無ければ（親クラスのフィールド）、a.b.foo() と同じく索引で調べる
        "field_access" | "member_expression" => {
            let o = e.child_by_field_name("object")?;
            let f = text(e.child_by_field_name("field").or_else(|| e.child_by_field_name("property"))?.byte_range());
            if o.kind() == "this" {
                if let Some(t) = binds.get(&f).and_then(|v| v.iter().find_map(|(_, d)| declared_type(tree, *d, text, binds, depth))) {
                    return Some(t);
                }
            }
            Some(Receiver::Member(Box::new(expr_type(tree, o, text, binds, depth + 1)?), f))
        }
        // a.m().foo(): m の戻り値の型
        "method_invocation" => {
            let m = text(e.child_by_field_name("name")?.byte_range());
            let inner = match e.child_by_field_name("object") {
                Some(o) => expr_type(tree, o, text, binds, depth + 1)?,
                None => Receiver::This,
            };
            Some(Receiver::Call(Box::new(inner), m))
        }
        "call_expression" => {
            let f = e.child_by_field_name("function")?;
            if f.kind() != "member_expression" {
                return None;
            }
            let m = text(f.child_by_field_name("property")?.byte_range());
            Some(Receiver::Call(Box::new(expr_type(tree, f.child_by_field_name("object")?, text, binds, depth + 1)?), m))
        }
        // (x as Foo).bar() / ((Foo) x).bar()
        "as_expression" => type_name(e.named_child(1)?, text).map(Receiver::Type),
        "cast_expression" => type_name(e.child_by_field_name("type")?, text).map(Receiver::Type),
        "object_creation_expression" | "new_expression" => {
            type_name(e.child_by_field_name("type").or_else(|| e.child_by_field_name("constructor"))?, text).map(Receiver::Type)
        }
        "parenthesized_expression" | "non_null_expression" => expr_type(tree, e.named_child(0)?, text, binds, depth + 1),
        _ => None,
    }
}

/// 束縛（`at` の識別子）の宣言の型。型の注釈が無ければ右辺から（`let h = this.headers` / `const c = new Cart()`）
fn declared_type(tree: &Tree, at: usize, text: &dyn Fn(Range<usize>) -> String, binds: &HashMap<String, Vec<(Range<usize>, usize)>>, depth: usize) -> Option<Receiver> {
    let id = tree.root_node().descendant_for_byte_range(at, at)?;
    let mut n = id;
    for _ in 0..3 {
        let a = n.parent()?;
        if matches!(a.kind(), "variable_declarator" | "public_field_definition" | "field_definition") && a.child_by_field_name("type").is_none() {
            if let Some(v) = a.child_by_field_name("value") {
                // フィールドの x = inject(Foo)
                if v.kind() == "call_expression" && v.child_by_field_name("function").is_some_and(|f| text(f.byte_range()) == "inject") {
                    return v.child_by_field_name("arguments")?.named_child(0).and_then(|t| type_name(t, text)).map(Receiver::Type);
                }
                // 自分自身を右辺に含む（let x = x.y）ものは追わない
                if v.byte_range().contains(&at) {
                    return None;
                }
                return expr_type(tree, v, text, binds, depth + 1);
            }
        }
        if let Some(mut t) = a.child_by_field_name("type") {
            if t.kind() == "type_annotation" {
                t = t.named_child(0)?;
            }
            match type_name(t, text) {
                // Java の var: 右辺から
                Some(v) if v == "var" => {
                    let v = n.child_by_field_name("value")?;
                    return expr_type(tree, v, text, binds, depth + 1);
                }
                r => return r.map(Receiver::Type),
            }
        }
        n = a;
    }
    None
}

/// 型の名前（総称型は外側、修飾付きは最後の名前）
fn type_name(t: Node, text: &dyn Fn(Range<usize>) -> String) -> Option<String> {
    match t.kind() {
        "type_identifier" | "identifier" => Some(text(t.byte_range())),
        "generic_type" => type_name(t.child_by_field_name("name").or_else(|| t.named_child(0))?, text),
        "scoped_type_identifier" | "nested_type_identifier" | "member_expression" => {
            let last = t.named_child(u32::try_from(t.named_child_count().checked_sub(1)?).ok()?)?;
            type_name(last, text)
        }
        _ => None,
    }
}

/// 参照の種類（定義ジャンプの的中率を測るため）
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum RefKind {
    /// メソッド・関数の呼び出し
    Call,
    /// 型の参照
    Type,
}

/// 呼び出しと型の参照の位置（名前, 種類, byte）
pub fn reference_sites(tree: &Tree, text: &dyn Fn(Range<usize>) -> String) -> Vec<(String, RefKind, usize)> {
    let mut out = Vec::new();
    let mut c = tree.walk();
    'walk: loop {
        let n = c.node();
        if n.child_count() == 0 && !is_binding(n) && !in_import(n) {
            let kind = match n.kind() {
                "type_identifier" => Some(RefKind::Type),
                "identifier" | "property_identifier" | "field_identifier" => n.parent().and_then(|p| {
                    let call = |q: Node| matches!(q.kind(), "call_expression" | "method_invocation");
                    let is_fn = |q: Node, x: Node| q.child_by_field_name("function") == Some(x) || q.child_by_field_name("name") == Some(x);
                    if call(p) && is_fn(p, n) {
                        return Some(RefKind::Call);
                    }
                    let member = matches!(p.kind(), "member_expression" | "field_expression" | "scoped_identifier");
                    let prop = p.child_by_field_name("property") == Some(n) || p.child_by_field_name("field") == Some(n) || p.child_by_field_name("name") == Some(n);
                    match p.parent() {
                        Some(g) if member && prop && call(g) && is_fn(g, p) => Some(RefKind::Call),
                        _ => None,
                    }
                }),
                _ => None,
            };
            if let Some(k) = kind {
                out.push((text(n.byte_range()), k, n.start_byte()));
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
    out
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

    /// 参照ごとに (名前, 種類, ファイルの中で定義が見つかったか)（的中率の計測用）
    pub fn reference_report(&mut self, lang: Lang, src: &str) -> Vec<(String, RefKind, bool, Option<Receiver>, Option<Vec<Arg>>)> {
        if self.parser.set_language(&lang.language()).is_err() {
            return Vec::new();
        }
        let Some(tree) = self.parser.parse(src, None) else { return Vec::new() };
        let text = |r: Range<usize>| src.get(r).unwrap_or("").to_string();
        // 束縛の一覧は 1 回だけ作る（参照ごとに構文木をたどると、大きなファイルで終わらなくなる）
        let binds = local_bindings(&tree, &text);
        reference_sites(&tree, &text)
            .into_iter()
            .map(|(name, k, at)| {
                let local = ident_at(&tree, at, &text).and_then(|n| pick_binding(binds.get(&n)?, at)).is_some_and(|d| d != at);
                let recv = if k == RefKind::Call { receiver_at(&tree, at, &text, &binds) } else { None };
                let args = if k == RefKind::Call { call_args(&tree, at, &text, &binds) } else { None };
                (name, k, local, recv, args)
            })
            .collect()
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

    fn recv(lang: Lang, src: &str, call: &str) -> Option<Receiver> {
        let mut p = Parser::new();
        p.set_language(&lang.language()).unwrap();
        let tree = p.parse(src, None).unwrap();
        let at = src.find(call).unwrap();
        let text = |r: Range<usize>| src[r].to_string();
        receiver_at(&tree, at, &text, &local_bindings(&tree, &text))
    }

    #[test]
    fn receivers() {
        let t = |s: &str| Some(Receiver::Type(s.into()));
        let java = "class A {\n  private UserService users;\n  void f(OrderRepo repo, List<Item> xs) {\n    users.find1();\n    repo.save2();\n    var b = new Builder();\n    b.build3();\n    Util.parse4();\n    run5();\n    this.users.find6();\n    xs.get7();\n    new Foo().go8();\n    make().go9();\n  }\n}\n";
        assert_eq!(recv(Lang::Java, java, "find1"), t("UserService"));
        assert_eq!(recv(Lang::Java, java, "save2"), t("OrderRepo"));
        assert_eq!(recv(Lang::Java, java, "build3"), t("Builder"));
        assert_eq!(recv(Lang::Java, java, "parse4"), t("Util"));
        assert_eq!(recv(Lang::Java, java, "run5"), Some(Receiver::This));
        assert_eq!(recv(Lang::Java, java, "find6"), t("UserService"));
        assert_eq!(recv(Lang::Java, java, "get7"), t("List"));
        assert_eq!(recv(Lang::Java, java, "go8"), t("Foo"));
        assert_eq!(recv(Lang::Java, java, "go9"), Some(Receiver::Call(Box::new(Receiver::This), "make".into())));
        let java2 = "class B {\n  void f(Req req) {\n    req.getJokn().getCd1();\n    ex.createCriteria().andNo2();\n    ((Foo) o).bar3();\n  }\n}\n";
        let call = |r: Receiver, m: &str| Some(Receiver::Call(Box::new(r), m.into()));
        assert_eq!(recv(Lang::Java, java2, "getCd1"), call(Receiver::Type("Req".into()), "getJokn"));
        assert_eq!(recv(Lang::Java, java2, "andNo2"), None, "ex は束縛でも大文字でもないので分からない");
        assert_eq!(recv(Lang::Java, java2, "bar3"), t("Foo"));
        let ts = "class P {\n  private api = inject(ApiService);\n  constructor(private users: UserService) {}\n  f(o: Order) {\n    this.users.load1();\n    this.api.get2();\n    o.total3();\n    this.refresh4();\n    const c = new Cart();\n    c.add5();\n    helper6();\n  }\n}\n";
        assert_eq!(recv(Lang::TypeScript, ts, "load1"), t("UserService"));
        assert_eq!(recv(Lang::TypeScript, ts, "get2"), t("ApiService"));
        assert_eq!(recv(Lang::TypeScript, ts, "total3"), t("Order"));
        assert_eq!(recv(Lang::TypeScript, ts, "refresh4"), Some(Receiver::This));
        assert_eq!(recv(Lang::TypeScript, ts, "add5"), t("Cart"));
        assert_eq!(recv(Lang::TypeScript, ts, "helper6"), None);
        let ts2 = "class Q extends Base {\n  f() {\n    this.store.set1(1);\n    this.route.snapshot.get2('a');\n    this.form()?.get3('x');\n    (c as FormControl).reset4();\n  }\n}\n";
        let mem = |r: Receiver, f: &str| Receiver::Member(Box::new(r), f.into());
        assert_eq!(recv(Lang::TypeScript, ts2, "set1"), Some(mem(Receiver::This, "store")));
        assert_eq!(recv(Lang::TypeScript, ts2, "get2"), Some(mem(mem(Receiver::This, "route"), "snapshot")));
        assert_eq!(recv(Lang::TypeScript, ts2, "get3"), Some(Receiver::Call(Box::new(Receiver::This), "form".into())));
        assert_eq!(recv(Lang::TypeScript, ts2, "reset4"), t("FormControl"));
        let ts3 = "class S {\n  f() {\n    let h = this.defaultHeaders;\n    h = h.set1('a');\n    const r = svc.load();\n    r.go2();\n  }\n}\n";
        assert_eq!(recv(Lang::TypeScript, ts3, "set1"), Some(mem(Receiver::This, "defaultHeaders")));
        assert_eq!(recv(Lang::TypeScript, ts3, "go2"), None, "svc が分からないので r も分からない");
        // 引数の型
        let java4 = "class C {\n  void f(Date d, LocalDate ld) {\n    DateUtil.format(d, \"x\");\n    DateUtil.format(ld, 3L, null, new Foo(), true, other);\n  }\n}\n";
        let mut p = Parser::new();
        p.set_language(&Lang::Java.language()).unwrap();
        let tree = p.parse(java4, None).unwrap();
        let text = |r: Range<usize>| java4[r].to_string();
        let binds = local_bindings(&tree, &text);
        let a1 = call_args(&tree, java4.find("format").unwrap(), &text, &binds).unwrap();
        assert_eq!(a1, vec![Arg::Expr(Receiver::Type("Date".into())), Arg::Lit("String")]);
        let a2 = call_args(&tree, java4.rfind("format").unwrap(), &text, &binds).unwrap();
        assert_eq!(a2, vec![Arg::Expr(Receiver::Type("LocalDate".into())), Arg::Lit("long"), Arg::Lit("null"), Arg::Expr(Receiver::Type("Foo".into())), Arg::Lit("boolean"), Arg::Unknown]);
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
