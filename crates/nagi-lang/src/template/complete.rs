//! 補完の共通の手順。
//!
//! `.` の後: 受け手の頭を広げる（言語）→ パッケージなら中身（言語）→ 受け手の型をプロジェクトの索引で求めてそのメンバー
//! → ライブラリのメンバー（言語）→ クラスの名前の後の予約語（言語）→ 何も無ければファイルの中で `.` の後に出てくる語。
//! それ以外: 今の型のメンバー → ファイルの中の語（近い順）→ 言語の名前（import を伴う）→ プロジェクトの型 → ライブラリの型（言語）→ 予約語

use std::collections::HashSet;

use nagi_syntax::defs::{DefKind, Receiver};
use crate::project::Ranker;

use crate::lang::{Cx, Head, Item, Language, LibMember, mark_of};
use crate::text;

/// 候補（同じ名前は最初のものだけ）
struct Out {
    items: Vec<Item>,
    seen: HashSet<String>,
}

impl Out {
    fn push(&mut self, c: Item) {
        if self.seen.insert(c.name.clone()) {
            self.items.push(c);
        }
    }
}

/// 候補を集める
pub fn complete(cx: &Cx) -> Vec<Item> {
    let lang = cx.lang;
    if let Some(v) = lang.complete_all(cx) {
        return v;
    }
    let mut out = Out { items: Vec::new(), seen: HashSet::new() };
    let mut ranker = Ranker::new(cx.index);
    if cx.after_dot() {
        after_dot(lang, cx, &mut ranker, &mut out);
    } else {
        in_scope(lang, cx, &mut ranker, &mut out);
    }
    out.items
}

/// 型 `ty` のプロジェクトのメンバー（private はその型が今のファイルにあれば。`types` なら入れ子の型も）と、ライブラリの親から継ぐもの
fn members_of(lang: &dyn Language, cx: &Cx, ranker: &mut Ranker, ty: &str, out: &mut Out, rank: u8, types: bool) {
    for (name, kind, owner, params, private) in ranker.members(ty) {
        // private は、その型が今のファイルにあれば（入れ子のクラスどうしも）見える
        let here = || cx.inside.as_deref() == Some(owner.as_str()) || cx.index.is_some_and(|i| i.lookup(&owner).iter().any(|l| l.kind.is_type() && *l.path == *cx.rel));
        if (private && !here()) || (kind.is_type() && !types) {
            continue;
        }
        let call = matches!(kind, DefKind::Method | DefKind::Function).then(|| params != Some(0));
        out.push(Item { name, mark: mark_of(kind), note: owner, call, rank, insert: None, import: None });
    }
    for c in lang.inherited(cx, ranker, ty, rank) {
        out.push(c);
    }
}

/// 受け手がクラスの名前（Outer・Map.Entry）か。変数は宣言の型に印 `=` を付けて分ける
fn class_name(r: &Receiver) -> bool {
    match r {
        Receiver::Type(t) => !t.starts_with('='),
        Receiver::Member(i, n) => n.starts_with(char::is_uppercase) && class_name(i),
        _ => false,
    }
}

fn after_dot(lang: &dyn Language, cx: &Cx, ranker: &mut Ranker, out: &mut Out) {
    let lx = lang.lexis();
    let src = cx.src;
    let (expanded, fqn) = match lang.head(cx, &cx.line[..cx.ws - 1]) {
        Head::Expr(e, f) => (e, f),
        Head::Items(items) => {
            for c in items {
                out.push(c);
            }
            return;
        }
    };
    let var = |n: &str| match n.strip_prefix("new ") {
        Some(t) => Some(t.split('<').next().unwrap_or(t).to_string()),
        None => lang.var_type(src, n, cx.start),
    };
    let marked = |n: &str| match n.strip_prefix("new ") {
        Some(t) => Some(format!("={t}")),
        None => lang.decl_text(src, n, cx.start).map(|d| format!("={d}")),
    };
    let class_recv = text::receiver_before(lx, &expanded, expanded.len(), &marked).as_ref().is_some_and(class_name);
    let recv = text::receiver_before(lx, &expanded, expanded.len(), &var);
    let ty = recv.as_ref().and_then(|r| ranker.receiver_type_name(&cx.rel, src, cx.start, r));
    if let Some(t) = &ty {
        members_of(lang, cx, ranker, t, out, 0, class_recv);
    }
    let found = !out.items.is_empty();
    for m in lang.library_members(cx, ranker, &expanded, fqn.as_ref(), class_recv, found) {
        match m {
            LibMember::Item(c) => out.push(c),
            LibMember::Project(t) => members_of(lang, cx, ranker, &t, out, 0, class_recv),
        }
    }
    if class_recv && !out.items.is_empty() {
        for k in lang.class_keywords() {
            out.push(Item::new(*k, 'K', "", 0));
        }
    }
    if out.items.is_empty() {
        // 型が分からない・プロジェクトの外: ファイルの中で `.` の後に出てくる語（コメント・文字列の中の `www.example.com` などは除く）
        static DOT_WORD: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let re = DOT_WORD.get_or_init(|| regex::Regex::new(r"\.([A-Za-z_$][\w$]*)").unwrap());
        let src = &code_words(lang, src);
        for c in re.captures_iter(src) {
            let call = src[c.get(0).unwrap().end()..].starts_with('(');
            out.push(Item { name: c[1].to_string(), mark: if call { 'm' } else { 'f' }, note: ty.clone().unwrap_or_default(), call: call.then_some(true), rank: 1, insert: None, import: None });
        }
    }
}

/// 語を集める元の文字（言語が除くなら、コメントと文字列の中を空白にする。位置は変えない）
fn code_words<'a>(lang: &dyn Language, src: &'a str) -> std::borrow::Cow<'a, str> {
    if lang.words_skip_literals() { std::borrow::Cow::Owned(text::blank_literals(lang.lexis(), src)) } else { std::borrow::Cow::Borrowed(src) }
}

fn in_scope(lang: &dyn Language, cx: &Cx, ranker: &mut Ranker, out: &mut Out) {
    let src = cx.src;
    // 今の型のメンバー
    if let Some(t) = cx.inside.clone() {
        members_of(lang, cx, ranker, &t, out, 1, false);
    }
    // ファイルの中の語（変数・引数など）。カーソルに近い順
    let all_kws = lang.keywords();
    if cx.words {
        let src = &code_words(lang, src);
        static WORDS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let words = WORDS.get_or_init(|| regex::Regex::new(r"[A-Za-z_$][\w$]*").unwrap());
        let mut list: Vec<(usize, &str)> = words.find_iter(src).filter(|m| m.start() != cx.start).map(|m| (m.start().abs_diff(cx.start), m.as_str())).collect();
        list.sort_by_key(|x| x.0);
        for (_, w) in list {
            if w.len() > 1 && !all_kws.contains(&w) {
                let call = src.contains(&format!("{w}("));
                out.push(Item::new(w, 'v', "", u8::from(call) * 2));
            }
        }
    }
    let names = lang.scope_names(cx);
    for c in names.first {
        out.push(c);
    }
    // プロジェクトの型
    if let Some(idx) = cx.index {
        let mut types: Vec<(&str, DefKind)> = idx.all().filter(|(_, l)| l.kind.is_type()).map(|(n, l)| (n, l.kind)).collect();
        types.sort_unstable_by(|a, b| a.0.cmp(b.0));
        for (n, k) in types {
            out.push(Item::new(n, mark_of(k), "", 3));
        }
    }
    // ライブラリの型（同じ名前でも別のものとして並べる。同じ名前・説明のものだけ除く）
    for c in names.last {
        if !out.items.iter().any(|x| x.name == c.name && x.note == c.note) {
            out.items.push(c);
        }
    }
    // 予約語
    if cx.keywords {
        for k in all_kws {
            out.push(Item::new(*k, 'K', "", 4));
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::lang::{Cx, Language};

    /// `@` の所で補完した名前
    fn names(lang: &dyn Language, src: &str) -> Vec<String> {
        let libs = crate::libs::Libraries::default();
        let at = src.find('@').unwrap();
        let src = src.replace('@', "");
        let ls = src[..at].rfind('\n').map_or(0, |i| i + 1);
        let line = src[ls..].lines().next().unwrap_or("");
        let cx = Cx::new(lang, &src, None, line, at - ls, at, "", None, &libs);
        super::complete(&cx).into_iter().map(|i| i.name).collect()
    }

    /// ファイルの中の語は、コメントと文字列の中から集めない（前は説明の文や URL の語まで出た）
    #[test]
    fn words_skip_comments_and_strings() {
        let src = "// see www.example.com for details\nclass A {\n  /* block comment words */\n  void run(int count) {\n    String s = \"hello world.text\";\n    co@\n  }\n}";
        let v = names(&crate::java::lang::JAVA, src);
        assert!(v.contains(&"count".to_string()) && v.contains(&"run".to_string()), "{v:?}");
        for w in ["details", "example", "block", "comment", "words", "hello", "world"] {
            assert!(!v.contains(&w.to_string()), "{w}: {v:?}");
        }
        let v = names(&crate::ts::lang::TS, "const t = `template words`; // note here\nlet value = 1;\nva@");
        assert!(v.contains(&"value".to_string()) && !v.contains(&"template".to_string()) && !v.contains(&"note".to_string()), "{v:?}");
        // `.` の後で型が分からないときの語も
        let v = names(&crate::java::lang::JAVA, "// call www.example.com\nclass A {\n  void f(Object o) {\n    o.toString();\n    x.@\n  }\n}");
        assert!(v.contains(&"toString".to_string()) && !v.contains(&"example".to_string()), "{v:?}");
        // 文章のファイル（知らない種類）は ' の後も語として集める
        let v = names(&crate::plain::OTHER, "don't forget the meeting\nfo@");
        assert!(v.contains(&"forget".to_string()) && v.contains(&"meeting".to_string()), "{v:?}");
    }
}
