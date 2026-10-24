//! プロジェクトのソースの文字から型を求める部分（言語をまたぐ）

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
