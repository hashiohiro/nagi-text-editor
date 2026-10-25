//! TypeScript のプロジェクトの規則（Ranker が言語の差し込み口から呼ぶ）: 名前の後ろの型、引数の型

/// TS: 名前の後ろの型（`f: Foo` / `m(a, b): Foo {` / `x = inject(Foo)` / `x = new Foo(`）
pub fn ts_decl_type(after: &str) -> Option<String> {
    let mut a = after.trim_start_matches(|c: char| c.is_alphanumeric() || c == '_' || c == '$').trim_start().trim_start_matches(['?', '!']).trim_start();
    if a.starts_with('(') || a.starts_with('<') {
        // 引数の ( … ) を飛ばす
        let mut depth = 0;
        let mut end = None;
        for (i, c) in a.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        a = a[end? + 1..].trim_start();
    }
    let ident = |s: &str| -> Option<String> {
        let w: String = s.trim_start().chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$' || *c == '.').collect();
        let last = w.rsplit('.').next().unwrap_or("").to_string();
        let prim = ["string", "number", "boolean", "any", "unknown", "void", "never", "object", "null", "undefined"];
        (!last.is_empty() && !prim.contains(&last.as_str())).then_some(last)
    };
    if let Some(t) = a.strip_prefix(':') {
        return ident(t);
    }
    let v = a.strip_prefix('=')?.trim_start();
    if let Some(x) = v.strip_prefix("inject(") {
        return ident(x);
    }
    if let Some(x) = v.strip_prefix("new ") {
        return ident(x);
    }
    None
}

/// 引数 1 つの宣言 `p`（`a: Foo = x`）の型
pub fn ts_param_type(p: &str) -> Option<String> {
    let t = p.split_once(':')?.1;
    let t = t.split('=').next().unwrap_or(t);
    let w: String = t.trim().chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$').collect();
    (!w.is_empty()).then_some(w)
}

/// 何でも受ける引数の型（any・unknown と型変数）
pub fn loose_param(p: &str) -> bool {
    matches!(p, "any" | "unknown") || p.chars().count() == 1
}

/// 値（字句で分かる種類: String・number・boolean・null）と引数の宣言の型の合い方
pub fn literal_fit(lit: &str, p: &str) -> i32 {
    match lit {
        "String" => if matches!(p, "string" | "String") { 2 } else { -3 },
        "number" => if matches!(p, "number" | "Number") { 2 } else { -3 },
        "boolean" => if matches!(p, "boolean" | "Boolean") { 2 } else { -3 },
        "null" => if p == "boolean" { -3 } else { 0 },
        _ => 0,
    }
}

const TYPES: &[&str] = &["string", "number", "boolean", "any", "unknown", "never", "void", "object", "symbol", "bigint", "undefined", "null", "keyof", "typeof", "readonly"];

/// 今の場所に書ける予約語（`kinds` は構文木の節の種類、内側から外へ）。決められなければ None
pub fn keywords_here(kinds: &[&str], src: &str, line: &str, ws: usize, at: usize) -> Option<(Vec<&'static str>, bool)> {
    use crate::text::{block_head, ends_with_word, paren_keyword};
    let code = crate::text::blank_literals(super::types::LX, &src[..at]);
    let prev = code.trim_end();
    let last = prev.chars().last().unwrap_or('\n');
    let stmt = &prev[prev.rfind([';', '{', '}']).map_or(0, |i| i + 1)..];
    let fn_edge = ["function_declaration", "method_definition", "arrow_function", "function_expression", "function", "class_body", "program"];
    let container = kinds.iter().find_map(|k| match *k {
        "type_annotation" | "type_arguments" | "type_alias_declaration" | "union_type" | "intersection_type" | "array_type" | "generic_type" => Some('y'),
        "object" | "object_pattern" => Some('o'),
        "statement_block" | "switch_body" | "switch_case" | "switch_default" => Some('s'),
        "class_body" => Some('c'),
        "interface_body" | "object_type" => Some('i'),
        "program" => Some('t'),
        _ => None,
    })?;
    let within = |want: &[&str]| kinds.iter().take_while(|k| !fn_edge.contains(k)).any(|k| want.contains(k));
    let prev_word = &prev[prev.rfind(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$')).map_or(0, |i| i + 1)..];
    if container == 'y' || last == ':' && ["required_parameter", "optional_parameter", "public_field_definition", "property_signature"].iter().any(|k| kinds.contains(k)) {
        // 型の所は型の予約語を名前より先に
        return Some((TYPES.to_vec(), true));
    }
    if ["new", "extends", "implements", "instanceof", "interface", "type", "enum", "class", "function", "namespace"].contains(&prev_word) {
        return Some((Vec::new(), false));
    }
    let expr = ["new", "this", "super", "null", "true", "false", "undefined", "typeof", "await", "async", "function", "void"];
    // コンストラクタの引数の修飾子
    if matches!(last, '(' | ',') && kinds.contains(&"formal_parameters") && ends_with_word(stmt.split('(').next().unwrap_or(""), "constructor") {
        return Some((vec!["public", "private", "protected", "readonly"], false));
    }
    // case の後の : は文の頭
    let prev_line = prev[prev.rfind('\n').map_or(0, |i| i + 1)..].trim_start();
    let case_label = last == ':' && (prev_line.starts_with("case ") || prev_line.starts_with("default"));
    if !case_label && "=(,?:!&|+-*/%<>[^~".contains(last) || ["return", "throw", "yield", "await", "case", "of", "in"].iter().any(|w| ends_with_word(prev, w)) {
        return Some((if container == 'o' && matches!(last, ',' | '{') { Vec::new() } else { expr.to_vec() }, false));
    }
    let after_value = (last.is_alphanumeric() || last == '_' || last == '$' || last == ')' || last == ']') && !super::lang::KEYWORDS.contains(&prev_word);
    if after_value && line[..ws].ends_with(' ') {
        if ["class ", "interface "].iter().any(|k| stmt.contains(k)) && !stmt.contains('=') {
            return Some((vec!["extends", "implements"], false));
        }
        if ["=", "(", "return", "&&", "||", "?"].iter().any(|k| stmt.contains(k)) && paren_keyword(prev).is_none_or(|k| !["if", "for", "while", "switch", "catch"].contains(&k)) {
            return Some((vec!["as", "instanceof", "in", "satisfies"], false));
        }
        if last != ')' {
            return Some((Vec::new(), false));
        }
    }
    let mut v: Vec<&'static str> = Vec::new();
    match container {
        's' | 't' => {
            v.extend(["if", "for", "while", "do", "switch", "try", "return", "throw", "const", "let", "var", "function", "class", "async", "await", "new", "this", "super", "typeof", "delete"]);
            if within(&["for_statement", "for_in_statement", "while_statement", "do_statement"]) {
                v.extend(["break", "continue"]);
            } else if within(&["switch_body"]) {
                v.push("break");
            }
            if kinds.iter().find(|k| ["statement_block", "switch_body"].contains(k)) == Some(&"switch_body") || kinds.first().is_some_and(|k| *k == "switch_body") {
                v.extend(["case", "default"]);
            }
            if container == 't' {
                v.extend(["import", "export", "interface", "type", "enum", "declare", "abstract", "namespace"]);
            }
            if let Some(head) = block_head(prev) {
                if ends_with_word(head, "try") || paren_keyword(head) == Some("catch") || ends_with_word(head, "catch") {
                    v.splice(0..0, ["catch", "finally"]);
                } else if paren_keyword(head) == Some("if") {
                    v.insert(0, "else");
                } else if ends_with_word(head, "do") {
                    v.insert(0, "while");
                }
            }
        }
        'c' => v.extend(["public", "private", "protected", "static", "readonly", "abstract", "override", "async", "get", "set", "constructor", "declare"]),
        'i' => v.push("readonly"),
        _ => {}
    }
    // 文・メンバーの頭の予約語は、名前より先に
    Some((v, true))
}
