//! HTML（Angular のテンプレート）。言語はファイルの範囲で決まる: テンプレートの式は、部品（.component.ts）のクラスの中の
//! TypeScript として、プロジェクトの索引（TypeScript の規則）で読む。定義へ（gd・F12・Ctrl+クリック）:
//!
//! - 式の中の名前（{{ title }}、(click)="onGo()"、*ngIf="isShown"、@if (user) …）→ このテンプレートの部品（.component.ts）
//!   のメソッド・プロパティ。部品に無ければ親クラスへたどる（基底の部品クラスのメソッドなど）
//! - 部品のタグ（<app-button>）→ selector が同じ部品のクラス
//! - 部品の属性（[width]、(clicked)）→ その部品の @Input・@Output のプロパティ
//!
//! `x.y` の `y` のように受け手のあるもの、テンプレートの中の変数（let item of items、#ref）は扱わず、いつもの gd に任せる。
//!
//! 補完: `{{ }}`・`[x]="…"`・`(x)="…"`・`*ngIf="…"`・`@if (…)`・`@for (…)` の中は、部品のクラスの中の TypeScript の式として
//! （頭の名前は `this.` を付けて）TypeScript の型の求め方で。`let item of items`・`@for (item of items` の変数は `items.find(…)` に
//! 置き換える。`<` の後は、プロジェクトの部品の selector

use std::path::{Path, PathBuf};

use nagi_syntax::HtmlCtx;
use nagi_syntax::defs::Receiver;
use nagi_workers::index::{Index, Loc};

use crate::lang::{Cx, Found, Item, Language, Target};
use crate::project::{Ranked, Ranker, supertypes};
use crate::text::{C_LIKE, Lexis};

/// テンプレートの部品の .ts（同じ名前の .ts、無ければ同じフォルダで templateUrl がこのファイルを指すもの）
pub fn component_of(html: &Path) -> Option<PathBuf> {
    let ts = html.with_extension("ts");
    if ts.is_file() {
        return Some(ts);
    }
    let name = html.file_name()?.to_string_lossy().into_owned();
    std::fs::read_dir(html.parent()?).ok()?.flatten().map(|e| e.path()).find(|p| {
        p.extension().is_some_and(|e| e == "ts") && std::fs::read_to_string(p).is_ok_and(|s| s.contains("templateUrl") && s.contains(&name))
    })
}

/// selector が `tag` の部品の .ts（プロジェクトの .component.ts から探す）
pub fn component_by_selector(root: &Path, tag: &str) -> Option<PathBuf> {
    let pats = [format!("selector: '{tag}'"), format!("selector: \"{tag}\""), format!("selector:'{tag}'"), format!("selector: `{tag}`")];
    for e in nagi_workers::files::walker(root).build().flatten() {
        let p = e.path();
        if !p.to_string_lossy().ends_with(".ts") || p.to_string_lossy().ends_with(".spec.ts") {
            continue;
        }
        if let Ok(s) = std::fs::read_to_string(p) {
            if s.contains("@Component") && pats.iter().any(|x| s.contains(x.as_str())) {
                return Some(p.to_path_buf());
            }
        }
    }
    None
}

/// HTML（Angular のテンプレートなら、定義は部品の .ts から）
pub struct Template;

pub static TEMPLATE: Template = Template;

impl Language for Template {
    fn name(&self) -> &'static str {
        "html"
    }

    fn lexis(&self) -> &'static Lexis {
        &C_LIKE
    }

    /// テンプレートの式の中（部品のクラスのメンバー・テンプレートの変数）と、`<` の後（部品の selector）。ほかは None（いつもの手順へ）
    fn complete_all(&self, cx: &Cx) -> Option<Vec<Item>> {
        let html = cx.here?;
        let idx = cx.index?;
        let before = &cx.src[..cx.start];
        if before.ends_with('<') {
            return Some(selectors(idx).into_iter().map(|(sel, file)| Item::new(sel, 'c', file, 0)).collect());
        }
        let start = expression_start(before)?;
        let comp = component_of(html)?;
        let comp_src = std::fs::read_to_string(&comp).ok()?;
        let at = class_body_end(&comp_src)?;
        let ix = crate::eco::dts(cx.libs)?;
        let vars = template_vars(before);
        let types = |recv: &str| crate::ts::types::complete(&ix, &idx.root, &comp, &comp_src, recv, at);
        let item = |m: crate::ts::types::TsMember| Item::member(m.name, m.method, m.params, m.owner, 0);
        if cx.after_dot() {
            // 受け手の式（テンプレートの変数は置き換え、ほかは this. を付ける）
            let recv = &cx.src[start..cx.start - 1];
            let head_at = crate::text::chain_start(crate::ts::types::LX, recv.trim_end_matches(['?', '!']))?;
            let chain = &recv[head_at..];
            let head: String = chain.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$').collect();
            let rest = &chain[head.len()..];
            let expr = match vars.iter().rev().find(|(n, _)| *n == head) {
                Some((_, of)) => format!("this.{}.find(x => true){rest}", of.split('|').next().unwrap_or(of).trim()),
                None if head == "this" => chain.to_string(),
                None => format!("this.{chain}"),
            };
            return Some(types(&expr).into_iter().map(item).collect());
        }
        // 頭の名前: テンプレートの変数と部品のクラスのメンバー
        let mut out: Vec<Item> = vars.iter().rev().map(|(n, of)| Item::new(n.clone(), 'v', of.trim().to_string(), 0)).collect();
        for m in types("this") {
            if !out.iter().any(|x| x.name == m.name) {
                out.push(item(m));
            }
        }
        Some(out)
    }

    /// 式の中の名前・部品のタグ・部品の属性。部品が無い・テンプレートの中の変数かもしれないものは None（いつもの手順へ）
    fn definition(&self, cx: &Cx, name: &str) -> Option<Found> {
        let html = cx.here?;
        let idx = cx.index?;
        let ctx = html_ctx(cx.src, cx.start)?;
        let root = &idx.root;
        let rel = |p: &Path| p.strip_prefix(root).map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_default();
        // (部品の .ts, 探す名前)。タグはクラスそのもの
        let (ts, member) = match &ctx {
            HtmlCtx::Tag(t) if t.contains('-') => match component_by_selector(root, t) {
                Some(ts) => (ts, None),
                None => return Some(Found::Warn(format!("部品が見つかりません: <{t}>（selector: '{t}' の @Component）"))),
            },
            HtmlCtx::Attr { tag, name } if tag.contains('-') && !name.is_empty() => (component_by_selector(root, tag)?, Some(name.clone())),
            HtmlCtx::Other => {
                // 受け手のある名前（user.name の name）はいつもの手順に任せる
                let before = &cx.src[cx.start.saturating_sub(2)..cx.start];
                if before.ends_with('.') || before.ends_with("?.") {
                    return None;
                }
                (component_of(html)?, Some(name.to_string()))
            }
            _ => return None,
        };
        let ts_rel = rel(&ts);
        let src = std::fs::read_to_string(&ts).unwrap_or_default();
        let found = match &member {
            // 部品のクラス（@Component の後の最初のクラス。無ければファイルの中の最初の型）
            None => {
                let defs: Vec<Loc> = idx.all().filter(|(_, l)| &*l.path == ts_rel && l.kind.is_type()).map(|(_, l)| l.clone()).collect();
                let comp = src.lines().position(|l| l.trim_start().starts_with("@Component")).map(|k| k as u32);
                let class = comp.and_then(|c| defs.iter().filter(|l| l.line >= c && l.kind == nagi_syntax::defs::DefKind::Class).min_by_key(|l| l.line).cloned());
                class.or_else(|| defs.into_iter().min_by_key(|l| l.line))
            }
            // 部品のメンバー（無ければ親クラスへ）
            Some(m) => match Ranker::new(Some(idx)).resolve(&ts_rel, &src, m, Some(&Receiver::This), None, idx.lookup(m).to_vec()) {
                Ranked::Locs(v, best) if best >= 1 => v.into_iter().next().filter(|l| *l.path == *ts_rel || !idx.lookup(m).is_empty()),
                _ => None,
            },
        };
        match found {
            Some(l) if member.is_none() || *l.path == *ts_rel || in_hierarchy(idx, &ts_rel, &src, &l) => {
                Some(Found::Jump(Target { path: root.join(&*l.path), line: l.line as usize, col: l.col as usize, note: None }))
            }
            _ => match &member {
                // テンプレートの中の変数（let x、#ref）かもしれないので、いつもの手順に任せる
                Some(_) if matches!(ctx, HtmlCtx::Other) => None,
                Some(m) => Some(Found::Info(format!("{} に {m} がありません", ts.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned())))),
                None => Some(Found::Stop),
            },
        }
    }
}

/// `at` の場所の種類（テンプレートを構文解析して）
fn html_ctx(src: &str, at: usize) -> Option<HtmlCtx> {
    let mut buf = nagi_core::Buffer::from_str(src);
    let mut syn = nagi_syntax::Syntax::new(nagi_syntax::Lang::Html)?;
    syn.parse(&mut buf);
    syn.html_context(&buf, at)
}

/// `l` が部品のクラスか、その親クラスのファイルにあるか（同じ名前の別のファイルへ跳ばない）
fn in_hierarchy(idx: &Index, ts_rel: &str, src: &str, l: &Loc) -> bool {
    let mut types: Vec<String> = supertypes(src, None);
    let mut seen = std::collections::HashSet::new();
    while let Some(t) = types.pop() {
        if !seen.insert(t.clone()) || seen.len() > 16 {
            continue;
        }
        for d in idx.lookup(&t).iter().filter(|d| d.kind.is_type()) {
            if d.path == l.path {
                return true;
            }
            if let Ok(s) = std::fs::read_to_string(idx.root.join(&*d.path)) {
                types.extend(supertypes(&s, Some(&t)));
            }
        }
    }
    *l.path == *ts_rel
}

/// カーソルの前 `before` が Angular の式の中なら、その式の始まり（`{{ }}`・バインドの属性の値・@if などの括弧の中）
fn expression_start(before: &str) -> Option<usize> {
    let mut best: Option<usize> = None;
    let mut take = |k: usize| best = Some(best.map_or(k, |b: usize| b.max(k)));
    // {{ … （閉じていない）
    if let Some(i) = before.rfind("{{").filter(|&i| !before[i..].contains("}}")) {
        take(i + 2);
    }
    // [x]="… ・(x)="… ・*ngIf="… ・bind-x="…（引用符が閉じていない）
    for q in ['"', '\''] {
        if let Some(i) = before.rfind(&format!("={q}")).filter(|&i| !before[i + 2..].contains(q)) {
            let name: String = before[..i].trim_end().chars().rev().take_while(|c| !c.is_whitespace() && *c != '<').collect::<Vec<_>>().into_iter().rev().collect();
            if name.starts_with(['[', '(', '*']) || name.starts_with("bind-") || name.starts_with("on-") {
                take(i + 2);
            }
        }
    }
    // @if ( … ・@for ( … ・@switch ( … ・@case ( … （括弧が閉じていない）
    static CTRL: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = CTRL.get_or_init(|| regex::Regex::new(r"@(?:if|for|switch|case|else if|defer)\s*\(").unwrap());
    if let Some(m) = re.find_iter(before).last() {
        let rest = &before[m.end()..];
        if rest.matches('(').count() >= rest.matches(')').count() {
            take(m.end());
        }
    }
    best
}

/// テンプレートの変数（`let item of items`・`@for (item of items`）の (名前, 並びの式)
fn template_vars(before: &str) -> Vec<(String, String)> {
    static VARS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = VARS.get_or_init(|| regex::Regex::new(r#"(?:\blet\s+|@for\s*\(\s*)(\w+)\s+of\s+([^;"')]+)"#).unwrap());
    re.captures_iter(before).map(|c| (c[1].to_string(), c[2].to_string())).collect()
}

/// 部品のクラス（@Component の後の最初の class、無ければ最初の class）の本体の終わりの } の位置（そこはクラスの中）
fn class_body_end(src: &str) -> Option<usize> {
    let from = src.find("@Component").unwrap_or(0);
    let k = from + src[from..].find("class ")?;
    let open = k + src[k..].find('{')?;
    let b = src.as_bytes();
    let (mut depth, mut i) = (0i32, open);
    while i < b.len() {
        match b[i] {
            b'\'' | b'"' | b'`' => {
                let q = b[i];
                i += 1;
                while i < b.len() && b[i] != q {
                    i += 1 + usize::from(b[i] == b'\\');
                }
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// プロジェクトの部品の (selector, ファイル)（索引から作って覚える。ファイルが変われば作り直す）
fn selectors(idx: &Index) -> Vec<(String, String)> {
    let table = idx.derived("angular.selectors", |i| {
        static SEL: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let re = SEL.get_or_init(|| regex::Regex::new(r#"selector:\s*['"`]([A-Za-z][\w-]*)['"`]"#).unwrap());
        let mut v: Vec<String> = Vec::new();
        for f in i.files().filter(|f| f.ends_with(".ts") && !f.ends_with(".spec.ts")) {
            let Ok(text) = std::fs::read_to_string(i.root.join(f)) else { continue };
            if !text.contains("@Component") {
                continue;
            }
            v.extend(re.captures_iter(&text).map(|c| format!("{}\t{f}", &c[1])));
        }
        v.sort();
        v
    });
    table.iter().filter_map(|x| x.split_once('\t').map(|(a, b)| (a.to_string(), b.to_string()))).collect()
}
