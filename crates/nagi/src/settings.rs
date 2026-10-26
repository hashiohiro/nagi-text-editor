//! 設定（IntelliJ の Settings と同じ考え方）。
//!
//! 項目は `SCHEMA` に 1 か所で決める（キー・分類・名前・種類・既定値・説明）。値は文字で持ち、読むときに型に直す。
//! 保存先は `%LOCALAPPDATA%\nagi\settings.conf`（全体）と、プロジェクトの `.nagi\settings.conf`（上書き。プロジェクトごとに
//! 変えたい項目だけ）。どちらも `キー = 値` の 1 行に 1 つ（`#` で始まる行は注釈）。既定値のままの項目は書かない。
//! 試験では全体のファイルを読み書きしない（使っている人の設定を変えないため）。

use nagi_core::Context;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 項目の種類
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SKind {
    Bool,
    /// 整数（下限, 上限）
    Int(i64, i64),
    Text,
    /// 選択肢（値, 画面に出す名前）
    Choice(&'static [(&'static str, &'static str)]),
    /// キーの組み合わせ（Ctrl+Alt+S など）
    Key,
    /// フォントの名前（Windows に入っているフォントから選ぶ）
    Font,
    /// JDK のフォルダ（入っているものから選ぶか、ダウンロードする）
    Jdk,
    /// 言語のツールの bin のフォルダ（nagi_workers::toolchains の種類。入っているものから選ぶか、ダウンロードする）
    Tool(&'static str),
}

const TOOL_HELP: &str = "Enter で入っているものから選ぶか、版を選んでダウンロード。空なら PATH のもの。WSL のプロジェクトでは WSL の中のものを使う";

/// 項目 1 つ
pub struct Item {
    pub key: &'static str,
    /// 分類（画面の左の一覧）
    pub cat: &'static str,
    pub label: &'static str,
    pub kind: SKind,
    pub default: &'static str,
    pub help: &'static str,
    /// プロジェクトの .nagi\settings.conf で上書きできるか
    pub project: bool,
}

const fn item(key: &'static str, cat: &'static str, label: &'static str, kind: SKind, default: &'static str, help: &'static str) -> Item {
    Item { key, cat, label, kind, default, help, project: false }
}

const fn pitem(key: &'static str, cat: &'static str, label: &'static str, kind: SKind, default: &'static str, help: &'static str) -> Item {
    Item { key, cat, label, kind, default, help, project: true }
}

const B: SKind = SKind::Bool;
const T: SKind = SKind::Text;
const K: SKind = SKind::Key;

/// Vim の割り当ての分類（項目は SCHEMA ではなく、vimrc と keys.vim の割り当て。settings_dialog.rs）
pub const MAPS_CAT: &str = "Vim の割り当て";

/// 分類（画面の左の一覧の並び）
pub const CATEGORIES: [&str; 12] = ["外観", "エディタ", "Vim", MAPS_CAT, "入力補完", "検索", "キーマップ", "ビルド・実行", "ツール", "プロジェクト", "その他", "（すべて）"];

pub static SCHEMA: &[Item] = &[
    // 外観
    item("appearance.font", "外観", "フォント（英数字）", SKind::Font, "Cascadia Mono", "Enter で一覧から選ぶ（等幅のものを先に）"),
    item("appearance.font_ja", "外観", "フォント（日本語）", SKind::Font, "BIZ UDGothic", "英数字のフォントに無い字に使う。Enter で一覧から選ぶ"),
    item("appearance.font_size", "外観", "ソースコードの文字の大きさ（pt）", SKind::Int(0, 48), "0", "0 なら既定（12pt）"),
    item("appearance.ui_font_size", "外観", "画面の部品の文字の大きさ（pt）", SKind::Int(0, 48), "0", "ツールバー・木・タブ・メニュー・ダイアログなど。0 ならソースコードと同じ。大きくすると行の高さも合わせて広がる"),
    item("appearance.line_height", "外観", "行の高さ（%）", SKind::Int(80, 200), "100", "文字の高さに対する行の高さ"),
    item("appearance.ambiguous_wide", "外観", "曖昧な幅の文字を全角で", B, "false", "○ → ① ※ など、幅の決まっていない文字を 2 文字ぶんで出す（日本語のフォントに合わせるならオン）"),
    item("appearance.theme", "外観", "配色", SKind::Choice(&[("dark", "IntelliJ ダーク（既定）"), ("darcula", "Darcula"), ("light", "IntelliJ ライト"), ("black", "ブラック（ターミナル風）")]), "dark", ""),
    item("appearance.accent", "外観", "強調の色", T, "#3574f0", "選んでいるタブの下線・ボタンなど（#rrggbb）"),
    item("appearance.remember_panels", "外観", "欄の大きさを覚える", B, "true", "ドラッグで変えた左・右・下の欄の大きさを次の起動でも使う"),
    item("appearance.status_run", "外観", "状態の行: 実行の構成", B, "false", "上の帯にもある"),
    item("appearance.status_branch", "外観", "状態の行: ブランチ", B, "true", ""),
    item("appearance.status_path", "外観", "状態の行: ファイルのパス", B, "true", ""),
    item("appearance.status_lang", "外観", "状態の行: 言語", B, "true", ""),
    item("appearance.status_encoding", "外観", "状態の行: 文字コードと改行", B, "true", "押すと変えるメニュー"),
    // エディタ
    item("editor.line_numbers", "エディタ", "行番号", SKind::Choice(&[("absolute", "絶対"), ("relative", "相対（今の行は絶対）"), ("off", "出さない")]), "absolute", "Vim の number / relativenumber"),
    item("editor.tabs", "エディタ", "タブ", B, "true", "本文の上に開いているファイルのタブを出す（× で閉じる・右クリックでまとめて閉じる）"),
    item("editor.wrap", "エディタ", "折り返す", B, "false", "画面の幅で行を折り返して表示する"),
    pitem("editor.indent_style", "エディタ", "インデント", SKind::Choice(&[("spaces", "空白"), ("tabs", "タブ")]), "spaces", "Tab キー・>>・自動インデントで入れるもの"),
    pitem("editor.indent_width", "エディタ", "インデントの幅", SKind::Int(1, 16), "4", "shiftwidth"),
    pitem("editor.indent_overrides", "エディタ", "言語ごとのインデントの幅", T, "js=2, ts=2, json=2, yaml=2, html=2, css=2, scss=2", "拡張子=幅 をカンマで区切る"),
    pitem("editor.tab_width", "エディタ", "タブの表示幅", SKind::Int(1, 16), "4", "タブの文字を何文字ぶんで出すか（tabstop）"),
    pitem("editor.soft_tabs", "エディタ", "ソフトタブ（Backspace で 1 段ずつ消す）", B, "true", "空白のインデントの中の Backspace で、前のインデントの位置まで消す（softtabstop）"),
    pitem("editor.detect_indent", "エディタ", "ファイルのインデントを見分けて使う", B, "true", "開いたファイルがタブか空白か・空白の幅を中身から見分け、上の設定より先に使う（IntelliJ の Detect and use existing file indents）"),
    pitem("editor.auto_indent", "エディタ", "自動インデント", B, "true", "改行した行を前の行のインデントに揃える（autoindent）"),
    pitem("editor.auto_pairs", "エディタ", "括弧・引用符を自動で閉じる", B, "true", "( [ { \" ' ` を打つと閉じる方も入れる。閉じる方を打てば上を進むだけ、間で Backspace なら両方消す"),
    pitem("editor.smart_indent", "エディタ", "括弧で字下げ", B, "true", "{ ( [ の後の改行で 1 段深く、閉じ括弧を打てば 1 段浅く（smartindent）"),
    pitem("editor.editorconfig", "エディタ", ".editorconfig を使う", B, "true", "プロジェクトの .editorconfig のインデント・タブの幅・保存時の処理・新しいファイルの改行と文字コードを、ここの設定より先に使う"),
    pitem("editor.trim_trailing", "エディタ", "保存時に行末の空白を消す", B, "false", ""),
    pitem("editor.final_newline", "エディタ", "保存時に最後に改行を入れる", B, "false", ""),
    pitem("editor.new_newline", "エディタ", "新しいファイルの改行", SKind::Choice(&[("lf", "LF"), ("crlf", "CRLF")]), "lf", "今のファイルの改行は、状態の行の LF・CRLF を押すか :set ff=unix・ff=dos で変える"),
    pitem("editor.encoding_fallback", "エディタ", "UTF-8 でないファイルの文字コード", SKind::Choice(&[("auto", "自動（Shift_JIS か EUC-JP）"), ("shift_jis", "Shift_JIS（CP932）"), ("euc-jp", "EUC-JP"), ("utf-8", "UTF-8 として読む（壊れた所は置き換え）")]), "auto", "BOM が無く UTF-8 として読めないファイルを何で読むか。読み直すなら状態の行の文字コードを押すか :e ++enc=sjis"),
    pitem("editor.new_encoding", "エディタ", "新しいファイルの文字コード", SKind::Choice(&[("utf-8", "UTF-8"), ("shift_jis", "Shift_JIS（CP932）"), ("euc-jp", "EUC-JP")]), "utf-8", "今のファイルは状態の行の文字コードを押すか :set fenc=sjis で変える（保存で変わる）"),
    pitem("editor.new_bom", "エディタ", "新しいファイルに BOM を付ける", B, "false", ""),
    item("editor.grammar_download", "エディタ", "文法を自動でダウンロード", B, "true", "C・Go・C# など 21 言語の文法（色分け・定義へ移動）は、初めて開いたときに GitHub から取ってくる（1 言語 60〜510KB）。オフなら :GrammarInstall で"),
    item("editor.restore_session", "エディタ", "前回開いていたファイルを開き直す", B, "true", "プロジェクトを開いたとき、前に nagi で開いていたファイルとカーソルの位置を戻す（無ければ IntelliJ で開いていたもの）"),
    item("editor.autosave", "エディタ", "自動で保存", SKind::Choice(&[("off", "しない"), ("focus", "窓を離れたとき・別のファイルへ移ったとき"), ("idle", "それと、打つのを止めて決めた秒数たったとき")]), "off", "IntelliJ と同じく、名前のある変更したファイルをすべて保存する"),
    item("editor.autosave_secs", "エディタ", "自動で保存するまでの秒数", SKind::Int(1, 600), "15", "「自動で保存」が「それと、打つのを止めて…」のとき"),
    item("editor.autoread", "エディタ", "外部での変更", SKind::Choice(&[("auto", "自動で読み直す（変えていなければ）"), ("ask", "知らせるだけ"), ("off", "何もしない")]), "auto", ""),
    item("editor.scrolloff", "エディタ", "カーソルの上下に残す行", SKind::Int(0, 50), "0", "カーソルを画面の端からこの行数より近づけない（scrolloff）"),
    item("editor.show_whitespace", "エディタ", "空白を見せる", SKind::Choice(&[("off", "出さない"), ("trailing", "タブと行末の空白"), ("all", "すべて")]), "off", "空白は ·、タブは →（list）"),
    item("editor.show_fullwidth_space", "エディタ", "全角の空白を見せる", B, "true", "全角の空白（U+3000）を □ で出す"),
    item("editor.cursorline", "エディタ", "カーソルの行を目立たせる", B, "true", "cursorline（IntelliJ と同じく既定でオン）"),
    pitem("editor.colorcolumn", "エディタ", "右端の線", T, "", "列の数をカンマで区切る（120 など。colorcolumn）。.editorconfig の max_line_length も出す"),
    item("editor.split_below", "エディタ", "上下の分割は下に開く", B, "false", "splitbelow"),
    item("editor.split_right", "エディタ", "左右の分割は右に開く", B, "false", "splitright"),
    item("editor.wheel_lines", "エディタ", "ホイールで動く行の数", SKind::Int(1, 20), "3", "マウスのホイール 1 目盛りで送る行"),
    item("editor.scroll_ms", "エディタ", "スムーズスクロールの時間（ms）", SKind::Int(0, 1000), "110", "0 ならスクロールを滑らかにしない"),
    item("editor.repeat_delay", "エディタ", "押しっぱなし: 繰り返すまで（ms）", SKind::Int(50, 1000), "250", "hjkl などの移動のキー"),
    item("editor.repeat_interval", "エディタ", "押しっぱなし: 間隔（ms）", SKind::Int(5, 200), "25", ""),
    item("editor.repeat_fast", "エディタ", "押しっぱなし: 速めた間隔（ms）", SKind::Int(5, 200), "16", "1.2 秒押し続けた後"),
    item("editor.squiggles", "エディタ", "問題を波線で出す", B, "true", "エラーは赤、警告は黄色"),
    // Vim
    item("vim.enabled", "Vim", "Vim のキー操作", B, "false", "オフなら普通のエディタ（打てばそのまま入る・Shift+矢印で選ぶ・Ctrl+C/V/Z）。オンなら Vim のモードと .ideavimrc"),
    item("vim.rc_path", "Vim", "設定ファイル", T, "", "空なら ~\\.ideavimrc（無ければ ~\\_vimrc・~\\.vimrc）"),
    item("vim.ignorecase", "Vim", "検索で大文字小文字を区別しない", B, "true", "ignorecase"),
    item("vim.smartcase", "Vim", "大文字を含めば区別する", B, "true", "smartcase"),
    item("vim.hlsearch", "Vim", "検索の一致を強調する", B, "true", "hlsearch"),
    item("vim.incsearch", "Vim", "打ちながら検索する", B, "true", "incsearch"),
    item("vim.wrapscan", "Vim", "検索で端から反対の端へ続ける", B, "true", "wrapscan"),
    item("vim.timeoutlen", "Vim", "割り当てのキーを待つ時間（ms）", SKind::Int(100, 5000), "1000", "<leader>f のように続くキーを待つ時間（timeoutlen）"),
    item("vim.highlight_yank", "Vim", "ヤンクした範囲を光らせる", B, "false", "highlightedyank"),
    item("vim.ideajoin", "Vim", "J で注釈・文字列をつなぐ", B, "false", "J で注釈の続きの // や #、\"a\" +\n\"b\" の引用符を外してつなぐ（ideajoin）"),
    item("vim.clipboard", "Vim", "クリップボード", SKind::Choice(&[("unnamed", "ヤンク・削除をいつもクリップボードへ"), ("explicit", "\"+ と \"* のときだけ")]), "unnamed", "clipboard=unnamed"),
    // 入力補完
    item("completion.auto", "入力補完", "打っている間に自動で出す", B, "false", "オフなら Ctrl+Space で出す"),
    item("completion.delay_ms", "入力補完", "自動で出すまで（ms）", SKind::Int(0, 2000), "300", ""),
    item("completion.min_chars", "入力補完", "自動で出す文字数", SKind::Int(1, 10), "2", ". の後はすぐ出す"),
    item("completion.case", "入力補完", "大文字小文字", SKind::Choice(&[("first", "最初の文字だけ区別"), ("none", "区別しない"), ("all", "区別する")]), "none", "IntelliJ の Match case"),
    item("completion.keywords", "入力補完", "予約語を出す", B, "true", ""),
    item("completion.words", "入力補完", "ファイルの中の語を出す", B, "true", ""),
    // 全文検索（Ctrl+Shift+F。その場では Alt+C・O・X・F・D・P・T と見出しの札で変えられる）
    item("search.grep_case", "検索", "全文検索で大文字小文字を区別する", B, "false", "IntelliJ の Match case（Alt+C）"),
    item("search.grep_words", "検索", "全文検索で単語として", B, "false", "Words（Alt+O）"),
    item("search.grep_regex", "検索", "全文検索で正規表現", B, "false", "Regex（Alt+X）"),
    item("search.grep_mask", "検索", "全文検索のファイルマスク", SKind::Text, "", "File mask（Alt+F）。*.java, !*Test.java のように , で区切る。空ならすべて"),
    item("search.grep_context", "検索", "全文検索の文脈", SKind::Choice(&[("anywhere", "どこでも"), ("incomments", "コメントの中"), ("instrings", "文字列の中"), ("exceptcomments", "コメントを除く"), ("exceptstrings", "文字列を除く"), ("exceptboth", "コメント・文字列を除く")]), "anywhere", "Alt+T で切り替え"),
    // キーマップ
    item("keymap.settings", "キーマップ", "設定", K, "Ctrl+Alt+S", ""),
    item("keymap.double_shift", "キーマップ", "Shift 2 回でどこでも検索", B, "true", "IntelliJ の Search Everywhere（型・シンボル・ファイル・コマンド）"),
    item("keymap.complete", "キーマップ", "入力補完", K, "Ctrl+Space", ""),
    item("keymap.run", "キーマップ", "実行", K, "Shift+F10", ""),
    item("keymap.run_choose", "キーマップ", "選んで実行", K, "Alt+Shift+F10", ""),
    item("keymap.run_stop", "キーマップ", "止める", K, "Ctrl+F2", ""),
    item("keymap.run_again", "キーマップ", "もう一度実行", K, "Ctrl+F5", ""),
    item("keymap.run_test", "キーマップ", "カーソルの位置のテストを実行", K, "Ctrl+Shift+F10", ""),
    item("keymap.goto_def", "キーマップ", "定義へ", K, "F12", "Ctrl+クリックでも"),
    item("keymap.terminal", "キーマップ", "ターミナル", K, "Alt+F12", ""),
    item("keymap.project", "キーマップ", "プロジェクト", K, "Alt+1", ""),
    item("keymap.commit", "キーマップ", "コミット", K, "Alt+0", ""),
    item("keymap.run_panel", "キーマップ", "実行の欄", K, "Alt+8", ""),
    item("keymap.git", "キーマップ", "Git", K, "Alt+9", ""),
    item("keymap.find", "キーマップ", "検索の欄", K, "Alt+3", ""),
    item("keymap.problems", "キーマップ", "問題", K, "Alt+6", ""),
    item("keymap.log", "キーマップ", "ログ", K, "Alt+4", ""),
    item("keymap.structure", "キーマップ", "ファイルの構造（欄）", K, "Alt+7", ""),
    item("keymap.hide_all", "キーマップ", "欄をすべて隠す", K, "Ctrl+Shift+F12", ""),
    item("keymap.select_in", "キーマップ", "開いているファイルを木で選ぶ", K, "Alt+F1", "IntelliJ の Select In"),
    item("keymap.debug", "キーマップ", "デバッグ", K, "Shift+F9", "選んでいる実行の構成をデバッグで動かす"),
    item("keymap.debug_test", "キーマップ", "カーソルの位置のテストをデバッグ", K, "Ctrl+Shift+F9", ""),
    item("keymap.breakpoint", "キーマップ", "ブレークポイント", K, "F9", "Visual Studio と同じ"),
    item("keymap.debug_resume", "キーマップ", "デバッグ: 続ける", K, "F5", "Visual Studio と同じ。デバッグしていなければ始める"),
    item("keymap.step_over", "キーマップ", "デバッグ: 次へ", K, "F10", "Visual Studio と同じ"),
    item("keymap.step_into", "キーマップ", "デバッグ: 中へ", K, "F11", "Visual Studio と同じ"),
    item("keymap.step_out", "キーマップ", "デバッグ: 外へ", K, "Shift+F11", "Visual Studio と同じ"),
    item("keymap.debug_stop", "キーマップ", "デバッグ: 止める", K, "Shift+F5", "Visual Studio と同じ（Ctrl+F2 でも）"),
    item("keymap.debug_panel", "キーマップ", "デバッグの欄", K, "Alt+5", ""),
    item("keymap.palette", "キーマップ", "コマンドパレット", K, "Ctrl+Shift+A", "IntelliJ の Find Action"),
    item("keymap.goto_file", "キーマップ", "ファイルを開く", K, "Ctrl+Shift+N", ""),
    item("keymap.goto_symbol", "キーマップ", "シンボルへ", K, "Ctrl+Alt+Shift+N", ""),
    item("keymap.file_structure", "キーマップ", "ファイルの構造", K, "Ctrl+F12", ""),
    item("keymap.goto_impl", "キーマップ", "実装へ", K, "Ctrl+Alt+B", ""),
    item("keymap.find_usages", "キーマップ", "参照を探す", K, "Alt+F7", ""),
    item("keymap.find_in_files", "キーマップ", "プロジェクトを検索", K, "Ctrl+Shift+F", ""),
    item("keymap.recent_locations", "キーマップ", "最近の編集箇所", K, "Ctrl+Shift+E", ""),
    item("keymap.back", "キーマップ", "前の場所へ", K, "Ctrl+Alt+Left", ""),
    item("keymap.forward", "キーマップ", "次の場所へ", K, "Ctrl+Alt+Right", ""),
    item("keymap.last_edit", "キーマップ", "最後に変えた場所へ", K, "Ctrl+Shift+Backspace", ""),
    item("keymap.error_next", "キーマップ", "次のエラーへ", K, "F2", ""),
    item("keymap.error_prev", "キーマップ", "前のエラーへ", K, "Shift+F2", ""),
    item("keymap.method_next", "キーマップ", "次のメソッドへ", K, "Alt+Down", ""),
    item("keymap.method_prev", "キーマップ", "前のメソッドへ", K, "Alt+Up", ""),
    item("keymap.reformat", "キーマップ", "整形", K, "Ctrl+Alt+L", ""),
    item("keymap.optimize_imports", "キーマップ", "import の整理", K, "Ctrl+Alt+O", ""),
    item("keymap.rename", "キーマップ", "名前の変更", K, "Shift+F6", ""),
    item("keymap.switcher", "キーマップ", "バッファを選ぶ", K, "Ctrl+Tab", "IntelliJ の Switcher"),
    item("keymap.close_tab", "キーマップ", "ファイルを閉じる", K, "Ctrl+F4", ""),
    item("keymap.next_tab", "キーマップ", "次のファイル（タブ）", K, "Alt+Right", ""),
    item("keymap.prev_tab", "キーマップ", "前のファイル（タブ）", K, "Alt+Left", ""),
    item("keymap.hunk_next", "キーマップ", "次の変更へ", K, "Ctrl+Alt+Shift+Down", ""),
    item("keymap.hunk_prev", "キーマップ", "前の変更へ", K, "Ctrl+Alt+Shift+Up", ""),
    item("keymap.rollback", "キーマップ", "カーソルの変更を戻す", K, "Ctrl+Alt+Z", ""),
    item("keymap.push", "キーマップ", "push", K, "Ctrl+Shift+K", ""),
    // Vim のキー操作を使わないときだけ効く（keymap.rs の PLAIN_ACTIONS）
    item("keymap.undo", "キーマップ", "元に戻す", K, "Ctrl+Z", "Vim を使わないとき"),
    item("keymap.redo", "キーマップ", "やり直す", K, "Ctrl+Shift+Z", "Vim を使わないとき"),
    item("keymap.copy", "キーマップ", "コピー", K, "Ctrl+C", "Vim を使わないとき。選んでいなければ行"),
    item("keymap.cut", "キーマップ", "切り取り", K, "Ctrl+X", "Vim を使わないとき。選んでいなければ行"),
    item("keymap.paste", "キーマップ", "貼り付け", K, "Ctrl+V", "Vim を使わないとき"),
    item("keymap.select_all", "キーマップ", "すべて選ぶ", K, "Ctrl+A", "Vim を使わないとき"),
    item("keymap.save", "キーマップ", "保存", K, "Ctrl+S", "Vim を使わないとき"),
    item("keymap.find_text", "キーマップ", "ファイルの中を検索", K, "Ctrl+F", "Vim を使わないとき"),
    item("keymap.replace", "キーマップ", "置換", K, "Ctrl+R", "Vim を使わないとき"),
    item("keymap.duplicate", "キーマップ", "行の複製", K, "Ctrl+D", "Vim を使わないとき"),
    item("keymap.delete_line", "キーマップ", "行の削除", K, "Ctrl+Y", "Vim を使わないとき"),
    item("keymap.comment", "キーマップ", "行コメント", K, "Ctrl+/", "Vim を使わないとき"),
    item("keymap.goto_line", "キーマップ", "行へ移動", K, "Ctrl+G", "Vim を使わないとき"),
    item("keymap.goto_class", "キーマップ", "型へ", K, "Ctrl+N", "Vim を使わないとき"),
    item("keymap.recent", "キーマップ", "最近のファイル", K, "Ctrl+E", "Vim を使わないとき"),
    item("keymap.vcs_commit", "キーマップ", "コミット", K, "Ctrl+K", "Vim を使わないとき"),
    item("keymap.vcs_update", "キーマップ", "pull", K, "Ctrl+T", "Vim を使わないとき"),
    // ツール
    item("terminal.shell", "ツール", "ターミナル: 既定のシェル", T, "", "空なら最後に選んだもの（cmd・powershell・pwsh・bash・wsl、またはコマンド）"),
    item("terminal.start_dir", "ツール", "ターミナル: 始めるフォルダ", SKind::Choice(&[("project", "プロジェクト"), ("file", "開いているファイルのフォルダ"), ("home", "ホーム")]), "project", ""),
    item("terminal.utf8", "ツール", "ターミナル: UTF-8 で始める", B, "true", "オフなら Windows の既定（CP932）"),
    pitem("maven.jdk", "ビルド・実行", "JDK（コンパイル・実行）", SKind::Jdk, "", "Enter で入っているものから選ぶか、Eclipse Temurin をダウンロード（%LOCALAPPDATA%\\nagi\\jdks に置く）。空なら JAVA_HOME か nagi が入れたものから探し、無ければ動かすときに 21 を入れる"),
    item("build.jdk_env", "ビルド・実行", "JDK を nagi から起動するものに渡す", B, "true", "ターミナル・実行の構成・Maven に JAVA_HOME と PATH（JDK の bin）として"),
    pitem("toolchain.maven", "ビルド・実行", "Maven", SKind::Tool("maven"), "", "Enter で入っているものから選ぶか、ダウンロード（%LOCALAPPDATA%\\nagi\\tools に置く）。空なら PATH・MAVEN_HOME・nagi が入れたもの・Wrapper のものから探し、どれも無ければ動かすときに入れる"),
    pitem("maven.use_wrapper", "ビルド・実行", "Maven: Wrapper（mvnw）を使う", B, "true", "設定ファイルもそろっているときだけ"),
    pitem("maven.options", "ビルド・実行", "Maven: 追加のオプション", T, "", "いつも付ける（-o・-T 4 など）"),
    pitem("toolchain.python", "ビルド・実行", "Python", SKind::Tool("python"), "", TOOL_HELP),
    pitem("toolchain.node", "ビルド・実行", "Node.js", SKind::Tool("node"), "", TOOL_HELP),
    pitem("toolchain.go", "ビルド・実行", "Go", SKind::Tool("go"), "", TOOL_HELP),
    pitem("toolchain.rust", "ビルド・実行", "Rust（cargo）", SKind::Tool("rust"), "", TOOL_HELP),
    pitem("toolchain.dotnet", "ビルド・実行", ".NET SDK", SKind::Tool("dotnet"), "", TOOL_HELP),
    pitem("toolchain.clang", "ビルド・実行", "C/C++（clang）", SKind::Tool("clang"), "", TOOL_HELP),
    pitem("toolchain.cmake", "ビルド・実行", "CMake", SKind::Tool("cmake"), "", TOOL_HELP),
    pitem("toolchain.zig", "ビルド・実行", "Zig", SKind::Tool("zig"), "", TOOL_HELP),
    pitem("toolchain.dart", "ビルド・実行", "Dart", SKind::Tool("dart"), "", TOOL_HELP),
    pitem("toolchain.php", "ビルド・実行", "PHP", SKind::Tool("php"), "", TOOL_HELP),

    pitem("tools.formatters", "ツール", "整形のコマンド", T, "java=google-java-format -, rs=rustfmt --edition 2021, go=gofmt, py=black -q -, ts|tsx|js|jsx|mjs|cjs|json|css|scss|less|html|vue|md|yaml|yml=npx --no-install prettier --stdin-filepath {file}, c|h|cpp|hpp|cc=clang-format --assume-filename={file}", "拡張子=コマンド をカンマで区切る（| で拡張子を並べる。{file} はファイルのパス）。標準入力で中身を受け、標準出力に整形した中身を出すもの。ReformatCode・:Format で動かす"),
    item("tools.extra_path", "ツール", "追加の PATH", T, "", "gradle・npm・cargo などの場所（; で区切る）"),
    item("git.path", "ツール", "Git: git の場所", T, "", "空なら PATH の git"),
    item("git.wsl", "ツール", "Git: WSL のプロジェクトは WSL の git", B, "true", ""),
    item("db.drivers_dir", "ツール", "データベース: ドライバーの置き場所", T, "", "空なら %LOCALAPPDATA%\\nagi\\drivers"),
    item("tools.browser", "ツール", "ブラウザ", T, "", "空なら既定のブラウザ（実行の構成の URL・Markdown のリンク）"),
    // プロジェクト
    pitem("project.exclude", "プロジェクト", "除外するフォルダ", T, "", "名前をカンマで区切る（検索・ツリー・索引から外す）"),
    pitem("project.index", "プロジェクト", "定義の索引を作る", B, "true", "定義へ移動・入力補完に使う"),
    item("project.startup", "プロジェクト", "起動時（引数なし）", SKind::Choice(&[("launcher", "ランチャー"), ("last", "前回のプロジェクト"), ("empty", "空のバッファ")]), "launcher", ""),
    item("project.recent_ide", "プロジェクト", "最近のプロジェクトに IntelliJ などの履歴を混ぜる", B, "true", ""),
    // その他
    item("other.log_max_kb", "その他", "ログ（nagi.log）の上限（KB）", SKind::Int(16, 102400), "1024", "超えたら古いものを消す"),
    item("other.jumplist_count", "その他", "ジャンプリストの件数", SKind::Int(0, 30), "10", "タスクバーの右クリックの最近のプロジェクト"),
];

/// キーマップでコマンドに付けるキーの項目の頭（keymap.cmd.名前）
pub const CMD_KEY: &str = "keymap.cmd.";

/// すべての項目: SCHEMA と、キーマップのコマンドごとの項目（keymap.cmd.名前。決まった項目の無いコマンドすべて）
pub fn items() -> &'static [Item] {
    static ALL: std::sync::OnceLock<Vec<Item>> = std::sync::OnceLock::new();
    ALL.get_or_init(|| {
        let fixed: Vec<&str> = crate::keymap::all_actions().filter_map(|(_, a)| if let crate::keymap::Action::Cmd(c) = a { Some(*c) } else { None }).collect();
        let mut all: Vec<Item> = SCHEMA.iter().map(|i| Item { ..*i }).collect();
        for c in crate::commands::COMMANDS.iter().filter(|c| !c.help.is_empty() && !c.names.iter().any(|n| fixed.contains(n))) {
            let name = c.names[0];
            let short = c.help.split(['（', '。']).next().unwrap_or(c.help);
            let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
            all.push(Item { key: leak(format!("{CMD_KEY}{name}")), cat: "キーマップ", label: leak(short.to_string()), kind: SKind::Key, default: "", help: leak(format!(":{name}  {}", c.help)), project: false });
        }
        all
    })
}

pub fn schema(key: &str) -> Option<&'static Item> {
    items().iter().find(|i| i.key == key)
}

/// `key`（キーの項目）と同じキーの組み合わせを付けたほかの項目
pub fn key_conflicts(s: &Settings, key: &str) -> Vec<&'static Item> {
    let Some(c) = crate::keymap::parse_chord(&s.get(key)) else { return Vec::new() };
    items().iter().filter(|i| i.kind == SKind::Key && i.key != key && crate::keymap::parse_chord(&s.get(i.key)).as_ref() == Some(&c)).collect()
}

/// 設定の値（キー → 文字。既定値と違うものだけ）
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    /// 全体の設定
    pub global: HashMap<String, String>,
    /// プロジェクトの上書き
    pub project: HashMap<String, String>,
    /// プロジェクトの根（上書きの読み書きの場所）
    pub root: Option<PathBuf>,
    /// vimrc の set で決めた値（全体の設定より弱い。Vim のキー操作がオンのときだけ効く。ファイルには書かない）
    pub rc: HashMap<String, String>,
    /// vimrc のどこで決めたか（キー → "vimrc（.ideavimrc:12 set sw=2）"。設定の画面に出す）
    pub rc_from: HashMap<String, String>,
    /// `:set` で変えた値（いちばん強い。この窓が閉じるまで。ファイルには書かない）
    pub session: HashMap<String, String>,
}

/// 全体の設定のファイル
pub fn global_path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("nagi").join("settings.conf"))
}

/// 設定の画面で決めた Vim の割り当て（vimrc の書き方。vimrc の後に読む。.ideavimrc は IntelliJ と共有するので書き換えない）。試験では None
pub fn keys_path() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    global_path().map(|p| p.with_file_name("keys.vim"))
}

pub fn project_path(root: &Path) -> PathBuf {
    root.join(".nagi").join("settings.conf")
}

/// `キー = 値` の行を読む（知らないキーも残す）
pub fn parse(s: &str) -> HashMap<String, String> {
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .filter(|(k, _)| !k.is_empty())
        .collect()
}

/// 設定のファイルを読む（無ければ空）
fn read_file(p: &Path) -> HashMap<String, String> {
    std::fs::read_to_string(p).map(|s| parse(&s)).unwrap_or_default()
}

/// `before` から `after` へ変えた項目だけを `disk` に重ねる（足した・変えたものは入れ、消したものは消す）
fn merge(disk: &mut HashMap<String, String>, before: &HashMap<String, String>, after: &HashMap<String, String>) {
    for k in before.keys().chain(after.keys()) {
        match (before.get(k), after.get(k)) {
            (b, a) if b == a => {}
            (_, Some(v)) => {
                disk.insert(k.clone(), v.clone());
            }
            (_, None) => {
                disk.remove(k);
            }
        }
    }
}

/// 書き出す（SCHEMA の順。既定値と同じものは書かない）
pub fn render(values: &HashMap<String, String>, header: &str) -> String {
    let mut s = format!("# {header}（nagi の設定の画面で直せる。キー = 値）\n");
    let mut cat = "";
    for it in items() {
        if let Some(v) = values.get(it.key).filter(|v| v.as_str() != it.default) {
            if it.cat != cat {
                s.push_str(&format!("\n# {}\n", it.cat));
                cat = it.cat;
            }
            s.push_str(&format!("{} = {v}\n", it.key));
        }
    }
    // 知らないキー（新しい版で消えた項目など）も消さずに残す
    let mut rest: Vec<(&String, &String)> = values.iter().filter(|(k, _)| schema(k).is_none()).collect();
    rest.sort();
    for (k, v) in rest {
        s.push_str(&format!("{k} = {v}\n"));
    }
    s
}

impl Settings {
    /// 全体の設定と、`root` のプロジェクトの上書きを読む（試験では全体を読まない）
    pub fn load(root: Option<&Path>) -> Settings {
        // 試験は Vim の操作で書いてあり、本文は 1 行目から（Vim を使わないときの試験は set_vim(false) で、タブは settings.set で出す）
        let global = if cfg!(test) { HashMap::from([("vim.enabled".to_string(), "true".to_string()), ("editor.tabs".to_string(), "false".to_string())]) } else { global_path().and_then(|p| std::fs::read_to_string(p).ok()).map(|s| parse(&s)).unwrap_or_default() };
        let mut s = Settings { global, ..Default::default() };
        s.set_root(root);
        s
    }

    /// プロジェクトが変わった（上書きを読み直す）
    pub fn set_root(&mut self, root: Option<&Path>) {
        self.root = root.map(Path::to_path_buf);
        self.project = root.and_then(|r| std::fs::read_to_string(project_path(r)).ok()).map(|s| parse(&s)).unwrap_or_default();
    }

    /// 値（:set → プロジェクトの上書き → 全体 → vimrc → 既定値）
    pub fn get(&self, key: &str) -> String {
        if let Some(v) = self.session.get(key) {
            return v.clone();
        }
        let it = schema(key);
        if it.is_some_and(|i| i.project) {
            if let Some(v) = self.project.get(key) {
                return v.clone();
            }
        }
        if let Some(v) = self.global.get(key) {
            return v.clone();
        }
        if let Some(v) = self.rc.get(key).filter(|_| self.rc_on()) {
            return v.clone();
        }
        it.map_or(String::new(), |i| i.default.to_string())
    }

    /// vimrc の set を効かせるか（Vim のキー操作がオン。IdeaVim と同じく、Vim を使わないなら vimrc は読まない）
    fn rc_on(&self) -> bool {
        !self.rc.is_empty() && matches!(self.get("vim.enabled").as_str(), "true" | "on" | "yes" | "1")
    }

    /// 値がどこで決まったか（設定の画面に出す）。:set・vimrc で決めたときだけ
    pub fn origin(&self, key: &str) -> Option<String> {
        if self.session.contains_key(key) {
            return Some(":set で（この窓だけ）".into());
        }
        let project = schema(key).is_some_and(|i| i.project) && self.project.contains_key(key);
        if project || self.global.contains_key(key) || !self.rc.contains_key(key) || !self.rc_on() {
            return None;
        }
        Some(self.rc_from.get(key).cloned().unwrap_or_else(|| "vimrc".into()))
    }

    /// 利用者が決めた値か（:set・プロジェクト・全体の設定。既定と vimrc の set は含めない）
    pub fn user_set(&self, key: &str) -> bool {
        self.session.contains_key(key) || (schema(key).is_some_and(|i| i.project) && self.project.contains_key(key)) || self.global.contains_key(key)
    }

    /// 利用者が決めた項目のうち、`prefix` で始まるもの（今は無い項目のものも）
    pub fn user_keys(&self, prefix: &str) -> Vec<String> {
        let mut v: Vec<String> = self.session.keys().chain(self.project.keys()).chain(self.global.keys()).filter(|k| k.starts_with(prefix)).cloned().collect();
        v.sort();
        v.dedup();
        v
    }

    pub fn bool(&self, key: &str) -> bool {
        matches!(self.get(key).to_ascii_lowercase().as_str(), "true" | "on" | "yes" | "1")
    }

    /// 整数（範囲の外は端に。読めなければ既定値）
    pub fn int(&self, key: &str) -> i64 {
        let it = schema(key);
        let v = self.get(key).trim().parse::<i64>().ok().or_else(|| it.and_then(|i| i.default.parse().ok())).unwrap_or(0);
        match it.map(|i| i.kind) {
            Some(SKind::Int(lo, hi)) => v.clamp(lo, hi),
            _ => v,
        }
    }

    /// 空なら None
    pub fn text(&self, key: &str) -> Option<String> {
        let v = self.get(key);
        (!v.trim().is_empty()).then(|| v.trim().to_string())
    }

    /// カンマで区切った並び
    pub fn list(&self, key: &str) -> Vec<String> {
        self.get(key).split([',', ';']).map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()
    }

    /// 値を変える（`project` ならプロジェクトの上書きに）。既定値に戻すときは消す
    pub fn set(&mut self, key: &str, value: &str, project: bool) {
        // 既定値と同じでも、vimrc が別の値にしていれば書いておく（画面で決めた値が vimrc に勝つように）
        let rc = self.rc.contains_key(key);
        let map = if project { &mut self.project } else { &mut self.global };
        if schema(key).is_some_and(|i| i.default == value) && !project && !rc {
            map.remove(key);
        } else {
            map.insert(key.to_string(), value.to_string());
        }
    }

    /// プロジェクトの上書きを消す（全体の値に戻す）
    pub fn unset_project(&mut self, key: &str) {
        self.project.remove(key);
    }

    /// ファイルに書く（試験では全体を書かない）。`old`（この窓が前に持っていた設定）から変えた項目だけを、ファイルの今の中身に
    /// 重ねる（窓が何枚あっても、ほかの窓が先に保存した項目を、この窓の古い値で消さない）。書いた後の中身を自分も持つ
    pub fn save(&mut self, old: &Settings) -> nagi_core::Result<()> {
        if !cfg!(test) {
            if let Some(p) = global_path() {
                let mut disk = read_file(&p);
                merge(&mut disk, &old.global, &self.global);
                write(&p, &render(&disk, "nagi の設定"))?;
                self.global = disk;
            }
        }
        if let Some(r) = self.root.clone() {
            let p = project_path(&r);
            let mut disk = read_file(&p);
            // プロジェクトが変わっていれば、前の上書きは別のプロジェクトのもの
            let before = if old.root == self.root { old.project.clone() } else { HashMap::new() };
            merge(&mut disk, &before, &self.project);
            if disk.is_empty() {
                if p.exists() {
                    let _ = std::fs::remove_file(&p);
                }
            } else {
                write(&p, &render(&disk, "このプロジェクトの nagi の設定（全体の設定を上書き）"))?;
            }
            self.project = disk;
        }
        Ok(())
    }
}

/// 起動したときの PATH と JAVA_HOME（設定で足すものは、いつもここから作り直す。前に足したものが混ざらないように）
fn base_env() -> (&'static str, Option<&'static str>) {
    static BASE: std::sync::OnceLock<(String, Option<String>)> = std::sync::OnceLock::new();
    let b = BASE.get_or_init(|| (std::env::var("PATH").unwrap_or_default(), std::env::var("JAVA_HOME").ok()));
    (b.0.as_str(), b.1.as_deref())
}

/// 裏の処理が読む設定（`s` が全体の設定ならプロジェクトの上書きを除いたもの）。
/// Windows で動かすものに渡す PATH（追加の PATH・JDK の bin・言語のツール・Maven を頭に）と JAVA_HOME も作る
fn tool_config(s: &Settings) -> nagi_workers::config::Config {
    let mut c = nagi_workers::config::Config {
        maven: s.text("toolchain.maven").map(PathBuf::from),
        use_wrapper: s.bool("maven.use_wrapper"),
        mvn_options: s.get("maven.options"),
        jdk: s.text("maven.jdk").map(PathBuf::from),
        git: s.text("git.path"),
        git_wsl: s.bool("git.wsl"),
        browser: s.text("tools.browser"),
        terminal_utf8: s.bool("terminal.utf8"),
        exclude: s.list("project.exclude"),
        env: Vec::new(),
    };
    if cfg!(test) {
        return c;
    }
    let (base, _) = base_env();
    let mut extra = s.list("tools.extra_path");
    // 設定の JDK を渡す（JAVA_HOME と、java・javac が先に見つかるように PATH の頭に bin）
    let jdk = s.text("maven.jdk").filter(|j| s.bool("build.jdk_env") && Path::new(j).join("bin").is_dir());
    if let Some(j) = &jdk {
        extra.insert(0, Path::new(j).join("bin").display().to_string());
    }
    // 言語のツール（設定で選んだ bin のフォルダを PATH の頭に。WSL の中のコマンドには届かない）
    // Maven は下で（maven::bin が決める）
    for t in nagi_workers::toolchains::TOOLS.iter().filter(|t| t.id != "maven") {
        if let Some(b) = s.text(&format!("toolchain.{}", t.id)).filter(|b| Path::new(b).is_dir()) {
            extra.insert(0, b);
        }
    }
    let extra = extra.join(";");
    let mut path = if extra.is_empty() { base.to_string() } else { format!("{extra};{base}") };
    // Maven（maven::bin が決めたもの）を頭に置く（mvn と打てばどこでも同じ mvn が動くように）。
    // 前に置いたものが混ざらないよう、起動したときの PATH から探す
    if let Some(b) = nagi_workers::maven::bin(&c, Some(base.into())) {
        path = format!("{};{path}", b.display());
    }
    c.env.push(("PATH".into(), path));
    if let Some(j) = jdk {
        c.env.push(("JAVA_HOME".into(), j));
    }
    c
}

impl crate::editor::Editor {
    /// 設定を動いている nagi に効かせる（`old` は前の設定。変わった項目だけ効かせる）
    pub(crate) fn apply_settings(&mut self, old: Option<&Settings>) {
        // 外観: 配色・強調の色・タブの名前は描き方へ、フォント・大きさ・行の高さが変わったら App が作り直す
        crate::view::set_appearance(&self.settings.get("appearance.theme"), &self.settings.get("appearance.accent"));
        if let Some(o) = old {
            if ["appearance.font", "appearance.font_ja", "appearance.font_size", "appearance.ui_font_size", "appearance.line_height"].iter().any(|k| o.get(k) != self.settings.get(k)) {
                self.appearance_changed = true;
            }
        }
        // 欄の大きさを覚えていれば、まだ決めていない欄に入れる
        if self.settings.bool("appearance.remember_panels") {
            let num = |k: &str| self.settings.global.get(k).and_then(|v| v.parse::<usize>().ok());
            self.left_w = self.left_w.or(num("state.left_width"));
            self.right_w = self.right_w.or(num("state.right_width"));
            self.bottom_h = self.bottom_h.or(num("state.bottom_height"));
            self.log_branch_w = self.log_branch_w.or(num("state.log_branch_width"));
            self.log_detail_w = self.log_detail_w.or(num("state.log_detail_width"));
        }
        self.ec_cache.borrow_mut().clear();
        self.sync_clipboard();
        self.sync_tab();
        nagi_core::set_ambiguous_wide(self.settings.bool("appearance.ambiguous_wide"));
        nagi_core::encoding::set_fallback(match self.settings.get("editor.encoding_fallback").as_str() {
            "auto" => None,
            n => nagi_core::encoding::by_name(n),
        });
        self.set_vim(self.settings.bool("vim.enabled"));
        if !self.settings.bool("vim.hlsearch") {
            self.search.highlight = false;
        }
        // 裏の処理（Maven・git・JDK・ターミナル・ブラウザ）が読む設定。全体の設定と、このプロジェクトの設定（上書きつき）を分けて入れる。
        // 1 つのプロセスに窓が何枚あっても、プロジェクトの JDK・Maven・言語のツールが別のプロジェクトの窓に混ざらないように
        // （裏の処理はフォルダを渡して config::for_path で読む）
        let mut global = self.settings.clone();
        global.project.clear();
        let g = tool_config(&global);
        // 全体の設定の PATH・JAVA_HOME・ドライバーの置き場所は、nagi の環境変数にも（nagi の中で読むもの: Oracle の oci.dll など）。
        // どの窓も同じ全体の設定から作るので、どの窓が書いても同じ値。試験では変えない
        if !cfg!(test) {
            // SAFETY: 設定を適用するのは UI のスレッドだけで、ほかのスレッドが同時に環境変数を書くことはない
            unsafe {
                for (k, v) in &g.env {
                    if std::env::var(k).ok().as_deref() != Some(v.as_str()) {
                        std::env::set_var(k, v);
                    }
                }
                if !g.env.iter().any(|(k, _)| k == "JAVA_HOME") {
                    match base_env().1 {
                        Some(b) => std::env::set_var("JAVA_HOME", b),
                        None => std::env::remove_var("JAVA_HOME"),
                    }
                }
                match self.settings.text("db.drivers_dir") {
                    Some(d) => std::env::set_var("NAGI_DRIVERS_DIR", d),
                    None => std::env::remove_var("NAGI_DRIVERS_DIR"),
                }
            }
        }
        nagi_workers::config::set(None, g);
        // プロジェクトの設定は、上書きを読んだ根（settings.root）に
        if let Some(r) = self.settings.root.clone() {
            nagi_workers::config::set(Some(&r), tool_config(&self.settings));
        }
    }

    /// ほかの窓が保存した設定を読み直して効かせる（窓が前に出たとき）。変わったら true
    pub(crate) fn reload_settings(&mut self) -> bool {
        if cfg!(test) {
            return false;
        }
        let mut new = Settings::load(self.settings.root.as_deref());
        new.rc = self.settings.rc.clone();
        new.rc_from = self.settings.rc_from.clone();
        new.session = self.settings.session.clone();
        if new == self.settings {
            return false;
        }
        let old = std::mem::replace(&mut self.settings, new);
        self.apply_settings(Some(&old));
        true
    }

    /// 保存して、ほかの窓が保存していた項目を読み込んだら（save が重ねる）それも効かせる
    pub(crate) fn save_settings(&mut self, old: &Settings) -> nagi_core::Result<()> {
        let mine = self.settings.clone();
        self.settings.save(old)?;
        if self.settings != mine {
            self.apply_settings(Some(&mine));
        }
        Ok(())
    }

    /// ドラッグで変えた欄の大きさを覚える（設定の「欄の大きさを覚える」）
    pub(crate) fn remember_panel_sizes(&mut self) {
        if !self.settings.bool("appearance.remember_panels") {
            return;
        }
        let old = self.settings.clone();
        for (k, v) in [
            ("state.left_width", self.left_w),
            ("state.right_width", self.right_w),
            ("state.bottom_height", self.bottom_h),
            ("state.log_branch_width", self.log_branch_w),
            ("state.log_detail_width", self.log_detail_w),
        ] {
            if let Some(v) = v {
                self.settings.global.insert(k.to_string(), v.to_string());
            }
        }
        if let Err(e) = self.save_settings(&old) {
            self.notify.error(format!("欄の大きさを覚えられません: {e}"));
        }
    }

    /// ヤンク・削除をいつもクリップボードへ送るか（.ideavimrc の clipboard=unnamed か、設定の「クリップボード」）
    pub(crate) fn sync_clipboard(&mut self) {
        // 試験では OS のクリップボードを触らない（試験が入れた偽物のまま）
        if cfg!(test) {
            return;
        }
        let on = self.settings.get("vim.clipboard") == "unnamed";
        self.clipboard = on.then_some(crate::editor::Clipboard { get: crate::clipboard::get, set: crate::clipboard::set });
    }

    /// "+ と "* のクリップボード（いつも OS。試験では入れた偽物だけ）
    pub(crate) fn plus_clipboard(&self) -> Option<crate::editor::Clipboard> {
        if cfg!(test) {
            return self.clipboard;
        }
        Some(self.clipboard.unwrap_or(crate::editor::Clipboard { get: crate::clipboard::get, set: crate::clipboard::set }))
    }
}

fn write(p: &Path, text: &str) -> nagi_core::Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    std::fs::write(p, text).context(format!("{}", p.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_sound() {
        let mut keys = std::collections::HashSet::new();
        for it in items() {
            assert!(keys.insert(it.key), "キーが重なっている: {}", it.key);
            assert!(CATEGORIES.contains(&it.cat), "分類が無い: {}", it.cat);
            match it.kind {
                SKind::Bool => assert!(matches!(it.default, "true" | "false"), "{}", it.key),
                SKind::Int(lo, hi) => {
                    let v: i64 = it.default.parse().unwrap();
                    assert!(v >= lo && v <= hi, "{}", it.key);
                }
                SKind::Choice(c) => assert!(c.iter().any(|x| x.0 == it.default), "{}", it.key),
                _ => {}
            }
        }
    }

    /// 保存は変えた項目だけをファイルに重ねる（ほかの窓が先に保存した項目を、この窓の古い値で消さない）
    #[test]
    fn save_merges_only_changes() {
        let mut disk = HashMap::from([("a".to_string(), "other".to_string()), ("b".to_string(), "1".to_string()), ("c".to_string(), "1".to_string())]);
        // この窓: 読んだときは a=old, b=1, c=1。b を 2 に、c を消した（a はほかの窓が other にしたが、この窓は古いまま）
        let before = HashMap::from([("a".to_string(), "old".to_string()), ("b".to_string(), "1".to_string()), ("c".to_string(), "1".to_string())]);
        let after = HashMap::from([("a".to_string(), "old".to_string()), ("b".to_string(), "2".to_string())]);
        merge(&mut disk, &before, &after);
        assert_eq!(disk.get("a").map(String::as_str), Some("other"), "変えていない項目はほかの窓の値のまま");
        assert_eq!(disk.get("b").map(String::as_str), Some("2"));
        assert!(!disk.contains_key("c"), "消した項目は消す");
    }

    #[test]
    fn layering_and_round_trip() {
        let dir = std::env::temp_dir().join(format!("nagi-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = Settings::load(Some(&dir));
        let before = s.clone();
        assert_eq!(s.int("editor.indent_width"), 4);
        s.set("editor.indent_width", "2", false);
        s.set("editor.indent_width", "8", true);
        // プロジェクトの上書きが勝つ。上書きできない項目は全体の値
        assert_eq!(s.int("editor.indent_width"), 8);
        s.set("editor.scroll_ms", "0", true);
        assert_eq!(s.int("editor.scroll_ms"), 110, "上書きできない項目は全体の値");
        // 範囲の外は端に
        s.set("appearance.line_height", "500", false);
        assert_eq!(s.int("appearance.line_height"), 200);
        s.save(&before).unwrap();
        let text = std::fs::read_to_string(project_path(&dir)).unwrap();
        assert!(text.contains("editor.indent_width = 8"), "{text}");
        let again = Settings::load(Some(&dir));
        assert_eq!(again.project.get("editor.indent_width").map(String::as_str), Some("8"));
        // 既定値に戻すと全体からは消える
        s.set("editor.indent_width", "4", false);
        assert!(!s.global.contains_key("editor.indent_width"));
        assert_eq!(render(&HashMap::from([("x.unknown".to_string(), "1".to_string())]), "h").lines().last(), Some("x.unknown = 1"), "知らないキーも残す");
    }

    /// ソースの中で読んでいる設定のキー（settings.get("…") など）が、すべて表（SCHEMA）にある（打ち間違いを見つける。ADR 0016）
    #[test]
    fn keys_in_source_exist() {
        let re = regex::Regex::new(r#"settings\.(?:get|bool|text|list|int)\("([a-z_.]+)"\)"#).unwrap();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut bad = Vec::new();
        let mut n = 0;
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let text = std::fs::read_to_string(e.path()).unwrap_or_default();
            for c in re.captures_iter(&text) {
                n += 1;
                if schema(&c[1]).is_none() {
                    bad.push(format!("{}: {}", e.file_name().to_string_lossy(), &c[1]));
                }
            }
        }
        assert!(n > 30, "読んでいるキーを見つけられない（{n}）");
        assert!(bad.is_empty(), "表に無いキー: {bad:?}");
    }
}
