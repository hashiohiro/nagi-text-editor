//! IntelliJ のような入力補完（Insert モードの Ctrl+Space）。
//!
//! 一覧を出したまま、打つたびに絞り込む（前方一致を先に、`gNa` → `getName` のような飛ばし打ちも当てる）。
//! ↑↓・Ctrl-n/p・Ctrl-j/k で選び、Enter・Tab で決める（メソッドは `()` も入れ、引数があれば括弧の中へ）。Esc は一覧だけ閉じる。
//! 語の文字でないキー（空白・記号）を打つと閉じてそのまま入る。
//!
//! 候補:
//! - `.` の後: 受け手の型を求め（変数は宣言から、続く呼び出し・フィールドは定義へ移動と同じ型の推定で）、その型と親の型の
//!   メソッド・フィールド（index::Ranker::members）。型がプロジェクトの外なら、ファイルの中で `.` の後に出てくる語
//! - それ以外: 今の型のメンバー、ファイルの中の語（変数など）、プロジェクトの型、言語の予約語

use std::collections::HashSet;

use nagi_core::fuzzy::Query;
use nagi_syntax::defs::{DefKind, Receiver};
use nagi_vim::Key;

use crate::complete::Completion;
use crate::editor::Editor;

/// 候補 1 つ
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cand {
    pub name: String,
    /// 一覧の印（m メソッド・f フィールド・c クラス・I インターフェース・e 列挙・K 予約語・v 語）
    pub mark: char,
    /// 右に出す説明（持っている型など）
    pub note: String,
    /// メソッドなら Some(引数があるか)。決めたときに () を入れる
    pub call: Option<bool>,
    /// 同じ一致の度合いの中での順（小さいほど先）
    pub rank: u8,
    /// 決めたときに入れる文字（名前と違うとき。SQL の "名前" など）
    pub insert: Option<String>,
    /// 決めたときに足す import（Java の完全な名前 `java.util.List`）
    pub import: Option<String>,
}

/// 補完の一覧の中身（絞り込む前の全部と、出している候補の番号）
pub struct SmartState {
    pub all: Vec<Cand>,
    pub shown: Vec<usize>,
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

fn mark_of(kind: DefKind) -> char {
    match kind {
        DefKind::Method | DefKind::Function => 'm',
        DefKind::Field | DefKind::Constant | DefKind::Variable => 'f',
        DefKind::Interface => 'I',
        DefKind::Enum => 'e',
        DefKind::Class | DefKind::Type => 'c',
        DefKind::Module => 'v',
    }
}

/// 言語の予約語（よく使うもの）
fn keywords(lang: Option<nagi_syntax::Lang>) -> &'static [&'static str] {
    use nagi_syntax::Lang::*;
    match lang {
        Some(Java) => &[
            "abstract", "boolean", "break", "case", "catch", "char", "class", "continue", "default", "double", "else", "enum", "extends", "final", "finally", "float", "for", "if",
            "implements", "import", "instanceof", "int", "interface", "long", "new", "null", "package", "private", "protected", "public", "return", "static", "super", "switch",
            "synchronized", "this", "throw", "throws", "try", "var", "void", "while", "true", "false", "record", "yield",
        ],
        Some(Kotlin) => &[
            "as", "break", "class", "companion", "const", "continue", "data", "do", "else", "enum", "false", "for", "fun", "if", "import", "in", "interface", "is", "lateinit",
            "null", "object", "open", "override", "package", "private", "protected", "public", "return", "sealed", "super", "this", "throw", "true", "try", "val", "var", "when",
            "while",
        ],
        Some(TypeScript | Tsx | JavaScript) => &[
            "async", "await", "break", "case", "catch", "class", "const", "constructor", "continue", "default", "delete", "else", "enum", "export", "extends", "false", "finally",
            "for", "from", "function", "if", "implements", "import", "in", "instanceof", "interface", "let", "new", "null", "private", "protected", "public", "readonly", "return",
            "static", "super", "switch", "this", "throw", "true", "try", "type", "typeof", "undefined", "var", "void", "while", "yield",
        ],
        Some(Rust) => &[
            "as", "async", "await", "break", "const", "continue", "crate", "else", "enum", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
            "pub", "ref", "return", "Self", "self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while", "Some", "None", "Ok", "Err",
        ],
        Some(Python) => &[
            "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except", "False", "finally", "for", "from", "global", "if",
            "import", "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return", "self", "True", "try", "while", "with", "yield",
        ],
        _ => &[],
    }
}

/// `text` の中の `name` の変数・引数・フィールドの宣言の型（`at` より前の最後の宣言、無ければ後ろの最初）
pub fn var_type(text: &str, name: &str, at: usize) -> Option<String> {
    // Java・C#: Type name / Type<..> name / Type[] name（宣言・引数・フィールド）。型引数と配列は外す
    let mut all: Vec<(usize, String)> = crate::java_members::typed_decls(text, name).into_iter().map(|(p, t)| (p, t.split(['<', '[']).next().unwrap_or("").rsplit('.').next().unwrap_or("").to_string())).collect();
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
    crate::java_members::pick(all, at)
}

/// `.` の前の式を受け手にする（`a.b().c` → Call・Member の連なり）。`dot` は `.` の位置（行の中の byte）。
/// `var` は変数の型。`new X<..>(..)` の頭は `var("new X<..>")`（無ければ X）
pub fn receiver_before(line: &str, dot: usize, var: &dyn Fn(&str) -> Option<String>) -> Option<Receiver> {
    let s = &line[..dot];
    let mut parts: Vec<(String, bool)> = Vec::new();
    let mut end = s.len();
    let mut newed = None;
    loop {
        let mut i = end;
        let mut call = false;
        // 呼び出しの括弧を飛ばす
        if s[..i].ends_with(')') {
            let mut depth = 0;
            let mut open = None;
            for (k, c) in s[..i].char_indices().rev() {
                match c {
                    ')' => depth += 1,
                    '(' => {
                        depth -= 1;
                        if depth == 0 {
                            open = Some(k);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            i = open?;
            call = true;
        }
        let paren = i;
        // new ArrayList<String>() の型引数
        if call && s[..i].ends_with('>') {
            i = crate::java_members::generic_open(&s[..i])?;
        }
        let word_start = s[..i].char_indices().rev().take_while(|(_, c)| word_char(*c)).last().map(|x| x.0)?;
        parts.push((s[word_start..i].to_string(), call));
        if word_start > 0 && s[..word_start].ends_with('.') {
            end = word_start - 1;
        } else {
            let t = s[..word_start].trim_end();
            if call && t.ends_with("new") && !t[..t.len() - 3].ends_with(word_char) {
                newed = Some(s[word_start..paren].to_string());
            }
            break;
        }
    }
    parts.reverse();
    let mut it = parts.into_iter();
    let (base, call) = it.next()?;
    let mut r = if let Some(n) = newed {
        Receiver::Type(var(&format!("new {n}")).unwrap_or(base))
    } else if call {
        Receiver::Call(Box::new(Receiver::This), base)
    } else if base == "this" || base == "super" || base == "self" {
        Receiver::This
    } else if let Some(t) = var(&base) {
        Receiver::Type(t)
    } else if base.chars().next().is_some_and(char::is_uppercase) {
        Receiver::Type(base)
    } else {
        Receiver::Member(Box::new(Receiver::This), base)
    };
    for (name, call) in it {
        r = if call { Receiver::Call(Box::new(r), name) } else { Receiver::Member(Box::new(r), name) };
    }
    Some(r)
}

/// 打った語に合うか。(一致の度合い: 0 前方一致（大文字小文字も同じ）・1 前方一致・2 飛ばし打ち, あいまいさの点)
/// `case`: 設定の「大文字小文字」（first 最初の文字だけ区別・none 区別しない・all 区別する）
fn matches(prefix: &str, q: &Query, name: &str, case: &str) -> Option<(u8, i32)> {
    if prefix.is_empty() {
        return Some((0, 0));
    }
    if name == prefix {
        return None;
    }
    // 区別する文字が合わなければ外す
    match case {
        "all" if !subsequence(prefix, name) => return None,
        "first" if prefix.chars().next() != name.chars().next() && !name.starts_with(prefix.chars().next().unwrap_or(' ')) => return None,
        _ => {}
    }
    if name.starts_with(prefix) {
        return Some((0, 0));
    }
    if name.to_lowercase().starts_with(&prefix.to_lowercase()) {
        return Some((1, 0));
    }
    q.score(name).map(|s| (2, s))
}

/// `p` の文字が `s` に同じ順で（大文字小文字も同じに）出てくるか
fn subsequence(p: &str, s: &str) -> bool {
    let mut it = s.chars();
    p.chars().all(|c| it.any(|x| x == c))
}

impl Editor {
    /// Ctrl+Space: 候補を集めて一覧を出す
    pub(crate) fn start_smart_completion(&mut self) {
        let line = self.buf.line(self.cursor.line);
        let b = self.cursor.byte.min(line.len());
        let ws = line[..b].char_indices().rev().take_while(|(_, c)| word_char(*c)).last().map_or(b, |(i, _)| i);
        let prefix = line[ws..b].to_string();
        let start = self.buf.line_start(self.cursor.line) + ws;
        let all = if self.console_source().is_some() { self.sql_smart_candidates(start, &prefix) } else { self.smart_candidates(&line, ws, start) };
        if all.is_empty() {
            self.notify.info("-- 補完の候補はありません");
            return;
        }
        self.completion = Some(Completion { start, original: prefix, items: Vec::new(), sel: Some(0), notes: Vec::new(), smart: Some(SmartState { all, shown: Vec::new() }), drawn: Default::default() });
        self.refilter_smart();
    }

    /// Java のファイルか（色付けがまだでもパスから）
    pub(crate) fn is_java(&self) -> bool {
        self.syntax.as_ref().map(|s| s.lang).or_else(|| self.buf.path.as_deref().and_then(nagi_syntax::Lang::detect)) == Some(nagi_syntax::Lang::Java)
    }

    /// 候補を集める（`ws` は打っている語の始まり（行の中の byte）、`start` はその絶対位置）
    fn smart_candidates(&mut self, line: &str, ws: usize, start: usize) -> Vec<Cand> {
        let src = self.buf.text();
        let here = self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok());
        let lang = self.syntax.as_ref().map(|s| s.lang).or_else(|| self.buf.path.as_deref().and_then(nagi_syntax::Lang::detect));
        let mut out: Vec<Cand> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut push = |out: &mut Vec<Cand>, c: Cand| {
            if seen.insert(c.name.clone()) {
                out.push(c);
            }
        };
        let member = ws > 0 && line[..ws].ends_with('.');
        let idx = self.nav.index.as_ref();
        let rel = match (idx, &here) {
            (Some(i), Some(h)) => h.strip_prefix(&i.root).ok().map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_default(),
            _ => String::new(),
        };
        let mut ranker = nagi_workers::index::Ranker::new(idx);
        // 今いる型（その中からは private も出す）
        let inside = nagi_workers::index::enclosing_type(&src, start);
        // `types`: 入れ子の型も出す（受け手がクラスの名前のとき）
        let member_cands = |ranker: &mut nagi_workers::index::Ranker, ty: &str, out: &mut Vec<Cand>, push: &mut dyn FnMut(&mut Vec<Cand>, Cand), rank: u8, types: bool| {
            for (name, kind, owner, params, private) in ranker.members(ty) {
                // private は、その型が今のファイルにあれば（入れ子のクラスどうしも）見える
                let here = || inside.as_deref() == Some(owner.as_str()) || idx.is_some_and(|i| i.lookup(&owner).iter().any(|l| l.kind.is_type() && *l.path == *rel));
                if (private && !here()) || (kind.is_type() && !types) {
                    continue;
                }
                let call = matches!(kind, DefKind::Method | DefKind::Function).then(|| params != Some(0));
                push(out, Cand { name, mark: mark_of(kind), note: owner, call, rank, insert: None, import: None });
            }
        };
        // プロジェクトの型の、ライブラリの親（extends ControllerBase<..> など）のメンバー（protected も）
        let jars = if self.is_java() { self.nav.jars.clone() } else { None };
        let lib_super_cands = |ranker: &mut nagi_workers::index::Ranker, ty: &str, out: &mut Vec<Cand>, push: &mut dyn FnMut(&mut Vec<Cand>, Cand), rank: u8| {
            let Some(jars) = &jars else { return };
            for (sup, lines) in ranker.outside_supers(ty) {
                let lib = crate::java_members::LibTypes::new(jars, &lines.join("\n"));
                let Some(t @ crate::java_members::JType::Class(..)) = lib.parse(&sup) else { continue };
                for m in lib.members(&t, false, true) {
                    push(out, Cand { name: m.name, mark: if m.method { 'm' } else { 'f' }, note: m.owner, call: m.method.then_some(m.params), rank, insert: None, import: None });
                }
            }
        };
        if member {
            // 頭を展開し（前の行からの連なり・型を書かない変数）、パッケージの名前の頭を外す。パッケージだけなら下のパッケージとクラス
            let known = |pkg: &str| idx.is_some_and(|i| i.is_java_package(pkg)) || jars.as_ref().is_some_and(|j| j.is_package(pkg));
            let head = if self.is_java() {
                crate::java_members::receiver_text(&src, &line[..ws - 1], start - ws, start, &known)
            } else {
                Ok((crate::java_members::with_previous_lines(&src, start - ws, &line[..ws - 1]), None))
            };
            let (expanded, fqn) = match head {
                Ok(x) => x,
                Err(pkg) => {
                    let dotted = pkg.replace('/', ".");
                    let under = format!("{pkg}/");
                    let proj: Vec<(String, &str)> = idx.map(|i| i.files().filter(|f| f.ends_with(".java")).filter_map(|f| Some((crate::java_members::source_package(f)?, f))).collect()).unwrap_or_default();
                    for (p, f) in &proj {
                        if *p == pkg {
                            let name = f.rsplit('/').next().unwrap_or(f).trim_end_matches(".java");
                            let mark = idx.and_then(|i| i.file_type_kind(f)).map_or('c', mark_of);
                            push(&mut out, Cand { name: name.to_string(), mark, note: dotted.clone(), call: None, rank: 0, insert: None, import: None });
                        }
                    }
                    if let Some(j) = &jars {
                        for c in j.classes_in(&pkg) {
                            let name = c.rsplit('/').next().unwrap_or(c);
                            push(&mut out, Cand { name: name.to_string(), mark: 'c', note: dotted.clone(), call: None, rank: 0, insert: None, import: None });
                        }
                    }
                    let mut subs: std::collections::BTreeSet<&str> = proj.iter().filter_map(|(p, _)| p.strip_prefix(&under)?.split('/').next()).collect();
                    if let Some(j) = &jars {
                        subs.extend(j.subpackages(&pkg));
                    }
                    for s in subs {
                        push(&mut out, Cand { name: s.to_string(), mark: 'p', note: String::new(), call: None, rank: 0, insert: None, import: None });
                    }
                    return out;
                }
            };
            let var = |n: &str| match n.strip_prefix("new ") {
                Some(t) => Some(t.split('<').next().unwrap_or(t).to_string()),
                None => var_type(&src, n, start),
            };
            // 受け手がクラスの名前（Outer・Map.Entry）か。変数は宣言の型に印 `=` を付けて分ける
            fn class_name(r: &Receiver) -> bool {
                match r {
                    Receiver::Type(t) => !t.starts_with('='),
                    Receiver::Member(i, n) => n.starts_with(char::is_uppercase) && class_name(i),
                    _ => false,
                }
            }
            let marked = |n: &str| match n.strip_prefix("new ") {
                Some(t) => Some(format!("={t}")),
                None => crate::java_members::var_decl_text(&src, n, start).map(|d| format!("={d}")),
            };
            let class_recv = receiver_before(&expanded, expanded.len(), &marked).as_ref().is_some_and(class_name);
            let recv = receiver_before(&expanded, expanded.len(), &var);
            let ty = recv.as_ref().and_then(|r| ranker.receiver_type_name(&rel, &src, start, r));
            if let Some(t) = &ty {
                member_cands(&mut ranker, t, &mut out, &mut push, 0, class_recv);
                lib_super_cands(&mut ranker, t, &mut out, &mut push, 0);
            }
            // ライブラリの型（Java。jar・JDK の索引から、型引数もたどって）
            if out.is_empty() && self.is_java() {
                if let Some(jars) = &self.nav.jars {
                    let statics = class_recv;
                    let (lib, t) = crate::java_members::lib_receiver(jars, &mut ranker, &src, &rel, start, inside.as_deref(), &expanded, fqn.as_ref());
                    match t {
                        Some(crate::java_members::JType::Class(name, _)) if name.starts_with('?') => {
                            let simple = crate::java_members::project_name(&name).map_or("", |x| x.0);
                            member_cands(&mut ranker, simple, &mut out, &mut push, 0, class_recv);
                            lib_super_cands(&mut ranker, simple, &mut out, &mut push, 0);
                        }
                        Some(t) => {
                            for m in lib.members(&t, statics, false) {
                                push(&mut out, Cand { name: m.name, mark: if m.method { 'm' } else { 'f' }, note: m.owner, call: m.method.then_some(m.params), rank: 0, insert: None, import: None });
                            }
                            // クラスの名前の後は入れ子のクラスも
                            if let (true, crate::java_members::JType::Class(n, _)) = (statics, &t) {
                                for c in jars.nested(n) {
                                    let simple = c.rsplit('$').next().unwrap_or(c);
                                    push(&mut out, Cand { name: simple.to_string(), mark: 'c', note: String::new(), call: None, rank: 0, insert: None, import: None });
                                }
                            }
                        }
                        None => {}
                    }
                }
            }
            // TypeScript: 組み込み・node_modules の .d.ts とプロジェクトの .ts から型を求める（プロジェクトの索引の候補に足す。
            // コンストラクタの引数のフィールドなどは索引に無い）
            if matches!(lang, Some(nagi_syntax::Lang::TypeScript | nagi_syntax::Lang::Tsx | nagi_syntax::Lang::JavaScript)) {
                if let (Some(ix), Some(h), Some(i)) = (self.nav.dts.clone(), &here, idx) {
                    for m in crate::ts_members::complete(&ix, &i.root, h, &src, &expanded, start) {
                        push(&mut out, Cand { name: m.name, mark: if m.method { 'm' } else { 'f' }, note: m.owner, call: m.method.then_some(m.params), rank: 0, insert: None, import: None });
                    }
                }
            }
            // クラス名の後は Type.class も
            if self.is_java() && class_recv && !out.is_empty() {
                push(&mut out, Cand { name: "class".into(), mark: 'K', note: String::new(), call: None, rank: 0, insert: None, import: None });
            }
            if out.is_empty() {
                // 型が分からない・プロジェクトの外: ファイルの中で `.` の後に出てくる語
                let re = regex::Regex::new(r"\.([A-Za-z_$][\w$]*)").unwrap();
                for c in re.captures_iter(&src) {
                    let w = c[1].to_string();
                    let call = src[c.get(0).unwrap().end()..].starts_with('(');
                    push(&mut out, Cand { name: w, mark: if call { 'm' } else { 'f' }, note: ty.clone().unwrap_or_default(), call: call.then_some(true), rank: 1, insert: None, import: None });
                }
            }
            return out;
        }
        // 今の型のメンバー
        if let Some(t) = inside.clone() {
            member_cands(&mut ranker, &t, &mut out, &mut push, 1, false);
            lib_super_cands(&mut ranker, &t, &mut out, &mut push, 1);
        }
        // ファイルの中の語（変数・引数など）。カーソルに近い順
        let words = regex::Regex::new(r"[A-Za-z_$][\w$]*").unwrap();
        let mut ws_list: Vec<(usize, &str)> = words.find_iter(&src).filter(|m| m.start() != start).map(|m| (m.start().abs_diff(start), m.as_str())).collect();
        ws_list.sort_by_key(|x| x.0);
        let kws: &[&str] = if self.settings.bool("completion.keywords") { keywords(lang) } else { &[] };
        let all_kws = keywords(lang);
        let words_on = self.settings.bool("completion.words");
        for (_, w) in ws_list.into_iter().filter(|_| words_on) {
            if w.len() > 1 && !all_kws.contains(&w) {
                let call = src.contains(&format!("{w}("));
                push(&mut out, Cand { name: w.to_string(), mark: 'v', note: String::new(), call: None, rank: 0 + u8::from(call) * 2, insert: None, import: None });
            }
        }
        let ts = matches!(lang, Some(nagi_syntax::Lang::TypeScript | nagi_syntax::Lang::Tsx));
        // TypeScript: 依存のパッケージとプロジェクトのほかのファイルの名前（打った語で始まるもの。決めたら import を足す）
        if ts {
            if let (Some(ix), Some(h)) = (self.nav.dts.clone(), &here) {
                let b = self.cursor.byte.min(line.len());
                let prefix = &line[ws..b];
                let mut names: Vec<&str> = ix.exports_with_prefix(prefix, 300).into_iter().map(|x| x.0).collect();
                if let Some(idx) = idx.filter(|_| !prefix.is_empty()) {
                    names.extend(idx.names().filter(|n| n.starts_with(prefix)).take(300));
                }
                names.sort_unstable();
                names.dedup();
                let imported: Vec<String> = nagi_workers::dts::scan_source(&src).imports.into_iter().map(|i| i.from).collect();
                for n in names {
                    let Some(spec) = crate::ts_import::candidates_with(&ix, idx, h, &imported, n, None).into_iter().next() else { continue };
                    let kind = idx.and_then(|i| i.lookup(n).first().map(|l| l.kind)).map_or('c', mark_of);
                    push(&mut out, Cand { name: n.to_string(), mark: kind, note: spec.clone(), call: None, rank: 3, insert: None, import: Some(format!("ts:{spec}")) });
                }
            }
        }
        // プロジェクトの型
        if let Some(idx) = idx {
            let mut types: Vec<(&str, DefKind)> = idx.all().filter(|(_, l)| l.kind.is_type()).map(|(n, l)| (n, l.kind)).collect();
            types.sort_unstable_by(|a, b| a.0.cmp(b.0));
            for (n, k) in types {
                push(&mut out, Cand { name: n.to_string(), mark: mark_of(k), note: String::new(), call: None, rank: 3, insert: None, import: None });
            }
        }
        // ライブラリの型（Java。依存の jar・JDK の索引から、打った語で始まるもの。決めたら import を足す）
        if self.is_java() {
            if let Some(jars) = &self.nav.jars {
                let b = self.cursor.byte.min(line.len());
                let prefix = &line[ws..b];
                let imported = |fqn: &str| src.contains(&format!("import {fqn};"));
                let mut up = prefix.to_string();
                if let Some(f) = up.get(0..1) {
                    up.replace_range(0..1, &f.to_ascii_uppercase());
                }
                let mut names = jars.by_prefix(prefix, 200);
                if up != prefix {
                    names.extend(jars.by_prefix(&up, 200));
                }
                for internal in names {
                    let fqn = crate::java_import::java_name(internal);
                    let Some((pkg, simple)) = fqn.rsplit_once('.') else { continue };
                    if out.iter().any(|c| c.name == simple && c.note == pkg) {
                        continue;
                    }
                    let rank = if pkg == "java.lang" || imported(&fqn) { 2 } else { 3 };
                    out.push(Cand { name: simple.to_string(), mark: 'c', note: pkg.to_string(), call: None, rank, insert: None, import: Some(fqn.clone()) });
                }
            }
        }
        // 予約語
        for k in kws {
            push(&mut out, Cand { name: k.to_string(), mark: 'K', note: String::new(), call: None, rank: 4, insert: None, import: None });
        }
        out
    }

    /// 打った語で絞り込み直す（合うものが無ければ閉じる）
    pub(crate) fn refilter_smart(&mut self) {
        let abs = self.abs();
        // SQL は大文字小文字を区別しない（Oracle の名前は大文字、打つのは小文字が多い）
        let case = if self.console_source().is_some() { "none".to_string() } else { self.settings.get("completion.case") };
        let Some(c) = &mut self.completion else { return };
        let Some(st) = &mut c.smart else { return };
        if abs < c.start {
            self.completion = None;
            return;
        }
        let prefix = self.buf.slice_string(c.start, abs);
        let q = Query::new(&prefix);
        let mut scored: Vec<(u8, u8, i32, usize, usize)> = st
            .all
            .iter()
            .enumerate()
            // 打っていなければ集めた順（定義の順・近い語の順）、打っていれば一致の度合い → 短い順
            .filter_map(|(i, cand)| matches(&prefix, &q, &cand.name, &case).map(|(m, s)| (m, cand.rank, -s, if prefix.is_empty() { 0 } else { cand.name.len() }, i)))
            .collect();
        scored.sort();
        st.shown = scored.into_iter().map(|x| x.4).take(500).collect();
        if st.shown.is_empty() {
            self.completion = None;
            return;
        }
        c.original = prefix;
        c.items = st.shown.iter().map(|&i| st.all[i].name.clone()).collect();
        c.notes = st.shown.iter().map(|&i| (st.all[i].mark, st.all[i].note.clone())).collect();
        c.sel = Some(0);
    }

    /// 補完の一覧を出している間のキー。扱ったら true（false なら一覧を閉じて普通に処理する）
    pub(crate) fn smart_key(&mut self, key: Key) -> bool {
        let Some(c) = &mut self.completion else { return false };
        let n = c.items.len();
        match key {
            Key::Down | Key::Ctrl('n') | Key::Ctrl('j') => {
                c.sel = Some(c.sel.map_or(0, |i| (i + 1) % n.max(1)));
                true
            }
            Key::Up | Key::Ctrl('p') | Key::Ctrl('k') => {
                c.sel = Some(c.sel.map_or(0, |i| (i + n.max(1) - 1) % n.max(1)));
                true
            }
            Key::Enter | Key::Tab => {
                self.accept_smart();
                true
            }
            Key::Esc => {
                self.completion = None;
                true
            }
            Key::Ctrl(' ') => {
                self.completion = None;
                self.start_smart_completion();
                true
            }
            Key::Char(ch) if word_char(ch) => {
                let mut b = [0u8; 4];
                self.insert_text(ch.encode_utf8(&mut b));
                self.refilter_smart();
                true
            }
            Key::Backspace => {
                let start = c.start;
                let abs = self.abs();
                if abs <= start {
                    self.completion = None;
                    return false;
                }
                let prev = self.buf.slice_string(start, abs).chars().last().map_or(1, char::len_utf8);
                self.edit(abs - prev, abs, "");
                self.set_abs(abs - prev);
                self.refilter_smart();
                if self.completion.is_none() && abs - prev > start {
                    // 合うものが無くなっても、打っている途中なら閉じるだけ
                }
                true
            }
            _ => {
                self.completion = None;
                false
            }
        }
    }

    /// 打ったキーの後に、設定の「自動で出す」の予約をする（語の文字が設定の数だけ並んだとき・`.` の後。続けて打てば待ち直す）
    pub(crate) fn schedule_auto_complete(&mut self, key: Key) {
        self.auto_complete_at = None;
        if self.state() != crate::editor::State::Insert || self.completion.is_some() || !self.settings.bool("completion.auto") {
            return;
        }
        let Key::Char(c) = key else { return };
        let line = self.buf.line(self.cursor.line);
        let b = self.cursor.byte.min(line.len());
        let n = line[..b].chars().rev().take_while(|c| word_char(*c)).count();
        // SQL コンソール: FROM・JOIN などの後の空白でも（テーブルの一覧）
        let sql_table = c == ' '
            && self.console_source().is_some()
            && line[..b.saturating_sub(1)].rsplit(|ch: char| !word_char(ch)).next().is_some_and(|w| ["from", "join", "update", "into"].contains(&w.to_ascii_lowercase().as_str()));
        if c == '.' || sql_table || (word_char(c) && n >= self.settings.int("completion.min_chars") as usize) {
            self.auto_complete_at = Some(std::time::Instant::now() + std::time::Duration::from_millis(self.settings.int("completion.delay_ms") as u64));
        }
    }

    /// 予約した時刻が来たら一覧を出す（候補が無ければ何も出さない）。出したら true
    pub fn fire_auto_complete(&mut self, now: std::time::Instant) -> bool {
        if !self.auto_complete_at.is_some_and(|t| now >= t) {
            return false;
        }
        self.auto_complete_at = None;
        if self.state() != crate::editor::State::Insert || self.completion.is_some() {
            return false;
        }
        let msg = self.notify.text.clone();
        self.start_smart_completion();
        // 自動で出すときは「候補はありません」を出さない
        if self.completion.is_none() {
            self.notify.info(msg);
        }
        self.completion.is_some()
    }

    /// 選んでいる候補で決める（メソッドは () も入れる）
    fn accept_smart(&mut self) {
        let Some(c) = self.completion.take() else { return };
        let Some(st) = &c.smart else { return };
        let Some(&i) = c.sel.and_then(|s| st.shown.get(s)) else { return };
        let cand = &st.all[i];
        let abs = self.abs();
        // すでに後ろに ( があれば括弧は足さない
        let after = self.buf.slice_string(abs, (abs + 1).min(self.buf.len_bytes()));
        let name = cand.insert.clone().unwrap_or_else(|| cand.name.clone());
        // Java のライブラリの型: import を足す（同じ単純名の別の型を import していれば完全な名前で書く）。TypeScript は `ts:モジュール`
        let plan = cand.import.as_deref().map(|fqn| match fqn.strip_prefix("ts:") {
            Some(spec) => (name.clone(), crate::ts_import::plan(&self.buf.text(), &name, spec)),
            None => (fqn.to_string(), crate::java_import::plan(&self.buf.text(), fqn)),
        });
        let name = match &plan {
            Some((fqn, crate::java_import::Plan::Conflict)) => fqn.clone(),
            _ => name,
        };
        let (text, back) = match cand.call {
            Some(args) if after != "(" => (format!("{name}()"), usize::from(args)),
            _ => (name, 0),
        };
        // 打っている間の 1 つの取り消しの単位に入れる（import も一緒に戻る）
        self.edit(c.start, abs, &text);
        let mut cursor = c.start + text.len() - back;
        if let Some((_, crate::java_import::Plan::Insert(at, line))) = plan {
            if at <= c.start {
                self.edit(at, at, &line);
                cursor += line.len();
            }
        }
        self.set_abs(cursor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;
    use std::collections::HashMap;

    #[test]
    fn var_types_and_receivers() {
        let src = "class A {\n  private UserService userService;\n  void f(List<Order> orders, String name) {\n    var repo = new OrderRepo();\n    let x: Item = y;\n  }\n}";
        assert_eq!(var_type(src, "userService", 200).as_deref(), Some("UserService"));
        assert_eq!(var_type(src, "orders", 200).as_deref(), Some("List"));
        assert_eq!(var_type(src, "repo", 200).as_deref(), Some("OrderRepo"));
        assert_eq!(var_type(src, "x", 200).as_deref(), Some("Item"));
        let var = |n: &str| var_type(src, n, 200);
        let line = "    userService.find(id).getItems().";
        assert_eq!(
            receiver_before(line, line.len() - 1, &var),
            Some(Receiver::Call(Box::new(Receiver::Call(Box::new(Receiver::Type("UserService".into())), "find".into())), "getItems".into()))
        );
        assert_eq!(receiver_before("this.", 4, &var), Some(Receiver::This));
        assert_eq!(receiver_before("  Math.", 6, &var), Some(Receiver::Type("Math".into())));
    }

    fn keys(e: &mut Editor, k: &str) {
        for key in nagi_vim::parse_keys(k, " ") {
            e.key(key);
        }
    }

    /// 型のメンバーを出し、打つと絞り込み、Enter で () も入れる
    #[test]
    fn members_after_dot() {
        let dir = std::env::temp_dir().join(format!("nagi-smart-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join("src/Base.java"), "public class Base {\n  public String baseName() { return \"\"; }\n}\n").unwrap();
        std::fs::write(dir.join("src/User.java"), "public class User extends Base {\n  public User() {}\n  private String name;\n  public String getName() { return name; }\n  public void setName(String n) { name = n; }\n}\n").unwrap();
        std::fs::write(dir.join("src/Dto.java"), "import lombok.Data;\n\n/** 注釈の後に説明 */\n@Data\npublic class Dto {\n  private String name;\n  private boolean active;\n  private final int id = 0;\n  private static int count;\n}\n").unwrap();
        std::fs::write(dir.join("src/Point.java"), "public record Point(String title,\n    Map<String, Integer> counts) {\n  public int size() { return 0; }\n}\n").unwrap();
        std::fs::write(dir.join("src/Outer.java"), "public class Outer {\n  public enum Kind {\n    A, B;\n    public String label() { return \"\"; }\n  }\n  public void run() {}\n}\n").unwrap();
        let main = "public class App {\n  void run(User user) {\n    user.\n  }\n}\n";
        std::fs::write(dir.join("src/App.java"), main).unwrap();
        let mut e = Editor::new(Buffer::open(&dir.join("src/App.java")).unwrap(), None);
        e.buf.path = Some(dir.join("src/App.java"));
        e.set_screen(120, 40);
        e.start_index();
        for _ in 0..500 {
            e.poll_index();
            if e.nav.index.as_ref().is_some_and(|i| i.file_count() == 6) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(e.nav.index.is_some(), "索引ができない");
        // user. の後で Ctrl+Space
        keys(&mut e, "3G$a<C-Space>");
        let names = e.completion.as_ref().map(|c| c.items.clone()).unwrap_or_default();
        assert!(names.contains(&"getName".to_string()) && names.contains(&"setName".to_string()) && names.contains(&"baseName".to_string()), "{names:?} {}", e.notify.text);
        assert!(!names.contains(&"name".to_string()), "ほかのクラスの private は出さない: {names:?}");
        assert!(!names.contains(&"User".to_string()), "コンストラクタは出さない: {names:?}");
        assert_eq!(&names[..2], ["getName", "setName"], "定義の順（子の型が先）");
        // 打って絞り込む（飛ばし打ち）
        keys(&mut e, "gN");
        assert_eq!(e.completion.as_ref().unwrap().items[0], "getName");
        // Enter で決める（引数なしは () の後へ）
        keys(&mut e, "<CR>");
        assert_eq!(e.buf.line(2), "    user.getName()");
        assert!(e.completion.is_none());
        // setName は引数があるので括弧の中へ
        keys(&mut e, ";<CR>user.<C-Space>setN<Tab>x");
        assert_eq!(e.buf.line(3), "    user.setName(x)");
        // Esc は一覧だけ閉じる
        keys(&mut e, "<Esc>o<C-Space><Esc>");
        assert_eq!(e.state(), crate::editor::State::Insert);
        // 入れ子の型: クラスの名前の後に出し、その中のメンバーへ
        let items = |e: &mut Editor, text: &str| -> Vec<String> {
            keys(e, &format!("<Esc>o{text}<C-Space>"));
            let v = e.completion.as_ref().map(|c| c.items.clone()).unwrap_or_default();
            keys(e, "<Esc><Esc>dd");
            v
        };
        let v = items(&mut e, "Outer.");
        assert!(v.contains(&"Kind".to_string()), "{v:?}");
        let v = items(&mut e, "user.");
        assert!(!v.contains(&"Kind".to_string()), "変数の後に入れ子の型は出さない: {v:?}");
        let v = items(&mut e, "Outer.Kind.");
        assert!(v.contains(&"A".to_string()) || v.contains(&"label".to_string()), "{v:?}");
        // Lombok の @Data: getter・setter（boolean は is、final は setter なし、static は無し）
        let mut ranker = nagi_workers::index::Ranker::new(e.nav.index.as_ref());
        let names: Vec<String> = ranker.members("Dto").into_iter().filter(|m| !m.4).map(|m| m.0).collect();
        for n in ["getName", "setName", "isActive", "setActive", "getId"] {
            assert!(names.contains(&n.to_string()), "{n}: {names:?}");
        }
        assert!(!names.iter().any(|n| n == "setId" || n == "getCount"), "{names:?}");
        // record の部品の読み出し
        let names: Vec<String> = ranker.members("Point").into_iter().map(|m| m.0).collect();
        for n in ["title", "counts", "size"] {
            assert!(names.contains(&n.to_string()), "{n}: {names:?}");
        }
    }

    /// Java のライブラリの型: 打った語で始まる型を出し、決めると import を足す（JDK の ct.sym から作った索引で）
    #[test]
    fn library_types_add_import() {
        use nagi_workers::jvm::{JarIndex, JarSource};
        let Some(ct) = std::env::var_os("JAVA_HOME").map(|h| std::path::PathBuf::from(h).join("lib").join("ct.sym")).filter(|p| p.is_file()) else { return };
        let dir = std::env::temp_dir().join(format!("nagi-libtypes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let rel = nagi_workers::jvm::ct_sym_latest(&ct).unwrap();
        let keep = |n: &str| n.split('/').next().is_some_and(|d| d.contains(rel)) && (n.contains("/java/util/") || n.contains("/java/awt/List") || n.contains("/java/lang/"));
        let ix = JarIndex::load(&[JarSource { path: &ct, keep: &keep, tag: "t" }], &dir.join("jars"));
        std::fs::write(dir.join("App.java"), "package app;\n\nclass App {\n  void f() {\n    \n  }\n}\n").unwrap();
        let mut e = Editor::new(Buffer::open(&dir.join("App.java")).unwrap(), None);
        e.buf.path = Some(dir.join("App.java"));
        e.set_screen(120, 40);
        e.nav.jars = Some(std::sync::Arc::new(ix));
        // 小文字で打っても型を出す。決めると import を足す
        keys(&mut e, "5G$aarrayLi<C-Space>");
        let c = e.completion.as_ref().unwrap_or_else(|| panic!("{} / {:?}", e.notify.text, e.buf.line(4)));
        assert_eq!(c.items[0], "ArrayList", "{:?}", c.items);
        keys(&mut e, "<CR>");
        assert_eq!(e.buf.text(), "package app;\n\nimport java.util.ArrayList;\n\nclass App {\n  void f() {\n    ArrayList\n  }\n}\n");
        assert_eq!((e.cursor.line, e.cursor.byte), (6, 13), "カーソルは入れた名前の後");
        // 同じ単純名が 2 つ（java.util.List と java.awt.List）なら両方を出す。2 つ目は名前の順で後ろ
        keys(&mut e, " <Esc>o");
        keys(&mut e, "Lis<C-Space>");
        let c = e.completion.as_ref().unwrap();
        let notes: Vec<String> = c.items.iter().zip(&c.notes).filter(|(n, _)| *n == "List").map(|(_, x)| x.1.clone()).collect();
        assert!(notes.contains(&"java.util".to_string()) && notes.contains(&"java.awt".to_string()), "{notes:?}");
        // java.lang は import しない
        keys(&mut e, "<Esc><Esc>oStringBui<C-Space><CR>");
        assert!(!e.buf.text().contains("import java.lang"), "{}", e.buf.text());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Java のライブラリの型のメンバー: 変数の宣言の型引数から、メソッドの戻り値をたどる（JDK の ct.sym から作った索引で）
    #[test]
    fn library_members_after_dot() {
        use nagi_workers::jvm::{JarIndex, JarSource};
        let Some(ct) = std::env::var_os("JAVA_HOME").map(|h| std::path::PathBuf::from(h).join("lib").join("ct.sym")).filter(|p| p.is_file()) else { return };
        let dir = std::env::temp_dir().join(format!("nagi-libmembers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let rel = nagi_workers::jvm::ct_sym_latest(&ct).unwrap();
        let keep = |n: &str| n.split('/').next().is_some_and(|d| d.contains(rel)) && (n.contains("/java/util/") || n.contains("/java/lang/"));
        let ix = JarIndex::load(&[JarSource { path: &ct, keep: &keep, tag: "m" }], &dir.join("jars"));
        std::fs::write(dir.join("App.java"), "package app;\n\nimport java.util.*;\nimport static java.util.Collections.emptyList;\n\nclass App {\n  void f(List<String> names, Map<String, List<Integer>> byName) {\n    var xs = new ArrayList<Integer>();\n    var s = names.get(0);\n  }\n}\n").unwrap();
        let mut e = Editor::new(Buffer::open(&dir.join("App.java")).unwrap(), None);
        e.buf.path = Some(dir.join("App.java"));
        e.set_screen(120, 40);
        e.nav.jars = Some(std::sync::Arc::new(ix));
        // プロジェクトの型のメソッドが返す型引数つきの型（List<String>）
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join("Repo.java"), "package app;\n\nimport java.util.List;\n\npublic class Repo {\n  public List<String> names() { return null; }\n}\n").unwrap();
        std::fs::write(dir.join("Ctx.java"), "package app;\n\nimport java.util.ArrayList;\n\npublic class Ctx extends ArrayList<String> {\n}\n").unwrap();
        e.start_index();
        for _ in 0..500 {
            e.poll_index();
            if e.nav.index.as_ref().is_some_and(|i| i.file_count() == 3) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let items = |e: &mut Editor, text: &str| -> Vec<String> {
            keys(e, &format!("9GA<CR>{text}<C-Space>"));
            let v = e.completion.as_ref().map(|c| c.items.clone()).unwrap_or_default();
            keys(e, "<Esc><Esc>dd");
            v
        };
        let has = |v: &[String], names: &[&str]| names.iter().all(|n| v.iter().any(|x| x == n));
        let v = items(&mut e, "names.");
        assert!(has(&v, &["add", "size", "stream", "get", "hashCode"]), "{v:?}");
        assert!(!v.iter().any(|x| x == "of"), "変数からは static を出さない: {v:?}");
        let v = items(&mut e, "names.get(0).");
        assert!(has(&v, &["substring", "length", "isEmpty"]), "List<String>.get() は String: {v:?}");
        let v = items(&mut e, "names.stream().filter(s -> true).");
        assert!(has(&v, &["map", "collect", "forEach"]), "Stream: {v:?}");
        let v = items(&mut e, "byName.get(\"a\").get(0).");
        assert!(has(&v, &["intValue", "compareTo"]), "Map<String, List<Integer>> → Integer: {v:?}");
        let v = items(&mut e, "xs.");
        assert!(has(&v, &["ensureCapacity", "add"]), "var = new ArrayList<..>: {v:?}");
        let v = items(&mut e, "Collections.");
        assert!(has(&v, &["emptyList", "sort"]) && !v.iter().any(|x| x == "hashCode"), "クラス名からは static: {v:?}");
        // 型を書かない変数（var・for の var・ラムダの引数）、new、static の import
        let v = items(&mut e, "s.");
        assert!(has(&v, &["substring"]), "var s = names.get(0): {v:?}");
        let v = items(&mut e, "for (var x : names) x.");
        assert!(has(&v, &["substring"]), "for (var x : names): {v:?}");
        let v = items(&mut e, "names.forEach(v -> v.");
        assert!(has(&v, &["substring"]), "Consumer<String> の引数: {v:?}");
        let v = items(&mut e, "byName.forEach((k, w) -> w.");
        assert!(has(&v, &["size"]), "BiConsumer<String, List<Integer>> の 2 つ目: {v:?}");
        let v = items(&mut e, "new ArrayList<String>().");
        assert!(has(&v, &["ensureCapacity"]), "new: {v:?}");
        let v = items(&mut e, "emptyList().");
        assert!(has(&v, &["size"]), "static の import: {v:?}");
        // 引数の型で形を選び、メソッドの型変数を決める（<T> T requireNonNull(T)・<E> List<E> of(E)）
        let v = items(&mut e, "Objects.requireNonNull(names).");
        assert!(has(&v, &["size"]), "requireNonNull(List<String>): {v:?}");
        let v = items(&mut e, "List.of(\"a\").get(0).");
        assert!(has(&v, &["substring"]), "List.of(String): {v:?}");
        let v = items(&mut e, "Collections.singletonList(names).get(0).");
        assert!(has(&v, &["size"]), "singletonList(List<String>): {v:?}");
        let v = items(&mut e, "names.get(0).split(\",\").");
        assert!(has(&v, &["length"]), "配列: {v:?}");
        // 入れ子の型
        let v = items(&mut e, "Map.");
        assert!(has(&v, &["Entry", "of"]), "Map.: {v:?}");
        let v = items(&mut e, "Map.Entry.");
        assert!(has(&v, &["comparingByKey"]), "Map.Entry.: {v:?}");
        let v = items(&mut e, "new Ctx().get(0).");
        assert!(has(&v, &["substring"]), "ライブラリの親の型引数（extends ArrayList<String>）: {v:?}");
        let v = items(&mut e, "new Repo().names().get(0).");
        assert!(has(&v, &["substring"]), "プロジェクトのメソッドの List<String>: {v:?}");
        // パッケージの名前の後は下のパッケージとクラス、完全な名前のクラスの後は static
        let v = items(&mut e, "java.util.");
        assert!(has(&v, &["function", "List", "Collections"]) && !v.iter().any(|x| x == "String"), "java.util.: {v:?}");
        let v = items(&mut e, "java.");
        assert!(has(&v, &["util", "lang"]), "java.: {v:?}");
        let v = items(&mut e, "java.util.Collections.");
        assert!(has(&v, &["emptyList", "class"]), "完全な名前: {v:?}");
        let v = items(&mut e, "java.util.Map.");
        assert!(has(&v, &["of"]), "import していない完全な名前: {v:?}");
        // 決めると () を入れる（引数があれば中へ）
        keys(&mut e, "9GA<CR>names.siz<C-Space><CR>");
        assert_eq!(e.buf.line(9).trim(), "names.size()");
        // 定義へ（gd）: ライブラリのメソッドは JDK の src.zip のソースへ（無ければ骨組み）
        e.nav.sources_dir = Some(dir.join("sources"));
        keys(&mut e, "<Esc>0fzgd");
        let p = e.buf.path.clone().unwrap();
        assert!(p.ends_with("java/util/List.java") || p.ends_with("java\\util\\List.java"), "{}: {}", p.display(), e.notify.text);
        assert!(e.buf.line(e.cursor.line).contains("size()"), "{}", e.buf.line(e.cursor.line));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// TypeScript: 組み込み（lib の .d.ts）と node_modules のパッケージの型の、. の後のメンバー
    #[test]
    fn typescript_members_after_dot() {
        let dir = std::env::temp_dir().join(format!("nagi-tsmembers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let lib = dir.join("node_modules/typescript/lib");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join("tsconfig.json"), "{ // コメント\n  \"compilerOptions\": { \"target\": \"ES2022\", },\n}\n").unwrap();
        std::fs::write(lib.join("lib.es2022.full.d.ts"), "/// <reference lib=\"es5\" />\n/// <reference lib=\"dom\" />\n").unwrap();
        std::fs::write(lib.join("lib.es5.d.ts"), "interface String {\n  trim(): string;\n  readonly length: number;\n}\ninterface Array<T> {\n  map<U>(callbackfn: (value: T, index: number) => U): U[];\n  find(predicate: (value: T) => unknown): T | undefined;\n  readonly length: number;\n}\n").unwrap();
        std::fs::write(lib.join("lib.dom.d.ts"), "interface Console {\n  log(...data: any[]): void;\n}\ndeclare var console: Console;\n").unwrap();
        let pkg = dir.join("node_modules/@acme/http");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(dir.join("package.json"), "{ \"dependencies\": { \"@acme/http\": \"1.0.0\" } }").unwrap();
        std::fs::write(pkg.join("package.json"), "{ \"types\": \"./index.d.ts\" }").unwrap();
        std::fs::write(pkg.join("index.d.ts"), "export interface Res<T> { body: T; status: number }\nexport declare class HttpClient {\n  get<T>(url: string): Res<T>;\n}\n").unwrap();
        std::fs::write(dir.join("app.ts"), "import { HttpClient } from '@acme/http';\n\ninterface User { name: string }\n\nexport class App {\n  constructor(private http: HttpClient) {}\n  run(users: User[]) {\n  }\n}\n").unwrap();
        let mut e = Editor::new(Buffer::open(&dir.join("app.ts")).unwrap(), None);
        e.buf.path = Some(dir.join("app.ts"));
        e.set_screen(120, 40);
        e.start_index();
        e.nav.dts = Some(std::sync::Arc::new(nagi_workers::dts::DtsIndex::load(&dir)));
        for _ in 0..500 {
            e.poll_index();
            if e.nav.index.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let items = |e: &mut Editor, text: &str| -> Vec<String> {
            keys(e, &format!("7GA<CR>{text}<C-Space>"));
            let v = e.completion.as_ref().map(|c| c.items.clone()).unwrap_or_default();
            keys(e, "<Esc><Esc>dd");
            v
        };
        let has = |v: &[String], names: &[&str]| names.iter().all(|n| v.iter().any(|x| x == n));
        let v = items(&mut e, "console.");
        assert!(has(&v, &["log"]), "大域の変数: {v:?}");
        let v = items(&mut e, "this.http.");
        assert!(has(&v, &["get"]), "コンストラクタの引数のフィールド・パッケージのクラス: {v:?}");
        let v = items(&mut e, "this.http.get<User>('/u').body.");
        assert!(has(&v, &["name"]), "型引数: {v:?}");
        let v = items(&mut e, "users.find(u => u.name === 'a')?.name.");
        assert!(has(&v, &["trim", "length"]), "?. と string: {v:?}");
        let v = items(&mut e, "users.map(u => u.");
        assert!(has(&v, &["name"]), "ラムダの引数: {v:?}");
        let v = items(&mut e, "const xs = users;\n    xs.");
        assert!(has(&v, &["map", "find"]), "const の初期値: {v:?}");
        // パッケージの名前を決めると import を足す（同じモジュールの import の { } の中へ）
        keys(&mut e, "7GA<CR>let r: Re<C-Space><CR><Esc>");
        assert_eq!(e.buf.line(0), "import { HttpClient, Res } from '@acme/http';", "{}", e.buf.text());
        keys(&mut e, "uu");
        assert_eq!(e.buf.line(0), "import { HttpClient } from '@acme/http';");
        // 定義へ（gd）: パッケージの .d.ts のメソッドの宣言の行
        keys(&mut e, "7GA<CR>this.http.get('/u');<Esc>0fggd");
        assert!(e.buf.path.as_ref().unwrap().ends_with("index.d.ts"), "{:?} {}", e.buf.path, e.notify.text);
        assert!(e.buf.line(e.cursor.line).contains("get<T>(url: string)"), "{}", e.buf.line(e.cursor.line));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 実際の Java のプロジェクトで、`x.m` の m（コンパイルが通るコードなので正しい答え）が `.` の後の候補に入る割合
    /// （NAGI_JAVA_ROOT=… LOCALAPPDATA=… cargo test -p nagi member_rate -- --ignored --nocapture。読むだけ）
    #[test]
    #[ignore]
    fn member_rate() {
        let Some(root) = std::env::var_os("NAGI_JAVA_ROOT").map(std::path::PathBuf::from) else { return };
        let rx = nagi_workers::index::build(&root, std::sync::Arc::new(|| {}));
        let idx = loop {
            if let Ok(nagi_workers::index::Progress::Done(i)) = rx.recv() {
                break i;
            }
        };
        let mut e = Editor::new(Buffer::from_str(""), None);
        e.nav.index = Some(*idx);
        // NAGI_NO_JARS があればライブラリの型なしで（比べる用）
        if std::env::var_os("NAGI_NO_JARS").is_none() {
            let task = nagi_workers::jvm::load_maven(root.clone(), std::sync::Arc::new(|| {}));
            let jars = loop {
                if let Ok(r) = task.try_recv() {
                    break r.unwrap();
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            };
            e.nav.jars = Some(std::sync::Arc::new(jars));
        }
        // x.m と、行の頭の .m（前の行からの連なり）
        // TypeScript の型の宣言（node_modules があれば）
        if root.join("node_modules").join("typescript").is_dir() {
            e.nav.dts = Some(std::sync::Arc::new(nagi_workers::dts::DtsIndex::load(&root)));
        }
        let site = regex::Regex::new(r"(?:[\w)]|^\s*)\.([A-Za-z_]\w*)").unwrap();
        // [型が分かって当たった, 型が分かったが無い, ファイルの語で代わり, 何も無い]
        let mut n = [0usize; 4];
        let mut miss: HashMap<String, usize> = HashMap::new();
        let mut untyped: HashMap<String, usize> = HashMap::new();
        let mut example: HashMap<String, String> = HashMap::new();
        let t0 = std::time::Instant::now();
        // NAGI_RATE_EXT（, 区切り）の拡張子のファイル（既定は java）
        let exts: Vec<String> = std::env::var("NAGI_RATE_EXT").unwrap_or_else(|_| "java".into()).split(',').map(String::from).collect();
        let mut files = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(d) = stack.pop() {
            for f in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = f.path();
                let name = f.file_name().to_string_lossy().into_owned();
                if p.is_dir() && !name.starts_with('.') && name != "target" && name != "node_modules" {
                    stack.push(p);
                } else if exts.iter().any(|x| name.ends_with(&format!(".{x}"))) && !name.ends_with(".d.ts") && !name.ends_with(".spec.ts") {
                    files.push(p);
                }
            }
        }
        files.sort();
        // NAGI_RATE_FILES があれば、その数ごとに 1 つのファイルだけ（速く測る）
        let step = std::env::var("NAGI_RATE_FILES").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).max(1);
        let mut slow: Vec<(u128, String)> = Vec::new();
        // NAGI_RATE_ONLY があれば、パスにその文字を含むファイルだけ
        let only = std::env::var("NAGI_RATE_ONLY").ok();
        // NAGI_RATE_DUMP があれば、1 か所ごとの候補（名前・印・説明・順・import を集めた順に）の要約を書く（作り替えの前後で比べる）
        let mut dump = std::env::var_os("NAGI_RATE_DUMP").map(|f| std::io::BufWriter::new(std::fs::File::create(f).unwrap()));
        for p in files.iter().step_by(step).filter(|p| only.as_ref().is_none_or(|o| p.to_string_lossy().contains(o.as_str()))) {
            let p = p.as_path();
            let Ok(buf) = Buffer::open(p) else { continue };
            e.buf = buf;
            e.buf.path = Some(p.to_path_buf());
            for li in 0..e.buf.line_count() {
                let line = e.buf.line(li).to_string();
                let t = line.trim_start();
                if ["import", "package", "//", "*", "/*", "@"].iter().any(|x| t.starts_with(x)) {
                    continue;
                }
                for m in site.captures_iter(&line) {
                    let g = m.get(1).unwrap();
                    // 文字列の中は除く
                    if ['"', '\'', '`'].iter().any(|q| line[..g.start()].matches(*q).count() % 2 == 1) {
                        continue;
                    }
                    let ws = g.start();
                    let start = e.buf.line_start(li) + ws;
                    let t1 = std::time::Instant::now();
                    let c = e.smart_candidates(&line, ws, start);
                    let us = t1.elapsed().as_micros();
                    if let Some(d) = &mut dump {
                        use std::hash::{Hash, Hasher};
                        let mut h = std::collections::hash_map::DefaultHasher::new();
                        for x in &c {
                            (&x.name, x.mark, &x.note, x.call, x.rank, &x.insert, &x.import).hash(&mut h);
                        }
                        let rel = p.strip_prefix(&root).unwrap_or(p).to_string_lossy().replace('\\', "/");
                        std::io::Write::write_fmt(d, format_args!("{rel}:{}:{ws}\t{}\t{:016x}\n", li + 1, c.len(), h.finish())).unwrap();
                    }
                    if std::env::var("NAGI_RATE_SHOW").is_ok_and(|x| line.contains(&x)) {
                        println!("{}:{} {} -> {:?}", p.display(), li + 1, line.trim(), c.iter().take(8).map(|x| (&x.name, x.rank)).collect::<Vec<_>>());
                    }
                    slow.push((us, format!("{}:{} {}", p.file_name().unwrap().to_string_lossy(), li + 1, line.trim())));
                    // 受け手の式（呼び出しの引数は省く）
                    let before = crate::java_members::with_previous_lines(&e.buf.text(), e.buf.line_start(li), &line[..ws - 1]);
                    let before = before.as_str();
                    let recv = match crate::java_members::chain_start(before) {
                        Some(i) => {
                            let mut r = String::new();
                            let mut depth = 0;
                            for c in before[i..].chars() {
                                match c {
                                    '(' => {
                                        if depth == 0 {
                                            r.push('(');
                                        }
                                        depth += 1
                                    }
                                    ')' => {
                                        depth -= 1;
                                        if depth == 0 {
                                            r.push(')');
                                        }
                                    }
                                    _ if depth == 0 => r.push(c),
                                    _ => {}
                                }
                            }
                            r
                        }
                        None => before.trim_start().rsplit([' ', '(', '!', '=', ',']).next().unwrap_or("").to_string(),
                    };
                    let k = if c.is_empty() {
                        3
                    } else if c.iter().all(|x| x.rank == 1) {
                        2
                    } else if c.iter().any(|x| x.name == g.as_str()) {
                        0
                    } else {
                        1
                    };
                    n[k] += 1;
                    match k {
                        1 => *miss.entry(format!("{recv}.{}", g.as_str())).or_default() += 1,
                        2 | 3 => {
                            example.entry(recv.clone()).or_insert_with(|| line.trim().chars().take(100).collect());
                            *untyped.entry(recv).or_default() += 1
                        }
                        _ => {}
                    }
                }
            }
        }
        slow.sort_by(|a, b| b.0.cmp(&a.0));
        let total: u128 = slow.iter().map(|x| x.0).sum();
        println!("1 回: 平均 {} µs、中央 {} µs、最大 {:?}", total / slow.len().max(1) as u128, slow.get(slow.len() / 2).map_or(0, |x| x.0), &slow[..slow.len().min(5)]);
        let all: usize = n.iter().sum();
        let pc = |x: usize| x as f64 * 100.0 / all.max(1) as f64;
        println!("{all} 箇所 {:?}: 当たり {:.1}% / 型は分かったが無い {:.1}% / ファイルの語 {:.1}% / 無し {:.1}%", t0.elapsed(), pc(n[0]), pc(n[1]), pc(n[2]), pc(n[3]));
        for (title, m) in [("型は分かったが無い", miss), ("型が分からない受け手", untyped)] {
            let mut v: Vec<_> = m.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1));
            println!("-- {title}");
            for (k, c) in v.iter().take(40) {
                println!("{c:5} {k}    {}", example.get(k).map_or("", String::as_str));
            }
        }
    }

    /// 設定: 自動で出す（語の文字が 2 つ並んだら、待ち時間の後）、大文字小文字を区別する、予約語を出さない
    #[test]
    fn settings_auto_and_case() {
        let mut e = Editor::new(Buffer::from_str("fooBar foobaz Fooqux\n"), None);
        e.set_screen(100, 30);
        e.settings.set("completion.auto", "true", false);
        e.settings.set("completion.delay_ms", "0", false);
        e.settings.set("completion.case", "all", false);
        e.settings.set("completion.keywords", "false", false);
        keys(&mut e, "Go");
        keys(&mut e, "f");
        assert!(e.auto_complete_at.is_none(), "1 文字では出さない");
        keys(&mut e, "o");
        assert!(e.auto_complete_at.is_some());
        assert!(e.fire_auto_complete(std::time::Instant::now()));
        let items = e.completion.as_ref().unwrap().items.clone();
        assert!(items.contains(&"fooBar".to_string()) && items.contains(&"foobaz".to_string()));
        assert!(!items.contains(&"Fooqux".to_string()), "区別するので Fooqux は出ない: {items:?}");
        // 打つと絞り込む（区別する: B は fooBar だけ）
        keys(&mut e, "oB");
        assert_eq!(e.completion.as_ref().unwrap().items, ["fooBar"]);
    }
}
