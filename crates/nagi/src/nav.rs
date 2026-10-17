//! コードナビゲーション（LSP なし。architecture.md §10）。
//!
//! - `gd` / `Ctrl-]`: ファイルの中の束縛（引数・ローカル変数・定義）を探し、無ければプロジェクトの索引から。
//!   候補が 1 つならそこへ跳び、複数ならファインダーで選ぶ（同じファイル、近いフォルダの順）
//! - `gr`: 名前が単語として一致する箇所を全文検索のパネルに出す（参照の検索。名前の一致に基づく）
//! - シンボル検索（GotoSymbol / GotoClass）、ファイルの構造（FileStructurePopup）、`[m` `]m`
//! - 索引は起動後に裏で作り、保存したファイルは 1 つずつ索引し直す

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use nagi_syntax::defs::DefKind;
use nagi_workers::index::{Entry, Index, Progress};

use crate::editor::{Editor, Mode};
use crate::finder::Source;

/// 索引の状態（Editor が持つ）
#[derive(Default)]
pub struct IndexState {
    pub index: Option<Index>,
    building: Option<Receiver<Progress>>,
    /// 作っている途中 (解析したファイル, 解析が要るファイル)
    pub progress: Option<(usize, usize)>,
    /// 保存したファイルの索引し直し
    reindex: Option<(Sender<(String, (u64, u64), Vec<Entry>)>, Receiver<(String, (u64, u64), Vec<Entry>)>)>,
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
        let root = self.project_root();
        let waker = self.waker.clone().unwrap_or_else(|| std::sync::Arc::new(|| {}));
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
            while let Ok((rel, st, defs)) = rx.try_recv() {
                idx.replace_file(&rel, st, defs);
                updated = true;
            }
        }
        if updated {
            // 保存は裏で（UI スレッドでファイルを書かない）。中身は今の索引の写し
            if let Some(idx) = &self.nav.index {
                let snapshot = idx.snapshot();
                let _ = std::thread::Builder::new().name("index-save".into()).spawn(move || snapshot.save());
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
        let (tx, _) = self.nav.reindex.get_or_insert_with(channel);
        let tx = tx.clone();
        let waker = self.waker.clone();
        let _ = std::thread::Builder::new().name("reindex".into()).spawn(move || {
            let mut p = nagi_syntax::defs::DefParser::default();
            if let Some((st, defs)) = nagi_workers::index::index_file(&abs, &mut p) {
                if tx.send((rel, st, defs)).is_ok() {
                    if let Some(w) = waker {
                        w();
                    }
                }
            }
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
            self.message = "E348: カーソルの下に識別子がありません".into();
            return;
        };
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
        let mut cands: Vec<(PathBuf, usize, usize, DefKind)> = match &self.nav.index {
            Some(idx) => idx.lookup(&name).iter().map(|l| (idx.root.join(&*l.path), l.line as usize, l.col as usize, l.kind)).collect(),
            None => Vec::new(),
        };
        if cands.is_empty() {
            if let (Some(syn), Some(p)) = (&self.syntax, &here) {
                cands = syn.defs(&self.buf).into_iter().filter(|d| d.name == name).map(|d| (p.clone(), d.line, d.col, d.kind)).collect();
            }
        }
        // 今いる定義そのものは候補から外す（ほかに候補があれば）
        let cur = (self.cursor.line, word_at - self.buf.line_start(self.cursor.line));
        if cands.len() > 1 {
            cands.retain(|(p, l, c, _)| !(Some(p) == here.as_ref() && (*l, *c) == cur));
        }
        match cands.len() {
            0 => {
                self.message = match &self.nav.progress {
                    Some((a, b)) => format!("定義が見つかりません: {name}（索引を作っています {a}/{b}）"),
                    None => format!("定義が見つかりません: {name}"),
                }
            }
            1 => {
                let (p, l, c, _) = cands.remove(0);
                self.jump_to(&p, l, c);
            }
            _ => {
                // 同じファイル → 型の名前なら型 → 近いフォルダ の順
                let upper = name.chars().next().is_some_and(char::is_uppercase);
                let near = |p: &Path| here.as_ref().map_or(0, |h| h.components().zip(p.components()).take_while(|(a, b)| a == b).count());
                cands.sort_by_key(|(p, l, _, k)| (Some(p) != here.as_ref(), !(upper && k.is_type()), std::cmp::Reverse(near(p)), p.clone(), *l));
                let root = self.nav.index.as_ref().map(|i| i.root.clone()).unwrap_or_default();
                let items = cands.iter().map(|(p, l, _, k)| format!("{name}  {}  {}:{}", k.label(), rel(p, &root), l + 1)).collect();
                let locs = cands.into_iter().map(|(p, l, c, _)| (p, l, c)).collect();
                self.open_locations(Source::Definitions, items, locs, false);
            }
        }
    }

    /// `gr`: 名前が単語として一致する箇所（全文検索のパネル）
    pub(crate) fn find_references(&mut self) {
        let Some((name, _)) = self.word_at_cursor() else { return };
        self.open_grep_regex(&format!(r"\b{}\b", regex::escape(&name)));
    }

    /// シンボル検索（`types` なら型だけ）
    pub(crate) fn symbol_search(&mut self, types: bool) {
        let Some(idx) = &self.nav.index else {
            self.message = match &self.nav.progress {
                Some((a, b)) => format!("索引を作っています（{a}/{b}）。しばらくしてからもう一度"),
                None => "索引がありません".into(),
            };
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
            self.message = "このファイルには構文木が無いので使えません".into();
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
            self.message = "ファイル名がありません".into();
            return;
        };
        let Some(exe) = find_idea() else {
            self.message = "IntelliJ が見つかりません（NAGI_IDEA に idea64.exe の場所を入れてください）".into();
            return;
        };
        let line = (self.cursor.line + 1).to_string();
        match std::process::Command::new(&exe).arg("--line").arg(&line).arg(&path).spawn() {
            Ok(_) => self.message = format!("IntelliJ で開きました: {}:{line}", path.display()),
            Err(e) => self.message = format!("IntelliJ を起動できません: {e}"),
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
        assert!(e.buf.path.as_ref().unwrap().ends_with("b.ts"), "{:?} {}", e.buf.path, e.message);
        assert_eq!((e.cursor.line, e.cursor.byte), (0, 16));
        // Ctrl-o で戻る
        keys(&mut e, "<C-o>");
        assert!(e.buf.path.as_ref().unwrap().ends_with("a.ts"));
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
        keys(&mut e, "<Esc>");
        e.symbol_search(true);
        assert_eq!(e.finder.as_ref().unwrap().items.len(), 2, "型は Box が 2 つ");
        keys(&mut e, "<Esc>");
    }
}
