//! TypeScript・JavaScript の差し込み口

use nagi_workers::index::Ranker;

use crate::eco;
use crate::lang::{Cx, Import, Item, Language, LibMember, ScopeNames, mark_of};
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
    "static", "super", "switch", "this", "throw", "true", "try", "type", "typeof", "undefined", "var", "void", "while", "yield",
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
    fn scope_names(&self, cx: &Cx) -> ScopeNames {
        let (Some(ix), Some(h), false) = (eco::dts(cx.libs), cx.here, self.js) else { return ScopeNames::default() };
        let prefix = cx.prefix;
        let idx = cx.index;
        let mut names: Vec<&str> = ix.exports_with_prefix(prefix, 300).into_iter().map(|x| x.0).collect();
        if let Some(idx) = idx.filter(|_| !prefix.is_empty()) {
            names.extend(idx.names().filter(|n| n.starts_with(prefix)).take(300));
        }
        names.sort_unstable();
        names.dedup();
        let imported: Vec<String> = eco::dts::scan_source(cx.src).imports.into_iter().map(|i| i.from).collect();
        let mut first = Vec::new();
        for n in names {
            let Some(spec) = super::import::candidates_with(&ix, idx, h, &imported, n, None).into_iter().next() else { continue };
            let kind = idx.and_then(|i| i.lookup(n).first().map(|l| l.kind)).map_or('c', mark_of);
            first.push(Item { name: n.to_string(), mark: kind, note: spec.clone(), call: None, rank: 3, insert: None, import: Some(spec) });
        }
        ScopeNames { first, last: Vec::new() }
    }

    fn import_plan(&self, src: &str, name: &str, key: &str) -> Import {
        match super::import::plan(src, name, key) {
            super::import::Plan::Insert(at, line) => Import::Insert(at, line),
            _ => Import::None,
        }
    }
}
