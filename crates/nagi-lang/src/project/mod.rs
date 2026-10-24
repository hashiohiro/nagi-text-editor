//! プロジェクトのソースの文字から型を求める部分（言語をまたぐ）

mod rank;

pub use rank::*;

/// `text` の中の `name` の変数・引数・フィールドの宣言の型（`at` より前の最後の宣言、無ければ後ろの最初）
pub fn var_type(text: &str, name: &str, at: usize) -> Option<String> {
    // Java・C#: Type name / Type<..> name / Type[] name（宣言・引数・フィールド）。型引数と配列は外す
    let mut all: Vec<(usize, String)> = crate::java::types::typed_decls(text, name).into_iter().map(|(p, t)| (p, t.split(['<', '[']).next().unwrap_or("").rsplit('.').next().unwrap_or("").to_string())).collect();
    // 名前を入れずに一度だけ作り、合ったものの名前を比べる（呼ぶたびに作ると遅い）
    static PATS: std::sync::OnceLock<[regex::Regex; 2]> = std::sync::OnceLock::new();
    let pats = PATS.get_or_init(|| {
        [
            // TypeScript・Kotlin: name: Type
            regex::Regex::new(r"([\w$]+)\s*\??:\s*([A-Z]\w*)").unwrap(),
            // var・val・let・const name = new Type( / Type(
            regex::Regex::new(r"\b(?:var|val|let|const|auto)\s+([\w$]+)\s*=\s*(?:new\s+)?([A-Z]\w*)").unwrap(),
        ]
    });
    if text.contains(name) {
        for re in pats {
            all.extend(re.captures_iter(text).filter(|c| &c[1] == name).map(|c| (c.get(0).map_or(0, |m| m.start()), c[2].to_string())));
        }
    }
    crate::java::types::pick(all, at)
}

/// C 系の言語（型を名前の前に書く）: 名前の前の型（`public Criteria createCriteria(` / `private Foo bar;` → Criteria / Foo）。総称型は外側
pub fn c_decl_type(before: &str) -> Option<String> {
    let mut b = before.trim_end();
    // 配列は分からないものとする
    if b.ends_with(']') {
        return None;
    }
    if b.ends_with('>') {
        let mut depth = 0;
        let mut cut = None;
        for (i, c) in b.char_indices().rev() {
            match c {
                '>' => depth += 1,
                '<' => {
                    depth -= 1;
                    if depth == 0 {
                        cut = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        b = b[..cut?].trim_end();
    }
    let word: String = b.chars().rev().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$' || *c == '.').collect::<Vec<_>>().into_iter().rev().collect();
    let last = word.rsplit('.').next().unwrap_or("").to_string();
    let not_type = ["class", "interface", "enum", "record", "void", "public", "private", "protected", "static", "final", "abstract", "synchronized", "default", "native", "transient", "volatile", "int", "long", "short", "byte", "char", "boolean", "float", "double", "var", "return", "new"];
    (!last.is_empty() && last.chars().next().is_some_and(|c| c.is_alphabetic()) && !not_type.contains(&last.as_str())).then_some(last)
}

/// C 系の言語: 引数 1 つの宣言 `p`（`final List<Foo> xs`）の型（注釈・final を除き、名前の前の型。総称型は外側、可変長の ... と配列は外す）
pub fn c_param_type(p: &str) -> Option<String> {
    let mut flat = String::new();
    let mut d = 0;
    for c in p.chars() {
        match c {
            '<' | '(' => d += 1,
            '>' | ')' => d -= 1,
            _ if d == 0 => flat.push(c),
            _ => {}
        }
    }
    let words: Vec<&str> = flat.split_whitespace().filter(|w| !w.starts_with('@') && *w != "final").collect();
    let ty = words.len().checked_sub(2).map(|i| words[i])?;
    let ty = ty.trim_end_matches("...").trim_end_matches("[]");
    Some(ty.rsplit('.').next().unwrap_or(ty).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn var_types() {
        let src = "class A {\n  private UserService userService;\n  void f(List<Order> orders, String name) {\n    var repo = new OrderRepo();\n    let x: Item = y;\n  }\n}";
        assert_eq!(var_type(src, "userService", 200).as_deref(), Some("UserService"));
        assert_eq!(var_type(src, "orders", 200).as_deref(), Some("List"));
        assert_eq!(var_type(src, "repo", 200).as_deref(), Some("OrderRepo"));
        assert_eq!(var_type(src, "x", 200).as_deref(), Some("Item"));
        // 受け手（変数の型から）
        use nagi_syntax::defs::Receiver;
        let var = |n: &str| var_type(src, n, 200);
        let rb = |line: &str, dot: usize| crate::text::receiver_before(crate::java::types::LX, line, dot, &var);
        let line = "    userService.find(id).getItems().";
        assert_eq!(rb(line, line.len() - 1), Some(Receiver::Call(Box::new(Receiver::Call(Box::new(Receiver::Type("UserService".into())), "find".into())), "getItems".into())));
        assert_eq!(rb("this.", 4), Some(Receiver::This));
        assert_eq!(rb("  Math.", 6), Some(Receiver::Type("Math".into())));
    }
}
