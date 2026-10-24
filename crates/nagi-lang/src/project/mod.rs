//! プロジェクトのソースの文字から型を求める部分（言語をまたぐ）

mod rank;

pub use rank::*;

/// 変数・引数・フィールドの宣言の書き方（言語が決める）
#[derive(Clone, Copy, Debug)]
pub struct VarStyles {
    /// `Type name`（Java・C#）
    pub type_first: bool,
    /// `name: Type`（TypeScript・Kotlin）
    pub colon: bool,
    /// `var・val・let・const name = new Type(` / `Type(`
    pub init: bool,
}

/// どの書き方も読む（言語が決めていないとき）
pub const ANY_STYLE: VarStyles = VarStyles { type_first: true, colon: true, init: true };

/// import の書き方（言語が決める）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportStyle {
    /// 完全な名前（`import a.b.C;`。Java・Kotlin）
    Dotted,
    /// モジュールの指定（`import { A } from './x'`。TypeScript・JavaScript）
    Path,
    /// どちらも
    Both,
}

/// `text` の中の `name` の変数・引数・フィールドの宣言の型（`at` より前の最後の宣言、無ければ後ろの最初。どの書き方も読む）
pub fn var_type(text: &str, name: &str, at: usize) -> Option<String> {
    var_type_with(ANY_STYLE, text, name, at)
}

/// `var_type` を、書き方 `st` だけで
pub fn var_type_with(st: VarStyles, text: &str, name: &str, at: usize) -> Option<String> {
    // Java・C#: Type name / Type<..> name / Type[] name（宣言・引数・フィールド）。型引数と配列は外す
    let mut all: Vec<(usize, String)> = Vec::new();
    if st.type_first {
        all.extend(crate::java::types::typed_decls(text, name).into_iter().map(|(p, t)| (p, t.split(['<', '[']).next().unwrap_or("").rsplit('.').next().unwrap_or("").to_string())));
    }
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
        let on = [st.colon, st.init];
        for (re, _) in pats.iter().zip(on).filter(|x| x.1) {
            all.extend(re.captures_iter(text).filter(|c| &c[1] == name).map(|c| (c.get(0).map_or(0, |m| m.start()), c[2].to_string())));
        }
    }
    crate::java::types::pick(all, at)
}

/// 何でも受ける引数の型か（言語が決めていないとき: Object・any・unknown・1 文字の型変数）
pub fn loose_param_any(p: &str) -> bool {
    matches!(p, "Object" | "any" | "unknown") || p.chars().count() == 1
}

/// 値（字句で分かる種類 `lit`: String・int・long・double・number・boolean・char・null）と、引数の宣言の型 `p` の合い方
/// （2: 同じ、1: 広げれば合う、0: 分からない、負: 合わない）。言語が決めていないとき（Java と TypeScript の名前を両方）
pub fn literal_fit_any(lit: &str, p: &str) -> i32 {
    let numeric = ["int", "long", "short", "byte", "double", "float", "Integer", "Long", "Short", "Byte", "Double", "Float", "Number", "number", "BigDecimal", "BigInteger"];
    match lit {
        "String" => if matches!(p, "String" | "CharSequence" | "string") { 2 } else { -3 },
        "int" => match p {
            "int" | "Integer" | "number" => 2,
            _ if numeric.contains(&p) => 1,
            _ => -3,
        },
        "long" => match p {
            "long" | "Long" => 2,
            "float" | "double" | "Float" | "Double" | "Number" => 1,
            _ => -3,
        },
        "double" => match p {
            "double" | "Double" | "number" => 2,
            "float" | "Float" | "Number" | "BigDecimal" => 1,
            _ => -3,
        },
        "number" => if numeric.contains(&p) { 2 } else { -3 },
        "boolean" => if matches!(p, "boolean" | "Boolean") { 2 } else { -3 },
        "char" => match p {
            "char" | "Character" => 2,
            "int" | "long" => 1,
            _ => -3,
        },
        "null" => if ["int", "long", "short", "byte", "double", "float", "boolean", "char"].contains(&p) { -3 } else { 0 },
        _ => 0,
    }
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
