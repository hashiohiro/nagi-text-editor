//! コードナビゲーション（LSP なし。architecture.md §10）。
//!
//! - `gd` / `Ctrl-]`: ファイルの中の束縛（引数・ローカル変数・定義）を探し、無ければプロジェクトの索引から。
//!   候補が 1 つならそこへ跳び、複数ならファインダーで選ぶ（同じファイル、近いフォルダの順）
//! - `gr`: 名前が単語として一致する箇所を全文検索のパネルに出す（参照の検索。名前の一致に基づく）
//! - シンボル検索（GotoSymbol / GotoClass）、ファイルの構造（FileStructurePopup）、`[m` `]m`
//! - 索引は起動後に裏で作り、保存したファイルは 1 つずつ索引し直す

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use nagi_syntax::defs::DefKind;
use nagi_workers::index::{Entry, Index, Progress};

use crate::editor::{Editor, State};
use crate::finder::Source;

/// 索引の状態（Editor が持つ）
#[derive(Default)]
pub struct IndexState {
    pub index: Option<Index>,
    building: Option<nagi_core::task::Task<Progress>>,
    /// 作っている途中 (解析したファイル, 解析が要るファイル)
    pub progress: Option<(usize, usize)>,
    /// ライブラリ（依存の jar・JDK）の型の索引（Java。jvm.rs）。補完と import に使う
    pub jars: Option<std::sync::Arc<nagi_workers::jvm::JarIndex>>,
    jars_task: Option<nagi_core::task::Task<Result<nagi_workers::jvm::JarIndex, String>>>,
    /// ライブラリのソース・骨組みを書く場所（試験で変える。None なら %LOCALAPPDATA%\nagi\index\sources）
    pub sources_dir: Option<PathBuf>,
    /// 保存したファイルの索引し直し
    reindex: Option<(Sender<Option<(String, (u64, u64), Vec<Entry>)>>, nagi_core::task::Task<Option<(String, (u64, u64), Vec<Entry>)>>)>,
}

/// 識別子に使える文字か
fn ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

impl Editor {
    fn project_root(&self) -> PathBuf {
        let root = self
            .buf
            .path
            .as_deref()
            .and_then(|p| std::path::absolute(p).ok())
            .and_then(|p| nagi_workers::files::project_root(&p))
            .or_else(|| Some(self.cwd.clone()))
            .unwrap_or_default();
        std::path::absolute(&root).unwrap_or(root)
    }

    /// 索引を作り始める（App が表示の後に呼ぶ）
    pub fn start_index(&mut self) {
        // 設定の「定義の索引を作る」がオフなら作らない
        if !self.settings.bool("project.index") {
            self.nav.index = None;
            return;
        }
        let root = self.project_root();
        let waker = self.waker();
        self.nav.building = Some(nagi_workers::index::build(&root, waker.clone()));
        self.nav.progress = Some((0, 0));
        self.start_jars(&root);
    }

    /// Maven のプロジェクトなら、依存の jar と JDK の型の索引を用意する（pom.xml から依存を求め、jar ごとのかけらは
    /// 変わったものだけ作り直す。ほかは写すだけ）。pom.xml を保存したときも
    fn start_jars(&mut self, root: &Path) {
        if cfg!(test) {
            return;
        }
        if let Some(m) = nagi_workers::maven::find_root(root).filter(|m| m.join("pom.xml").is_file()) {
            self.nav.jars_task = Some(nagi_workers::jvm::load_maven(m, self.waker()));
        }
    }

    /// 索引の進み具合と、保存したファイルの索引し直しを受け取る。変わったら true
    pub fn poll_index(&mut self) -> bool {
        let mut changed = false;
        if let Some(rx) = &self.nav.building {
            while let Ok(p) = rx.try_recv() {
                match p {
                    Progress::Step(a, b) => self.nav.progress = Some((a, b)),
                    Progress::Done(i) => {
                        self.nav.index = Some(*i);
                        self.nav.progress = None;
                        self.nav.building = None;
                        changed = true;
                        break;
                    }
                }
            }
        }
        if let Some(Ok(r)) = self.nav.jars_task.as_ref().map(|t| t.try_recv()) {
            self.nav.jars_task = None;
            match r {
                Ok(ix) => {
                    nagi_core::log::info(&format!("ライブラリの型の索引: {} クラス（作り直したかけら {}）", ix.classes(), ix.built));
                    self.nav.jars = Some(std::sync::Arc::new(ix));
                }
                Err(e) => nagi_core::log::warn(&format!("ライブラリの型の索引を作れません: {e}")),
            }
        }
        let mut updated = false;
        if let (Some((_, rx)), Some(idx)) = (&self.nav.reindex, &mut self.nav.index) {
            for (rel, st, defs) in rx.drain().into_iter().flatten() {
                idx.replace_file(&rel, st, defs);
                updated = true;
            }
        }
        if updated {
            // 保存は裏で（UI スレッドでファイルを書かない）。中身は今の索引の写し
            if let Some(idx) = &self.nav.index {
                let snapshot = idx.snapshot();
                nagi_core::task::detach("index-save", move || snapshot.save());
            }
        }
        changed
    }

    /// 保存したファイルを索引し直す（裏で）
    pub(crate) fn reindex_saved(&mut self) {
        if self.buf.path.as_deref().and_then(Path::file_name).is_some_and(|n| n.eq_ignore_ascii_case("pom.xml")) && self.settings.bool("project.index") {
            let root = self.project_root();
            self.start_jars(&root);
        }
        let (Some(path), Some(idx)) = (self.buf.path.clone(), &self.nav.index) else { return };
        let Ok(abs) = std::path::absolute(&path) else { return };
        let Ok(rel) = abs.strip_prefix(&idx.root) else { return };
        if nagi_syntax::Lang::detect(&abs).is_none() {
            return;
        }
        let rel = rel.to_string_lossy().replace('\\', "/");
        let tx = self.nav.reindex.get_or_insert_with(nagi_core::task::manual).0.clone();
        nagi_core::task::spawn_to("reindex", tx, self.waker(), move || {
            let mut p = nagi_syntax::defs::DefParser::default();
            nagi_workers::index::index_file(&abs, &mut p).map(|(st, defs)| (rel, st, defs))
        });
    }

    /// カーソルの下の識別子（と、その始まりの絶対 byte）
    fn word_at_cursor(&self) -> Option<(String, usize)> {
        let line = self.buf.line(self.cursor.line);
        let b = self.cursor.byte.min(line.len());
        let start = line[..b].char_indices().rev().take_while(|(_, c)| ident_char(*c)).last().map_or(b, |(i, _)| i);
        let end = line[b..].char_indices().find(|(_, c)| !ident_char(*c)).map_or(line.len(), |(i, _)| b + i);
        (end > start).then(|| (line[start..end].to_string(), self.buf.line_start(self.cursor.line) + start))
    }

    /// (パス, 行, 列) へ跳ぶ（ジャンプリストに残す）
    pub(crate) fn jump_to(&mut self, path: &Path, line: usize, col: usize) {
        // 索引の相対パスは / 区切りなので、Windows の区切りにそろえる（題名や同じファイルの判定がずれないように）
        #[cfg(windows)]
        let fixed = PathBuf::from(path.to_string_lossy().replace('/', "\\"));
        #[cfg(windows)]
        let path = fixed.as_path();
        let same = self.buf.path.as_deref().is_some_and(|p| std::path::absolute(p).ok().as_deref() == Some(path));
        if same {
            self.push_jump();
        } else {
            self.edit_file(path.to_path_buf());
        }
        let line = line.min(self.buf.line_count().saturating_sub(1));
        self.cursor.line = line;
        self.cursor.byte = col.min(self.buf.line_len(line));
        let text = self.buf.line(line);
        self.cursor.want_col = nagi_core::display_col(&text, self.cursor.byte, false);
        self.clamp_normal();
        // 跳んだ先は画面の中ほどに
        self.top = line.saturating_sub(self.rows / 3);
    }

    /// `gd`: 定義へ
    pub(crate) fn goto_definition(&mut self) {
        let Some((name, word_at)) = self.word_at_cursor() else {
            self.notify.error("E348: カーソルの下に識別子がありません");
            return;
        };
        // Angular のテンプレート: 式の名前は部品の .ts（と親クラス）、タグは部品、属性は部品の @Input / @Output
        if self.buf.path.as_deref().is_some_and(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("html"))) {
            self.ensure_syntax();
            if self.angular_goto(&name, word_at) {
                return;
            }
        }
        // メソッド・型の定義そのものの上なら、それを実装・継承しているもの（インタフェースの宣言から実装へ）
        if self.on_definition(&name, word_at) {
            if let Ok((locs, root)) = self.find_implementations() {
                self.show_implementations(locs, root);
                return;
            }
        }
        // ファイルの中の束縛
        self.ensure_syntax();
        if let Some(at) = self.syntax.as_ref().and_then(|s| s.local_definition(&self.buf, word_at)) {
            if at != word_at {
                self.push_jump();
                self.set_abs(at);
                self.clamp_normal();
                return;
            }
        }
        // プロジェクトの索引（まだなら、このファイルの定義から）
        let here = self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok());
        let cur = (self.cursor.line, word_at - self.buf.line_start(self.cursor.line));
        // 有力な順に並べる（受け手の型とその親 → import → 同じモジュール）。best はいちばん有力なものの数
        let mut best = 0;
        // 呼び出しなら引数の数（オーバーロードを選ぶ・フィールドを候補から外す）
        let args = self.syntax.as_ref().and_then(|s| s.call_args(&self.buf, word_at));
        let mut cands: Vec<(PathBuf, usize, usize, DefKind)> = match &self.nav.index {
            Some(idx) => {
                let rel = here.as_ref().and_then(|h| h.strip_prefix(&idx.root).ok()).map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                let src = self.buf.text();
                let recv = self.syntax.as_ref().and_then(|s| s.receiver(&self.buf, word_at));
                let mut all = idx.lookup(&name).to_vec();
                // 今いる定義そのものは候補から外す（ほかに候補があれば）
                if all.len() > 1 {
                    all.retain(|l| !(l.path.as_ref() == rel && (l.line as usize, l.col as usize) == cur));
                }
                match nagi_workers::index::Ranker::new(Some(idx)).resolve(&rel, &src, &name, recv.as_ref(), args.as_deref(), all) {
                    nagi_workers::index::Ranked::Locs(locs, b) => {
                        best = b;
                        locs.iter().map(|l| (idx.root.join(&*l.path), l.line as usize, l.col as usize, l.kind)).collect()
                    }
                    nagi_workers::index::Ranked::Outside(t) => {
                        // 受け手の型（またはその親）がライブラリ。同じ名前のプロジェクトのメソッドへは跳ばない
                        if !self.goto_library(&name, word_at) {
                            self.notify.info(format!("定義はプロジェクトの外です: {name}（{t} から先はライブラリ）"));
                        }
                        return;
                    }
                }
            }
            None => Vec::new(),
        };
        if cands.is_empty() {
            if let (Some(syn), Some(p)) = (&self.syntax, &here) {
                cands = syn.defs(&self.buf).into_iter().filter(|d| d.name == name).map(|d| (p.clone(), d.line, d.col, d.kind)).collect();
                if cands.len() > 1 {
                    cands.retain(|(p, l, c, _)| !(Some(p) == here.as_ref() && (*l, *c) == cur));
                }
                best = cands.len();
            }
        }
        if best == 1 && cands.len() > 1 {
            cands.truncate(1);
        }
        // 呼び出しの候補に、有力でない同じ名前のフィールド・定数は出さない
        if args.is_some() && cands.len() > best {
            let mut i = 0;
            cands.retain(|(_, _, _, k)| {
                i += 1;
                i <= best || matches!(k, DefKind::Function | DefKind::Method)
            });
        }
        // プロジェクトに無ければライブラリ（Java の jar・JDK）
        if cands.is_empty() && self.goto_library(&name, word_at) {
            return;
        }
        match cands.len() {
            0 => {
                self.notify.warn(match &self.nav.progress {
                    Some((a, b)) => format!("定義が見つかりません: {name}（索引を作っています {a}/{b}）"),
                    None => format!("定義が見つかりません: {name}"),
                });
            }
            1 => {
                let (p, l, c, _) = cands.remove(0);
                self.jump_to(&p, l, c);
            }
            n => {
                // 有力な順（上の best 件が同じくらい有力）。有力なものには ● を付ける
                let root = self.nav.index.as_ref().map(|i| i.root.clone()).unwrap_or_default();
                let mark = best < n;
                let items = cands
                    .iter()
                    .enumerate()
                    .map(|(i, (p, l, _, k))| {
                        let m = if !mark { "" } else if i < best { "● " } else { "  " };
                        format!("{m}{name}  {}  {}:{}", k.label(), rel(p, &root), l + 1)
                    })
                    .collect();
                let locs = cands.into_iter().map(|(p, l, c, _)| (p, l, c)).collect();
                self.open_locations(Source::Definitions, items, locs, false);
                if mark {
                    self.notify.info(format!("定義の候補 {n} 件（● の {best} 件が有力）"));
                }
            }
        }
    }

    /// Java: ライブラリ（依存の jar・JDK）の型・メンバーの定義へ（ソースか、無ければ class ファイルから作った骨組みを開く）。跳べたら true
    fn goto_library(&mut self, name: &str, word_at: usize) -> bool {
        let Some(jars) = self.nav.jars.clone().filter(|_| self.is_java()) else { return false };
        let Some((owner, member)) = self.library_target(name, word_at) else { return false };
        let Some(dir) = self.nav.sources_dir.clone().or_else(crate::java_source::sources_dir) else { return false };
        let Some(o) = crate::java_source::open(&jars, &owner, member.as_deref(), &dir) else { return false };
        self.jump_to(&o.path, o.line, o.col);
        let fqn = owner.replace(['/', '$'], ".");
        self.notify.info(if o.source { format!("ライブラリ: {fqn}（ソース。読むだけ）") } else { format!("ライブラリ: {fqn}（ソースが無いので class ファイルの宣言から作った骨組み）") });
        true
    }

    /// Java: `word_at` の名前 `name` のライブラリの (型の内部名, メンバー)
    pub(crate) fn library_target(&mut self, name: &str, word_at: usize) -> Option<(String, Option<String>)> {
        use crate::java_members::{JType, LibTypes};
        let jars = self.nav.jars.clone()?;
        let li = self.buf.pos_of(word_at).0;
        let line = self.buf.line(li).to_string();
        let ls = self.buf.line_start(li);
        let ws = word_at - ls;
        let src = self.buf.text();
        let idx = self.nav.index.as_ref();
        let here = self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok());
        let rel = match (idx, &here) {
            (Some(i), Some(h)) => h.strip_prefix(&i.root).ok().map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_default(),
            _ => String::new(),
        };
        let mut ranker = nagi_workers::index::Ranker::new(idx);
        let inside = nagi_workers::index::enclosing_type(&src, word_at);
        let target = if ws > 0 && line[..ws].ends_with('.') {
            let known = |pkg: &str| idx.is_some_and(|i| i.is_java_package(pkg)) || jars.is_package(pkg);
            match crate::java_members::receiver_text(&src, &line[..ws - 1], ls, word_at, &known) {
                // パッケージの中のクラス
                Err(pkg) => Some(format!("{pkg}/{name}")).filter(|c| jars.class(c).is_some()).map(|c| (c, None)),
                Ok((expanded, fqn)) => {
                    let (lib, t) = crate::java_members::lib_receiver(&jars, &mut ranker, &src, &rel, word_at, inside.as_deref(), &expanded, fqn.as_ref());
                    match t {
                        Some(t @ JType::Class(..)) if !matches!(&t, JType::Class(n, _) if n.starts_with('?')) => {
                            let JType::Class(n, _) = &t else { unreachable!() };
                            let nested = format!("{n}${name}");
                            if jars.class(&nested).is_some() {
                                Some((nested, None))
                            } else {
                                lib.declaring(&t, name).map(|c| (c, Some(name.to_string())))
                            }
                        }
                        // プロジェクトの型のライブラリの親のメンバー
                        Some(JType::Class(n, _)) => {
                            let simple = crate::java_members::project_name(&n).map_or("", |x| x.0).to_string();
                            self.lib_super_member(&mut ranker, &simple, name)
                        }
                        // this.・super. は今の型のライブラリの親
                        None if crate::java_members::chain_start(&expanded).is_some_and(|i| matches!(&expanded[i..], "this" | "super")) => inside.as_deref().and_then(|t| self.lib_super_member(&mut ranker, t, name)),
                        _ => None,
                    }
                }
            }
        } else {
            let lib = LibTypes::new(&jars, &src);
            if name.starts_with(char::is_uppercase) && let Some(c) = lib.resolve(name) {
                Some((c, None))
            } else if let Some(c) = lib.static_owner(name) {
                Some((c, Some(name.to_string())))
            } else {
                // 今の型のライブラリの親のメソッド・フィールド
                inside.as_deref().and_then(|t| self.lib_super_member(&mut ranker, t, name))
            }
        };
        target
    }

    /// プロジェクトの型 `ty` のライブラリの親（型引数つき）で、メンバー `name` を宣言している型
    fn lib_super_member(&self, ranker: &mut nagi_workers::index::Ranker, ty: &str, name: &str) -> Option<(String, Option<String>)> {
        let jars = self.nav.jars.as_ref()?;
        ranker.outside_supers(ty).into_iter().find_map(|(sup, lines)| {
            let l = crate::java_members::LibTypes::from_lines(jars, lines.iter().map(String::as_str));
            let t = l.parse(&sup).filter(|x| matches!(x, crate::java_members::JType::Class(n, _) if !n.starts_with('?')))?;
            l.declaring(&t, name).map(|c| (c, Some(name.to_string())))
        })
    }

    /// `gI`: 実装へ。型の上なら継承・実装している型、メソッドの上なら受け手の型（宣言の上なら囲む型）の子の中のそのメソッド
    pub(crate) fn goto_implementation(&mut self) {
        match self.find_implementations() {
            Ok((locs, root)) => self.show_implementations(locs, root),
            Err(m) => self.notify.error(m),
        }
    }

    /// 実装の一覧（名前, 場所）と索引の根。見つからなければ伝える文
    fn find_implementations(&mut self) -> nagi_core::Result<(Vec<(String, nagi_workers::index::Loc)>, PathBuf)> {
        let Some((name, word_at)) = self.word_at_cursor() else {
            return Err("E348: カーソルの下に識別子がありません".into());
        };
        self.ensure_syntax();
        let Some(idx) = self.nav.index.as_ref() else {
            return Err(match &self.nav.progress {
                Some((a, b)) => format!("索引を作っています（{a}/{b}）。しばらくしてからもう一度").into(),
                None => "索引がありません".into(),
            });
        };
        let here = self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok());
        let rel = here.as_ref().and_then(|h| h.strip_prefix(&idx.root).ok()).map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_default();
        let src = self.buf.text();
        let mut r = nagi_workers::index::Ranker::new(Some(idx));
        let is_type = idx.lookup(&name).iter().any(|l| l.kind.is_type()) && name.chars().next().is_some_and(char::is_uppercase);
        let (ty, member) = if is_type {
            (Some(name.clone()), None)
        } else {
            let recv = self.syntax.as_ref().and_then(|s| s.receiver(&self.buf, word_at));
            let ty = match recv {
                Some(rc) => r.receiver_type_name(&rel, &src, word_at, &rc),
                None => nagi_workers::index::enclosing_type(&src, word_at),
            };
            (ty, Some(name.as_str()))
        };
        let Some(ty) = ty else { return Err(format!("{name} の型が分かりません").into()) };
        let locs = r.implementations(&ty, member);
        let root = idx.root.clone();
        if locs.is_empty() {
            return Err(format!("実装が見つかりません: {}{name}", if is_type { String::new() } else { format!("{ty}.") }).into());
        }
        Ok((locs, root))
    }

    fn show_implementations(&mut self, locs: Vec<(String, nagi_workers::index::Loc)>, root: PathBuf) {
        if let [(_, l)] = locs.as_slice() {
            self.jump_to(&root.join(&*l.path), l.line as usize, l.col as usize);
            return;
        }
        let n = locs.len();
        let items = locs.iter().map(|(n, l)| format!("{n}  {}  {}:{}", l.kind.label(), l.path, l.line + 1)).collect();
        let at = locs.iter().map(|(_, l)| (root.join(&*l.path), l.line as usize, l.col as usize)).collect();
        self.open_locations(Source::Definitions, items, at, false);
        self.notify.info(format!("実装 {n} 件"));
    }

    /// カーソルがメソッド・型の定義そのものの上か（gd を実装へ向ける）
    fn on_definition(&self, name: &str, word_at: usize) -> bool {
        let (Some(idx), Some(here)) = (&self.nav.index, self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok())) else { return false };
        let Some(rel) = here.strip_prefix(&idx.root).ok().map(|r| r.to_string_lossy().replace('\\', "/")) else { return false };
        let cur = (self.cursor.line, word_at - self.buf.line_start(self.cursor.line));
        idx.lookup(name).iter().any(|l| *l.path == *rel && (l.line as usize, l.col as usize) == cur && (l.kind.is_type() || l.kind == DefKind::Method))
    }

    /// `gr`: 名前が単語として一致する箇所（全文検索のパネル）
    pub(crate) fn find_references(&mut self) {
        let Some((name, _)) = self.word_at_cursor() else { return };
        self.open_grep_regex(&format!(r"\b{}\b", regex::escape(&name)));
    }

    /// シンボル検索（`types` なら型だけ）
    pub(crate) fn symbol_search(&mut self, types: bool) {
        let Some(idx) = &self.nav.index else {
            self.notify.info(match &self.nav.progress {
                Some((a, b)) => format!("索引を作っています（{a}/{b}）。しばらくしてからもう一度"),
                None => "索引がありません".into(),
            });
            return;
        };
        let root = idx.root.clone();
        let mut all: Vec<(String, PathBuf, usize, usize, DefKind)> = idx
            .all()
            .filter(|(_, l)| !types || l.kind.is_type())
            .map(|(n, l)| (n.to_string(), root.join(&*l.path), l.line as usize, l.col as usize, l.kind))
            .collect();
        // 検索語が空のときは短い名前から
        all.sort_by(|a, b| a.0.len().cmp(&b.0.len()).then_with(|| a.0.cmp(&b.0)));
        let items = all.iter().map(|(n, p, l, _, k)| format!("{n}  {}  {}:{}", k.label(), rel(p, &root), l + 1)).collect();
        let locs = all.into_iter().map(|(_, p, l, c, _)| (p, l, c)).collect();
        self.open_locations(if types { Source::Types } else { Source::Symbols }, items, locs, true);
    }

    /// ファイルの構造（このファイルの定義を行の順に）
    pub(crate) fn outline(&mut self) {
        self.ensure_syntax();
        let (Some(syn), Some(path)) = (&self.syntax, self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok())) else {
            self.notify.error("このファイルには構文木が無いので使えません");
            return;
        };
        let defs = syn.defs(&self.buf);
        let items = defs.iter().map(|d| format!("{}  {}  :{}", d.name, d.kind.label(), d.line + 1)).collect();
        let locs = defs.into_iter().map(|d| (path.clone(), d.line, d.col)).collect();
        self.open_locations(Source::Outline, items, locs, false);
    }

    /// `[m` `]m`: 前・次の関数・メソッドの頭へ
    pub(crate) fn method_jump(&mut self, next: bool) {
        self.ensure_syntax();
        let Some(syn) = &self.syntax else { return };
        let line = self.cursor.line;
        let defs = syn.defs(&self.buf);
        let it = defs.iter().filter(|d| matches!(d.kind, DefKind::Function | DefKind::Method)).map(|d| (d.line, d.col));
        let to = if next { it.filter(|(l, _)| *l > line).min() } else { it.filter(|(l, _)| *l < line).max() };
        if let Some((l, _)) = to {
            self.push_jump();
            self.cursor.line = l;
            self.with_motion(|m, c| m.first_non_blank(c));
        }
    }

    /// `:Idea`: 今のファイルと行を IntelliJ で開く（重いリファクタリング用）
    pub(crate) fn open_in_intellij(&mut self) {
        let Some(path) = self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok()) else {
            self.notify.info("ファイル名がありません");
            return;
        };
        let Some(exe) = find_idea() else {
            self.notify.warn("IntelliJ が見つかりません（NAGI_IDEA に idea64.exe の場所を入れてください）");
            return;
        };
        let line = (self.cursor.line + 1).to_string();
        match std::process::Command::new(&exe).arg("--line").arg(&line).arg(&path).spawn() {
            Ok(_) => self.notify.info(format!("IntelliJ で開きました: {}:{line}", path.display())),
            Err(e) => self.notify.error(format!("IntelliJ を起動できません: {e}")),
        }
    }

    fn open_locations(&mut self, source: Source, items: Vec<String>, locs: Vec<(PathBuf, usize, usize)>, _big: bool) {
        self.open_finder(source);
        if let Some(f) = &mut self.finder {
            f.set_locations(items, locs);
        }
        if self.state() != State::Finder {
            self.set_state(State::Finder);
        }
    }
}

fn rel(p: &Path, root: &Path) -> String {
    p.strip_prefix(root).map_or_else(|_| p.display().to_string(), |r| r.to_string_lossy().replace('\\', "/"))
}

/// idea64.exe の場所（NAGI_IDEA、PATH の idea / idea64、Toolbox の scripts、Program Files の JetBrains）
fn find_idea() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("NAGI_IDEA").map(PathBuf::from).filter(|p| p.exists()) {
        return Some(p);
    }
    if let Some(path) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&path) {
            for n in ["idea64.exe", "idea.cmd", "idea.bat"] {
                if d.join(n).exists() {
                    return Some(d.join(n));
                }
            }
        }
    }
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    if let Some(s) = local.as_ref().map(|l| l.join("JetBrains").join("Toolbox").join("scripts").join("idea.cmd")).filter(|p| p.exists()) {
        return Some(s);
    }
    for base in [local.map(|l| l.join("Programs")), std::env::var_os("ProgramFiles").map(|p| PathBuf::from(p).join("JetBrains"))].into_iter().flatten() {
        let Ok(rd) = std::fs::read_dir(&base) else { continue };
        let mut found: Vec<PathBuf> = rd.flatten().map(|e| e.path().join("bin").join("idea64.exe")).filter(|p| p.exists()).collect();
        found.sort();
        if let Some(p) = found.pop() {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    fn keys(e: &mut Editor, k: &str) {
        for key in nagi_vim::parse_keys(k, " ") {
            e.key(key);
        }
    }

    fn open(path: &Path) -> Editor {
        let mut buf = Buffer::open(path).unwrap();
        buf.path = Some(path.to_path_buf());
        let lang = nagi_syntax::Lang::detect(path).unwrap();
        let mut syn = nagi_syntax::Syntax::new(lang).unwrap();
        syn.parse(&mut buf);
        let mut e = Editor::new(buf, Some(syn));
        e.rows = 20;
        e
    }

    fn wait_index(e: &mut Editor) {
        e.start_index();
        for _ in 0..1000 {
            e.poll_index();
            if e.nav.index.is_some() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("索引が終わらない");
    }

    /// 実際の Java のプロジェクトで、プロジェクトの索引に無い名前の x.m（ライブラリのメソッド・フィールド）から、ライブラリの宣言の行へ跳べる割合
    /// （NAGI_JAVA_ROOT=… LOCALAPPDATA=… cargo test -p nagi lib_jump_rate -- --ignored --nocapture。ソース・骨組みは LOCALAPPDATA の下に書く）
    #[test]
    #[ignore]
    fn lib_jump_rate() {
        let Some(root) = std::env::var_os("NAGI_JAVA_ROOT").map(PathBuf::from) else { return };
        let rx = nagi_workers::index::build(&root, std::sync::Arc::new(|| {}));
        let idx = loop {
            if let Ok(nagi_workers::index::Progress::Done(i)) = rx.recv() {
                break i;
            }
        };
        let mut e = Editor::new(Buffer::from_str(""), None);
        e.nav.index = Some(*idx);
        let task = nagi_workers::jvm::load_maven(root.clone(), std::sync::Arc::new(|| {}));
        let jars = loop {
            if let Ok(r) = task.try_recv() {
                break r.unwrap();
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let jars = std::sync::Arc::new(jars);
        e.nav.jars = Some(jars.clone());
        let dir = crate::java_source::sources_dir().unwrap();
        let site = regex::Regex::new(r"(?:[\w)]|^\s*)\.([A-Za-z_]\w*)").unwrap();
        // [宣言の行へ, 型までは分かった（行が違う）, 分からない]
        let mut n = [0usize; 3];
        let (mut source, mut stub) = (0, 0);
        let mut miss: std::collections::HashMap<String, (usize, String)> = Default::default();
        let mut files = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(d) = stack.pop() {
            for f in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = f.path();
                let name = f.file_name().to_string_lossy().into_owned();
                if p.is_dir() && !name.starts_with('.') && name != "target" && name != "node_modules" {
                    stack.push(p);
                } else if name.ends_with(".java") {
                    files.push(p);
                }
            }
        }
        files.sort();
        let step = std::env::var("NAGI_RATE_FILES").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).max(1);
        let t0 = std::time::Instant::now();
        for p in files.iter().step_by(step) {
            let Ok(buf) = Buffer::open(p) else { continue };
            e.buf = buf;
            e.buf.path = Some(p.clone());
            for li in 0..e.buf.line_count() {
                let line = e.buf.line(li).to_string();
                let t = line.trim_start();
                if ["import", "package", "//", "*", "/*"].iter().any(|x| t.starts_with(x)) {
                    continue;
                }
                for m in site.captures_iter(&line) {
                    let g = m.get(1).unwrap();
                    let idx = e.nav.index.as_ref().unwrap();
                    if line[..g.start()].matches('"').count() % 2 == 1 || !idx.lookup(g.as_str()).is_empty() || g.as_str() == "class" {
                        continue;
                    }
                    // Lombok の getter・setter（プロジェクトのフィールド）と、パッケージの名前の途中は除く
                    let field = ["get", "set", "is"].iter().find_map(|p| g.as_str().strip_prefix(p)).filter(|f| f.starts_with(char::is_uppercase)).map(|f| f[..1].to_lowercase() + &f[1..]);
                    if field.is_some_and(|f| !idx.lookup(&f).is_empty()) {
                        continue;
                    }
                    let chain = crate::java_members::dotted_chain(&line[..g.start() - 1]).unwrap_or("").replace('.', "/");
                    if !chain.is_empty() && (idx.is_java_package(&chain) || jars.is_package(&chain)) {
                        continue;
                    }
                    let at = e.buf.line_start(li) + g.start();
                    let k = match e.library_target(g.as_str(), at) {
                        Some((owner, member)) => match crate::java_source::open(&jars, &owner, member.as_deref(), &dir) {
                            Some(o) => {
                                let text = std::fs::read_to_string(&o.path).unwrap_or_default();
                                if o.source { source += 1 } else { stub += 1 }
                                if text.lines().nth(o.line).is_some_and(|l| l.contains(g.as_str())) { 0 } else { 1 }
                            }
                            None => 2,
                        },
                        None => 2,
                    };
                    n[k] += 1;
                    if k > 0 {
                        let recv = line[..g.start()].trim().rsplit([' ', '(', '!', '=', ',']).next().unwrap_or("").to_string();
                        let e = miss.entry(format!("{k} {recv}{}", g.as_str())).or_insert((0, line.trim().chars().take(90).collect()));
                        e.0 += 1;
                    }
                }
            }
        }
        // 型の名前（大文字で始まり小文字を含む語で、`.` の後でなく、プロジェクトの索引に無いもの）
        let ty_re = regex::Regex::new(r"\b[A-Z][A-Za-z0-9_]*[a-z][A-Za-z0-9_]*\b").unwrap();
        let (mut ty_ok, mut ty_all) = (0usize, 0usize);
        let mut ty_miss: std::collections::HashMap<String, usize> = Default::default();
        for p in files.iter().step_by(step) {
            let Ok(buf) = Buffer::open(p) else { continue };
            e.buf = buf;
            e.buf.path = Some(p.clone());
            for li in 0..e.buf.line_count() {
                let line = e.buf.line(li).to_string();
                let t = line.trim_start();
                if ["import", "package", "//", "*", "/*"].iter().any(|x| t.starts_with(x)) {
                    continue;
                }
                for m in ty_re.find_iter(&line) {
                    if line[..m.start()].ends_with('.') || line[..m.start()].matches('"').count() % 2 == 1 || !e.nav.index.as_ref().unwrap().lookup(m.as_str()).is_empty() {
                        continue;
                    }
                    // 型として使っている形（new X・@X・X<・X[]・X 名前・X.）
                    let (pre, post) = (line[..m.start()].trim_end(), &line[m.end()..]);
                    let decl = post.strip_prefix(' ').is_some_and(|r| r.starts_with(|c: char| c.is_lowercase()));
                    if !(pre.ends_with("new") || pre.ends_with('@') || post.starts_with(['<', '.']) || post.starts_with("[]") || decl) {
                        continue;
                    }
                    ty_all += 1;
                    let at = e.buf.line_start(li) + m.start();
                    let ok = e.library_target(m.as_str(), at).and_then(|(owner, member)| crate::java_source::open(&jars, &owner, member.as_deref(), &dir)).is_some_and(|o| {
                        std::fs::read_to_string(&o.path).unwrap_or_default().lines().nth(o.line).is_some_and(|l| l.contains(m.as_str()))
                    });
                    if ok {
                        ty_ok += 1;
                    } else {
                        *ty_miss.entry(m.as_str().to_string()).or_default() += 1;
                    }
                }
            }
        }
        println!("型の名前 {ty_all} 箇所: 宣言の行へ {:.1}%", ty_ok as f64 * 100.0 / ty_all.max(1) as f64);
        let mut v: Vec<_> = ty_miss.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        println!("  跳べない型: {:?}", &v[..v.len().min(25)]);
        let all: usize = n.iter().sum();
        let pc = |x: usize| x as f64 * 100.0 / all.max(1) as f64;
        println!("{all} 箇所 {:?}: 宣言の行へ {:.1}% / 行が違う {:.1}% / 分からない {:.1}%（開いたうちソース {source}・骨組み {stub}）", t0.elapsed(), pc(n[0]), pc(n[1]), pc(n[2]));
        let mut v: Vec<_> = miss.into_iter().collect();
        v.sort_by(|a, b| b.1.0.cmp(&a.1.0));
        for (k, (c, ex)) in v.iter().take(40) {
            println!("{c:5} {k}    {ex}");
        }
    }

    #[test]
    fn definition_jumps() {
        let dir = std::env::temp_dir().join(format!("nagi-nav-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        unsafe { std::env::set_var("NAGI_INDEX_DIR", dir.join("cache")) };
        std::fs::write(dir.join("src/b.ts"), "export function helper(n: number) {\n  return n;\n}\nexport class Box {}\n").unwrap();
        std::fs::write(dir.join("src/c.ts"), "export class Box {}\n").unwrap();
        std::fs::write(dir.join("src/a.ts"), "import { helper } from './b';\nconst local = 1;\nhelper(local);\nnew Box();\n").unwrap();
        let mut e = open(&dir.join("src/a.ts"));
        wait_index(&mut e);
        // ファイルの中: local → 2 行目
        keys(&mut e, "3Gf(l");
        assert_eq!(e.word_at_cursor().map(|w| w.0).as_deref(), Some("local"));
        keys(&mut e, "gd");
        assert_eq!(e.cursor.line, 1);
        // 別のファイル: helper → b.ts の定義（import の束縛ではなく、索引の定義へ… import 文の中は束縛として扱わない）
        keys(&mut e, "3G0gd");
        assert!(e.buf.path.as_ref().unwrap().ends_with("b.ts"), "{:?} {}", e.buf.path, e.notify.text);
        assert_eq!((e.cursor.line, e.cursor.byte), (0, 16));
        // Ctrl-o で戻る
        keys(&mut e, "<C-o>");
        assert!(e.buf.path.as_ref().unwrap().ends_with("a.ts"));
        // F12 でも（Insert の最中でも）
        keys(&mut e, "3G0<F12>");
        assert!(e.buf.path.as_ref().unwrap().ends_with("b.ts"), "{:?} {}", e.buf.path, e.notify.text);
        keys(&mut e, "<C-o>3G0i<F12>");
        assert!(e.buf.path.as_ref().unwrap().ends_with("b.ts"), "{:?} {}", e.buf.path, e.notify.text);
        assert_eq!(e.state(), State::Normal);
        keys(&mut e, "<C-o>");
        // 候補が 2 つ（Box）: ファインダーで選ぶ
        keys(&mut e, "4Gwgd");
        assert_eq!(e.state(), State::Finder);
        assert_eq!(e.finder.as_ref().unwrap().hits.len(), 2);
        keys(&mut e, "c.ts<CR>");
        assert!(e.buf.path.as_ref().unwrap().ends_with("c.ts"));
        // ファイルの構造とシンボル検索
        keys(&mut e, &format!(":e {}<CR>", dir.join("src/b.ts").display()));
        e.outline();
        assert_eq!(e.finder.as_ref().unwrap().items.len(), 2, "helper と Box");
        keys(&mut e, "<Esc><Esc>");
        e.symbol_search(true);
        assert_eq!(e.finder.as_ref().unwrap().items.len(), 2, "型は Box が 2 つ");
        keys(&mut e, "<Esc><Esc>");
    }

    #[test]
    fn implementation_jumps() {
        let dir = std::env::temp_dir().join(format!("nagi-impl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        unsafe { std::env::set_var("NAGI_INDEX_DIR", dir.join("cache")) };
        std::fs::write(dir.join("src/UserService.java"), "interface UserService {\n  User find(long id);\n}\n").unwrap();
        std::fs::write(dir.join("src/UserServiceImpl.java"), "class UserServiceImpl implements UserService {\n  public User find(long id) { return null; }\n}\n").unwrap();
        std::fs::write(dir.join("src/Other.java"), "class Other {\n  User find(long id) { return null; }\n}\n").unwrap();
        std::fs::write(dir.join("src/Use.java"), "class Use {\n  UserService users;\n  void f() { users.find(1); }\n}\n").unwrap();
        let mut e = open(&dir.join("src/Use.java"));
        wait_index(&mut e);
        // users.find の gd はインタフェースの宣言、gI は実装
        keys(&mut e, "3G0/find<CR>gd");
        assert!(e.buf.path.as_ref().unwrap().ends_with("UserService.java"), "{:?} {}", e.buf.path, e.notify.text);
        keys(&mut e, "<C-o>3G0/find<CR>gI");
        assert!(e.buf.path.as_ref().unwrap().ends_with("UserServiceImpl.java"), "{:?} {}", e.buf.path, e.notify.text);
        assert_eq!(e.cursor.line, 1);
        // インタフェースの宣言の上で gI → 実装
        keys(&mut e, &format!(":e {}<CR>2G0/find<CR>gI", dir.join("src/UserService.java").display()));
        assert!(e.buf.path.as_ref().unwrap().ends_with("UserServiceImpl.java"), "{:?} {}", e.buf.path, e.notify.text);
        // 宣言の上の gd も実装へ（実装が無ければ従来どおり）
        keys(&mut e, &format!(":e {}<CR>2G0/find<CR>gd", dir.join("src/UserService.java").display()));
        assert!(e.buf.path.as_ref().unwrap().ends_with("UserServiceImpl.java"), "{:?} {}", e.buf.path, e.notify.text);
        // 型の上で gI → 実装している型
        keys(&mut e, &format!(":e {}<CR>1G0wgI", dir.join("src/UserService.java").display()));
        assert!(e.buf.path.as_ref().unwrap().ends_with("UserServiceImpl.java"), "{:?} {}", e.buf.path, e.notify.text);
        assert_eq!(e.cursor.line, 0);
    }
}
