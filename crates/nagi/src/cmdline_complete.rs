//! `:` のコマンドラインの候補（ターミナルの Vim の wildmenu と同じ形）。
//!
//! 打つたびに最下行の上へ候補を説明付きで出す。コマンド名を打っている間はコマンドの一覧を前方一致で、
//! ファイル名を取るコマンド（`:e` `:w` `:sp` `:vs` `:saveas`）の後ろではプロジェクトのファイルをファジーに、
//! `:b` の後ろでは開いているバッファを出す。Tab と `Ctrl-n` で次、`Ctrl-p` で前の候補を入力欄へ入れる。

use nagi_core::fuzzy::{Query, rank_ids};
use nagi_vim::Key;

use crate::editor::Editor;

/// コマンドと説明（候補の一覧に出す順）
const EX_COMMANDS: &[(&str, &str)] = &[
    ("w", "保存（:w 名前 で名前を付けて）"),
    ("wa", "すべて保存"),
    ("q", "閉じる"),
    ("q!", "保存せずに閉じる"),
    ("qa", "すべて閉じて終了"),
    ("wq", "保存して閉じる"),
    ("x", "変更があれば保存して閉じる"),
    ("e", "ファイルを開く"),
    ("e!", "読み直す（変更を捨てる）"),
    ("saveas", "名前を付けて保存（名前ごと変える）"),
    ("b", "バッファへ（番号か名前）"),
    ("bn", "次のバッファ"),
    ("bp", "前のバッファ"),
    ("bd", "バッファを閉じる"),
    ("ls", "バッファの一覧"),
    ("sp", "上下に分割"),
    ("vs", "左右に分割"),
    ("new", "新しいバッファを上下に"),
    ("vnew", "新しいバッファを左右に"),
    ("only", "ほかの窓を閉じる"),
    ("close", "窓を閉じる"),
    ("s", "置換（:s/検索/置換/フラグ）"),
    ("g", "当たる行ごとに実行（:g/検索/コマンド）"),
    ("v", "当たらない行ごとに実行"),
    ("normal", "ノーマルモードのキーを流す"),
    ("d", "行を消す"),
    ("y", "行を取る"),
    ("m", "行を移す"),
    ("t", "行を写す"),
    ("j", "行をつなぐ"),
    ("noh", "検索の強調を消す"),
    ("marks", "マークの一覧"),
    ("delmarks", "マークを消す"),
    ("earlier", "時間をさかのぼって戻す"),
    ("later", "時間を進める"),
    ("Files", "ファイルを開く（ファインダー）"),
    ("Buffers", "バッファを選ぶ（ファインダー）"),
    ("Recent", "最近のファイル"),
    ("Commands", "コマンドパレット"),
    ("Grep", "全文検索"),
    ("Tree", "ファイルツリー"),
    ("Symbols", "シンボル検索"),
    ("Types", "型の検索"),
    ("Outline", "ファイルの構造"),
    ("Refs", "参照を探す"),
    ("Make", "診断のコマンドを動かす"),
    ("eslint", "eslint の診断"),
    ("cn", "次の結果"),
    ("cp", "前の結果"),
    ("copen", "結果のパネルへ"),
    ("cclose", "結果のパネルを閉じる"),
    ("Idea", "IntelliJ で開く"),
    ("source", "設定（ideavimrc）を読み直す"),
    ("NagiStats", "入力の遅延の計測"),
    ("Git", "コミット（左の欄のコミットのタブ）"),
    ("Gcommit", "コミット（変更の一覧とメッセージ）"),
    ("Gdiff", "今のファイルと HEAD の差分"),
    ("Gblame", "行ごとのコミット（出す・消す）"),
    ("Glog", "グラフ付きの履歴"),
    ("Ghistory", "今のファイルの履歴"),
    ("Gbranches", "ブランチ"),
    ("Gbranch", "ブランチを作る（名前 [起点]）"),
    ("Gpush", "push（送る前に確かめる）"),
    ("Gpull", "pull"),
    ("Gfetch", "フェッチ"),
    ("Maven", "Maven（右の欄）。後にゴールを書けば実行"),
    ("Terminal", "ターミナル（下の欄。Alt+F12）"),
    ("Problems", "問題（下の欄。Alt+6）"),
    ("Output", "実行の出力（下の欄のログ。Alt+4）"),
    ("TreeSync", "プロジェクトの木を開いているファイルに合わせる・合わせない（切り替え）"),
    ("MarkdownPreview", "Markdown のプレビュー（右の欄）を出す・閉じる"),
    ("Project", "プロジェクトを開く（最近のプロジェクト・IntelliJ・Eclipse）。後にフォルダを書けばそこを開く"),
    ("Db", "データベース（右の欄）"),
    ("DbAdd", "接続先を足す（名前 jdbc:URL [利用者]）"),
    ("DbRun", "SQL コンソールでカーソルのある文を実行"),
    ("DbSchema", "スキーマを切り替える"),
    ("DbExport", "結果を書き出す（csv / tsv / insert）"),
    ("DbCommit", "コミット"),
    ("DbRollback", "ロールバック"),
    ("DbTx", "トランザクションの自動・手動を切り替える"),
    ("DbLogout", "Salesforce の保存したログインを消す"),
    ("DbNew", "新しいクエリを開く（Ctrl+Enter で実行）"),
    ("GitHunk", "カーソルの変更の差分"),
    ("GitStage", "カーソルの変更をステージ"),
    ("GitRevert", "カーソルの変更を戻す"),
];

/// 打った名前に合うコマンド（大文字小文字は区別しない）。頭が合うものを先に、途中に含むものを後に。
/// 日本語を打ったら説明文からも探す。打った名前とまったく同じ 1 件だけなら（`force` でなければ）出さない
pub fn match_commands(body: &str, force: bool) -> Vec<(String, String)> {
    let low = body.to_lowercase();
    let mut head = Vec::new();
    let mut mid = Vec::new();
    for (c, d) in EX_COMMANDS {
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

/// 実行するコマンド名: 知っている名前と大文字小文字だけ違うなら、その名前に直す（:gblame → Gblame）
pub fn canonical_cmd(name: &str) -> &str {
    if name.is_empty() || EX_COMMANDS.iter().any(|(c, _)| *c == name) {
        return name;
    }
    let mut hits = EX_COMMANDS.iter().filter(|(c, _)| c.eq_ignore_ascii_case(name));
    match (hits.next(), hits.next()) {
        (Some((c, _)), None) => c,
        _ => name,
    }
}

/// ファイル名を取るコマンド
const FILE_ARG: &[&str] = &["e", "edit", "w", "write", "w!", "sp", "split", "vs", "vsplit", "saveas", "sav", "tabe"];

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
}

impl Editor {
    /// 入力欄が変わったら候補を作り直す
    /// コマンドラインに入る（パネルの中の `:` もここを通して、候補を出す）
    pub(crate) fn enter_command(&mut self, initial: &str) {
        self.mode = crate::editor::Mode::Command;
        self.cmdline = Default::default();
        self.cmdline.set(initial);
        self.pending_keys.clear();
        self.update_cmd_complete();
    }

    pub(crate) fn update_cmd_complete(&mut self) {
        let text = self.cmdline.text[..self.cmdline.cursor].to_string();
        // ファイル名の候補が要るのに一覧がまだ無ければ、裏で集め始める（届いたら poll_cmd_files で出し直す）
        let wants_files = text.split_once(' ').is_some_and(|(n, _)| FILE_ARG.contains(&n.trim_start_matches(|c: char| !c.is_ascii_alphabetic())));
        let root = self.finder_root();
        if wants_files && !self.file_cache.contains_key(&root) && self.cmd_files.is_none() {
            let waker = self.waker.clone().unwrap_or_else(|| std::sync::Arc::new(|| {}));
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
        if self.mode == crate::editor::Mode::Command {
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
                (!items.is_empty()).then(|| CmdComplete { items, sel: None, start: body_at, original: body.to_string() })
            }
            Some((name, arg)) => {
                let start = body_at + name.len() + 1;
                // スキーマ・書き出しの形
                let fixed: Option<Vec<String>> = match canonical_cmd(name) {
                    "DbSchema" => Some(self.db_schema_names()),
                    "DbExport" => Some(vec!["csv".into(), "tsv".into(), "insert".into()]),
                    _ => None,
                };
                if let Some(list) = fixed {
                    let low = arg.to_lowercase();
                    let items: Vec<(String, String)> = list.into_iter().filter(|s| s.to_lowercase().starts_with(&low)).map(|s| (s, String::new())).collect();
                    return (!items.is_empty()).then(|| CmdComplete { items, sel: None, start, original: arg.to_string() });
                }
                if matches!(name, "b" | "buffer" | "bd" | "bdelete") {
                    let low = arg.to_lowercase();
                    let items: Vec<(String, String)> = self
                        .buffer_list()
                        .into_iter()
                        .filter_map(|(i, p, dirty, _)| {
                            let n = p?.file_name()?.to_string_lossy().into_owned();
                            n.to_lowercase().contains(&low).then(|| (n, format!("バッファ {}{}", i + 1, if dirty { " [+]" } else { "" })))
                        })
                        .collect();
                    return (!items.is_empty()).then(|| CmdComplete { items, sel: None, start, original: arg.to_string() });
                }
                if FILE_ARG.contains(&name) {
                    // プロジェクトのファイル（ファインダーで集めた一覧）をファジーに。まだ無ければ裏で集める
                    let root = self.finder_root();
                    let files = self.file_cache.get(&root)?;
                    let q = Query::new(arg);
                    let ids: Vec<u32> = (0..files.len() as u32).collect();
                    let hits = rank_ids(&q, files, &ids);
                    let items: Vec<(String, String)> = hits.iter().take(200).map(|&(i, _)| (files[i as usize].clone(), String::new())).collect();
                    return (!items.is_empty()).then(|| CmdComplete { items, sel: None, start, original: arg.to_string() });
                }
                None
            }
        }
    }

    /// Tab / Ctrl-n / Ctrl-p。扱ったら true
    pub(crate) fn cmd_complete_key(&mut self, key: Key) -> bool {
        let forward = match key {
            Key::Tab | Key::Ctrl('n') => true,
            Key::Ctrl('p') => false,
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
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        std::path::absolute(&root).unwrap_or(root)
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
        assert_eq!(e.mode, crate::editor::Mode::Normal);
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
        assert_eq!(canonical_cmd("git"), "Git");
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
        assert!(!e.message.contains("E492"), "{}", e.message);
        assert_eq!(canonical_cmd("gblame"), "Gblame");
        assert_eq!(canonical_cmd("w"), "w");
        // Insert の Ctrl+Space は単語の候補
        let mut e = Editor::new(Buffer::from_str("alpha\nal"), None);
        keys(&mut e, "GA<C-Space>");
        assert!(e.completion.is_some());
    }
}
