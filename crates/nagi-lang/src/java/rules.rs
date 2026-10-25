//! Java のプロジェクトの規則（Ranker が言語の差し込み口から呼ぶ）: JDK のよくある型の親、Lombok・record の書いていないメンバー、
//! 宣言の型を型引数ごと読む

use nagi_syntax::defs::DefKind;
use nagi_workers::index::Entry;

/// JDK のよくある型の親（オーバーロードを引数の型で選ぶため）
pub fn jdk_supers(t: &str) -> Option<&'static [&'static str]> {
    Some(match t {
        "LocalDate" | "LocalDateTime" | "LocalTime" | "ZonedDateTime" | "OffsetDateTime" | "OffsetTime" | "Instant" | "Year" | "YearMonth" => &["Temporal", "TemporalAccessor", "Comparable", "Serializable"],
        "Temporal" => &["TemporalAccessor"],
        "Timestamp" => &["Date"],
        "Date" | "Calendar" => &["Serializable", "Comparable"],
        "Integer" | "Long" | "Short" | "Byte" | "Double" | "Float" | "BigDecimal" | "BigInteger" => &["Number", "Comparable", "Serializable"],
        "String" => &["CharSequence", "Comparable", "Serializable"],
        "StringBuilder" | "StringBuffer" => &["CharSequence"],
        "ArrayList" | "LinkedList" => &["List"],
        "List" | "Set" | "Queue" => &["Collection"],
        "HashSet" | "LinkedHashSet" | "TreeSet" => &["Set"],
        "Collection" => &["Iterable"],
        "HashMap" | "LinkedHashMap" | "TreeMap" | "ConcurrentHashMap" => &["Map"],
        _ => return None,
    })
}

/// 表の親の側に出てくる型（親が分かっているものとして扱う）
pub fn jdk_known(t: &str) -> bool {
    matches!(t, "Temporal" | "TemporalAccessor" | "Number" | "CharSequence" | "Map" | "Iterable" | "Comparable" | "Serializable" | "int" | "long" | "short" | "byte" | "double" | "float" | "boolean" | "char")
}

/// getFoo / isFoo / setFoo → foo
pub fn accessor_field(name: &str) -> Option<String> {
    let rest = name.strip_prefix("get").or_else(|| name.strip_prefix("set")).or_else(|| name.strip_prefix("is"))?;
    let mut c = rest.chars();
    let first = c.next().filter(|c| c.is_uppercase())?;
    Some(first.to_lowercase().chain(c).collect())
}

/// Java: 名前の前の型を型引数・配列ごと（`List<Foo>`・`String[]`）
pub fn java_decl_text(before: &str) -> Option<String> {
    let b = before.trim_end();
    let mut i = b.len();
    while b[..i].ends_with("[]") {
        i -= 2;
    }
    if b[..i].ends_with('>') {
        let mut depth = 0;
        let mut cut = None;
        for (k, c) in b[..i].char_indices().rev() {
            match c {
                '>' => depth += 1,
                '<' => {
                    depth -= 1;
                    if depth == 0 {
                        cut = Some(k);
                        break;
                    }
                }
                _ => {}
            }
        }
        i = cut?;
    }
    let head = b[..i].char_indices().rev().take_while(|(_, c)| c.is_alphanumeric() || *c == '_' || *c == '$' || *c == '.').last()?.0;
    crate::project::c_decl_type(&b[..i])?;
    Some(b[head..].to_string())
}

/// `record Name(部品, …)` の部品の名前（型の定義の行 `line` から、括弧が閉じるまで）
fn record_components(lines: &[String], line: usize, name: &str) -> Vec<String> {
    let Some(first) = lines.get(line) else { return Vec::new() };
    let Some(at) = first.find(&format!("record {name}")) else { return Vec::new() };
    let mut text = String::new();
    for l in std::iter::once(&first[at..]).chain(lines.iter().skip(line + 1).take(20).map(String::as_str)) {
        text.push_str(l);
        text.push(' ');
        if l.contains(')') {
            break;
        }
    }
    let Some(open) = text.find('(') else { return Vec::new() };
    let mut depth = 0;
    let mut body = String::new();
    for c in text[open..].chars() {
        match c {
            '(' | '<' => depth += 1,
            ')' | '>' => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            break;
        }
        body.push(if c == ',' && depth == 1 { '\n' } else { c });
    }
    body.trim_start_matches('(')
        .split('\n')
        .filter_map(|p| p.split_whitespace().last().map(|w| w.trim_matches(|c: char| !(c.is_alphanumeric() || c == '_')).to_string()))
        .filter(|n| !n.is_empty())
        .collect()
}

/// `line` の宣言に付いた注釈の名前（前の行の `@Data`・`@Getter(AccessLevel.NONE)` と、同じ行の頭のもの。`lombok.Data` は Data）
fn annotations_before(lines: &[String], line: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut take = |l: &str| {
        for (i, _) in l.match_indices('@') {
            let name: String = l[i + 1..].chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.').collect();
            out.push(name.rsplit('.').next().unwrap_or("").to_string());
        }
    };
    if let Some(l) = lines.get(line) {
        // 同じ行は宣言の名前より前（文字列の中の @ は気にしない）
        take(l.split('=').next().unwrap_or(""));
    }
    for l in lines[..line.min(lines.len())].iter().rev().take(12) {
        let t = l.trim();
        if t.is_empty() || t.starts_with("//") || t.starts_with('*') || t.starts_with("/*") {
            continue;
        }
        // 注釈の続き（`@Schema(` の次の行など）も見る。宣言・文の終わりで止める
        if t.ends_with(';') || t.ends_with('{') || t.ends_with('}') {
            break;
        }
        take(t);
    }
    out
}

/// 書いていないメンバー（名前, 引数の数）: record の部品の読み出し（`record P(String title, int n)` の title()・n()）、
/// Lombok（@Data・@Getter・@Setter・@Value をクラスかフィールドに。@Builder の builder()）。`line` は型 `ty` の定義の行、`defs` は型の中の定義
pub fn implicit_members(lines: &[String], line: usize, ty: &str, defs: &[Entry]) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = record_components(lines, line, ty).into_iter().map(|n| (n, 0)).collect();
    let class_ann = annotations_before(lines, line);
    let has = |ann: &[String], a: &str| ann.iter().any(|x| x == a);
    if has(&class_ann, "Builder") {
        out.push(("builder".into(), 0));
    }
    for d in defs {
        if d.kind != DefKind::Field {
            continue;
        }
        let Some(decl) = lines.get(d.line as usize).map(|l| &l[..(d.col as usize).min(l.len())]) else { continue };
        let words: Vec<&str> = decl.split_whitespace().collect();
        if words.contains(&"static") {
            continue;
        }
        let field_ann = annotations_before(lines, d.line as usize);
        let either = |a: &str| has(&class_ann, a) || has(&field_ann, a);
        let getter = either("Data") || either("Getter") || has(&class_ann, "Value");
        let setter = (either("Data") || either("Setter")) && !words.contains(&"final") && !has(&class_ann, "Value");
        let cap = d.name.chars().next().map(|c| c.to_uppercase().collect::<String>() + &d.name[c.len_utf8()..]).unwrap_or_default();
        if getter {
            out.push((format!("{}{cap}", if words.last() == Some(&"boolean") { "is" } else { "get" }), 0));
        }
        if setter {
            out.push((format!("set{cap}"), 1));
        }
    }
    out
}

/// Java のソースのパッケージ（`m/src/main/java/com/x/Foo.java` → `com/x`。`java/` の下でなければ None）
pub fn source_package(rel: &str) -> Option<String> {
    let dir = format!("/{}/", rel.rsplit_once('/')?.0);
    let at = dir.find("/java/")? + 6;
    Some(dir[at.min(dir.len())..].trim_end_matches('/').to_string())
}

/// プロジェクトの Java のソースのパッケージか、その上（`com`・`com/x`）か
pub fn is_project_package(idx: &nagi_workers::index::Index, pkg: &str) -> bool {
    let v = idx.derived("java.packages", |i| {
        let mut v: Vec<String> = i.files().filter(|f| f.ends_with(".java")).filter_map(source_package).collect();
        v.sort_unstable();
        v.dedup();
        v
    });
    let i = v.partition_point(|p| p.as_str() < pkg);
    v.get(i).is_some_and(|p| p == pkg || p.strip_prefix(pkg).is_some_and(|r| r.starts_with('/')))
}

/// 何でも受ける引数の型（Object と型変数）
pub fn loose_param(p: &str) -> bool {
    p == "Object" || p.chars().count() == 1
}

/// 値（字句で分かる種類）と引数の宣言の型の合い方（2 同じ・1 広げれば合う・0 分からない・負 合わない）
pub fn literal_fit(lit: &str, p: &str) -> i32 {
    let numeric = ["int", "long", "short", "byte", "double", "float", "Integer", "Long", "Short", "Byte", "Double", "Float", "Number", "BigDecimal", "BigInteger"];
    match lit {
        "String" => if matches!(p, "String" | "CharSequence") { 2 } else { -3 },
        "int" => match p {
            "int" | "Integer" => 2,
            _ if numeric.contains(&p) => 1,
            _ => -3,
        },
        "long" => match p {
            "long" | "Long" => 2,
            "float" | "double" | "Float" | "Double" | "Number" => 1,
            _ => -3,
        },
        "double" => match p {
            "double" | "Double" => 2,
            "float" | "Float" | "Number" | "BigDecimal" => 1,
            _ => -3,
        },
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

/// `case ` の後（`case A, ` も）なら、その switch の調べる式（`switch (cell.getType())` の `cell.getType()`）
pub fn case_subject(src: &str, line: &str, ws: usize, at: usize) -> Option<String> {
    let before = line[..ws].trim_end();
    let label = before.ends_with("case") && !before[..before.len() - 4].ends_with(|c: char| c.is_alphanumeric() || c == '_')
        || before.ends_with(',') && line.trim_start().starts_with("case ");
    if !label {
        return None;
    }
    // 一番内側の開いた { の前が switch (…)
    let code = crate::text::blank_literals(super::types::LX, &src[..at]);
    let b = code.as_bytes();
    let mut depth = 0usize;
    let mut i = b.len();
    let open = loop {
        i = i.checked_sub(1)?;
        match b[i] {
            b'}' => depth += 1,
            b'{' if depth == 0 => break i,
            b'{' => depth -= 1,
            _ => {}
        }
    };
    let head = code[..open].trim_end();
    let close = head.strip_suffix(')')?.len();
    let mut d = 0usize;
    let mut k = close;
    let start = loop {
        k = k.checked_sub(1)?;
        match b[k] {
            b')' => d += 1,
            b'(' if d == 0 => break k,
            b'(' => d -= 1,
            _ => {}
        }
    };
    let kw = code[..start].trim_end();
    (kw.ends_with("switch") && !kw[..kw.len() - 6].ends_with(|c: char| c.is_alphanumeric() || c == '_')).then(|| src[start + 1..close].trim().to_string())
}

#[cfg(test)]
mod case_tests {
    #[test]
    fn case_subject_of_switch() {
        let src = "class A {\n  void f(Kind k) {\n    switch (cell.getType()) {\n      case A:\n        if (x) { y(); }\n        break;\n      case B, \n    }\n  }\n}\n";
        let at = src.find("case B, ").unwrap() + 8;
        let ls = src[..at].rfind('\n').unwrap() + 1;
        let line = src[ls..].lines().next().unwrap();
        assert_eq!(super::case_subject(src, line, at - ls, at).as_deref(), Some("cell.getType()"));
        let at = src.find("case A").unwrap() + 5;
        let ls = src[..at].rfind('\n').unwrap() + 1;
        let line = src[ls..].lines().next().unwrap();
        assert_eq!(super::case_subject(src, line, at - ls, at).as_deref(), Some("cell.getType()"));
        // case の後でなければ無し
        let at = src.find("y()").unwrap();
        let ls = src[..at].rfind('\n').unwrap() + 1;
        let line = src[ls..].lines().next().unwrap();
        assert_eq!(super::case_subject(src, line, at - ls, at), None);
    }
}
