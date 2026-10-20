//! `:` のコマンドラインの候補（ターミナルの Vim の wildmenu と同じ形）。
//!
//! 打つたびに最下行の上へ候補を説明付きで出す。コマンド名を打っている間はコマンドの一覧を前方一致で、
//! ファイル名を取るコマンド（`:e` `:w` `:sp` `:vs` `:saveas`）の後ろではプロジェクトのファイルをファジーに、
//! `:b` の後ろでは開いているバッファを出す。Tab・↓・`Ctrl-n`・`Ctrl-j` で次、↑・`Ctrl-p`・`Ctrl-k` で前の候補を入力欄へ入れる
//! （h j k l は文字として打つので、選ぶのは Ctrl と組み合わせる）。マウスで押してもその候補を入れる。

use nagi_core::fuzzy::{Query, rank_ids};
use nagi_vim::Key;

use crate::editor::Editor;

/// 打った名前に合うコマンド（大文字小文字は区別しない）。頭が合うものを先に、途中に含むものを後に。
/// 日本語を打ったら説明文からも探す。打った名前とまったく同じ 1 件だけなら（`force` でなければ）出さない
pub fn match_commands(body: &str, force: bool) -> Vec<(String, String)> {
    let low = body.to_lowercase();
    let mut head = Vec::new();
    let mut mid = Vec::new();
    for (c, d) in crate::commands::listed() {
        let lc = c.to_lowercase();
        if lc.starts_with(&low) {
            head.push((c.to_string(), d.to_string()));
        } else if !low.is_empty() && (lc.contains(&low) || (!body.is_ascii() && d.contains(body))) {
            mid.push((c.to_string(), d.to_string()));
        }
    }
    head.extend(mid);
    if !force && head.len() == 1 && head[0].0.eq_ignore_ascii_case(body) {
        head.clear();
    }
    head
}


/// 一覧に出す件数の上限
pub const MAX_SHOWN: usize = 10;

pub struct CmdComplete {
    /// (入力欄に入れる文字列, 説明)
    pub items: Vec<(String, String)>,
    /// Tab で選んでいる候補
    pub sel: Option<usize>,
    /// 補う語の始まり（入力欄の byte）
    pub start: usize,
    /// 選ぶ前の語（候補の端の先で戻す）
    pub original: String,
    /// 描いた位置（マウス用）: (画面の行, 列の始め, 終わり, 候補の番号)
    pub rows: crate::ui::Hits<usize>,
}

impl Editor {
    /// 入力欄が変わったら候補を作り直す
    /// コマンドラインに入る（パネルの中の `:` もここを通して、候補を出す）
    pub(crate) fn enter_command(&mut self, initial: &str) {
        self.set_state(crate::editor::State::Command);
        self.cmdline = Default::default();
        self.cmdline.set(initial);
        self.pending_keys.clear();
        self.update_cmd_complete();
    }

    pub(crate) fn update_cmd_complete(&mut self) {
        let text = self.cmdline.text[..self.cmdline.cursor].to_string();
        // ファイル名の候補が要るのに一覧がまだ無ければ、裏で集め始める（届いたら poll_cmd_files で出し直す）
        let wants_files = text.split_once(' ').is_some_and(|(n, _)| crate::commands::args_of(n.trim_start_matches(|c: char| !c.is_ascii_alphabetic())) == crate::commands::Args::File);
        let root = self.finder_root();
        if wants_files && !self.file_cache.contains_key(&root) && self.cmd_files.is_none() {
            let waker = self.waker();
            self.cmd_files = Some((root, nagi_workers::files::walk(&self.finder_root(), waker), Vec::new()));
        }
        self.cmd_complete = self.cmd_candidates(&text);
    }

    /// 裏で集めたファイルの一覧を受け取る。集め終えたら候補を出し直す
    pub fn poll_cmd_files(&mut self) -> bool {
        let Some((_, walk, acc)) = &mut self.cmd_files else { return false };
        loop {
            match walk.rx.try_recv() {
                Ok(b) => acc.extend(b),
                Err(std::sync::mpsc::TryRecvError::Empty) => return false,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
        let (root, _, mut files) = self.cmd_files.take().unwrap();
        files.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
        self.file_cache.insert(root, files);
        if self.state() == crate::editor::State::Command {
            self.update_cmd_complete();
        }
        true
    }

    fn cmd_candidates(&self, text: &str) -> Option<CmdComplete> {
        self.cmd_candidates_forced(text, false)
    }

    /// `force` なら、打った名前と同じ 1 件でも出す（Ctrl+Space）
    pub(crate) fn cmd_candidates_forced(&self, text: &str, force: bool) -> Option<CmdComplete> {
        // 範囲（数字・% ・ ' など）は飛ばす
        // 日本語で説明文から探すこともあるので、ASCII 以外の文字でも名前の始まりとする
        let body_at = text.find(|c: char| c.is_ascii_alphabetic() || !c.is_ascii()).unwrap_or(text.len());
        if text[..body_at].chars().any(|c| !(c.is_ascii_digit() || matches!(c, '%' | '.' | '$' | ',' | '\'' | '<' | '>' | '+' | '-' | ' '))) {
            return None;
        }
        let body = &text[body_at..];
        match body.split_once(' ') {
            None => {
                // コマンド名を打っている
                let items = match_commands(body, force);
                (!items.is_empty()).then(|| CmdComplete { items, sel: None, start: body_at, original: body.to_string(), rows: Default::default() })
            }
            Some((name, arg)) => {
                let start = body_at + name.len() + 1;
                // スキーマ・書き出しの形
                // 引数の種類は commands.rs の表から
                let kind = crate::commands::args_of(name);
                let fixed: Option<Vec<String>> = match kind {
                    crate::commands::Args::Schema => Some(self.db_schema_names()),
                    crate::commands::Args::Choice(list) => Some(list.iter().map(|s| s.to_string()).collect()),
                    _ => None,
                };
                if let Some(list) = fixed {
                    let low = arg.to_lowercase();
                    let items: Vec<(String, String)> = list.into_iter().filter(|s| s.to_lowercase().starts_with(&low)).map(|s| (s, String::new())).collect();
                    return (!items.is_empty()).then(|| CmdComplete { items, sel: None, start, original: arg.to_string(), rows: Default::default() });
                }
                if kind == crate::commands::Args::Buffer {
                    let low = arg.to_lowercase();
                    let items: Vec<(String, String)> = self
                        .buffer_list()
                        .into_iter()
                        .filter_map(|(i, p, dirty, _)| {
                            let n = p?.file_name()?.to_string_lossy().into_owned();
                            n.to_lowercase().contains(&low).then(|| (n, format!("バッファ {}{}", i + 1, if dirty { " [+]" } else { "" })))
                        })
                        .collect();
                    return (!items.is_empty()).then(|| CmdComplete { items, sel: None, start, original: arg.to_string(), rows: Default::default() });
                }
                if kind == crate::commands::Args::File {
                    // プロジェクトのファイル（ファインダーで集めた一覧）をファジーに。まだ無ければ裏で集める
                    let root = self.finder_root();
                    let files = self.file_cache.get(&root)?;
                    let q = Query::new(arg);
                    let ids: Vec<u32> = (0..files.len() as u32).collect();
                    let hits = rank_ids(&q, files, &ids);
                    let items: Vec<(String, String)> = hits.iter().take(200).map(|&(i, _)| (files[i as usize].clone(), String::new())).collect();
                    return (!items.is_empty()).then(|| CmdComplete { items, sel: None, start, original: arg.to_string(), rows: Default::default() });
                }
                None
            }
        }
    }

    /// 候補 `i` を入力欄へ入れる（マウスで押したとき）
    pub(crate) fn cmd_complete_pick(&mut self, i: usize) {
        let Some(c) = &mut self.cmd_complete else { return };
        if i >= c.items.len() {
            return;
        }
        c.sel = Some(i);
        let word = c.items[i].0.clone();
        let start = c.start;
        let end = self.cmdline.cursor;
        self.cmdline.text.replace_range(start..end, &word);
        self.cmdline.cursor = start + word.len();
    }

    /// マウス: 画面の (行, 列) が候補の上ならそれを入れる。当たれば true
    pub(crate) fn cmd_complete_click(&mut self, row: usize, col: usize, clicks: u32) -> bool {
        let Some(c) = &self.cmd_complete else { return false };
        let hit = c.rows.at(row, col);
        let mut sel = c.sel.unwrap_or(0);
        // 候補は 1 回で入れる（メニューと同じ）
        match crate::list::pick(&mut sel, hit, clicks, 1) {
            Some(crate::list::Pick::Accept(i)) => {
                self.cmd_complete_pick(i);
                true
            }
            _ => false,
        }
    }

    /// Tab・↓・Ctrl-n・Ctrl-j で次、↑・Ctrl-p・Ctrl-k で前。扱ったら true
    pub(crate) fn cmd_complete_key(&mut self, key: Key) -> bool {
        let forward = match key {
            Key::Tab | Key::Down | Key::Ctrl('n') | Key::Ctrl('j') => true,
            Key::Up | Key::Ctrl('p') | Key::Ctrl('k') => false,
            _ => return false,
        };
        if self.cmd_complete.is_none() {
            self.update_cmd_complete();
        }
        let Some(c) = &mut self.cmd_complete else { return true };
        let n = c.items.len();
        c.sel = match (c.sel, forward) {
            (None, true) => Some(0),
            (None, false) => Some(n - 1),
            (Some(i), true) if i + 1 < n => Some(i + 1),
            (Some(i), false) if i > 0 => Some(i - 1),
            _ => None,
        };
        let word = match c.sel {
            Some(i) => c.items[i].0.clone(),
            None => c.original.clone(),
        };
        let start = c.start;
        let end = self.cmdline.cursor;
        self.cmdline.text.replace_range(start..end, &word);
        self.cmdline.cursor = start + word.len();
        true
    }

    /// ファインダーと同じプロジェクトの根
    pub(crate) fn finder_root(&self) -> std::path::PathBuf {
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
}



/// この部品の入口（component.rs）
pub struct CommandLineComponent;

impl crate::component::Component for CommandLineComponent {
    fn key(&self, ed: &mut crate::editor::Editor, key: nagi_vim::Key) -> crate::editor::Effect {
        ed.command(key)
    }

    fn text(&self, ed: &mut crate::editor::Editor, text: &str) {
        ed.cmdline.insert(text);
        ed.update_cmd_complete();
    }

    fn click(&self, ed: &mut crate::editor::Editor, row: usize, col: usize, clicks: u32) -> bool {
        ed.cmd_complete_click(row, col, clicks)
    }
}


/// コマンドの候補（layout.rs の View）
pub struct CmdCompleteView;

impl crate::layout::View for CmdCompleteView {
    fn rect(&self, ed: &crate::editor::Editor, s: crate::layout::Screen) -> Option<crate::panes::Rect> {
        let _ = s;
        (ed.cmd_complete.is_some() && ed.state() == crate::editor::State::Command).then_some(crate::panes::Rect { x: 0, y: 0, w: s.cols, h: s.rows })
    }

    fn draw(&self, ed: &crate::editor::Editor, p: &mut crate::view::Painter, r: crate::panes::Rect, s: crate::layout::Screen) {
        let _ = (r, s);
        if let Some(c) = &ed.cmd_complete {
            draw::draw_cmd_complete(p, ed, c, s.rows, s.cols);
        }
    }
}

/// 描き方（view.rs の Painter で描く）
pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    /// `:` のコマンドラインの候補（最下行のすぐ上に最大 10 件。補う語の位置から）
    pub(crate) fn draw_cmd_complete(p: &mut Painter, ed: &Editor, c: &crate::cmdline_complete::CmdComplete, rows: usize, cols: usize) {
        let max = crate::cmdline_complete::MAX_SHOWN.min(rows);
        let cell = p.fonts.cell();
        let n = c.items.len().min(max);
        let first = c.sel.map_or(0, |s| s.saturating_sub(max - 1));
        let shown: Vec<&(String, String)> = c.items.iter().skip(first).take(n).collect();
        let name_w = shown.iter().map(|(a, _)| p.width(a)).max().unwrap_or(1);
        let desc_w = shown.iter().map(|(_, d)| p.width(d)).max().unwrap_or(0);
        let width = (name_w + desc_w + if desc_w > 0 { 5 } else { 2 }).min(cols);
        let col = (1 + p.width(&ed.cmdline.text[..c.start.min(ed.cmdline.text.len())])).min(cols.saturating_sub(width));
        let top = rows - n;
        let mut hits = Vec::new();
        for (k, (name, desc)) in shown.iter().enumerate() {
            hits.push((top + k, col, col + width, first + k));
            let y = (top + k) as f32 * cell.h;
            let selected = c.sel == Some(first + k);
            let (bg, fg, dim) = if selected { (FG, BG, BG) } else { (MATCH_BG, FG, STATUS_FG) };
            p.out.push(Instance::fill(col as f32 * cell.w, y, width as f32 * cell.w, cell.h, bg));
            let end = p.text(&format!(" {name}"), col, y, fg);
            if !desc.is_empty() {
                p.text(desc, end.max(col + name_w + 1) + 2, y, dim);
            }
        }
        c.rows.set(hits);
        // 件数が多いときは、残りの数を一覧の右上に
        if c.items.len() > n {
            let more = format!(" {}/{} ", c.sel.map_or(0, |s| s + 1), c.items.len());
            let x = (col + width).saturating_sub(p.width(&more));
            p.text(&more, x, (top as f32 - 1.0).max(0.0) * cell.h, STATUS_FG);
        }
    }
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

    #[test]
    fn candidates_and_tab() {
        let mut e = Editor::new(Buffer::from_str("x\n"), None);
        keys(&mut e, ":");
        assert!(e.cmd_complete.as_ref().unwrap().items.len() > 20, "打つ前から全部出す");
        keys(&mut e, "sp");
        assert!(e.cmd_complete.is_none(), "sp と完全に同じ名前だけなら出さない");
        keys(&mut e, "<BS><BS>no");
        let names: Vec<&str> = e.cmd_complete.as_ref().unwrap().items.iter().map(|i| i.0.as_str()).collect();
        assert_eq!(names, vec!["normal", "noh"]);
        // Tab で入れて Enter で実行
        keys(&mut e, "<Tab><Tab>");
        assert_eq!(e.cmdline.text, "noh");
        keys(&mut e, "<Tab>");
        assert_eq!(e.cmdline.text, "no", "端の先は打った語");
        keys(&mut e, "<C-p><CR>");
        assert_eq!(e.state(), crate::editor::State::Normal);
        // 範囲の後ろでもコマンド名を出す
        keys(&mut e, ":%no");
        assert_eq!(e.cmd_complete.as_ref().unwrap().items.len(), 2);
        keys(&mut e, "<Esc>");
        assert!(e.cmd_complete.is_none());
    }

    #[test]
    fn ignore_case_and_ctrl_space() {
        let names = |e: &Editor| e.cmd_complete.as_ref().map(|c| c.items.iter().map(|i| i.0.clone()).collect::<Vec<_>>()).unwrap_or_default();
        let mut e = Editor::new(Buffer::from_str("x\n"), None);
        // 小文字でも大文字のコマンドを出す。頭が合うものが先、途中に含むものが後
        keys(&mut e, ":gbl");
        assert_eq!(names(&e), vec!["Gblame"]);
        keys(&mut e, "<C-u>blame");
        assert_eq!(names(&e), vec!["Gblame"], "途中に含むものも出す");
        keys(&mut e, "<C-u>GIT");
        assert!(names(&e).starts_with(&["Git".to_string(), "GitHunk".into(), "GitStage".into(), "GitRevert".into()]), "{:?}", names(&e));
        assert_eq!(crate::commands::find("git").map(|c| c.0.names[0]), Some("Git"));
        // 日本語なら説明文から
        keys(&mut e, "<C-u>ブランチ");
        assert!(names(&e).contains(&"Gbranches".to_string()));
        // 打ち切った名前と同じ 1 件は出さないが、Ctrl+Space なら出す
        keys(&mut e, "<C-u>gblame");
        assert!(e.cmd_complete.is_none());
        keys(&mut e, "<C-Space>");
        assert_eq!(names(&e), vec!["Gblame"]);
        // 小文字のまま実行できる
        keys(&mut e, "<CR>");
        assert!(!e.notify.text.contains("E492"), "{}", e.notify.text);
        assert_eq!(crate::commands::find("gblame").map(|c| c.0.names[0]), Some("Gblame"));
        assert_eq!(crate::commands::find("w").map(|c| c.0.names[0]), Some("w"));
        // Insert の Ctrl+Space は単語の候補
        let mut e = Editor::new(Buffer::from_str("alpha\nal"), None);
        keys(&mut e, "GA<C-Space>");
        assert!(e.completion.is_some());
    }

    /// ↑↓・Ctrl-j/k でも選べ、マウスで押した候補も入る
    #[test]
    fn arrows_and_click() {
        let mut e = Editor::new(nagi_core::Buffer::from_str(""), None);
        e.set_screen(100, 40);
        e.enter_command("Ru");
        e.key(Key::Down);
        let first = e.cmdline.text.clone();
        assert!(first.starts_with("Run"), "{first}");
        e.key(Key::Ctrl('j'));
        assert_ne!(e.cmdline.text, first);
        e.key(Key::Up);
        assert_eq!(e.cmdline.text, first);
        // 押す（描いた位置の代わりに入れる）
        let c = e.cmd_complete.as_ref().unwrap();
        let last = c.items.len() - 1;
        let want = c.items[last].0.clone();
        c.rows.set(vec![(30, 1, 40, last)]);
        assert!(e.cmd_complete_click(30, 5, 1));
        assert_eq!(e.cmdline.text, want);
        assert!(!e.cmd_complete_click(29, 5, 1));
    }
}
