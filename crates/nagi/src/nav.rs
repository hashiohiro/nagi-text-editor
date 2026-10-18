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

use crate::editor::{Editor, Mode};
use crate::finder::Source;

/// 索引の状態（Editor が持つ）
#[derive(Default)]
pub struct IndexState {
    pub index: Option<Index>,
    building: Option<nagi_core::task::Task<Progress>>,
    /// 作っている途中 (解析したファイル, 解析が要るファイル)
    pub progress: Option<(usize, usize)>,
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
            .or_else(|| std::env::current_dir().ok())
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
        self.nav.building = Some(nagi_workers::index::build(&root, waker));
        self.nav.progress = Some((0, 0));
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
                        self.notify.info(format!("定義はプロジェクトの外です: {name}（{t} から先はライブラリ）"));
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

    /// `gI`: 実装へ。型の上なら継承・実装している型、メソッドの上なら受け手の型（宣言の上なら囲む型）の子の中のそのメソッド
    pub(crate) fn goto_implementation(&mut self) {
        match self.find_implementations() {
            Ok((locs, root)) => self.show_implementations(locs, root),
            Err(m) => self.notify.error(m),
        }
    }

    /// 実装の一覧（名前, 場所）と索引の根。見つからなければ伝える文
    fn find_implementations(&mut self) -> Result<(Vec<(String, nagi_workers::index::Loc)>, PathBuf), String> {
        let Some((name, word_at)) = self.word_at_cursor() else {
            return Err("E348: カーソルの下に識別子がありません".into());
        };
        self.ensure_syntax();
        let Some(idx) = self.nav.index.as_ref() else {
            return Err(match &self.nav.progress {
                Some((a, b)) => format!("索引を作っています（{a}/{b}）。しばらくしてからもう一度"),
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
        let Some(ty) = ty else { return Err(format!("{name} の型が分かりません")) };
        let locs = r.implementations(&ty, member);
        let root = idx.root.clone();
        if locs.is_empty() {
            return Err(format!("実装が見つかりません: {}{name}", if is_type { String::new() } else { format!("{ty}.") }));
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
        if self.mode != Mode::Finder {
            self.mode = Mode::Finder;
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
        assert_eq!(e.mode, Mode::Normal);
        keys(&mut e, "<C-o>");
        // 候補が 2 つ（Box）: ファインダーで選ぶ
        keys(&mut e, "4Gwgd");
        assert_eq!(e.mode, Mode::Finder);
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
