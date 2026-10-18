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
    let n = regex::escape(name);
    let pats = [
        // Java・C#: Type name / Type<..> name / Type[] name（宣言・引数・フィールド）
        format!(r"\b([A-Z][\w]*)(?:<[^;(){{}}=]*>)?(?:\[\])*\s+{n}\s*(?:[=;,):]|$)"),
        // TypeScript・Kotlin: name: Type
        format!(r"\b{n}\s*\??:\s*([A-Z]\w*)"),
        // var・val・let・const name = new Type( / Type(
        format!(r"\b(?:var|val|let|const|auto)\s+{n}\s*=\s*(?:new\s+)?([A-Z]\w*)"),
    ];
    let mut best: Option<(usize, String)> = None;
    for p in pats {
        let Ok(re) = regex::Regex::new(&p) else { continue };
        for c in re.captures_iter(text) {
            let (pos, ty) = (c.get(0).map_or(0, |m| m.start()), c[1].to_string());
            // 前の最後を選び、前に無ければ後ろの最初
            let better = match &best {
                None => true,
                Some((b, _)) => (pos < at && (*b > at || pos > *b)) || (pos >= at && *b >= at && pos < *b),
            };
            if better {
                best = Some((pos, ty));
            }
        }
    }
    best.map(|b| b.1)
}

/// `.` の前の式を受け手にする（`a.b().c` → Call・Member の連なり）。`dot` は `.` の位置（行の中の byte）
pub fn receiver_before(line: &str, dot: usize, var: &dyn Fn(&str) -> Option<String>) -> Option<Receiver> {
    let s = &line[..dot];
    let mut parts: Vec<(String, bool)> = Vec::new();
    let mut end = s.len();
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
        let word_start = s[..i].char_indices().rev().take_while(|(_, c)| word_char(*c)).last().map(|x| x.0)?;
        parts.push((s[word_start..i].to_string(), call));
        if word_start > 0 && s[..word_start].ends_with('.') {
            end = word_start - 1;
        } else {
            break;
        }
    }
    parts.reverse();
    let mut it = parts.into_iter();
    let (base, call) = it.next()?;
    let mut r = if call {
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
        let all = self.smart_candidates(&line, ws, start);
        if all.is_empty() {
            self.notify.info("-- 補完の候補はありません");
            return;
        }
        self.completion = Some(Completion { start, original: prefix, items: Vec::new(), sel: Some(0), notes: Vec::new(), smart: Some(SmartState { all, shown: Vec::new() }) });
        self.refilter_smart();
    }

    /// 候補を集める（`ws` は打っている語の始まり（行の中の byte）、`start` はその絶対位置）
    fn smart_candidates(&mut self, line: &str, ws: usize, start: usize) -> Vec<Cand> {
        let src = self.buf.text();
        let here = self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok());
        let lang = self.syntax.as_ref().map(|s| s.lang);
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
        let member_cands = |ranker: &mut nagi_workers::index::Ranker, ty: &str, out: &mut Vec<Cand>, push: &mut dyn FnMut(&mut Vec<Cand>, Cand), rank: u8| {
            for (name, kind, owner, params, private) in ranker.members(ty) {
                if private && inside.as_deref() != Some(owner.as_str()) {
                    continue;
                }
                let call = matches!(kind, DefKind::Method | DefKind::Function).then(|| params != Some(0));
                push(out, Cand { name, mark: mark_of(kind), note: owner, call, rank });
            }
        };
        if member {
            let var = |n: &str| var_type(&src, n, start);
            let recv = receiver_before(line, ws - 1, &var);
            let ty = recv.as_ref().and_then(|r| ranker.receiver_type_name(&rel, &src, start, r));
            if let Some(t) = &ty {
                member_cands(&mut ranker, t, &mut out, &mut push, 0);
            }
            if out.is_empty() {
                // 型が分からない・プロジェクトの外: ファイルの中で `.` の後に出てくる語
                let re = regex::Regex::new(r"\.([A-Za-z_$][\w$]*)").unwrap();
                for c in re.captures_iter(&src) {
                    let w = c[1].to_string();
                    let call = src[c.get(0).unwrap().end()..].starts_with('(');
                    push(&mut out, Cand { name: w, mark: if call { 'm' } else { 'f' }, note: ty.clone().unwrap_or_default(), call: call.then_some(true), rank: 1 });
                }
            }
            return out;
        }
        // 今の型のメンバー
        if let Some(t) = inside.clone() {
            member_cands(&mut ranker, &t, &mut out, &mut push, 1);
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
                push(&mut out, Cand { name: w.to_string(), mark: 'v', note: String::new(), call: None, rank: 0 + u8::from(call) * 2 });
            }
        }
        // プロジェクトの型
        if let Some(idx) = idx {
            let mut types: Vec<(&str, DefKind)> = idx.all().filter(|(_, l)| l.kind.is_type()).map(|(n, l)| (n, l.kind)).collect();
            types.sort_unstable_by(|a, b| a.0.cmp(b.0));
            for (n, k) in types {
                push(&mut out, Cand { name: n.to_string(), mark: mark_of(k), note: String::new(), call: None, rank: 3 });
            }
        }
        // 予約語
        for k in kws {
            push(&mut out, Cand { name: k.to_string(), mark: 'K', note: String::new(), call: None, rank: 4 });
        }
        out
    }

    /// 打った語で絞り込み直す（合うものが無ければ閉じる）
    pub(crate) fn refilter_smart(&mut self) {
        let abs = self.abs();
        let Some(c) = &mut self.completion else { return };
        let Some(st) = &mut c.smart else { return };
        if abs < c.start {
            self.completion = None;
            return;
        }
        let prefix = self.buf.slice_string(c.start, abs);
        let q = Query::new(&prefix);
        let case = self.settings.get("completion.case");
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
        if self.state() != crate::editor::State::Insert || self.completion.is_some() || !self.settings.bool("completion.auto") || self.console_source().is_some() {
            return;
        }
        let Key::Char(c) = key else { return };
        let line = self.buf.line(self.cursor.line);
        let b = self.cursor.byte.min(line.len());
        let n = line[..b].chars().rev().take_while(|c| word_char(*c)).count();
        if c == '.' || (word_char(c) && n >= self.settings.int("completion.min_chars") as usize) {
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
        let (text, back) = match cand.call {
            Some(args) if after != "(" => (format!("{}()", cand.name), usize::from(args)),
            _ => (cand.name.clone(), 0),
        };
        self.edit(c.start, abs, &text);
        self.set_abs(c.start + text.len() - back);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

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
        let main = "public class App {\n  void run(User user) {\n    user.\n  }\n}\n";
        std::fs::write(dir.join("src/App.java"), main).unwrap();
        let mut e = Editor::new(Buffer::open(&dir.join("src/App.java")).unwrap(), None);
        e.buf.path = Some(dir.join("src/App.java"));
        e.set_screen(120, 40);
        e.start_index();
        for _ in 0..500 {
            e.poll_index();
            if e.nav.index.as_ref().is_some_and(|i| i.file_count() == 3) {
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
