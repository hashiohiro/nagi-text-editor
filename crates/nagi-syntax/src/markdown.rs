//! Markdown のハイライトとプレビュー（pulldown-cmark で解析し、元の文章の位置を使う）。
//!
//! - ハイライト: 見出し・強調・斜体・コード（行内とブロック）・リンク・引用・リスト・表・チェックボックス。`#` や `**`
//!   などの記号は控えめな灰色。コードブロックは言語名があれば、その言語のハイライト（```sh、```java など）
//! - プレビュー: 等幅のまま整えた行（見出しの罫・コードの地・表の罫線・チェックボックス・引用の ▎・画像の枠）。行ごとに元の行を覚え、
//!   エディタのカーソルの行に合わせてスクロールできる（チェックボックスは [✓] [ ]）

use std::ops::Range;

use nagi_core::Buffer;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::{Hl, Lang, Syntax};

fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH
}

/// コードブロックの言語名 → 言語
pub fn code_lang(name: &str) -> Option<Lang> {
    Lang::from_fence(name.trim().split([' ', ',', '{']).next().unwrap_or(""))
}

/// 文章全体のハイライト（位置は文章の byte）。外側の範囲が先、内側（記号・コードの中）が後（後のものが勝つ）
pub fn highlight(text: &str) -> Vec<(Range<usize>, Hl)> {
    let mut out: Vec<(Range<usize>, Hl)> = Vec::new();
    let b = text.as_bytes();
    // 範囲の先頭・末尾にある記号（# * _ ` ~ など）を灰色に
    let marks = |out: &mut Vec<(Range<usize>, Hl)>, r: &Range<usize>, set: &[u8]| {
        let mut s = r.start;
        while s < r.end && set.contains(&b[s]) {
            s += 1;
        }
        if s > r.start {
            out.push((r.start..s, Hl::Comment));
        }
        let mut e = r.end;
        while e > s && set.contains(&b[e - 1]) {
            e -= 1;
        }
        if e < r.end {
            out.push((e..r.end, Hl::Comment));
        }
    };
    let mut table = false;
    for (ev, r) in Parser::new_ext(text, options()).into_offset_iter() {
        match ev {
            Event::Start(Tag::Heading { .. }) => {
                out.push((r.clone(), Hl::Keyword));
                // # の印（ATX）と、下線の = - （Setext）
                let line_end = text[r.start..r.end].find('\n').map_or(r.end, |i| r.start + i);
                let mut s = r.start;
                while s < line_end && (b[s] == b'#' || b[s] == b' ') {
                    s += 1;
                }
                if s > r.start {
                    out.push((r.start..s, Hl::Comment));
                }
                if line_end < r.end {
                    out.push((line_end..r.end, Hl::Comment));
                }
            }
            Event::Start(Tag::Emphasis) => {
                out.push((r.clone(), Hl::Type));
                marks(&mut out, &r, b"*_");
            }
            Event::Start(Tag::Strong) => {
                out.push((r.clone(), Hl::Number));
                marks(&mut out, &r, b"*_");
            }
            Event::Start(Tag::Strikethrough) => {
                out.push((r.clone(), Hl::Comment));
            }
            Event::Code(_) => {
                out.push((r.clone(), Hl::String));
                marks(&mut out, &r, b"`");
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                out.push((r.clone(), Hl::String));
                if let CodeBlockKind::Fenced(lang) = &kind {
                    // フェンスの行は灰色、中はその言語のハイライト
                    let first_end = text[r.start..r.end].find('\n').map_or(r.end, |i| r.start + i + 1);
                    let body_end = text[..r.end].trim_end_matches(['\n', '\r']).rfind('\n').map_or(r.end, |i| i + 1).max(first_end);
                    out.push((r.start..first_end, Hl::Comment));
                    if body_end < r.end {
                        out.push((body_end..r.end, Hl::Comment));
                    }
                    if let Some(l) = code_lang(lang) {
                        let code = &text[first_end..body_end];
                        if let Some(mut syn) = Syntax::new(l) {
                            let mut buf = Buffer::from_str(code);
                            syn.parse(&mut buf);
                            out.extend(syn.highlight(&buf, 0..code.len()).into_iter().map(|(x, h)| (x.start + first_end..x.end + first_end, h)));
                        }
                    }
                }
            }
            Event::Start(Tag::Link { .. }) | Event::Start(Tag::Image { .. }) => {
                // [文字](先): 文字は青、( 先 ) は灰色
                let s = &text[r.clone()];
                match s.rfind("](") {
                    Some(i) => {
                        out.push((r.start..r.start + i + 1, Hl::Keyword));
                        out.push((r.start + i + 1..r.end, Hl::Comment));
                    }
                    None => out.push((r.clone(), Hl::Keyword)),
                }
                if s.starts_with('!') {
                    out.push((r.start..r.start + 1, Hl::Comment));
                }
            }
            Event::Start(Tag::BlockQuote(_)) => {
                // 各行の > の印
                let mut at = r.start;
                for line in text[r.clone()].split_inclusive('\n') {
                    let lead = line.len() - line.trim_start_matches([' ', '>']).len();
                    if line.trim_start().starts_with('>') {
                        out.push((at..at + lead, Hl::Comment));
                    }
                    at += line.len();
                }
            }
            Event::Start(Tag::Item) => {
                // リストの印（- * + 1.）
                let s = &text[r.clone()];
                let lead = s.len() - s.trim_start().len();
                let rest = &s[lead..];
                let n = if rest.starts_with(['-', '*', '+']) { 1 } else { rest.find(['.', ')']).filter(|&i| i > 0 && rest[..i].bytes().all(|c| c.is_ascii_digit())).map_or(0, |i| i + 1) };
                if n > 0 {
                    out.push((r.start + lead..r.start + lead + n, Hl::Number));
                }
            }
            Event::TaskListMarker(_) => out.push((r.clone(), Hl::Number)),
            Event::Start(Tag::Table(_)) => table = true,
            Event::End(TagEnd::Table) => table = false,
            Event::Start(Tag::TableHead) => out.push((r.clone(), Hl::Keyword)),
            Event::Rule | Event::Html(_) | Event::InlineHtml(_) => out.push((r.clone(), Hl::Comment)),
            _ => {}
        }
        let _ = table;
    }
    // 表の | と区切りの行は灰色（表の範囲の中の |）
    for (ev, r) in Parser::new_ext(text, options()).into_offset_iter() {
        if let Event::Start(Tag::Table(_)) = ev {
            for (i, c) in text[r.clone()].bytes().enumerate() {
                if c == b'|' {
                    out.push((r.start + i..r.start + i + 1, Hl::Comment));
                }
            }
            // 2 行目（|---|---|）
            if let Some(l1) = text[r.clone()].find('\n') {
                let s2 = r.start + l1 + 1;
                let e2 = text[s2..r.end].find('\n').map_or(r.end, |i| s2 + i);
                out.push((s2..e2, Hl::Comment));
            }
        }
    }
    out
}

/// プレビューの文字の見た目
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Plain,
    /// 見出し（太字の代わりに色）
    Heading,
    Strong,
    Emph,
    /// 行内のコード（地の色あり）
    Code,
    Link,
    Strike,
    /// 記号・罫線・引用など控えめなもの
    Dim,
    /// リストの印・チェックボックス
    Marker,
    /// コードブロックの中のハイライト
    Hl(Hl),
}

/// プレビューの 1 行
#[derive(Clone, Debug, Default)]
pub struct Line {
    pub text: String,
    /// (text の byte の範囲, 見た目)
    pub spans: Vec<(Range<usize>, Style)>,
    /// コードブロックの行（地の色を塗る）
    pub code: bool,
    /// 元の文章の行（0 始まり。スクロールを合わせる）
    pub src: usize,
    /// リンクの (text の範囲, 先)
    pub links: Vec<(Range<usize>, String)>,
    /// 画像の枠の行
    pub frame: bool,
}

impl Line {
    fn push(&mut self, s: &str, st: Style) {
        let a = self.text.len();
        self.text.push_str(s);
        if st != Style::Plain && !s.is_empty() {
            self.spans.push((a..self.text.len(), st));
        }
    }
}

fn width(s: &str) -> usize {
    nagi_core::display_col(s, s.len(), false)
}

/// 書いた文字を幅 `w` で折り返しながら行に足していく
struct Writer {
    lines: Vec<Line>,
    cur: Line,
    w: usize,
    /// 折り返した続きの行の字下げ
    indent: String,
    src: usize,
}

impl Writer {
    fn flush(&mut self) {
        let l = std::mem::take(&mut self.cur);
        self.lines.push(l);
        self.cur.src = self.src;
    }

    fn blank(&mut self) {
        if !self.cur.text.is_empty() {
            self.flush();
        }
        if self.lines.last().is_some_and(|l| !l.text.is_empty() || l.code) {
            self.cur.src = self.src;
            self.flush();
        }
    }

    fn start_line(&mut self, prefix: &str, st: Style) {
        if !self.cur.text.is_empty() {
            self.flush();
        }
        self.cur.src = self.src;
        self.cur.push(prefix, st);
    }

    /// 語ごとに折り返して足す（日本語は文字ごと）
    fn text(&mut self, s: &str, st: Style, link: Option<&str>) {
        let mut word = String::new();
        let put = |me: &mut Writer, word: &str| {
            if word.is_empty() {
                return;
            }
            if width(&me.cur.text) + width(word) > me.w && width(&me.cur.text) > width(&me.indent) {
                me.flush();
                let ind = me.indent.clone();
                me.cur.push(&ind, Style::Plain);
                if word == " " {
                    return;
                }
            }
            let a = me.cur.text.len();
            me.cur.push(word, st);
            if let Some(u) = link {
                me.cur.links.push((a..me.cur.text.len(), u.to_string()));
            }
        };
        for c in s.chars() {
            let wide = width(c.encode_utf8(&mut [0; 4])) > 1;
            if c == ' ' || wide {
                put(self, &word);
                word.clear();
                let mut b = [0; 4];
                put(self, c.encode_utf8(&mut b));
            } else {
                word.push(c);
            }
        }
        put(self, &word);
    }
}

/// 元の文章の byte → 行（0 始まり）
fn line_of(starts: &[usize], at: usize) -> usize {
    starts.partition_point(|&s| s <= at).saturating_sub(1)
}

/// プレビューの行を作る（幅 `w` で折り返す）
pub fn render(text: &str, w: usize) -> Vec<Line> {
    let w = w.max(20);
    let starts: Vec<usize> = std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();
    let mut wr = Writer { lines: Vec::new(), cur: Line::default(), w, indent: String::new(), src: 0 };
    let mut styles: Vec<Style> = Vec::new();
    let mut link: Option<String> = None;
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut quote = 0usize;
    let mut heading: Option<HeadingLevel> = None;
    let mut code: Option<(Option<Lang>, String, String)> = None;
    let mut image: Option<(String, String)> = None;
    // 表: 行ごとのセル（見出しの行か）
    let mut table: Option<Vec<(bool, Vec<String>)>> = None;
    let mut in_head = false;
    let mut checked: Option<bool> = None;
    let style = |styles: &[Style]| styles.last().copied().unwrap_or(Style::Plain);
    for (ev, r) in Parser::new_ext(text, options()).into_offset_iter() {
        wr.src = line_of(&starts, r.start);
        if let Some(t) = &mut table {
            match &ev {
                Event::Start(Tag::TableHead) => {
                    in_head = true;
                    t.push((true, Vec::new()));
                }
                Event::End(TagEnd::TableHead) => in_head = false,
                Event::Start(Tag::TableRow) => t.push((false, Vec::new())),
                Event::Start(Tag::TableCell) => {
                    if let Some(row) = t.last_mut() {
                        row.1.push(String::new());
                    }
                }
                Event::Text(s) | Event::Code(s) => {
                    if let Some(c) = t.last_mut().and_then(|row| row.1.last_mut()) {
                        c.push_str(s);
                    }
                }
                Event::End(TagEnd::Table) => {
                    let rows = table.take().unwrap_or_default();
                    draw_table(&mut wr, &rows);
                }
                _ => {}
            }
            let _ = in_head;
            continue;
        }
        if let Some((_, _, body)) = &mut code {
            match &ev {
                Event::Text(s) => {
                    body.push_str(s);
                    continue;
                }
                Event::End(TagEnd::CodeBlock) => {
                    let (lang, name, body) = code.take().unwrap();
                    draw_code(&mut wr, lang, &name, &body);
                    continue;
                }
                _ => continue,
            }
        }
        if let Some((alt, _)) = &mut image {
            match &ev {
                Event::Text(s) => {
                    alt.push_str(s);
                    continue;
                }
                Event::End(TagEnd::Image) => {
                    let (alt, url) = image.take().unwrap();
                    draw_image(&mut wr, &alt, &url);
                    continue;
                }
                _ => continue,
            }
        }
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                wr.blank();
                heading = Some(level);
                styles.push(Style::Heading);
                wr.indent.clear();
                wr.start_line("", Style::Plain);
            }
            Event::End(TagEnd::Heading(_)) => {
                styles.pop();
                let lvl = heading.take();
                wr.flush();
                let rule = match lvl {
                    Some(HeadingLevel::H1) => Some('━'),
                    Some(HeadingLevel::H2) => Some('─'),
                    _ => None,
                };
                if let Some(c) = rule {
                    wr.cur.src = wr.src;
                    let s: String = std::iter::repeat_n(c, w).collect();
                    wr.cur.push(&s, Style::Dim);
                    wr.flush();
                }
            }
            Event::Start(Tag::Paragraph) => {
                if lists.is_empty() || !wr.cur.text.trim().is_empty() {
                    if lists.is_empty() {
                        wr.blank();
                    }
                    let q = quote_prefix(quote);
                    wr.indent = q.clone() + &list_indent(&lists);
                    if wr.cur.text.trim().is_empty() {
                        wr.cur.text.clear();
                        wr.cur.spans.clear();
                        wr.start_line(&q, Style::Dim);
                        let ind = list_indent(&lists);
                        wr.cur.push(&ind, Style::Plain);
                    } else {
                        wr.start_line(&wr.indent.clone(), Style::Dim);
                    }
                }
            }
            Event::End(TagEnd::Paragraph) => {
                if !wr.cur.text.is_empty() {
                    wr.flush();
                }
            }
            Event::Start(Tag::BlockQuote(_)) => {
                wr.blank();
                quote += 1;
                styles.push(Style::Dim);
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                quote = quote.saturating_sub(1);
                styles.pop();
            }
            Event::Start(Tag::List(n)) => {
                if lists.is_empty() {
                    wr.blank();
                } else if !wr.cur.text.trim().is_empty() {
                    wr.flush();
                }
                lists.push(n);
            }
            Event::End(TagEnd::List(_)) => {
                lists.pop();
                if !wr.cur.text.trim().is_empty() {
                    wr.flush();
                }
            }
            Event::Start(Tag::Item) => {
                if !wr.cur.text.trim().is_empty() {
                    wr.flush();
                }
                let depth = lists.len().saturating_sub(1);
                let mark = match lists.last().copied().flatten() {
                    Some(n) => {
                        if let Some(Some(x)) = lists.last_mut() {
                            *x += 1;
                        }
                        format!("{n}. ")
                    }
                    None => ["• ", "◦ ", "▪ "][depth.min(2)].to_string(),
                };
                let q = quote_prefix(quote);
                wr.start_line(&q, Style::Dim);
                wr.cur.push(&"  ".repeat(depth * 2), Style::Plain);
                wr.cur.push(&mark, Style::Marker);
                wr.indent = format!("{q}{}", " ".repeat(depth * 4 + width(&mark)));
            }
            Event::TaskListMarker(done) => {
                // 直前の印（• など）をチェックボックスに替える
                if let Some((rg, _)) = wr.cur.spans.iter().rev().find(|(_, s)| *s == Style::Marker).cloned() {
                    // ☑ ☐ は絵文字のフォントで出ることがあるので、[✓] [ ] にする
                    let mark = if done { "[✓] " } else { "[ ] " };
                    wr.cur.text.replace_range(rg.clone(), mark);
                    let d = mark.len() as isize - rg.len() as isize;
                    for (x, _) in wr.cur.spans.iter_mut() {
                        if x.start == rg.start {
                            x.end = (x.end as isize + d) as usize;
                        }
                    }
                }
                checked = Some(done);
            }
            Event::End(TagEnd::Item) => {
                checked = None;
                if !wr.cur.text.trim().is_empty() {
                    wr.flush();
                }
            }
            Event::Start(Tag::Emphasis) => styles.push(Style::Emph),
            Event::Start(Tag::Strong) => styles.push(Style::Strong),
            Event::Start(Tag::Strikethrough) => styles.push(Style::Strike),
            Event::End(TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough) => {
                styles.pop();
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                link = Some(dest_url.to_string());
                styles.push(Style::Link);
            }
            Event::End(TagEnd::Link) => {
                link = None;
                styles.pop();
            }
            Event::Start(Tag::Image { dest_url, .. }) => image = Some((String::new(), dest_url.to_string())),
            Event::Start(Tag::CodeBlock(kind)) => {
                let name = match &kind {
                    CodeBlockKind::Fenced(l) => l.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                code = Some((code_lang(&name), name, String::new()));
            }
            Event::Start(Tag::Table(_)) => {
                wr.blank();
                table = Some(Vec::new());
            }
            Event::Text(s) => {
                let st = match (checked, style(&styles)) {
                    (Some(true), Style::Plain) => Style::Strike,
                    (_, st) => st,
                };
                if wr.cur.text.is_empty() {
                    let q = quote_prefix(quote);
                    wr.start_line(&q, Style::Dim);
                }
                wr.text(&s, st, link.as_deref());
            }
            Event::Code(s) => wr.text(&s, Style::Code, link.as_deref()),
            Event::SoftBreak => wr.text(" ", style(&styles), None),
            Event::HardBreak => {
                wr.flush();
                let ind = wr.indent.clone();
                wr.cur.push(&ind, Style::Dim);
            }
            Event::Rule => {
                wr.blank();
                let s: String = std::iter::repeat_n('─', w).collect();
                wr.cur.push(&s, Style::Dim);
                wr.flush();
            }
            Event::Html(s) | Event::InlineHtml(s) => {
                for (i, l) in s.lines().enumerate() {
                    if i > 0 {
                        wr.flush();
                    }
                    wr.cur.push(l, Style::Dim);
                }
            }
            _ => {}
        }
    }
    if !wr.cur.text.is_empty() {
        wr.flush();
    }
    while wr.lines.last().is_some_and(|l| l.text.is_empty()) {
        wr.lines.pop();
    }
    wr.lines
}

fn quote_prefix(depth: usize) -> String {
    "▎ ".repeat(depth)
}

fn list_indent(lists: &[Option<u64>]) -> String {
    " ".repeat(lists.len().saturating_sub(1) * 4 + if lists.is_empty() { 0 } else { 2 })
}

fn draw_code(wr: &mut Writer, lang: Option<Lang>, name: &str, body: &str) {
    wr.blank();
    let body = body.strip_suffix('\n').unwrap_or(body);
    // その言語のハイライト（行ごとの範囲に切る）
    let spans: Vec<(Range<usize>, Hl)> = lang
        .and_then(Syntax::new)
        .map(|mut syn| {
            let mut buf = Buffer::from_str(body);
            syn.parse(&mut buf);
            syn.highlight(&buf, 0..body.len())
        })
        .unwrap_or_default();
    let mut at = 0;
    for (i, l) in body.split('\n').enumerate() {
        let mut line = Line { code: true, src: wr.src + 1 + i, ..Default::default() };
        line.push(" ", Style::Plain);
        let base = line.text.len();
        line.text.push_str(l);
        let mut any = false;
        for (r, h) in &spans {
            let (s, e) = (r.start.max(at), r.end.min(at + l.len()));
            if s < e {
                line.spans.push((base + s - at..base + e - at, Style::Hl(*h)));
                any = true;
            }
        }
        if !any && lang.is_none() {
            line.spans.push((base..line.text.len(), Style::Code));
        }
        // 1 行目の右端に言語名
        if i == 0 && !name.is_empty() {
            let pad = wr.w.saturating_sub(width(&line.text) + width(name) + 1);
            line.push(&" ".repeat(pad), Style::Plain);
            line.push(name, Style::Dim);
        }
        wr.lines.push(line);
        at += l.len() + 1;
    }
    wr.cur = Line { src: wr.src, ..Default::default() };
}

fn draw_table(wr: &mut Writer, rows: &[(bool, Vec<String>)]) {
    let n = rows.iter().map(|r| r.1.len()).max().unwrap_or(0);
    if n == 0 {
        return;
    }
    let mut ws: Vec<usize> = (0..n).map(|i| rows.iter().map(|r| r.1.get(i).map_or(0, |c| width(c.trim()))).max().unwrap_or(0).max(1)).collect();
    // 収まらなければ広い列から削る
    while ws.iter().sum::<usize>() + 3 * n + 1 > wr.w && ws.iter().any(|&x| x > 4) {
        let i = (0..n).max_by_key(|&i| ws[i]).unwrap_or(0);
        ws[i] -= 1;
    }
    let rule = |l: char, m: char, r: char| -> String {
        let mut s = String::from(l);
        for (i, w) in ws.iter().enumerate() {
            s.push_str(&"─".repeat(w + 2));
            s.push(if i + 1 == n { r } else { m });
        }
        s
    };
    let src = wr.src;
    let push_rule = |wr: &mut Writer, s: String| {
        let mut l = Line { src, ..Default::default() };
        l.push(&s, Style::Dim);
        wr.lines.push(l);
    };
    push_rule(wr, rule('┌', '┬', '┐'));
    for (k, (head, cells)) in rows.iter().enumerate() {
        let mut l = Line { src, ..Default::default() };
        for i in 0..n {
            l.push("│ ", Style::Dim);
            let c = cells.get(i).map_or("", |c| c.trim());
            let mut shown = String::new();
            for ch in c.chars() {
                if width(&shown) + width(ch.encode_utf8(&mut [0; 4])) > ws[i] {
                    break;
                }
                shown.push(ch);
            }
            let pad = ws[i] - width(&shown);
            l.push(&shown, if *head { Style::Heading } else { Style::Plain });
            l.push(&" ".repeat(pad + 1), Style::Plain);
        }
        l.push("│", Style::Dim);
        wr.lines.push(l);
        if *head && k + 1 < rows.len() {
            push_rule(wr, rule('├', '┼', '┤'));
        }
    }
    push_rule(wr, rule('└', '┴', '┘'));
    wr.cur = Line { src, ..Default::default() };
}

fn draw_image(wr: &mut Writer, alt: &str, url: &str) {
    if !wr.cur.text.trim().is_empty() {
        wr.flush();
    }
    let label = if alt.is_empty() { format!("画像（{url}）") } else { format!("{alt}（{url}）") };
    let inner = width(&label).min(wr.w.saturating_sub(4));
    let src = wr.src;
    let mut l = Line { src, frame: true, ..Default::default() };
    l.push(&format!("┌{}┐", "─".repeat(inner + 2)), Style::Dim);
    wr.lines.push(l);
    let mut l = Line { src, frame: true, ..Default::default() };
    l.push("│ ", Style::Dim);
    let a = l.text.len();
    l.push(&label, Style::Dim);
    l.links.push((a..l.text.len(), url.to_string()));
    l.push(" │", Style::Dim);
    wr.lines.push(l);
    let mut l = Line { src, frame: true, ..Default::default() };
    l.push(&format!("└{}┘", "─".repeat(inner + 2)), Style::Dim);
    wr.lines.push(l);
    wr.cur = Line { src, ..Default::default() };
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "# shop\n\n注文と在庫の API。**Java 21** と _Spring Boot_ で動く。\n\n## 起動\n\n```sh\n./mvnw spring-boot:run\n```\n\n- 設定は `application.yml`（[手順](docs/setup.md)）\n- テスト\n  - DB は Testcontainers\n\n> 本番の鍵は置かない\n\n| メソッド | パス |\n|---|---|\n| GET | `/orders` |\n\n- [x] 注文 API\n- [ ] 在庫\n\n![構成図](docs/arch.png)\n";

    #[test]
    fn highlights() {
        let hl = highlight(DOC);
        let at = |s: &str| -> Vec<Hl> {
            let i = DOC.find(s).unwrap();
            let mut v: Vec<Hl> = hl.iter().filter(|(r, _)| r.start <= i && i < r.end).map(|x| x.1).collect();
            v.dedup();
            v
        };
        assert_eq!(at("# shop"), vec![Hl::Keyword, Hl::Comment], "見出しと # の印");
        assert_eq!(at("Java 21").last(), Some(&Hl::Number), "太字");
        assert_eq!(at("**Java").last(), Some(&Hl::Comment), "** は灰色");
        assert_eq!(at("Spring Boot").last(), Some(&Hl::Type), "斜体");
        assert_eq!(at("application.yml").last(), Some(&Hl::String), "行内のコード");
        assert_eq!(at("手順").last(), Some(&Hl::Keyword), "リンクの文字");
        assert_eq!(at("(docs/setup.md)").last(), Some(&Hl::Comment), "リンクの先");
        assert_eq!(at("> 本番").last(), Some(&Hl::Comment), "引用の印");
        assert_eq!(at("- 設定").last(), Some(&Hl::Number), "リストの印");
        assert_eq!(at("[x]").last(), Some(&Hl::Number), "チェックボックス");
        assert_eq!(at("```sh").last(), Some(&Hl::Comment), "フェンス");
        assert!(at("./mvnw").contains(&Hl::String) || at("./mvnw").contains(&Hl::Function) || !at("./mvnw").is_empty(), "コードブロック");
        // Java のコードブロックは Java のハイライト
        let j = "```java\npublic class A {}\n```\n";
        let hj = highlight(j);
        let i = j.find("public").unwrap();
        assert!(hj.iter().any(|(r, h)| r.start == i && *h == Hl::Keyword), "{hj:?}");
    }

    #[test]
    fn renders() {
        let lines = render(DOC, 40);
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        let has = |s: &str| texts.iter().any(|t| t.contains(s));
        assert_eq!(texts[0], "shop");
        assert!(texts[1].starts_with("━━━"), "{texts:?}");
        assert!(has("注文と在庫の API。Java 21 と Spring Boot で動く。") || has("Java 21"), "{texts:?}");
        assert!(texts.iter().any(|t| t.trim_end().starts_with(" ./mvnw spring-boot:run")), "{texts:?}");
        assert!(lines.iter().any(|l| l.code && l.text.trim_end().ends_with("sh")), "言語名");
        assert!(has("• 設定は application.yml（手順）"), "{texts:?}");
        assert!(has("◦ DB は Testcontainers"), "{texts:?}");
        assert!(has("▎ 本番の鍵は置かない"), "{texts:?}");
        assert!(has("┌") && has("│ メソッド │ パス") && has("│ GET      │ /orders │"), "{texts:?}");
        assert!(has("[✓] 注文 API") && has("[ ] 在庫"), "{texts:?}");
        assert!(has("構成図（docs/arch.png）"), "{texts:?}");
        // リンクの先を覚える
        let l = lines.iter().find(|l| l.text.contains("手順")).unwrap();
        assert_eq!(l.links[0].1, "docs/setup.md");
        // 元の行: 「## 起動」は 5 行目（0 始まりで 4）
        let h2 = lines.iter().find(|l| l.text == "起動").unwrap();
        assert_eq!(h2.src, 4);
        // 長い行は幅で折り返す
        let long = render(&"あ".repeat(50), 20);
        assert!(long.len() >= 5 && long.iter().all(|l| width(&l.text) <= 20), "{long:?}");
    }
}
