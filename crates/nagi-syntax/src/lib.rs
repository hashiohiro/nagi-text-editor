//! tree-sitter による構文解析とハイライト（architecture.md §5.3、ADR 0008）。
//!
//! - 構文木はバッファごとに 1 つ持ち、編集はインクリメンタルに反映する（`Tree::edit` → 再パース）。
//! - ハイライトは表示範囲だけにクエリをかける（`QueryCursor::set_byte_range`）。
//! - クエリは言語ごとに 1 回だけコンパイルして使い回す。

use std::ops::Range;
use std::path::Path;
use std::sync::OnceLock;

use nagi_core::{Buffer, EditLog};
use tree_sitter::{InputEdit, Language, Node, Parser, Point, Query, QueryCursor, StreamingIterator, Tree};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    TypeScript,
    Tsx,
    JavaScript,
    Java,
    Kotlin,
}

impl Lang {
    pub fn detect(path: &Path) -> Option<Lang> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "ts" | "mts" | "cts" => Lang::TypeScript,
            "tsx" => Lang::Tsx,
            "js" | "mjs" | "cjs" | "jsx" => Lang::JavaScript,
            "java" => Lang::Java,
            "kt" | "kts" => Lang::Kotlin,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Lang::TypeScript => "TypeScript",
            Lang::Tsx => "TSX",
            Lang::JavaScript => "JavaScript",
            Lang::Java => "Java",
            Lang::Kotlin => "Kotlin",
        }
    }

    fn language(self) -> Language {
        match self {
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Lang::Java => tree_sitter_java::LANGUAGE.into(),
            Lang::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
        }
    }

    /// ハイライトのクエリ。TypeScript は JavaScript のクエリに TS 固有分を足して使う（grammar の想定どおり）
    fn highlights_source(self) -> String {
        match self {
            Lang::TypeScript | Lang::Tsx => {
                format!("{}\n{}", tree_sitter_javascript::HIGHLIGHT_QUERY, tree_sitter_typescript::HIGHLIGHTS_QUERY)
            }
            Lang::JavaScript => format!("{}\n{}", tree_sitter_javascript::HIGHLIGHT_QUERY, tree_sitter_javascript::JSX_HIGHLIGHT_QUERY),
            Lang::Java => tree_sitter_java::HIGHLIGHTS_QUERY.to_string(),
            Lang::Kotlin => include_str!("../queries/kotlin/highlights.scm").to_string(),
        }
    }

    /// 言語ごとに 1 回だけコンパイルしたクエリ
    pub fn highlights(self) -> &'static Result<Query, String> {
        static Q: [OnceLock<Result<Query, String>>; 5] = [const { OnceLock::new() }; 5];
        Q[self as usize].get_or_init(|| Query::new(&self.language(), &self.highlights_source()).map_err(|e| format!("{e}")))
    }
}

/// ハイライトの種類。色数を絞るため、キャプチャ名をこの数種類にまとめる
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hl {
    Keyword,
    String,
    Comment,
    Type,
    Number,
    Function,
    Attribute,
    Escape,
}

fn classify(capture: &str) -> Option<Hl> {
    let head = capture.split('.').next().unwrap_or(capture);
    Some(match head {
        "keyword" | "conditional" | "repeat" | "include" | "exception" => Hl::Keyword,
        "string" | "character" => {
            if capture.contains("escape") {
                Hl::Escape
            } else {
                Hl::String
            }
        }
        "escape" => Hl::Escape,
        "comment" => Hl::Comment,
        "type" | "constructor" => Hl::Type,
        "number" | "float" | "boolean" => Hl::Number,
        "constant" if capture.contains("builtin") => Hl::Number,
        "function" | "method" => Hl::Function,
        "attribute" | "decorator" | "annotation" | "label" => Hl::Attribute,
        _ => return None,
    })
}

pub struct Syntax {
    pub lang: Lang,
    parser: Parser,
    tree: Option<Tree>,
}

// Parser と Tree は別スレッドへ移せる（初回のパースを読み込みスレッドで行う）
unsafe impl Send for Syntax {}

fn point((row, column): (usize, usize)) -> Point {
    Point { row, column }
}

impl Syntax {
    pub fn new(lang: Lang) -> Option<Self> {
        let mut parser = Parser::new();
        parser.set_language(&lang.language()).ok()?;
        Some(Self { lang, parser, tree: None })
    }

    /// バッファ全体をパースする（初回、または編集を反映した後の再パース）
    pub fn parse(&mut self, buf: &mut Buffer) {
        for e in buf.take_edits() {
            if let Some(t) = &mut self.tree {
                t.edit(&input_edit(e));
            }
        }
        let tree = self.parser.parse_with_options(&mut |byte, _| buf.chunk_from(byte), self.tree.as_ref(), None);
        self.tree = tree;
    }

    /// 編集が溜まっていれば反映して再パースする
    pub fn sync(&mut self, buf: &mut Buffer) -> bool {
        if self.tree.is_some() && !buf.has_edits() {
            return false;
        }
        self.parse(buf);
        true
    }

    /// `range`（絶対 byte）にかかるハイライト。返す範囲は重ならないよう、後から来た内側のキャプチャで上書きした結果
    pub fn highlight(&self, buf: &Buffer, range: Range<usize>) -> Vec<(Range<usize>, Hl)> {
        let (Some(tree), Ok(query)) = (&self.tree, self.lang.highlights()) else { return Vec::new() };
        let names = query.capture_names();
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(range.clone());
        let text = |node: Node| buf.chunks_in(node.start_byte(), node.end_byte());
        let mut caps = cursor.captures(query, tree.root_node(), text);
        let mut out: Vec<(Range<usize>, Hl)> = Vec::new();
        while let Some((m, i)) = caps.next() {
            let c = m.captures()[*i];
            let Some(hl) = classify(names[c.index as usize]) else { continue };
            let r = c.node.start_byte()..c.node.end_byte();
            // 同じノードに複数のキャプチャがあれば、クエリで先に書かれたもの（tree-sitter の規約）を残す
            if out.last().is_some_and(|(lr, _)| *lr == r) {
                continue;
            }
            out.push((r, hl));
        }
        out
    }

    /// ERROR ノードの数（構文エラーの有無の目安。[e ]e で使う）
    pub fn has_error(&self) -> bool {
        self.tree.as_ref().is_some_and(|t| t.root_node().has_error())
    }
}

fn input_edit(e: EditLog) -> InputEdit {
    InputEdit {
        start_byte: e.start,
        old_end_byte: e.old_end,
        new_end_byte: e.new_end,
        start_position: point(e.start_pos),
        old_end_position: point(e.old_end_pos),
        new_end_position: point(e.new_end_pos),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_compile() {
        for lang in [Lang::TypeScript, Lang::Tsx, Lang::JavaScript, Lang::Java, Lang::Kotlin] {
            if let Err(e) = lang.highlights() {
                panic!("{}: {e}", lang.name());
            }
        }
    }

    fn kinds(lang: Lang, src: &str) -> Vec<(String, Hl)> {
        let mut buf = Buffer::from_str(src);
        let mut s = Syntax::new(lang).unwrap();
        s.parse(&mut buf);
        s.highlight(&buf, 0..src.len()).into_iter().map(|(r, h)| (src[r].to_string(), h)).collect()
    }

    #[test]
    fn typescript() {
        let k = kinds(Lang::TypeScript, "// c\nconst x: string = \"a\"; function f() { return 1; }\n");
        assert!(k.contains(&("// c".into(), Hl::Comment)), "{k:?}");
        assert!(k.contains(&("const".into(), Hl::Keyword)), "{k:?}");
        assert!(k.contains(&("\"a\"".into(), Hl::String)), "{k:?}");
        assert!(k.contains(&("string".into(), Hl::Type)), "{k:?}");
        assert!(k.contains(&("1".into(), Hl::Number)), "{k:?}");
    }

    #[test]
    fn kotlin() {
        let k = kinds(Lang::Kotlin, "// c\nfun main() { val s = \"a\"; println(1) }\n");
        assert!(k.contains(&("fun".into(), Hl::Keyword)), "{k:?}");
        assert!(k.contains(&("\"a\"".into(), Hl::String)), "{k:?}");
        assert!(k.contains(&("main".into(), Hl::Function)), "{k:?}");
    }

    #[test]
    fn incremental_edit() {
        let mut buf = Buffer::from_str("const a = 1;\n");
        let mut s = Syntax::new(Lang::TypeScript).unwrap();
        s.parse(&mut buf);
        buf.edit(0, 0, "// x\n");
        assert!(s.sync(&mut buf));
        let k: Vec<_> = s.highlight(&buf, 0..buf.len_bytes()).into_iter().map(|(_, h)| h).collect();
        assert_eq!(k.first(), Some(&Hl::Comment));
        assert!(!s.has_error());
    }
}
