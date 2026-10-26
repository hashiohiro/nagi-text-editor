//! `:` のコマンドの表（ADR 0016 の 5）。
//!
//! コマンドは `COMMANDS` に 1 行ずつ登録する（名前と別名・説明・引数の種類・処理）。`:` の実行（Editor::ex）、
//! 候補（cmdline_complete）、コマンドパレット（finder の Commands）、大文字小文字の直し（`:gblame` → `Gblame`）は
//! みなこの表を引く。新しいコマンドは、ここに 1 行と、処理（Editor のメソッド）を足すだけでよい。
//!
//! 名前の後ろの `!`（`:q!` `:w!` `:e!` など）は `Call::bang` になる。範囲を取るコマンド（`:s` `:g` `:normal` など）は
//! Editor::ex_ranged が先に受け取る（ここには候補と説明のために載せる）。

use std::path::PathBuf;

use crate::editor::{Editor, Effect};

/// 引数の種類（候補の出し方）
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Args {
    None,
    /// 任意の文字
    Text,
    /// プロジェクトのファイル
    File,
    /// 開いているバッファ
    Buffer,
    /// データベースのスキーマ
    Schema,
    /// 決まった語のどれか
    Choice(&'static [&'static str]),
}

/// 呼び出し（名前は打ったとおり、`!` を除いたもの）
pub struct Call<'a> {
    pub name: &'a str,
    pub arg: &'a str,
    pub bang: bool,
    /// 打った行全体（範囲を取るコマンドへ渡す）
    pub line: &'a str,
}

pub struct Cmd {
    /// 名前と別名（最初のものを候補に出す）
    pub names: &'static [&'static str],
    /// 説明（空なら候補とパレットに出さない）
    pub help: &'static str,
    pub args: Args,
    pub run: fn(&mut Editor, &Call) -> Effect,
}

/// 何もしない処理の結果
const NONE: Effect = Effect::None;

macro_rules! cmd {
    ($names:expr, $help:expr, $args:expr, |$e:ident, $c:ident| $body:expr) => {
        Cmd {
            names: &$names,
            help: $help,
            args: $args,
            run: |$e, $c| {
                let _ = &$c;
                $body;
                NONE
            },
        }
    };
}

/// 処理が Effect を返すもの
macro_rules! cmd_effect {
    ($names:expr, $help:expr, $args:expr, |$e:ident, $c:ident| $body:expr) => {
        Cmd {
            names: &$names,
            help: $help,
            args: $args,
            run: |$e, $c| {
                let _ = (&$e, &$c);
                $body
            },
        }
    };
}

/// コマンドの表（候補に出す順）
pub static COMMANDS: &[Cmd] = &[
    // ファイルと窓（Vim）
    cmd_effect!(["w", "write"], "保存（:w 名前 で名前を付けて）", Args::File, |e, c| write(e, c)),
    cmd!(["wa", "wall"], "すべて保存", Args::None, |e, c| e.write_all()),
    cmd_effect!(["q", "quit"], "バッファを閉じる（nagi は終わらない。終わるのは :qa）", Args::None, |e, c| quit(e, c, false)),
    cmd_effect!(["qa", "qall", "quitall"], "すべて閉じて終了", Args::None, |e, c| quit(e, c, true)),
    cmd_effect!(["wq", "x", "xit"], "保存してバッファを閉じる", Args::File, |e, c| write_quit(e, c)),
    cmd_effect!(["wqa", "wqall", "xa", "xall"], "すべて保存して終了", Args::None, |e, c| if e.write_all() { Effect::Quit } else { NONE }),
    cmd!(["e", "edit"], "ファイルを開く（:e! は読み直す）", Args::File, |e, c| edit(e, c)),
    cmd_effect!(["saveas", "sav"], "名前を付けて保存（名前ごと変える）", Args::File, |e, c| {
        if c.arg.is_empty() {
            Effect::SaveDialog
        } else {
            e.write_as(c.arg, c.bang, true);
            NONE
        }
    }),
    cmd_effect!(["browse"], "", Args::Text, |e, c| {
        if c.arg.starts_with('w') || c.arg.starts_with("sav") {
            Effect::SaveDialog
        } else {
            unknown(e, c.line);
            NONE
        }
    }),
    cmd!(["b", "buffer"], "バッファへ（番号か名前）", Args::Buffer, |e, c| if c.arg.is_empty() { unknown(e, c.line) } else { e.goto_buffer(c.arg) }),
    cmd!(["bn", "bnext"], "次のバッファ", Args::None, |e, c| e.cycle_buffer(true, c.arg.parse().unwrap_or(1))),
    cmd!(["bp", "bprevious", "bN", "bNext"], "前のバッファ", Args::None, |e, c| e.cycle_buffer(false, c.arg.parse().unwrap_or(1))),
    cmd!(["bd", "bdelete", "bw", "bwipeout"], "バッファを閉じる", Args::Buffer, |e, c| e.delete_buffer(c.arg, c.bang)),
    cmd!(["ls", "buffers", "files"], "バッファの一覧", Args::None, |e, c| e.list_buffers()),
    cmd!(["sp", "split"], "上下に分割", Args::File, |e, c| split(e, c, false)),
    cmd!(["vs", "vsplit"], "左右に分割", Args::File, |e, c| split(e, c, true)),
    cmd!(["new"], "新しいバッファを上下に", Args::None, |e, c| {
        e.split(false);
        e.new_empty_buffer()
    }),
    cmd!(["vnew", "vne"], "新しいバッファを左右に", Args::None, |e, c| {
        e.split(true);
        e.new_empty_buffer()
    }),
    cmd!(["only", "on"], "ほかの窓を閉じる", Args::None, |e, c| e.only_window()),
    cmd!(["close", "clo"], "窓を閉じる", Args::None, |e, c| if e.is_split() { e.close_window(e.cur_win) } else { e.notify.error("E444: 最後のウィンドウは閉じられません") }),
    // 範囲を取るもの（Editor::ex_ranged が受け取る。ここは候補と説明）
    cmd_effect!(["s", "substitute"], "置換（:s/検索/置換/フラグ）", Args::Text, |e, c| ranged(e, c)),
    cmd_effect!(["g", "global"], "当たる行ごとに実行（:g/検索/コマンド）", Args::Text, |e, c| ranged(e, c)),
    cmd_effect!(["v", "vglobal"], "当たらない行ごとに実行", Args::Text, |e, c| ranged(e, c)),
    cmd_effect!(["normal", "norm"], "ノーマルモードのキーを流す", Args::Text, |e, c| ranged(e, c)),
    cmd_effect!(["d", "delete"], "行を消す", Args::Text, |e, c| ranged(e, c)),
    cmd_effect!(["y", "yank"], "行を取る", Args::Text, |e, c| ranged(e, c)),
    cmd_effect!(["m", "move"], "行を移す", Args::Text, |e, c| ranged(e, c)),
    cmd_effect!(["t", "copy", "co"], "行を写す", Args::Text, |e, c| ranged(e, c)),
    cmd_effect!(["j", "join"], "行をつなぐ", Args::Text, |e, c| ranged(e, c)),
    // 検索・マーク・取り消し（Vim）
    cmd!(["set", "se", "setlocal", "setl"], "オプション（:set ts=4・:set ts?・:set だけで変えたものの一覧。この窓だけ）", Args::Text, |e, c| e.ex_set(c.arg)),
    cmd!(["noh", "nohlsearch"], "検索の強調を消す", Args::None, |e, c| e.search.highlight = false),
    cmd!(["marks"], "マークの一覧", Args::None, |e, c| e.list_marks()),
    cmd!(["delmarks", "delm"], "マークを消す", Args::Text, |e, c| e.delete_marks(c.arg, c.bang)),
    cmd!(["earlier", "ea"], "時間をさかのぼって戻す", Args::Text, |e, c| e.undo_time(false, c.arg.parse().unwrap_or(1))),
    cmd!(["later", "lat"], "時間を進める", Args::Text, |e, c| e.undo_time(true, c.arg.parse().unwrap_or(1))),
    cmd!(["echo"], "", Args::Text, |e, c| e.notify.info(c.arg.trim_matches(['"', '\'']).to_string())),
    // ideavimrc の `O`: `:call append(expand('.'), '')`（現在行の下に空行を足す）
    cmd!(["call"], "", Args::Text, |e, c| if c.arg.starts_with("append(") { e.append_blank_line() } else { unknown(e, c.line) }),
    cmd!(["action"], "", Args::Text, |e, c| e.action(c.arg)),
    cmd_effect!(["source", "so"], "設定（ideavimrc）を読み直す", Args::None, |e, c| Effect::ReloadRc),
    // ファインダー・検索
    cmd!(["Files"], "ファイルを開く（ファインダー）", Args::None, |e, c| e.open_finder(crate::finder::Source::Files)),
    cmd!(["Buffers"], "バッファを選ぶ（ファインダー）", Args::None, |e, c| e.open_finder(crate::finder::Source::Buffers)),
    cmd!(["Recent", "History"], "最近のファイル", Args::None, |e, c| e.open_finder(crate::finder::Source::Recent)),
    cmd!(["SearchEverywhere", "Everywhere"], "どこでも検索（型・シンボル・ファイル・コマンドをまとめて。Shift 2 回）", Args::None, |e, c| e.open_finder(crate::finder::Source::Everywhere)),
    cmd!(["Commands", "Palette"], "コマンドパレット", Args::None, |e, c| e.open_finder(crate::finder::Source::Commands)),
    // 普通のエディタの編集（Vim を使わないときのキーマップ・右クリックのメニューから。plain.rs）
    cmd!(["Undo"], "元に戻す", Args::None, |e, c| e.plain_undo(false)),
    cmd!(["Redo"], "やり直す", Args::None, |e, c| e.plain_undo(true)),
    cmd!(["Copy"], "コピー（選んでいなければ行）", Args::None, |e, c| e.plain_copy(false)),
    cmd!(["Cut"], "切り取り（選んでいなければ行）", Args::None, |e, c| e.plain_copy(true)),
    cmd!(["Paste"], "貼り付け", Args::None, |e, c| e.plain_paste()),
    cmd!(["SelectAll"], "すべて選ぶ", Args::None, |e, c| e.plain_select_all()),
    cmd!(["DuplicateLine"], "行の複製", Args::None, |e, c| e.plain_duplicate()),
    cmd!(["DeleteLine"], "行の削除", Args::None, |e, c| e.plain_delete_lines()),
    cmd!(["CommentLine"], "行コメントを付ける・外す", Args::None, |e, c| e.plain_comment()),
    cmd!(["GotoLine"], "行へ移動（番号を打つ）", Args::None, |e, c| e.enter_command("")),
    cmd!(["Grep", "grep", "Rg", "rg"], "全文検索", Args::Text, |e, c| e.open_grep(c.arg)),
    cmd!(["Tree", "Explore", "Ex", "NERDTreeToggle"], "ファイルツリー", Args::None, |e, c| e.toggle_tree()),
    cmd!(["TreeSync"], "プロジェクトの木を開いているファイルに合わせる・合わせない（切り替え）", Args::None, |e, c| {
        e.tree_sync = !e.tree_sync;
        e.notify.info(if e.tree_sync { "木を開いているファイルに合わせます" } else { "木を開いているファイルに合わせません" });
        e.sync_tree()
    }),
    cmd!(["Symbols"], "シンボル検索（Ctrl+Alt+Shift+N）", Args::None, |e, c| e.symbol_search(false)),
    cmd!(["Types", "Classes"], "型の検索", Args::None, |e, c| e.symbol_search(true)),
    cmd!(["Outline"], "ファイルの構造（Ctrl+F12・Alt+7）", Args::None, |e, c| e.outline()),
    cmd!(["Refs", "References"], "参照を探す（Alt+F7）", Args::None, |e, c| e.find_references()),
    // 診断・結果
    cmd!(["Make", "make"], "診断のコマンドを動かす", Args::Text, |e, c| e.make(c.arg)),
    cmd!(["eslint", "Lint"], "eslint の診断", Args::None, |e, c| e.make("npx eslint -f unix .")),
    cmd!(["cn", "cnext"], "次の結果", Args::None, |e, c| e.next_hit(true, c.arg.parse().unwrap_or(1))),
    cmd!(["cp", "cprevious", "cN", "cNext"], "前の結果", Args::None, |e, c| e.next_hit(false, c.arg.parse().unwrap_or(1))),
    cmd!(["copen", "cope"], "結果のパネルへ", Args::None, |e, c| e.focus_panel()),
    cmd!(["cclose", "ccl"], "結果のパネルを閉じる", Args::None, |e, c| e.close_grep()),
    cmd!(["Problems"], "問題（下の欄。Alt+6）", Args::None, |e, c| e.select_bottom_tab(crate::result_grid::BottomKind::Problems)),
    cmd!(["Output"], "実行の出力（下の欄のログ。Alt+4）", Args::None, |e, c| e.select_bottom_tab(crate::result_grid::BottomKind::Output)),
    cmd!(["messages", "mes", "Messages"], "知らせの履歴", Args::None, |e, c| e.show_messages()),
    // プロジェクト・外の道具
    cmd!(["Project", "project"], "プロジェクトを開く（最近のプロジェクト・IntelliJ・Eclipse）。後にフォルダを書けばそこを開く", Args::Text, |e, c| {
        if c.arg.is_empty() { e.pick_project() } else { e.open_project(PathBuf::from(c.arg)) }
    }),
    cmd!(["Idea", "IntelliJ"], "IntelliJ で開く", Args::None, |e, c| e.open_in_intellij()),
    cmd_effect!(["NagiStats"], "入力の遅延の計測（:NagiStats! で消す）", Args::None, |e, c| Effect::Stats { reset: c.bang }),
    cmd!(["Settings", "Preferences"], "設定（Ctrl+Alt+S）。後に語を書けばその項目を探す", Args::Text, |e, c| e.open_settings(c.arg)),
    cmd!(["GrammarInstall"], "文法をダウンロード（名前か all）", Args::Text, |e, c| e.grammar_install_command(c.arg)),
    cmd!(["Licenses", "License"], "使っているもののライセンス", Args::None, |e, c| e.open_licenses()),
    // 実行の構成・ターミナル
    cmd!(["Run"], "実行の構成を動かす（Shift+F10）。後に名前を書けばその構成（IntelliJ の構成と .nagi/run.conf）", Args::Text, |e, c| e.run_command(c.arg)),
    cmd!(["RunConfigs"], "実行の構成を選んで動かす（Alt+Shift+F10）", Args::None, |e, c| e.open_run_chooser()),
    cmd!(["EasyWord"], "EasyMotion: 後ろの単語へ跳ぶ（印を打つ）", Args::None, |e, c| e.start_easy(crate::easy::EasyKind::WordForward)),
    cmd!(["EasyLineDown"], "EasyMotion: 下の行へ跳ぶ", Args::None, |e, c| e.start_easy(crate::easy::EasyKind::LineForward)),
    cmd!(["EasyLineUp"], "EasyMotion: 上の行へ跳ぶ", Args::None, |e, c| e.start_easy(crate::easy::EasyKind::LineBackward)),
    cmd!(["EasyChar"], "EasyMotion: 打った 1 文字へ跳ぶ", Args::None, |e, c| e.start_easy(crate::easy::EasyKind::FindChar)),
    cmd!(["EasySearch"], "EasyMotion: 打った文字列へ跳ぶ（Enter で印）", Args::None, |e, c| e.start_easy(crate::easy::EasyKind::Search)),
    cmd!(["Debug"], "デバッグ（選んでいる実行の構成を JDWP の口を開けて動かし、つなぐ。Shift+F9。後に名前を書けばその構成）", Args::Text, |e, c| e.debug_run(c.arg)),
    cmd!(["DebugTest"], "カーソルの位置のテストをデバッグ（Maven）", Args::None, |e, c| e.debug_test()),
    cmd!(["DebugAttach"], "動いている JVM につなぐ（:DebugAttach 5005・ホスト:ポート）", Args::Text, |e, c| e.debug_attach(c.arg)),
    cmd!(["DebugBrowser"], "ブラウザ（Chrome・Edge）をデバッグの口を開けて起こし、TypeScript・JavaScript をデバッグ（後に URL。無ければ開発サーバー）", Args::Text, |e, c| e.debug_browser(c.arg)),
    cmd!(["DebugAttachBrowser"], "--remote-debugging-port で起こしたブラウザにつなぐ（:DebugAttachBrowser 9222）", Args::Text, |e, c| e.debug_attach_browser(c.arg)),
    cmd!(["DebugAttachPython"], "debugpy につなぐ（python -m debugpy --listen 5678 …。:DebugAttachPython 5678）", Args::Text, |e, c| e.debug_attach_python(c.arg)),
    cmd!(["Breakpoint", "Break"], "ブレークポイントを付ける・外す（F9）", Args::None, |e, c| e.toggle_breakpoint(None)),
    cmd!(["DebugResume"], "デバッグ: 続ける（F5。デバッグしていなければ始める）", Args::None, |e, c| e.debug_resume()),
    cmd!(["StepOver"], "デバッグ: 次へ（F10）", Args::None, |e, c| e.debug_step(nagi_workers::jdwp::Depth::Over)),
    cmd!(["StepInto"], "デバッグ: 中へ（F11）", Args::None, |e, c| e.debug_step(nagi_workers::jdwp::Depth::Into)),
    cmd!(["StepOut"], "デバッグ: 外へ（Shift+F11）", Args::None, |e, c| e.debug_step(nagi_workers::jdwp::Depth::Out)),
    cmd!(["DebugStop"], "デバッグを止める（Shift+F5）", Args::None, |e, c| e.debug_stop()),
    cmd!(["ToolDebug"], "デバッグの欄を出す・隠す（Alt+5）", Args::None, |e, c| e.toggle_bottom_tab(crate::result_grid::BottomKind::Debug)),
    cmd!(["Format", "Reformat"], "整形（Ctrl+Alt+L。設定の「整形のコマンド」。google-java-format・prettier・rustfmt など）", Args::None, |e, c| e.format_buffer()),
    cmd!(["RunTest"], "カーソルの位置のテストを実行（メソッドの中ならそのメソッド、外ならクラス。Maven・Gradle・Cargo・pytest）", Args::None, |e, c| e.run_test_at_cursor()),
    cmd!(["ReopenClosed"], "最後に閉じたファイルを開き直す", Args::None, |e, c| e.reopen_closed()),
    cmd!(["RecentLocations", "Changes"], "最近の編集箇所", Args::None, |e, c| e.recent_locations()),
    cmd!(["RunStop"], "動いている構成を止める（Ctrl+F2）", Args::None, |e, c| e.run_stop()),
    cmd!(["RunEdit"], "実行の構成の画面（追加・編集・削除。後に名前を書けばその構成を選んで）。:RunEdit! はファイルを開く", Args::Text, |e, c| {
        if c.bang { e.run_edit() } else { e.open_run_dialog((!c.arg.is_empty()).then_some(c.arg)) }
    }),
    cmd!(["RunAdd"], "実行の構成を足す（:RunAdd 名前 = コマンド）", Args::Text, |e, c| e.run_add(c.arg)),
    cmd!(["Terminal", "term"], "ターミナル（下の欄。Alt+F12）。後に cmd・powershell・bash・wsl を書けばそのシェルで新しい窓", Args::Text, |e, c| e.terminal_command(c.arg)),
    cmd!(["MarkdownPreview", "MdPreview"], "Markdown のプレビュー（右の欄）を出す・閉じる", Args::None, |e, c| e.toggle_md_preview()),
    cmd!(["Maven", "Mvn"], "Maven（右の欄）。後にゴールを書けば実行", Args::Text, |e, c| if c.arg.is_empty() { e.open_maven() } else { e.maven_run(c.arg) }),
    // 欄の出し入れ（IntelliJ のツールウィンドウ。キーマップの Alt+1 など）
    cmd!(["ToolProject"], "プロジェクトの欄を出す・隠す（Alt+1）", Args::None, |e, c| e.toggle_left_tab(crate::tree_panel::LeftTab::Project)),
    cmd!(["ToolCommit"], "コミットの欄を出す・隠す（Alt+0）", Args::None, |e, c| e.toggle_left_tab(crate::tree_panel::LeftTab::Commit)),
    cmd!(["ToolRun"], "実行の構成の欄を出す・隠す（Alt+8）", Args::None, |e, c| e.toggle_left_tab(crate::tree_panel::LeftTab::Run)),
    cmd!(["ToolGit"], "Git の欄を出す・隠す（Alt+9）", Args::None, |e, c| e.toggle_bottom_tab(crate::result_grid::BottomKind::Git)),
    cmd!(["ToolFind"], "検索の欄を出す・隠す（Alt+3）", Args::None, |e, c| e.toggle_bottom_tab(crate::result_grid::BottomKind::Grep)),
    cmd!(["ToolProblems"], "問題の欄を出す・隠す（Alt+6）", Args::None, |e, c| e.toggle_bottom_tab(crate::result_grid::BottomKind::Problems)),
    cmd!(["ToolLog"], "ログの欄を出す・隠す（Alt+4）", Args::None, |e, c| e.toggle_bottom_tab(crate::result_grid::BottomKind::Output)),
    cmd!(["ToolTerminal"], "ターミナルの欄を出す・隠す（Alt+F12）", Args::None, |e, c| e.toggle_bottom_tab(crate::result_grid::BottomKind::Terminal)),
    cmd!(["ToolMaven"], "Maven の欄を出す・隠す", Args::None, |e, c| e.toggle_right_tab(crate::right_panel::RightTab::Maven)),
    cmd!(["ToolDatabase"], "データベースの欄を出す・隠す", Args::None, |e, c| e.toggle_right_tab(crate::right_panel::RightTab::Database)),
    cmd!(["HideAll"], "左・右・下の欄をすべて隠す（Ctrl+Shift+F12）", Args::None, |e, c| {
        e.hide_left();
        e.hide_right();
        e.hide_bottom()
    }),
    cmd!(["SelectInTree"], "開いているファイルを木で選ぶ", Args::None, |e, c| e.select_in_tree()),
    // 移動（IntelliJ の Navigate）
    cmd!(["GotoImplementation", "Implementation"], "実装へ（Ctrl+Alt+B）", Args::None, |e, c| e.goto_implementation()),
    cmd!(["MethodNext"], "次のメソッドへ（Alt+Down）", Args::None, |e, c| e.method_jump(true)),
    cmd!(["MethodPrev"], "前のメソッドへ（Alt+Up）", Args::None, |e, c| e.method_jump(false)),
    cmd!(["JumpBack"], "前の場所へ（Ctrl+Alt+Left。Vim では Ctrl-o）", Args::None, |e, c| e.jump(true, 1)),
    cmd!(["JumpForward"], "次の場所へ（Ctrl+Alt+Right。Vim では Ctrl-i）", Args::None, |e, c| e.jump(false, 1)),
    cmd!(["JumpLastChange"], "最後に変えた場所へ（Ctrl+Shift+Backspace）", Args::None, |e, c| e.jump_last_change()),
    cmd!(["ErrorNext"], "次のエラーへ（F2）", Args::None, |e, c| e.next_error(true)),
    cmd!(["ErrorPrev"], "前のエラーへ（Shift+F2）", Args::None, |e, c| e.next_error(false)),
    cmd!(["HunkNext"], "次の変更へ（Ctrl+Alt+Shift+Down）", Args::None, |e, c| e.hunk_jump(true, 1)),
    cmd!(["HunkPrev"], "前の変更へ（Ctrl+Alt+Shift+Up）", Args::None, |e, c| e.hunk_jump(false, 1)),
    cmd!(["Find"], "ファイルの中を探す（/）", Args::None, |e, c| e.start_search(true, false, "")),
    cmd!(["Replace"], "ファイルの中を置き換える", Args::None, |e, c| e.start_search(true, true, "")),
    cmd!(["RunAgain"], "最後に動かした構成をもう一度（Ctrl+F5）", Args::None, |e, c| e.run_again()),
    cmd!(["GotoDefinition", "Definition"], "定義へ（F12）", Args::None, |e, c| e.goto_definition()),
    // git
    cmd!(["Git", "G", "Gcommit"], "コミット（変更の一覧とメッセージ）", Args::None, |e, c| e.open_commit_panel()),
    cmd!(["Gdiff", "Gd"], "今のファイルと HEAD の差分", Args::None, |e, c| e.diff_current()),
    cmd!(["Gblame", "Gb"], "行ごとのコミット（出す・消す）", Args::None, |e, c| e.toggle_blame()),
    cmd!(["Glog", "Gl"], "グラフ付きの履歴", Args::None, |e, c| e.open_log(false)),
    cmd!(["Ghistory", "Gfilelog"], "今のファイルの履歴", Args::None, |e, c| e.open_log(true)),
    cmd!(["Gbranches", "Gbr"], "ブランチ", Args::None, |e, c| e.open_branches()),
    cmd!(["Gbranch"], "ブランチを作る（名前 [起点]）", Args::Text, |e, c| e.git_new_branch(c.arg)),
    cmd!(["Grename"], "ブランチの名前を変える（今の名前 新しい名前）", Args::Text, |e, c| e.git_rename_branch(c.arg)),
    cmd!(["Gtag"], "タグを付ける（名前 [ハッシュ]）", Args::Text, |e, c| e.git_tag(c.arg)),
    cmd!(["Greword"], "コミットのメッセージを直す（ハッシュ 新しいメッセージ）", Args::Text, |e, c| e.git_reword(c.arg)),
    cmd!(["Gpush"], "push（送る前に確かめる。Ctrl+Shift+K）", Args::None, |e, c| e.push_from_anywhere()),
    cmd!(["Gpull"], "pull", Args::None, |e, c| e.run_git_job("pull ", &["pull", "--no-edit"])),
    cmd!(["Gfetch"], "フェッチ", Args::None, |e, c| e.run_git_job("フェッチ", &["fetch", "--all", "--prune"])),
    cmd!(["GitHunk"], "カーソルの変更の差分", Args::None, |e, c| e.open_hunk_popup()),
    cmd!(["GitStage"], "カーソルの変更をステージ", Args::None, |e, c| e.stage_hunk()),
    cmd!(["GitRevert"], "カーソルの変更を戻す（Ctrl+Alt+Z）", Args::None, |e, c| e.revert_hunk()),
    // データベース
    cmd!(["Db", "Database"], "データベース（右の欄）", Args::None, |e, c| e.open_db_panel()),
    cmd!(["DbAdd"], "接続先を足す（名前 jdbc:URL [利用者]）", Args::Text, |e, c| e.db_add(c.arg)),
    cmd!(["DbRun"], "SQL コンソールでカーソルのある文を実行", Args::None, |e, c| e.db_run_at_cursor()),
    cmd!(["DbSchema"], "スキーマを切り替える", Args::Schema, |e, c| e.db_set_schema(c.arg)),
    cmd!(["DbDefaultSchema"], "既定のスキーマ（つないだら切り替える。名前を省くと外す）", Args::Schema, |e, c| e.db_default_schema_command(c.arg)),
    cmd!(["DbExport"], "結果を書き出す（csv / tsv / insert）", Args::Choice(&["csv", "tsv", "insert"]), |e, c| e.db_export(c.arg)),
    cmd!(["DbCommit"], "コミット", Args::None, |e, c| e.db_tx_command("commit")),
    cmd!(["DbRollback"], "ロールバック", Args::None, |e, c| e.db_tx_command("rollback")),
    cmd!(["DbTx"], "トランザクションの自動・手動を切り替える", Args::None, |e, c| e.db_tx_command("toggle")),
    cmd!(["DbLogout"], "保存したパスワード・Salesforce のログインを消す", Args::None, |e, c| e.db_logout()),
    cmd!(["DbNew", "DbQuery"], "新しいクエリを開く（Ctrl+Enter で実行）", Args::None, |e, c| e.db_new_query_command()),
];

/// IdeaVim の `:action <IntelliJ のアクション>` の対応（nagi_vim::action が返す名前, コマンド）
pub const ACTIONS: &[(&str, &str)] = &[
    ("jump.last_change", "JumpLastChange"),
    ("search.buffer", "Find"),
    ("replace.buffer", "Replace"),
    ("finder.files", "Files"),
    ("finder.everything", "Files"),
    ("finder.recent", "Recent"),
    ("buffer.close", "bd"),
    ("search.project", "Grep"),
    ("tool.project", "Tree"),
    ("palette", "Commands"),
    ("finder.symbols", "Symbols"),
    ("finder.projects", "Project"),
    ("tool.terminal", "ToolTerminal"),
    ("tool.problems", "ToolProblems"),
    ("tool.git", "ToolGit"),
    ("tool.find", "ToolFind"),
    ("tool.output", "ToolLog"),
    ("tool.hide_all", "HideAll"),
    ("tree.select_file", "SelectInTree"),
    ("markdown.preview", "MarkdownPreview"),
    ("finder.types", "Types"),
    ("outline", "Outline"),
    ("nav.definition", "GotoDefinition"),
    ("nav.implementation", "GotoImplementation"),
    ("nav.references", "Refs"),
    ("nav.method_prev", "MethodPrev"),
    ("nav.method_next", "MethodNext"),
    ("jump.back", "JumpBack"),
    ("jump.forward", "JumpForward"),
    ("nav.error_next", "ErrorNext"),
    ("nav.error_prev", "ErrorPrev"),
    ("git.hunk_next", "HunkNext"),
    ("git.hunk_prev", "HunkPrev"),
    ("git.commit", "Git"),
    ("git.changes", "Git"),
    ("git.blame", "Gblame"),
    ("git.file_history", "Ghistory"),
    ("git.log", "Glog"),
    ("git.branches", "Gbranches"),
    ("tool.maven", "ToolMaven"),
    ("tool.database", "ToolDatabase"),
    ("db.new_query", "DbNew"),
    ("git.pull", "Gpull"),
    ("git.fetch", "Gfetch"),
    ("git.push", "Gpush"),
    ("git.hunk_popup", "GitHunk"),
    ("git.hunk_revert", "GitRevert"),
    ("jump.word", "EasyWord"),
    ("jump.line_down", "EasyLineDown"),
    ("jump.line_up", "EasyLineUp"),
    ("jump.char", "EasyChar"),
    ("jump.search", "EasySearch"),
    ("format", "Format"),
    ("debug.run", "Debug"),
    ("debug.test", "DebugTest"),
    ("debug.breakpoint", "Breakpoint"),
    ("debug.resume", "DebugResume"),
    ("debug.step_over", "StepOver"),
    ("debug.step_into", "StepInto"),
    ("debug.step_out", "StepOut"),
    ("tool.debug", "ToolDebug"),
    ("job.run", "Run"),
    ("job.stop", "RunStop"),
    ("job.test", "RunTest"),
    ("buffer.reopen", "ReopenClosed"),
    ("jump.recent", "RecentLocations"),
];

/// パレットにだけ出す行（`!` 付きなど。説明, 打つ行）
pub const PALETTE_EXTRA: &[(&str, &str)] = &[("保存せずに終了", "qa!"), ("ファイルを読み直す（変更を捨てる）", "e!"), ("入力の遅延の計測を消す", "NagiStats!")];

/// 打った名前のコマンド。`!` は外して bang に。名前と大文字小文字だけ違うものが 1 つだけならそれ（:gblame → Gblame）
pub fn find(name: &str) -> Option<(&'static Cmd, bool)> {
    let (base, bang) = match name.strip_suffix('!') {
        Some(b) if !b.is_empty() => (b, true),
        _ => (name, false),
    };
    if let Some(c) = COMMANDS.iter().find(|c| c.names.contains(&base)) {
        return Some((c, bang));
    }
    let mut hits = COMMANDS.iter().filter(|c| c.names.iter().any(|n| n.eq_ignore_ascii_case(base)));
    match (hits.next(), hits.next()) {
        (Some(c), None) => Some((c, bang)),
        _ => None,
    }
}

/// 候補に出すコマンド（名前, 説明）
pub fn listed() -> impl Iterator<Item = (&'static str, &'static str)> {
    COMMANDS.iter().filter(|c| !c.help.is_empty()).map(|c| (c.names[0], c.help))
}

/// 名前の引数の種類（候補の出し方。知らなければ None）
pub fn args_of(name: &str) -> Args {
    find(name).map_or(Args::None, |(c, _)| c.args)
}

fn unknown(e: &mut Editor, line: &str) {
    e.notify.error(format!("E492: エディタのコマンドではありません: {line}"));
}

fn ranged(e: &mut Editor, c: &Call) -> Effect {
    match e.ex_ranged(c.line) {
        Some(eff) => eff,
        None => {
            unknown(e, c.line);
            NONE
        }
    }
}

/// `:w [名前]`。名前の無いバッファはダイアログで選ぶ
fn write(e: &mut Editor, c: &Call) -> Effect {
    if !c.arg.is_empty() {
        e.write_as(c.arg, c.bang, false);
    } else if e.buf.path.is_none() {
        return Effect::SaveDialog;
    } else {
        e.write();
    }
    NONE
}

/// `:q` `:qa`。`:q` は今のバッファを閉じ（最後の 1 つなら空のバッファになる）、nagi は終えない。分割中は今の窓だけ。
/// `:qa` は全体を終える。変更が残っていれば止める（! で破棄）
fn quit(e: &mut Editor, c: &Call, all: bool) -> Effect {
    if !all && e.is_split() {
        e.close_window(e.cur_win);
        return NONE;
    }
    if !all {
        if e.buf.dirty && !c.bang {
            e.notify.error(format!("E37: 変更が保存されていません（:{}! で破棄して閉じる）", c.name));
        } else {
            e.delete_buffer("", true);
        }
        return NONE;
    }
    if !c.bang {
        if e.buf.dirty {
            e.notify.error(format!("E37: 変更が保存されていません（:{}! で破棄して終了）", c.name));
            return NONE;
        }
        if let Some(&i) = e.dirty_docs().first() {
            e.notify.error(format!("E162: バッファ {} の変更が保存されていません（:qa! で破棄して終了）", i + 1));
            return NONE;
        }
    }
    Effect::Quit
}

/// `:wq [名前]` `:x`。保存して今のバッファを閉じる（nagi は終えない）。名前があればその名前で保存してから
fn write_quit(e: &mut Editor, c: &Call) -> Effect {
    if !c.arg.is_empty() && !e.write_as(c.arg, c.bang, false) {
        return NONE;
    }
    if c.arg.is_empty() && e.buf.path.is_none() {
        return Effect::SaveDialog;
    }
    if e.is_split() {
        if e.write() {
            e.close_window(e.cur_win);
        }
        return NONE;
    }
    if e.write() {
        e.delete_buffer("", false);
    }
    NONE
}

/// `:e 名前`（フォルダならプロジェクトとして開く）・`:e!`（読み直す）
fn edit(e: &mut Editor, c: &Call) {
    // :e ++enc=sjis（今のファイルを文字コードを決めて読み直す）
    if let Some(name) = c.arg.strip_prefix("++enc=").or_else(|| c.arg.strip_prefix("++encoding=")) {
        match nagi_core::encoding::by_name(name) {
            Some(enc) => e.reload_as(enc, c.bang),
            None => e.notify.error(format!("E474: 知らない文字コード: {name}")),
        }
        return;
    }
    if c.bang {
        e.reload();
    } else if c.arg.is_empty() {
        e.notify.error("E32: ファイル名がありません");
    } else if e.resolve(c.arg).is_dir() {
        e.open_project(PathBuf::from(c.arg));
    } else {
        e.edit_file(PathBuf::from(c.arg));
    }
}

fn split(e: &mut Editor, c: &Call, vertical: bool) {
    e.split(vertical);
    if !c.arg.is_empty() {
        e.edit_file(PathBuf::from(c.arg));
    }
}

/// `:` の行を動かす（範囲を取るもの → 表のコマンド）
pub fn run(e: &mut Editor, line: &str) -> Effect {
    let line = line.trim_start_matches(':').trim();
    if let Some(eff) = e.ex_ranged(line) {
        return eff;
    }
    let (name, arg) = line.split_once(char::is_whitespace).map_or((line, ""), |(n, a)| (n, a.trim()));
    if name.is_empty() {
        return NONE;
    }
    if name.starts_with('!') {
        e.notify.info(format!("未実装: 外部コマンド（{line}）"));
        return NONE;
    }
    match find(name) {
        Some((cmd, bang)) => (cmd.run)(e, &Call { name: name.trim_end_matches('!'), arg, bang, line }),
        None => {
            unknown(e, line);
            NONE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_consistent() {
        let mut seen = std::collections::HashMap::new();
        for c in COMMANDS {
            for n in c.names {
                assert!(seen.insert(*n, c.names[0]).is_none(), ":{n} が {} と {} の両方にある", seen[n], c.names[0]);
            }
        }
        assert_eq!(find("gblame").map(|c| c.0.names[0]), Some("Gblame"));
        assert_eq!(find("q!").map(|c| (c.0.names[0], c.1)), Some(("q", true)));
        assert_eq!(args_of("e"), Args::File);
        for (id, cmd) in ACTIONS {
            assert!(find(cmd).is_some(), ":action {id} の :{cmd} がコマンドの表に無い");
        }
        assert!(find("nosuch").is_none());
    }
}
