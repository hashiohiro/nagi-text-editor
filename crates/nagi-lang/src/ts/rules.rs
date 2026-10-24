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
