//! TypeScript・JavaScript の差し込み口

use crate::project::Ranker;

use crate::eco;
use crate::lang::{Cx, Import, Item, Language, LibMember, ScopeNames, Target, mark_of};
use crate::text::Lexis;

/// `js` なら JavaScript（ほかのファイル・パッケージの名前を import で出さない）
pub struct Ts {
    pub js: bool,
}

pub static TS: Ts = Ts { js: false };
pub static JS: Ts = Ts { js: true };

const KEYWORDS: &[&str] = &[
    "async", "await", "break", "case", "catch", "class", "const", "constructor", "continue", "default", "delete", "else", "enum", "export", "extends", "false", "finally",
    "for", "from", "function", "if", "implements", "import", "in", "instanceof", "interface", "let", "new", "null", "private", "protected", "public", "readonly", "return",
    "static", "super", "switch", "this", "throw", "true", "try", "type", "typeof", "undefined", "var", "void", "while", "yield", "string", "number", "boolean", "any",
    "unknown", "never", "object", "symbol", "bigint", "override", "get", "set", "declare", "abstract", "as", "of", "keyof", "is", "satisfies", "namespace",
];

impl Language for Ts {
    fn name(&self) -> &'static str {
        if self.js { "javascript" } else { "typescript" }
    }

    fn lexis(&self) -> &'static Lexis {
        super::types::LX
    }

    fn keywords(&self) -> &'static [&'static str] {
        KEYWORDS
    }

    /// 組み込み・node_modules の .d.ts とプロジェクトの .ts から型を求める（プロジェクトの索引の候補に足す。
    /// コンストラクタの引数のフィールドなどは索引に無い）
    fn library_members(&self, cx: &Cx, _ranker: &mut Ranker, expanded: &str, _fqn: Option<&(String, String)>, _statics: bool, _found: bool) -> Vec<LibMember> {
        let (Some(ix), Some(h), Some(i)) = (eco::dts(cx.libs), cx.here, cx.index) else { return Vec::new() };
        super::types::complete(&ix, &i.root, h, cx.src, expanded, cx.start).into_iter().map(|m| LibMember::Item(Item::member(m.name, m.method, m.params, m.owner, 0))).collect()
    }

    /// 依存のパッケージとプロジェクトのほかのファイルの名前（打った語で始まるもの。決めたら import を足す）
    fn names_from_tree(&self) -> bool {
        true
    }

    /// オブジェクトのキー（`{ code: … }`）は型が分からないと出せないので、ファイルの中の語も下に
    fn words_below_tree(&self) -> bool {
        true
    }

    fn scope_names(&self, cx: &Cx) -> ScopeNames {
        let prefix = cx.prefix;
        // import した名前（構文木の束縛には入れていない）
        let scan = eco::dts::scan_source(cx.src);
        let mut first: Vec<Item> = scan.imports.iter().filter(|i| i.local != "*").map(|i| Item::new(i.local.as_str(), if i.local.starts_with(char::is_uppercase) { 'c' } else { 'v' }, i.from.as_str(), 1)).collect();
        let Some(ix) = eco::dts(cx.libs) else { return ScopeNames { first, last: Vec::new() } };
        // 大域の名前（console・Array・document など。打ったのが小文字でも大文字で始まるものも）
        let mut up = prefix.to_string();
        if let Some(f) = up.get(0..1) {
            up.replace_range(0..1, &f.to_ascii_uppercase());
        }
        if !prefix.is_empty() {
            let mut globals: Vec<&str> = ix.with_prefix(prefix).into_iter().chain(if up != prefix { ix.with_prefix(&up) } else { Vec::new() }).filter(|d| d.global).map(|d| d.name.as_str()).collect();
            globals.sort_unstable();
            globals.dedup();
            for g in globals {
                first.push(Item::new(g, if g.starts_with(char::is_uppercase) { 'c' } else { 'v' }, "", 2));
            }
        }
        let (Some(h), false) = (cx.here, self.js) else { return ScopeNames { first, last: Vec::new() } };
        let idx = cx.index;
        let mut names: Vec<&str> = ix.exports_with_prefix(prefix, 300).into_iter().map(|x| x.0).collect();
        if let Some(idx) = idx.filter(|_| !prefix.is_empty()) {
            names.extend(idx.names().filter(|n| n.starts_with(prefix)).take(300));
        }
        names.sort_unstable();
        names.dedup();
        let imported: Vec<String> = scan.imports.into_iter().map(|i| i.from).collect();
        for n in names {
            let Some(spec) = super::import::candidates_with(&ix, idx, h, &imported, n, None).into_iter().next() else { continue };
            let kind = idx.and_then(|i| i.lookup(n).first().map(|l| l.kind)).map_or('c', mark_of);
            first.push(Item { name: n.to_string(), mark: kind, note: spec.clone(), call: None, rank: 3, insert: None, import: Some(spec) });
        }
        ScopeNames { first, last: Vec::new() }
    }

    /// 型の宣言（.d.ts・import したプロジェクトの .ts）。`.` の後はメンバーを宣言している型の本体の中、頭の名前は import・大域の宣言
    fn library_definition(&self, cx: &Cx, name: &str) -> Option<Target> {
        let ix = eco::dts(cx.libs)?;
        let root = &cx.index?.root;
        let (src, at) = (cx.src, cx.start);
        let ls = at - cx.ws;
        let before = src[ls..at].ends_with('.').then(|| crate::text::with_previous_lines(super::types::LX, src, ls, &src[ls..at - 1]));
        let (path, line, col) = super::types::definition(&ix, root, cx.here?, src, before.as_deref(), name, at)?;
        Some(Target { path, line, col, note: None })
    }

    fn ecosystem(&self) -> Option<&'static str> {
        Some(eco::JS)
    }

    /// `name: Type` と `let・const name = new Type(`
    fn var_styles(&self) -> crate::project::VarStyles {
        crate::project::VarStyles { type_first: false, colon: true, init: true }
    }

    fn import_style(&self) -> crate::project::ImportStyle {
        crate::project::ImportStyle::Path
    }

    fn loose_param(&self, p: &str) -> bool {
        super::rules::loose_param(p)
    }

    fn literal_fit(&self, lit: &str, p: &str) -> i32 {
        super::rules::literal_fit(lit, p)
    }

    fn decl_type(&self, line: &str, col: usize) -> Option<String> {
        super::rules::ts_decl_type(&line[col..])
    }

    fn param_type(&self, p: &str) -> Option<String> {
        super::rules::ts_param_type(p)
    }

    /// 別のファイルからは export したものだけ
    fn exported(&self, decl_line: &str) -> bool {
        decl_line.trim_start().starts_with("export")
    }

    fn import_plan(&self, src: &str, name: &str, key: &str) -> Import {
        match super::import::plan(src, name, key) {
            super::import::Plan::Insert(at, line) => Import::Insert(at, line),
            _ => Import::None,
        }
    }
}
