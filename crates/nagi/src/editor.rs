//! 1 ファイル分の編集状態とキー処理。
//!
//! Normal / Visual モードのキーは、App がマッピング（nagi-vim の Matcher）を通してから渡す。
//! ここに届くキーは、すでにマッピングを展開した後のもの。Normal / Visual の実行は normal.rs。

use std::collections::HashMap;
use std::path::PathBuf;

use nagi_core::textops::{Finder, compile_search, find_matches, find_matches_cancellable, find_matches_in, next_match};
use nagi_core::{Buffer, Cursor, History, Motion, byte_at_col, display_col};
use nagi_vim::Key;
use unicode_segmentation::UnicodeSegmentation;

/// 行番号の欄の幅（セル）
pub const GUTTER: usize = 6;

/// 入力の状態の見え方（Vim の状態と、向いている部品を合わせたもの）。持つのは `vim`（Mode）と `focus`（Panel）で、
/// これは `Editor::state()` が導く。変えるときは `Editor::set_state`
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Normal,
    Insert,
    /// `line` は行単位（V）、`block` は矩形（Ctrl-v）
    Visual { line: bool, block: bool },
    /// `:` のコマンドライン
    Command,
    /// `/` `?` の検索（最下行）
    Search,
    /// 置換（最下行の 2 行）
    Replace,
    /// ファジーファインダー（最下行に検索語、その上に候補）
    Finder,
    /// 全文検索の検索語を最下行で打っている
    GrepPrompt,
    /// パネル（全文検索の結果）にいる（リストモード）
    List,
    /// コミットのメッセージの欄
    CommitMsg,
    /// 左右並べの差分の画面
    Diff,
    /// 履歴（グラフ付きのログ）の画面
    Log,
    /// ブランチのポップアップ
    Branches,
    /// テーブルのデータの編集
    DbTable,
    /// 下の欄のターミナルにキーを送っている
    Terminal,
    /// ランチャー（起動時のプロジェクトの選択）
    Launcher,
    /// 実行の構成の画面（run_dialog.rs）
    RunDialog,
    /// 設定の画面（settings_dialog.rs）
    Settings,
    Menu,
}

/// 見え方の状態を、持つもの（Vim の状態, 向いている部品）に分ける
pub fn split_state(s: State) -> (Mode, Option<Panel>) {
    match s {
        State::Normal => (Mode::Normal, None),
        State::Insert => (Mode::Insert, None),
        State::Visual { line, block } => (Mode::Visual { line, block }, None),
        State::Command => (Mode::Command, None),
        State::Search => (Mode::Search, None),
        State::Replace => (Mode::Replace, None),
        State::Finder => (Mode::Normal, Some(Panel::Finder)),
        State::GrepPrompt => (Mode::Normal, Some(Panel::GrepPrompt)),
        State::List => (Mode::Normal, Some(Panel::List)),
        State::CommitMsg => (Mode::Normal, Some(Panel::CommitMsg)),
        State::Diff => (Mode::Normal, Some(Panel::Diff)),
        State::Log => (Mode::Normal, Some(Panel::Log)),
        State::Branches => (Mode::Normal, Some(Panel::Branches)),
        State::DbTable => (Mode::Normal, Some(Panel::DbTable)),
        State::Terminal => (Mode::Normal, Some(Panel::Terminal)),
        State::Launcher => (Mode::Normal, Some(Panel::Launcher)),
        State::RunDialog => (Mode::Normal, Some(Panel::RunDialog)),
        State::Settings => (Mode::Normal, Some(Panel::Settings)),
        State::Menu => (Mode::Normal, Some(Panel::Menu)),
    }
}

/// Vim の状態
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Normal,
    Insert,
    /// `line` は行単位（V）、`block` は矩形（Ctrl-v）
    Visual { line: bool, block: bool },
    /// `:` のコマンドライン
    Command,
    /// `/` `?` の検索
    Search,
    /// 置換
    Replace,
}

/// 向いている部品（component.rs の Component の実装と対応する）
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Panel {
    Finder,
    GrepPrompt,
    /// 一覧（木・結果・問題・ログ・コミット・右の欄。どれかは list_side と bottom_kind で決まる）
    List,
    CommitMsg,
    Diff,
    Log,
    Branches,
    DbTable,
    Terminal,
    Launcher,
    RunDialog,
    Settings,
    /// 右クリックのメニュー（menu.rs）
    Menu,
}

/// リストモードでいるパネル
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    /// ファイルツリー
    Left,
    /// 全文検索の結果
    Bottom,
    /// コミットのパネル
    Commit,
    /// 実行の構成の欄（左の欄）
    Run,
    /// 右の欄（データベース・Maven）
    Right,
}

pub enum Effect {
    None,
    Quit,
    /// 設定（vimrc）を読み直す
    ReloadRc,
    /// `:NagiStats`（`!` 付きは記録を消す）
    Stats { reset: bool },
    /// 「名前を付けて保存」のダイアログを出す（App が出し、選んだパスで保存する）
    SaveDialog,
}

/// OS のクリップボード（`set clipboard+=unnamedplus` のとき無名レジスタの代わりに使う）
#[derive(Clone, Copy)]
pub struct Clipboard {
    pub get: fn() -> Option<String>,
    pub set: fn(&str),
}

/// 1 行の入力欄（コマンドライン・検索語・置換後）
#[derive(Default, Clone)]
pub struct Prompt {
    pub text: String,
    /// 入力欄の中のカーソル（byte）
    pub cursor: usize,
}

impl Prompt {
    pub fn set(&mut self, s: &str) {
        self.text = s.to_string();
        self.cursor = self.text.len();
    }

    pub fn insert(&mut self, s: &str) {
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    /// 編集キーを処理する。処理したら true
    pub(crate) fn key(&mut self, k: Key) -> bool {
        let prev = |t: &str, i: usize| t[..i].chars().next_back().map_or(0, char::len_utf8);
        match k {
            Key::Char(c) => self.insert(c.encode_utf8(&mut [0; 4])),
            Key::Backspace if self.cursor > 0 => {
                let n = prev(&self.text, self.cursor);
                self.cursor -= n;
                self.text.replace_range(self.cursor..self.cursor + n, "");
            }
            Key::Delete if self.cursor < self.text.len() => {
                let n = self.text[self.cursor..].chars().next().map_or(0, char::len_utf8);
                self.text.replace_range(self.cursor..self.cursor + n, "");
            }
            // Ctrl-u: カーソルより前を消す（`:<C-u>` は回数や範囲の自動入力を消す定番）
            Key::Ctrl('u') => {
                self.text.drain(..self.cursor);
                self.cursor = 0;
            }
            Key::Ctrl('w') => {
                let before = &self.text[..self.cursor];
                let start = before.trim_end().rfind(char::is_whitespace).map_or(0, |i| i + 1);
                self.text.replace_range(start..self.cursor, "");
                self.cursor = start;
            }
            Key::Left => self.cursor -= prev(&self.text, self.cursor),
            Key::Right => self.cursor += self.text[self.cursor..].chars().next().map_or(0, char::len_utf8),
            Key::Home | Key::Ctrl('a') => self.cursor = 0,
            Key::End | Key::Ctrl('e') => self.cursor = self.text.len(),
            _ => return false,
        }
        true
    }
}

/// 一致を数えた対象（バッファの版, 検索語, 正規表現か）
type SearchKey = (u64, String, bool);

/// 裏で全件を数えている検索。落とすと（検索語が変わると）数えるのをやめる（Task を捨てると取り消し）
struct CountJob {
    key: SearchKey,
    rx: nagi_core::task::Task<Vec<(usize, usize)>>,
}

/// 先頭部分だけを表示している間の最下行
const LOADING: &str = "読み込み中…";

/// これより小さいファイルは、全件をその場で数える
const SYNC_COUNT_LIMIT: usize = 1 << 20;

/// incsearch で 1 打ごとに探す範囲（カーソルの前後）。これより遠い一致は、裏で数えた全件から決める
const INCSEARCH_REACH: usize = 1 << 20;

/// 検索・置換の状態
#[derive(Default)]
pub struct Search {
    pub forward: bool,
    /// 正規表現として扱うか（Alt+R で切り替え）
    pub regex: bool,
    /// 入力を始めたときのカーソルと表示位置（Esc で戻す）
    origin: Option<(Cursor, usize)>,
    /// すべての一致（絶対 byte の範囲）。`computed` のバッファと検索語に対して数えたもの。
    /// 件数（3/12）と全件置換にだけ使う。移動先は `Finder::next` で近くから探す
    pub matches: Vec<(usize, usize)>,
    /// 今の一致（絶対 byte の範囲）
    pub current: Option<(usize, usize)>,
    computed: Option<SearchKey>,
    /// 裏で数えている全件（大きなファイルでは数十〜百 ms 以上かかるので、打鍵を待たせない）
    job: Option<CountJob>,
    /// 全件が届いたら最下行の件数を出し直す
    count_message: bool,
    /// incsearch で近くに一致が無かった（全件が届いたら、そこから移動先を決める）
    move_when_counted: bool,
    /// 一致が無かった検索語（バッファの版, 語）。正規表現でない語は、これを打ち足しても一致しないので探さない
    no_match: Option<(u64, String)>,
    /// 最後に確定した検索（検索語, 正規表現か, 下方向か）。n / N / hlsearch 用
    pub last: Option<(String, bool, bool)>,
    /// 一致を強調するか（hlsearch。:noh で消える）
    pub highlight: bool,
}

pub struct Editor {
    pub buf: Buffer,
    pub(crate) hist: History,
    pub cursor: Cursor,
    /// 画面最上段の行
    pub top: usize,
    /// スムーズスクロール中の、行に満たない分（0.0〜1.0 行。描画は top + frac から始める）
    pub frac: f32,
    /// 画面の左端の列（表示の列。折り返さないとき、長い行の右を見るために送る）
    pub left: usize,
    /// 左端をカーソルに合わせたときのカーソル（行, byte）。動いていなければ合わせ直さない（横のホイールで送った位置を保つ）
    left_at: (usize, usize),
    /// Vim の状態（Normal・Insert・Visual・コマンドライン・検索・置換）
    pub vim: Mode,
    /// 向いている部品（パネル・ダイアログ。None ならエディタ）。component::focused が引く
    pub focus: Option<Panel>,
    /// 右クリックのメニュー（menu.rs）
    pub(crate) menu: Option<crate::menu::Menu>,
    /// Vim のキー操作（設定の vim.enabled。オフなら普通のエディタ。plain.rs）
    pub(crate) vim_on: bool,
    /// Vim を使わないときの、取り消しをまとめている操作（plain.rs）
    pub(crate) plain_run: crate::plain::Run,
    /// 状態の行の文・知らせの履歴（notify.rs。self.notify.info・warn・error で書く）
    pub notify: crate::notify::Notify,
    pub cmdline: Prompt,
    /// 検索語（置換のときも）と置換後
    pub query: Prompt,
    pub repl: Prompt,
    /// 置換の入力欄のどちらにいるか（0: 検索語, 1: 置換後）
    pub replace_field: usize,
    pub search: Search,
    /// IME の変換中の文字列と、その中のカーソル（byte 範囲）
    pub preedit: String,
    pub preedit_cursor: Option<(usize, usize)>,
    /// レジスタ（文字列, 行単位か）
    pub(crate) registers: HashMap<char, (String, bool)>,
    pub clipboard: Option<Clipboard>,
    /// 最後に変更した位置（絶対 byte。g; 用）
    pub(crate) last_change: Option<usize>,
    /// 最近の編集箇所（ファイル, 行。新しいものが後ろ。IntelliJ の RecentLocations）
    pub(crate) recent_edits: Vec<(std::path::PathBuf, usize)>,
    /// EasyMotion の印を出している・文字を待っている（easy.rs）
    pub(crate) easy: Option<crate::easy::Easy>,
    /// 閉じたファイル（ファイル, 行, byte。新しいものが後ろ。ReopenClosedTab）
    pub(crate) closed_files: Vec<(std::path::PathBuf, usize, usize)>,
    /// マッピングの続きを待っているキー（showcmd。App が描画前に入れる）
    pub showcmd: String,
    /// 構文木（対応言語のときだけ）
    pub syntax: Option<nagi_syntax::Syntax>,
    /// 大きなファイルの全体（先頭部分だけを先に表示している間。届いたら `poll_load` で差し替える）
    loading: Option<nagi_core::task::Task<Buffer>>,
    /// 裏で解析中の構文木（届いたら `poll_syntax` で受け取る）
    pub(crate) pending_syntax: Option<nagi_core::task::Task<nagi_syntax::Syntax>>,
    /// 裏の解析が終わったときに UI スレッドを起こす（App が入れる）
    pub waker: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
    /// Normal / Visual で解釈中のキー列
    pub pending_keys: Vec<Key>,
    /// Visual の起点（絶対 byte）
    pub(crate) anchor: usize,
    /// 最後の f / t（; と , 用）
    pub(crate) last_find: Option<(char, bool, bool)>,
    /// `.` で繰り返すキー列と、記録中のキー列
    pub(crate) dot: Vec<Key>,
    pub(crate) dot_rec: Option<Vec<Key>>,
    pub(crate) replaying: bool,
    /// 画面の行数（App が描画前に入れる）
    pub rows: usize,
    /// 開いているバッファ（作業中のものは `Slot::Active` で、中身はこの構造体の欄にある）
    pub(crate) docs: Vec<crate::buffers::Slot>,
    pub(crate) cur_doc: usize,
    /// 直前のバッファ（`Ctrl-^` `:b#`）
    pub(crate) alt_doc: Option<usize>,
    /// 最近使ったバッファ（先頭が最近。構文木を持つものを選ぶ。buffers.rs の trim_trees）
    pub(crate) doc_recent: Vec<usize>,
    /// 窓（作業中のものは `WinSlot::Active` で、カーソルと表示位置はこの構造体の欄にある）
    pub(crate) wins: Vec<crate::panes::WinSlot>,
    pub(crate) cur_win: usize,
    /// 直前の窓（`Ctrl-w p`）
    pub(crate) prev_win: Option<usize>,
    pub(crate) layout: crate::panes::Layout,
    /// 画面の本文の範囲（列, 行）。最下行は含まない（App が描画前に入れる）
    pub(crate) screen: (usize, usize),
    pub finder: Option<crate::finder::Finder>,
    /// ランチャー（起動時のプロジェクトの選択）
    pub launcher: Option<crate::launcher::Launcher>,
    /// Markdown のプレビュー（右の欄）
    pub md_preview: Option<crate::md_preview::MdPreview>,
    /// Markdown のハイライトの覚え（(パス, 版) → 範囲）。描くたびに解析し直さない
    pub(crate) md_cache: std::cell::RefCell<Vec<(std::path::PathBuf, u64, std::rc::Rc<Vec<(std::ops::Range<usize>, nagi_syntax::Hl)>>)>>,
    /// 全文検索のパネル（開いている間だけ）
    pub grep: Option<crate::grep_panel::GrepPanel>,
    /// ファイルツリー（開いている間だけ）
    pub tree: Option<crate::tree_panel::TreePanel>,
    /// リストモードでいるパネル
    pub list_side: Side,
    /// マウスを寄せていて出しているストライプ（App がマウスの位置から入れる）
    pub stripe: Option<Side>,
    /// 一度集めたプロジェクトのファイル一覧（根 → 相対パス）
    pub(crate) file_cache: HashMap<PathBuf, Vec<String>>,
    /// ファイルの見張り（外部での変更と設定ファイル）
    pub(crate) watcher: Option<nagi_workers::watch::Watcher>,
    pub(crate) rc_files: Vec<PathBuf>,
    pub(crate) reloads: crate::autoread::Reloads,
    /// 作業中のバッファのマーク
    pub(crate) marks: crate::marks::Marks,
    /// 大文字のマーク（ファイルをまたぐ）
    pub(crate) global_marks: crate::marks::GlobalMarks,
    /// ジャンプリスト（バッファ, 位置）と、`Ctrl-o` / `Ctrl-i` で今いるところ
    pub(crate) jumps: Vec<(usize, Cursor)>,
    pub(crate) jump_idx: usize,
    /// ジャンプとして記録しない間（窓の移動や、ジャンプリストでの移動）
    pub(crate) jump_lock: bool,
    /// `:g` と `:normal` が 1 行ずつ処理する行の目印（入れ子の段ごと。編集でずらし、消えた行は None）
    pub(crate) line_anchors: Vec<Vec<Option<usize>>>,
    pub(crate) in_global: bool,
    /// 直前の :s（& で繰り返す）
    pub(crate) last_sub: Option<crate::ex_cmds::LastSub>,
    /// git（status・HEAD の版・差分・ポップアップ）
    pub(crate) git: crate::git_ui::GitState,
    /// 左の欄・下のパネルを隠しているか（中身は残す）
    pub left_hidden: bool,
    /// 開いているファイルに木を合わせる（:TreeSync で切り替え。既定は合わせる）
    pub tree_sync: bool,
    pub bottom_hidden: bool,
    /// 右の欄（データベース・Maven）と、下のパネルのクエリの結果
    pub db: Option<crate::db_panel::DbPanel>,
    pub maven: Option<crate::maven_panel::MavenPanel>,
    pub db_results: crate::result_grid::DbResults,
    /// テーブルのデータの編集（開いている間だけ）
    pub db_table: Option<crate::db_table::TableEditor>,
    pub bottom_kind: crate::result_grid::BottomKind,
    /// 検索とログのうち、出していない方（下の欄のタブで入れ替える）
    pub grep_other: Option<crate::grep_panel::GrepPanel>,
    /// 下の欄の「問題」
    pub problems: crate::bottom_panel::ProblemsView,
    /// 下の欄の「ターミナル」の窓たち
    pub terminals: crate::terminal::Terminals,
    /// 実行の構成: 選んでいるもの・最後に動かしたもの・一覧に出しているもの（run_configs.rs）
    pub run_selected: Option<String>,
    pub run_last: Option<String>,
    pub run_list: Vec<nagi_workers::runconfig::RunConfig>,
    pub run_dialog: Option<crate::run_dialog::RunDialog>,
    /// 設定（settings.rs）
    pub settings: crate::settings::Settings,
    /// この窓の作業フォルダ（開いたプロジェクトの根。起動したときはそのフォルダ）。プロセスの作業フォルダは
    /// 窓をまたいで 1 つなので使わない（窓ごとにプロジェクトが違う）。打ったパスの相対もここから
    pub cwd: PathBuf,
    /// この窓のハンドル（ダイアログの持ち主にする。0 なら無し: 試験）
    pub hwnd: isize,
    pub settings_dialog: Option<crate::settings_dialog::SettingsDialog>,
    /// 入力補完を自動で出す時刻（設定の「打っている間に自動で出す」）
    pub auto_complete_at: Option<std::time::Instant>,
    /// 補完で、まだ届いていない情報（SQL の列・テーブル、読み込み中のライブラリの索引）を待っている (何を, 打っている語の始まり)。
    /// 届いたら一覧を作り直す
    pub(crate) data_wait: Option<(crate::smart_complete::DataWait, usize)>,
    /// マウスで決めた操作の、App にしてほしいこと（押した後に App が取り出す。キーなら key の戻り値で返す）
    pub(crate) click_effect: Option<Effect>,
    /// 設定の外観（フォント・大きさ・行の高さ）が変わった（App がフォントを作り直す）
    pub appearance_changed: bool,
    /// Vim の割り当て（設定の画面に出す。vim_maps.rs）: 今効いているもの、keys.vim を除いたもの（nagi の既定と vimrc）、leader
    pub vim_maps: Vec<crate::vim_maps::MapRow>,
    pub vim_maps_base: Vec<crate::vim_maps::MapRow>,
    pub vim_leader: String,
    /// 割り当てを書いたので vimrc を読み直してほしい（App が見る）
    pub rc_dirty: bool,
    /// JDK のダウンロード（版, 進み具合）
    pub jdk_download: Option<(u32, nagi_workers::drivers::Download)>,
    /// 言語のツールのダウンロード（種類）
    pub tool_download: Option<(String, nagi_workers::drivers::Download)>,
    /// 足りないツール（Maven・JDK）を入れ終わったら動かすもの。入れた後に動かし直したか（2 度目は入れに行かない）
    pub(crate) after_tools: Option<crate::settings_dialog::AfterTools>,
    pub(crate) tools_fetched: bool,
    /// 文法のダウンロード（言語の名前）と、失敗したもの（開くたびにやり直さない。:GrammarInstall でやり直す）
    pub grammar_downloads: Vec<(String, nagi_workers::drivers::Download)>,
    pub grammar_failed: std::collections::HashSet<String>,
    pub right_tab: crate::right_panel::RightTab,
    pub right_hidden: bool,
    pub right_w: Option<usize>,
    pub(crate) right_pending_w: bool,
    /// ドラッグで変えた左の欄の幅・下のパネルの高さ（None は既定の大きさ）
    pub left_w: Option<usize>,
    /// 履歴の左のブランチの列・右の中身の列の幅（ドラッグで変えたとき）
    pub log_branch_w: Option<usize>,
    pub log_detail_w: Option<usize>,
    pub bottom_h: Option<usize>,
    /// 最後に描いた入力欄の縦棒カーソル（列, y の px）。IME の候補の窓の位置に使う
    pub prompt_caret: std::cell::Cell<Option<(usize, f32)>>,
    /// マウスを載せている SQL コンソールの帯のボタン
    pub hover_bar: Option<crate::db_panel::BarItem>,
    /// マウスを載せているバッファのタブ（バッファの番号, × の上か）
    pub hover_buftab: Option<(usize, bool)>,
    /// マウスを載せているボタン
    pub hover_button: Option<crate::toggles::Button>,
    /// 部品を左クリックしている間の修飾キー（Ctrl・Shift+クリックで一覧の複数を選ぶ）。押した間だけ App が入れる
    pub click_mods: nagi_vim::Mods,
    /// マウスを載せているタブのアイコン（行, 列, 名前）と、描いたアイコンの位置
    pub hover_tab: Option<(usize, usize, &'static str)>,
    /// 状態行の「▶ 構成の名前」の列（始め, 終わり）。押すと実行のメニュー
    pub run_hit: std::cell::Cell<Option<(usize, usize)>>,
    /// 状態の行の文字コード・改行の列（押すとメニュー）
    pub enc_hit: std::cell::Cell<Option<(usize, usize)>>,
    pub tab_hits: std::cell::RefCell<Vec<(usize, usize, usize, &'static str)>>,
    /// `--replay` の試験: 描いた文字を控える（行, 列, 文字）。replay.rs が押す所・待つ文字を探す
    pub record_texts: bool,
    pub drawn_texts: std::cell::RefCell<Vec<(usize, usize, String)>>,
    /// 左の欄で出しているタブ（ツリーとコミットのパネルが両方開いているとき）
    pub left_tab: crate::tree_panel::LeftTab,
    /// ブランチのポップアップ（開いている間だけ）
    pub branch_popup: Option<crate::branches::BranchPopup>,
    /// 裏で動かしているフェッチ・pull（見出し, 結果）
    pub git_job: Option<(String, nagi_core::task::Task<nagi_core::Result<String>>)>,
    /// 履歴の画面（開いている間だけ）
    pub log: Option<crate::log_view::LogView>,
    /// blame（出している間だけ）
    pub blame: Option<crate::blame::Blame>,
    /// コミットのパネル（開いている間だけ）
    pub commit: Option<crate::commit_panel::CommitPanel>,
    /// 実行の構成の欄（左の欄の「実行」タブ）
    pub run_panel: Option<crate::run_panel::RunPanel>,
    /// 左右並べの差分（開いている間だけ）と、閉じたときに戻るモード
    pub diff: Option<crate::diff_view::DiffView>,
    pub(crate) diff_return: Option<State>,
    pub(crate) diff_pending: Option<char>,
    /// 動いている診断のコマンドと、その結果
    pub(crate) make: Option<crate::diagnostics::MakeState>,
    pub diags: Vec<crate::diagnostics::Diag>,
    /// `:` のコマンドラインの候補
    pub cmd_complete: Option<crate::cmdline_complete::CmdComplete>,
    /// `:e` などの候補のために裏で集めているファイルの一覧（根, 途中, 集めた分）
    pub(crate) cmd_files: Option<(PathBuf, nagi_workers::files::FileWalk, Vec<String>)>,
    /// 補完の途中（Insert の Ctrl-n / Ctrl-p）
    pub completion: Option<crate::complete::Completion>,
    /// プロジェクトの定義の索引
    pub(crate) nav: crate::nav::IndexState,
    /// 作業中のバッファの閉じた折りたたみ
    pub(crate) folds: crate::folds::Folds,
    /// 矩形の I A c で、Insert を抜けたときにほかの行へ入れる約束
    pub(crate) block_insert: Option<crate::block::BlockInsert>,
    /// 矩形で取ったレジスタ（レジスタ → 矩形の幅）と、次に書くレジスタを矩形にする印
    pub(crate) reg_blocks: HashMap<char, usize>,
    pub(crate) reg_block_next: Option<usize>,
    /// 記録中のマクロ（レジスタ, 打ったキー）
    pub recording: Option<(char, Vec<Key>)>,
    /// 直前に再生したマクロ（`@@`）
    pub(crate) last_macro: Option<char>,
    /// 再生の入れ子の深さ（再生中のキーは記録しない。深すぎる再帰は止める）
    pub(crate) macro_depth: usize,
}

impl Editor {
    /// キーマップの組み合わせで `cmd`（コマンドの表の名前）を動かす。入力欄の最中や、ダイアログ・ファインダーを
    /// 出している間は動かさない（None。キーはその部品へ）。Insert・Visual は抜けてから
    pub fn shortcut(&mut self, cmd: &str) -> Option<Effect> {
        let ok = !self.has_prompt() && matches!(self.state(), State::Normal | State::Visual { .. } | State::Insert | State::List | State::CommitMsg | State::Log | State::Terminal | State::Diff);
        if !ok {
            return None;
        }
        // Vim のときは Insert・Visual を抜けてから。使わないときは選んだ所を保つ（コピー・貼り付けなどが使う）
        if self.vim_on && matches!(self.state(), State::Insert | State::Visual { .. }) {
            self.key(Key::Esc);
        }
        self.plain_break();
        Some(crate::commands::run(self, cmd))
    }

    /// 裏の処理から届いたものを全部受け取る（UI が起こされたとき）。どれかが変われば true。
    /// 裏の処理を足したら、その受け取りをここに 1 行足す（ADR 0016 の 1）
    pub fn poll_background(&mut self) -> bool {
        let mut changed = false;
        if self.poll_load() {
            crate::clock::mark_once("file: full buffer swapped in");
            changed = true;
        }
        for poll in [
            Self::poll_search,
            Self::poll_finder,
            Self::poll_launcher,
            Self::poll_grep,
            Self::poll_tree,
            Self::poll_index,
            Self::poll_make,
            Self::poll_jdk_download,
            Self::poll_grammars,
            Self::poll_cmd_files,
            Self::poll_git,
            Self::poll_commit,
            Self::poll_blame,
            Self::poll_log,
            Self::poll_branches,
            Self::poll_db,
        ] {
            changed |= poll(self);
        }
        changed
    }

    /// 入力の状態の見え方（Vim の状態と、向いている部品を合わせたもの）
    pub fn state(&self) -> State {
        let Some(p) = self.focus else {
            return match self.vim {
                Mode::Normal => State::Normal,
                Mode::Insert => State::Insert,
                Mode::Visual { line, block } => State::Visual { line, block },
                Mode::Command => State::Command,
                Mode::Search => State::Search,
                Mode::Replace => State::Replace,
            };
        };
        match p {
            Panel::Finder => State::Finder,
            Panel::GrepPrompt => State::GrepPrompt,
            Panel::List => State::List,
            Panel::CommitMsg => State::CommitMsg,
            Panel::Diff => State::Diff,
            Panel::Log => State::Log,
            Panel::Branches => State::Branches,
            Panel::DbTable => State::DbTable,
            Panel::Terminal => State::Terminal,
            Panel::Launcher => State::Launcher,
            Panel::RunDialog => State::RunDialog,
            Panel::Settings => State::Settings,
            Panel::Menu => State::Menu,
        }
    }

    /// 入力の状態を変える（部品へ向くなら Vim の状態は Normal に、Vim の状態にするなら部品から離れる）。
    /// パネルを借りている途中なら `(self.vim, self.focus) = split_state(…)` と書く（フィールドだけを借りる）
    pub fn set_state(&mut self, s: State) {
        // Vim を使わないときは Normal が無い（打てる状態へ。plain.rs）
        let s = if s == State::Normal && !self.vim_on { State::Insert } else { s };
        // 本文に向くとき（木・検索などで開いた・飛んだ、Esc で本文へ）は、本文を覆っている差分・テーブルを閉じる
        // （開いたものが覆いの下に隠れないように）。コマンドの行・検索の行から戻るときは閉じない（cmdline_back）
        let to_text = matches!(s, State::Normal | State::Insert | State::Visual { .. });
        let s = if to_text && !matches!(self.vim, Mode::Command | Mode::Search | Mode::Replace) && !self.uncover() { State::DbTable } else { s };
        (self.vim, self.focus) = split_state(s);
    }

    /// 本文を覆っている差分・テーブルを閉じる。テーブルに送っていない変更があれば閉じずに知らせて false（変更を黙って捨てない）
    fn uncover(&mut self) -> bool {
        if self.diff.take().is_some() {
            self.diff_return = None;
        }
        match self.db_table.as_ref().map(|t| t.changes()) {
            Some(0) => self.db_table = None,
            Some(n) => {
                self.notify.info(format!("テーブルに送っていない変更が {n} 件あります（Ctrl+Enter で送るか、q で捨てて閉じる）"));
                return false;
            }
            None => {}
        }
        true
    }

    /// コマンドの行・検索の行から戻る先（本文を覆う差分・テーブルがあればそこ、無ければ本文）
    pub(crate) fn cmdline_back(&self) -> State {
        if self.db_table.is_some() {
            State::DbTable
        } else if self.diff.is_some() {
            State::Diff
        } else {
            State::Normal
        }
    }

    /// UI を起こす関数（裏の処理に渡す。UI の無い試験では何もしない）
    pub(crate) fn waker(&self) -> nagi_core::task::Waker {
        self.waker.clone().unwrap_or_else(nagi_core::task::no_waker)
    }

    pub fn new(buf: Buffer, syntax: Option<nagi_syntax::Syntax>) -> Self {
        Self {
            buf,
            hist: History::default(),
            cursor: Cursor::default(),
            top: 0,
            frac: 0.0,
            left: 0,
            left_at: (usize::MAX, 0),
            vim: Mode::Normal,
            focus: None,
            menu: None,
            vim_on: true,
            plain_run: Default::default(),
            notify: Default::default(),
            cmdline: Prompt::default(),
            query: Prompt::default(),
            repl: Prompt::default(),
            replace_field: 0,
            search: Search::default(),
            preedit: String::new(),
            preedit_cursor: None,
            registers: HashMap::new(),
            clipboard: None,
            last_change: None,
            recent_edits: Vec::new(),
            easy: None,
            closed_files: Vec::new(),
            showcmd: String::new(),
            syntax,
            pending_syntax: None,
            loading: None,
            waker: None,
            pending_keys: Vec::new(),
            anchor: 0,
            last_find: None,
            dot: Vec::new(),
            dot_rec: None,
            replaying: false,
            rows: 1,
            docs: vec![crate::buffers::Slot::Active],
            cur_doc: 0,
            alt_doc: None,
            doc_recent: Vec::new(),
            wins: vec![crate::panes::WinSlot::Active],
            cur_win: 0,
            prev_win: None,
            layout: crate::panes::Layout::Leaf(0),
            screen: (80, 24),
            finder: None,
            launcher: None,
            md_cache: Default::default(),
            md_preview: None,
            tree_sync: true,
            grep: None,
            tree: None,
            list_side: Side::Bottom,
            stripe: None,
            file_cache: HashMap::new(),
            watcher: None,
            rc_files: Vec::new(),
            reloads: Default::default(),
            marks: Default::default(),
            global_marks: Default::default(),
            jumps: Vec::new(),
            jump_idx: 0,
            jump_lock: false,
            line_anchors: Vec::new(),
            in_global: false,
            last_sub: None,
            folds: Default::default(),
            nav: Default::default(),
            completion: None,
            cmd_complete: None,
            cmd_files: None,
            make: None,
            git: Default::default(),
            commit: None,
            run_panel: None,
            blame: None,
            log: None,
            branch_popup: None,
            left_tab: Default::default(),
            left_hidden: false,
            bottom_hidden: false,
            hover_button: None,
            click_mods: Default::default(),
            hover_tab: None,
            tab_hits: Default::default(),
            record_texts: false,
            drawn_texts: Default::default(),
            run_hit: Default::default(),
            enc_hit: Default::default(),
            hover_bar: None,
            hover_buftab: None,
            prompt_caret: Default::default(),
            left_w: None,
            log_branch_w: None,
            log_detail_w: None,
            db: None,
            maven: None,
            db_results: Default::default(),
            db_table: None,
            bottom_kind: Default::default(),
            grep_other: None,
            problems: Default::default(),
            terminals: Default::default(),
            run_selected: None,
            run_last: None,
            run_list: Vec::new(),
            run_dialog: None,
            settings: crate::settings::Settings::load(None),
            cwd: std::env::current_dir().unwrap_or_default(),
            hwnd: 0,
            settings_dialog: None,
            auto_complete_at: None,
            data_wait: None,
            click_effect: None,
            appearance_changed: false,
            vim_maps: Vec::new(),
            vim_maps_base: Vec::new(),
            vim_leader: "\\".into(),
            rc_dirty: false,
            jdk_download: None,
            tool_download: None,
            after_tools: None,
            tools_fetched: false,
            grammar_downloads: Vec::new(),
            grammar_failed: Default::default(),
            right_tab: Default::default(),
            right_hidden: false,
            right_w: None,
            right_pending_w: false,
            bottom_h: None,
            git_job: None,
            diff: None,
            diff_return: None,
            diff_pending: None,
            diags: Vec::new(),
            block_insert: None,
            reg_blocks: HashMap::new(),
            reg_block_next: None,
            recording: None,
            last_macro: None,
            macro_depth: 0,
        }
    }

    /// 先頭部分だけを表示しているバッファの、全体が `rx` に届くのを待つ。届いたらイベントループを起こす
    pub fn wait_full_buffer(&mut self, rx: std::sync::mpsc::Receiver<Buffer>) {
        let rx2 = nagi_core::task::spawn("full-buffer", self.waker(), move |ctx| {
            if let Ok(b) = rx.recv() {
                ctx.send(b);
            }
        });
        self.loading = Some(rx2);
        self.notify.info(LOADING);
    }

    /// 全体が届いていれば差し替える。差し替えたら true（描き直す）
    pub fn poll_load(&mut self) -> bool {
        let Some(rx) = &self.loading else { return false };
        match rx.try_recv() {
            Ok(b) => {
                self.swap_full_buffer(b);
                true
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.loading = None;
                false
            }
        }
    }

    /// 全体が届くまで待って差し替える（キーを処理する前）
    pub(crate) fn finish_load(&mut self) {
        if let Some(rx) = self.loading.take() {
            if let Ok(b) = rx.recv() {
                self.swap_full_buffer(b);
            }
        }
    }

    /// 版を鍵にした検索の計算結果を捨てる（バッファが入れ替わったとき。版の番号はバッファごとに 0 から始まる）
    pub(crate) fn reset_search_cache(&mut self) {
        self.search.computed = None;
        self.search.matches.clear();
        self.search.current = None;
        self.search.job = None;
        self.search.no_match = None;
    }

    fn swap_full_buffer(&mut self, b: Buffer) {
        self.loading = None;
        // 先頭部分は全体の頭と同じ内容なので、カーソルと表示位置はそのまま使える
        self.buf = b;
        self.reset_search_cache();
        if self.notify.text == LOADING {
            self.notify.clear();
        }
        self.start_syntax();
    }

    /// 構文解析を裏のスレッドで始める。大きなファイルでは数百 ms〜秒かかるので、表示や入力を待たせない。
    /// 解析は今の内容の複製に対して行い、その間の編集は届いた後の `sync` で差分として反映する
    pub fn start_syntax(&mut self) {
        let Some(lang) = self.buf.path.as_deref().and_then(nagi_syntax::Lang::detect) else { return };
        // 文法をまだダウンロードしていなければ取ってくる（届いたらもう一度ここへ来る）
        if !lang.available() {
            if let Some(id) = lang.grammar_id() {
                if self.settings.bool("editor.grammar_download") && !self.grammar_failed.contains(id) {
                    self.install_grammar(id);
                }
            }
            return;
        }
        let mut snap = self.buf.snapshot();
        let rx = nagi_core::task::spawn("syntax", self.waker(), move |ctx| {
            let Some(mut s) = nagi_syntax::Syntax::new(lang) else { return };
            s.parse(&mut snap);
            // ハイライトのクエリのコンパイル（大きい言語で数十 ms）もここで済ませる
            let _ = lang.highlights();
            ctx.send(s);
        });
        self.pending_syntax = Some(rx);
    }

    /// tree-sitter を使わない言語（Markdown・Sass）なら、文章全体のハイライト（版が変わったときだけ解析し直す）
    pub(crate) fn custom_spans(&self, buf: &Buffer) -> Option<std::rc::Rc<Vec<(std::ops::Range<usize>, nagi_syntax::Hl)>>> {
        let path = buf.path.as_deref()?;
        let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
        let f: fn(&str) -> Vec<(std::ops::Range<usize>, nagi_syntax::Hl)> = match ext.as_str() {
            "md" | "markdown" | "mdx" => nagi_syntax::markdown::highlight,
            "sass" => nagi_syntax::sass::highlight,
            _ => return None,
        };
        if buf.len_bytes() > 4 << 20 {
            return None;
        }
        let mut cache = self.md_cache.borrow_mut();
        if let Some((_, _, s)) = cache.iter().find(|(p, v, _)| p == path && *v == buf.version) {
            return Some(s.clone());
        }
        let spans = std::rc::Rc::new(f(&buf.text()));
        cache.retain(|(p, _, _)| p != path);
        cache.push((path.to_path_buf(), buf.version, spans.clone()));
        if cache.len() > 8 {
            cache.remove(0);
        }
        Some(spans)
    }

    /// 構文木を使うコマンドの前に: 裏で解析中なら終わるのを待ち、今の内容に合わせる
    pub(crate) fn ensure_syntax(&mut self) {
        if self.syntax.is_none() {
            if let Some(rx) = self.pending_syntax.take() {
                if let Ok(mut s) = rx.recv() {
                    s.waker = self.waker.clone();
                    self.syntax = Some(s);
                }
            }
        }
        if let Some(s) = &mut self.syntax {
            s.sync(&mut self.buf);
        }
    }

    /// 裏の構文解析が終わっていれば受け取る。受け取ったら true（描き直す）
    pub fn poll_syntax(&mut self) -> bool {
        let Some(rx) = &self.pending_syntax else { return false };
        match rx.try_recv() {
            Ok(mut s) => {
                s.waker = self.waker.clone();
                self.syntax = Some(s);
                self.pending_syntax = None;
                true
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.pending_syntax = None;
                false
            }
        }
    }

    /// 最下行に使う行数（置換は 2 行）
    pub fn status_rows(&self) -> usize {
        if self.state() == State::Replace { 2 } else { 1 }
    }

    /// 入力欄（IME の確定先）があるモードか
    pub fn has_prompt(&self) -> bool {
        matches!(self.state(), State::Command | State::Search | State::Replace | State::GrepPrompt)
            || (self.state() == State::Finder && self.finder.as_ref().is_some_and(|f| f.field.is_insert()))
            || self.state() == State::Terminal
            || (self.state() == State::RunDialog && self.run_dialog.as_ref().is_some_and(|d| d.typing()))
            || (self.state() == State::Settings && self.settings_typing())
            || (self.state() == State::Launcher && self.launcher.as_ref().is_some_and(|l| l.ask.as_ref().map_or(l.field.is_insert(), |a| a.1.is_insert())))
            || (self.state() == State::CommitMsg && self.commit.as_ref().is_some_and(|c| c.msg.is_insert()))
            || (self.state() == State::Log && self.log.as_ref().is_some_and(|l| l.filter.as_ref().is_some_and(|f| f.is_insert())))
            || (self.state() == State::Branches && self.branch_popup.as_ref().is_some_and(|b| b.naming.as_ref().is_none_or(|n| n.1.is_insert())))
            || self.db_typing()
            || self.db_table_typing()
            || (self.state() == State::List && self.list_side == Side::Bottom && self.db_results.editing.as_ref().is_some_and(|e| e.1.is_insert()))
            || (self.state() == State::List && self.list_side == Side::Bottom && self.db_results.value_edit.as_ref().is_some_and(|f| f.is_insert()))
    }

    pub(crate) fn motion(&self) -> Motion<'_> {
        Motion { buf: &self.buf, ambiguous_wide: false }
    }

    /// Motion を写しのカーソルに適用して戻す（Motion は self を借用するため）
    pub(crate) fn with_motion(&mut self, f: impl FnOnce(&Motion, &mut Cursor)) {
        let mut c = self.cursor;
        f(&self.motion(), &mut c);
        self.cursor = c;
    }

    pub(crate) fn abs(&self) -> usize {
        self.buf.line_start(self.cursor.line) + self.cursor.byte
    }

    pub(crate) fn set_abs(&mut self, abs: usize) {
        let (line, byte) = self.buf.pos_of(abs);
        self.cursor.line = line;
        self.cursor.byte = byte;
        let l = self.buf.line(line);
        self.cursor.want_col = display_col(&l, byte.min(l.len()), false);
    }

    /// Normal ではカーソルを最後の書記素より右に置かない
    pub(crate) fn clamp_normal(&mut self) {
        let line = self.buf.line(self.cursor.line);
        if self.cursor.byte >= line.len() {
            self.cursor.byte = line.grapheme_indices(true).next_back().map_or(0, |(i, _)| i);
        }
    }

    pub(crate) fn edit(&mut self, start: usize, end: usize, text: &str) {
        self.shift_marks(start, end, text);
        let c = self.buf.edit(start, end, text);
        self.hist.record(c);
        self.last_change = Some(start);
        self.note_edit_place(start);
    }

    /// 最近の編集箇所に足す（同じファイルの近く（3 行以内）なら置き換える。50 件まで）
    fn note_edit_place(&mut self, at: usize) {
        let Some(p) = self.buf.path.clone() else { return };
        let line = self.buf.pos_of(at).0;
        if let Some(last) = self.recent_edits.last_mut().filter(|(lp, ll)| *lp == p && ll.abs_diff(line) <= 3) {
            last.1 = line;
            return;
        }
        self.recent_edits.retain(|(lp, ll)| !(*lp == p && ll.abs_diff(line) <= 3));
        self.recent_edits.push((p, line));
        if self.recent_edits.len() > 50 {
            self.recent_edits.remove(0);
        }
    }

    pub(crate) fn enter_insert(&mut self) {
        self.hist.begin();
        self.set_state(State::Insert);
        self.notify.clear();
    }

    pub(crate) fn leave_insert_pub(&mut self) {
        self.leave_insert();
    }

    fn leave_insert(&mut self) {
        self.completion = None;
        // 矩形の I A c: 打った文字列をほかの行へ（同じ undo のグループに入れる）
        let placed = self.finish_block_insert();
        self.hist.close();
        self.set_state(State::Normal);
        self.preedit.clear();
        self.preedit_cursor = None;
        // Vim と同じく、Insert を抜けるとカーソルは 1 つ左へ（矩形の I A は左上の角に置いた）
        if self.cursor.byte > 0 && !placed {
            self.with_motion(|m, c| m.left(c, 1));
        }
        self.clamp_normal();
        self.finish_dot();
    }

    /// 文字列を挿入する。改行はこのファイルの改行コードにする
    pub fn insert_text(&mut self, s: &str) {
        self.finish_load();
        let nl = self.buf.newline();
        let text: String = s.replace("\r\n", "\n").replace('\r', "\n").replace('\n', nl);
        let at = self.abs();
        self.edit(at, at, &text);
        self.set_abs(at + text.len());
    }

    pub(crate) fn backspace_pub(&mut self) {
        self.backspace();
    }

    fn backspace(&mut self) {
        let line = self.buf.line(self.cursor.line);
        if let Some(to) = self.soft_tab_back() {
            let ls = self.buf.line_start(self.cursor.line);
            self.edit(ls + to, self.abs(), "");
            self.set_abs(ls + to);
        } else if self.cursor.byte > 0 {
            let prev = line[..self.cursor.byte].grapheme_indices(true).next_back().map_or(0, |(i, _)| i);
            let start = self.buf.line_start(self.cursor.line) + prev;
            self.edit(start, self.abs(), "");
            self.set_abs(start);
        } else if self.cursor.line > 0 {
            // 前の行と結合する（前の行の改行を消す）
            let prev = self.cursor.line - 1;
            let brk = self.buf.line_len_with_break(prev) - self.buf.line(prev).len();
            let end = self.buf.line_start(self.cursor.line);
            self.edit(end - brk, end, "");
            self.set_abs(end - brk);
        }
    }

    pub fn key(&mut self, key: Key) -> Effect {
        self.sync_tab();
        if self.easy.is_some() {
            self.easy_key(key);
            return Effect::None;
        }
        let effect = self.key_inner(key);
        // 入力補完を打っている間に自動で出す（設定）
        if self.state() == State::Insert && self.settings.bool("completion.auto") {
            self.schedule_auto_complete(key);
        } else {
            self.auto_complete_at = None;
        }
        effect
    }

    fn key_inner(&mut self, key: Key) -> Effect {
        // 先頭部分だけを表示している間は、全体が届くのを待ってから処理する（先頭部分を編集してしまわない）
        self.finish_load();
        // 入力欄での Ctrl+V（Shift+Insert）は貼り付け。ファインダーの Ctrl+V は縦に分割して開く
        if (key == Key::Paste || (key == Key::Ctrl('v') && self.state() != State::Finder)) && self.paste_target() {
            self.paste_clipboard();
            return Effect::None;
        }
        // 表（クエリの結果・テーブルの編集）の Ctrl+C・Ctrl+V はセルのコピー・貼り付け
        if matches!(key, Key::Ctrl('c' | 'v') | Key::Paste) && self.grid_clipboard_key(key) {
            return Effect::None;
        }
        // SQL コンソールの Ctrl+Enter（Ctrl-m として届く）: カーソルのある文を流す
        if key == Key::Ctrl('m') && matches!(self.state(), State::Normal | State::Insert | State::Visual { .. }) && self.console_source().is_some() {
            self.db_run_at_cursor();
            return Effect::None;
        }
        // キーマップの組み合わせ（Alt+1・F12 など。再生・マクロ・試験で届くもの）: 付けたコマンドを動かす
        if let Some(crate::keymap::Action::Cmd(c)) = crate::keymap::chord_of_key(key).and_then(|ch| crate::keymap::action_for(&self.settings, &ch)) {
            if let Some(eff) = self.shortcut(c) {
                return eff;
            }
        }
        // マクロの記録を止める q（Normal と Visual で、ほかのキーの途中でないとき）
        if self.recording.is_some()
            && self.macro_depth == 0
            && key == Key::Char('q')
            && matches!(self.state(), State::Normal | State::Visual { .. })
            && self.pending_keys.is_empty()
        {
            self.stop_recording();
            return Effect::None;
        }
        if self.macro_depth == 0 {
            if let Some((_, ks)) = &mut self.recording {
                ks.push(key);
            }
        }
        if !self.replaying {
            if let Some(rec) = &mut self.dot_rec {
                rec.push(key);
            }
        }
        // 変更のポップアップを開いている間の u s Esc
        if self.state() == State::Normal && self.hunk_popup_key(key) {
            self.scroll_into_view();
            return Effect::None;
        }
        // Vim を使わないとき、データベースの木で打った文字は絞り込みの欄へ
        if self.db_type_to_filter(key) {
            return Effect::None;
        }
        // Vim を使わないとき、一覧のパネル・ダイアログは文字のキーを受けない（plain.rs）
        if self.plain_drops(key) {
            return Effect::None;
        }
        // 修飾（Shift・Ctrl）つきの移動を見るのは、Vim を使わないときの本文だけ。ほかは修飾を外して今まで通りに
        let body = crate::component::focused(self).is_none() && matches!(self.state(), State::Insert | State::Visual { .. });
        let key = if !self.vim_on && body { key } else { key.base() };
        if !self.vim_on && body && self.plain_key(key) {
            self.scroll_into_view();
            return Effect::None;
        }
        // パネル・ダイアログなどの部品はそれぞれの Component へ（component.rs）。Vim の編集の状態はここで
        let effect = match crate::component::focused(self) {
            Some(c) => c.key(self, key),
            None => match self.state() {
                State::Insert => {
                    self.insert(key);
                    Effect::None
                }
                State::Search => {
                    self.search_key(key);
                    Effect::None
                }
                State::Replace => {
                    self.replace_key(key);
                    Effect::None
                }
                _ => self.normal_key(key),
            },
        };
        // 検索や :{行} で閉じた折りたたみの途中へ跳んだら開く
        if !self.folds.is_empty() && self.state() == State::Normal {
            self.open_folds_at_cursor();
        }
        self.scroll_into_view();
        effect
    }

    fn insert(&mut self, key: Key) {
        // IntelliJ のような補完（Ctrl+Space）: 一覧を出している間のキーは先に扱う
        if self.completion.as_ref().is_some_and(|c| c.smart.is_some()) {
            if self.smart_key(key) {
                return;
            }
        } else if key == Key::Ctrl(' ') {
            self.completion = None;
            self.start_smart_completion();
            return;
        }
        // 補完（Ctrl-n / Ctrl-p）の途中なら先に扱う
        if (self.completion.is_some() || matches!(key, Key::Ctrl('n') | Key::Ctrl('p'))) && self.completion_key(key) {
            return;
        }
        match key {
            Key::Esc | Key::Ctrl('c') => self.leave_insert(),
            // 新しい行のインデントは今の行に合わせ、{ の後なら 1 段深く（設定の自動インデント。editor_settings.rs）
            Key::Enter => {
                let (text, back) = self.newline_text();
                // 閉じ括弧を次の行へ送るときは、カーソルと閉じ括弧の間の空白を消しておく
                if back > 0 {
                    let line = self.buf.line(self.cursor.line);
                    let ws = line[self.cursor.byte..].len() - line[self.cursor.byte..].trim_start().len();
                    let at = self.abs();
                    self.edit(at, at + ws, "");
                }
                self.insert_text(&text);
                let back = if back > 0 { back - "\n".len() + self.buf.newline().len() } else { 0 };
                self.set_abs(self.abs() - back);
            }
            Key::Backspace => self.backspace(),
            Key::Tab => self.insert_tab(),
            Key::Left => self.with_motion(|m, c| m.left(c, 1)),
            Key::Up => self.with_motion(|m, c| m.vertical(c, -1)),
            Key::Down => self.with_motion(|m, c| m.vertical(c, 1)),
            Key::Right => {
                let line = self.buf.line(self.cursor.line);
                if let Some(g) = line[self.cursor.byte..].graphemes(true).next() {
                    self.cursor.byte += g.len();
                    self.cursor.want_col = display_col(&line, self.cursor.byte, false);
                }
            }
            Key::Char(c) => {
                self.dedent_for_closer(c);
                self.insert_text(c.encode_utf8(&mut [0; 4]));
                // SQL コンソールでは「別名.」の後にすぐ列の候補を出す（列がまだ無ければ、届いたら出る）
                if c == '.' && self.console_source().is_some() {
                    let msg = self.notify.text.clone();
                    self.completion = None;
                    self.start_smart_completion();
                    if self.completion.is_none() {
                        self.notify.info(msg);
                    }
                }
            }
            _ => {}
        }
    }

    pub(crate) fn command(&mut self, key: Key) -> Effect {
        // Ctrl+Space: 候補を出し直す（打った名前と同じでも出す）
        if key == Key::Ctrl(' ') {
            let text = self.cmdline.text[..self.cmdline.cursor].to_string();
            self.cmd_complete = self.cmd_candidates_forced(&text, true);
            return Effect::None;
        }
        // 候補を選ぶキー（Tab / Ctrl-n / Ctrl-p）
        if self.cmd_complete_key(key) {
            return Effect::None;
        }
        match key {
            Key::Esc | Key::Ctrl('c') => {
                self.cmdline = Prompt::default();
                self.set_state(self.cmdline_back());
                self.cmd_complete = None;
            }
            Key::Backspace if self.cmdline.text.is_empty() => {
                self.set_state(self.cmdline_back());
                self.cmd_complete = None;
            }
            Key::Enter => {
                self.cmd_complete = None;
                let line = std::mem::take(&mut self.cmdline).text;
                self.set_state(self.cmdline_back());
                let doc = self.cur_doc;
                // `|` で区切った複数のコマンド（`source ~/.ideavimrc | echo "..."`）。:s の中の | は区切らない
                let cmds: Vec<String> = if self.keeps_bar(&line) { vec![line] } else { line.split('|').map(str::to_string).collect() };
                let mut effect = Effect::None;
                for cmd in cmds {
                    match self.ex(cmd.trim()) {
                        Effect::None => {}
                        e => effect = e,
                    }
                }
                // 差分・テーブルの上から別のファイルへ移った（:e・:b など）なら、本文を見せる
                if self.cur_doc != doc && matches!(self.state(), State::Diff | State::DbTable) {
                    self.set_state(State::Normal);
                }
                return effect;
            }
            k => {
                if self.cmdline.key(k) {
                    self.update_cmd_complete();
                }
            }
        }
        Effect::None
    }

    // ---------------------------------------------------------------- 検索・置換

    /// 今の検索語（入力中ならその語、Normal なら最後に確定した語）
    fn active_pattern(&self) -> Option<(String, bool)> {
        let (p, regex) = match self.state() {
            State::Search | State::Replace if !self.query.text.is_empty() => (self.query.text.clone(), self.search.regex),
            State::Search | State::Replace => return None,
            _ => self.search.last.as_ref().map(|(p, r, _)| (p.clone(), *r))?,
        };
        Some(self.case_pattern(p, regex))
    }

    /// 設定の ignorecase・smartcase で大文字小文字を区別するときは、正規表現の (?-i) を付ける
    /// （検索の土台は区別しない作りなので、区別するときだけ正規表現にして印を付ける）
    pub(crate) fn case_pattern(&self, p: String, regex: bool) -> (String, bool) {
        let ignore = self.settings.bool("vim.ignorecase") && !(self.settings.bool("vim.smartcase") && p.chars().any(char::is_uppercase));
        if ignore {
            (p, regex)
        } else {
            (format!("(?-i){}", if regex { p } else { regex::escape(&p) }), true)
        }
    }

    fn active_key(&self) -> Option<SearchKey> {
        self.active_pattern().map(|(p, r)| (self.buf.version, p, r))
    }

    /// 全件を（バッファか検索語が変わっていれば）数え直す。大きなファイルでは裏のスレッドで数え、
    /// 届いたら `poll_search` で受け取る
    pub fn refresh_matches(&mut self) {
        let Some(key) = self.active_key() else {
            self.search.matches.clear();
            self.search.computed = None;
            self.search.job = None;
            return;
        };
        if self.search.computed.as_ref() == Some(&key) || self.search.job.as_ref().is_some_and(|j| j.key == key) {
            return;
        }
        if self.buf.len_bytes() < SYNC_COUNT_LIMIT || self.waker.is_none() {
            self.search.matches = find_matches_in(&self.buf, &key.1, key.2);
            self.search.computed = Some(key);
            self.search.job = None;
            return;
        }
        // 裏では今の内容の複製（rope の複製は O(1)）を数える
        let snap = self.buf.snapshot();
        let (pattern, regex) = (key.1.clone(), key.2);
        let rx = nagi_core::task::spawn("search-count", self.waker(), move |ctx| {
            if let Some(ms) = find_matches_cancellable(&snap, &pattern, regex, &ctx.cancel_flag()) {
                ctx.send(ms);
            }
        });
        // 前の検索語の数えは、ここで落とすと止まる
        self.search.job = Some(CountJob { key, rx });
    }

    /// 裏で数えていた全件が届いていれば受け取る。受け取ったら true（描き直す）
    pub fn poll_search(&mut self) -> bool {
        let Some(job) = &self.search.job else { return false };
        let Ok(ms) = job.rx.try_recv() else { return false };
        let key = job.key.clone();
        self.search.job = None;
        self.search.matches = ms;
        self.search.computed = Some(key);
        if self.search.move_when_counted {
            self.search.move_when_counted = false;
            self.update_incsearch();
        }
        if self.search.count_message {
            self.show_count();
        }
        true
    }

    /// 件数（今の一致が何番目か, 全件）。全件をまだ数え終えていなければ None
    pub fn match_count(&self) -> Option<(Option<usize>, usize)> {
        let key = self.active_key()?;
        if self.search.computed.as_ref() != Some(&key) {
            return None;
        }
        let ms = &self.search.matches;
        let index = self.search.current.map(|c| ms.partition_point(|m| m.0 < c.0)).filter(|&i| ms.get(i).is_some_and(|m| m.0 == self.search.current.unwrap().0));
        Some((index, ms.len()))
    }

    /// 今の検索語で、`at` の近く（INCSEARCH_REACH 以内）の一致
    fn find_near(&self, at: usize, forward: bool, include_at: bool) -> Option<(usize, usize)> {
        let (pattern, regex) = self.active_pattern()?;
        Finder::new(&pattern, regex)?.next_near(&self.buf, at, forward, include_at, INCSEARCH_REACH)
    }

    /// 今の検索語で、`at` から最寄りの一致
    fn find_next(&mut self, at: usize, forward: bool, include_at: bool) -> Option<(usize, usize)> {
        let (pattern, regex) = self.active_pattern()?;
        let version = self.buf.version;
        // 一致の無かった語を打ち足しただけなら、探さなくても一致しない（大きなファイルを 1 打ごとになめない）
        let extends_failed = |(v, p): &(u64, String)| *v == version && pattern.len() >= p.len() && pattern.as_bytes()[..p.len()].eq_ignore_ascii_case(p.as_bytes());
        if !regex && self.search.no_match.as_ref().is_some_and(extends_failed) {
            return None;
        }
        // 全件を数え終えていれば、その中から二分探索で選ぶ
        if self.match_count().is_some() {
            let ms = &self.search.matches;
            return next_match(ms, at, forward, include_at).map(|i| ms[i]);
        }
        let found = Finder::new(&pattern, regex)?.next(&self.buf, at, forward, include_at);
        if found.is_none() && !regex {
            self.search.no_match = Some((version, pattern));
        }
        found
    }

    /// 今の検索語の、[start, end) の中の一致（画面の強調用）
    /// 別の窓のバッファでも同じ語を強調する（hlsearch はどの窓にも出る）
    pub fn visible_matches_in(&self, buf: &Buffer, start: usize, end: usize) -> Vec<(usize, usize)> {
        let Some((pattern, regex)) = self.active_pattern() else { return Vec::new() };
        let text = buf.slice_string(start, end);
        find_matches(&text, &pattern, regex).into_iter().map(|(s, e)| (s + start, e + start)).collect()
    }

    pub(crate) fn start_search(&mut self, forward: bool, replace: bool, initial: &str) {
        self.search.origin = Some((self.cursor, self.top));
        self.search.forward = forward;
        self.search.current = None;
        self.query.set(initial);
        self.repl = Prompt::default();
        self.replace_field = if replace && !initial.is_empty() { 1 } else { 0 };
        self.set_state(if replace { State::Replace } else { State::Search });
        self.notify.clear();
        self.update_incsearch();
    }

    /// 入力のたびに、最寄りの一致へカーソルを動かす（incsearch）
    pub(crate) fn update_incsearch(&mut self) {
        // 設定の incsearch がオフなら、打っている間は動かない（Enter で探す）
        if !self.settings.bool("vim.incsearch") {
            return;
        }
        let Some((origin, top)) = self.search.origin else { return };
        self.refresh_matches();
        let at = self.buf.line_start(origin.line) + origin.byte;
        let include_at = self.state() == State::Replace;
        self.search.move_when_counted = false;
        self.search.current = if self.search.job.is_some() {
            // 全件を裏で数えている大きなファイル: 近くだけ探し、無ければ全件が届いてから決める
            let near = self.find_near(at, self.search.forward, include_at);
            self.search.move_when_counted = near.is_none();
            near
        } else if self.match_count().is_some() {
            next_match(&self.search.matches, at, self.search.forward, include_at).map(|i| self.search.matches[i])
        } else {
            self.find_next(at, self.search.forward, include_at)
        };
        match self.search.current {
            Some((s, _)) => self.set_abs(s),
            None => {
                self.cursor = origin;
                self.top = top;
            }
        }
    }

    fn cancel_search(&mut self) {
        if let Some((c, top)) = self.search.origin.take() {
            self.cursor = c;
            self.top = top;
        }
        self.set_state(State::Normal);
        self.clamp_normal();
    }

    fn search_key(&mut self, key: Key) {
        match key {
            Key::Esc | Key::Ctrl('c') => self.cancel_search(),
            Key::Backspace if self.query.text.is_empty() => self.cancel_search(),
            Key::Enter => {
                let pattern = self.query.text.clone();
                if pattern.is_empty() {
                    // 空なら前回の検索を繰り返す（Vim と同じ）
                    self.cancel_search();
                    if let Some(at) = self.search_next(false, 1) {
                        self.set_abs(at);
                    }
                    return;
                }
                if self.search.move_when_counted {
                    // 近くに無く、全件もまだ届いていない。裏で数えている残りを待って、そこから決める
                    if let Some(job) = self.search.job.take() {
                        if let Ok(ms) = job.rx.recv() {
                            self.search.matches = ms;
                            self.search.computed = Some(job.key.clone());
                        }
                    }
                    self.update_incsearch();
                    self.search.move_when_counted = false;
                }
                self.set_state(State::Normal);
                self.search.last = Some((pattern.clone(), self.search.regex, self.search.forward));
                self.search.highlight = self.settings.bool("vim.hlsearch");
                if let Some((from, _)) = self.search.origin.take() {
                    if self.search.current.is_some() {
                        self.push_jump_from(from);
                    }
                }
                if self.search.current.is_none() {
                    self.notify.error(format!("E486: パターンは見つかりませんでした: {pattern}"));
                } else {
                    self.show_count();
                }
                self.clamp_normal();
            }
            Key::Alt('r') => {
                self.search.regex = !self.search.regex;
                self.update_incsearch();
            }
            k => {
                if self.query.key(k) {
                    self.update_incsearch();
                }
            }
        }
    }

    fn show_count(&mut self) {
        if self.search.current.is_none() {
            return;
        }
        let (p, _, f) = self.search.last.clone().unwrap_or_default();
        let lead = if f { '/' } else { '?' };
        match self.match_count() {
            Some((Some(i), n)) => {
                self.notify.progress(format!("{lead}{p}  [{}/{n}]", i + 1));
                self.search.count_message = false;
            }
            _ => {
                // 全件はまだ数えている途中。届いたら出し直す
                self.notify.progress(format!("{lead}{p}  [?/…]"));
                self.search.count_message = true;
            }
        }
    }

    /// n / N の移動先。`reverse` は検索の向きを逆にする
    pub(crate) fn search_next(&mut self, reverse: bool, count: usize) -> Option<usize> {
        let Some((_, _, forward)) = self.search.last.clone() else {
            self.notify.error("E35: 前回の検索パターンがありません");
            return None;
        };
        self.search.highlight = self.settings.bool("vim.hlsearch");
        self.refresh_matches();
        let fwd = forward != reverse;
        let mut at = self.abs();
        let mut found = None;
        for _ in 0..count.max(1) {
            found = self.find_next(at, fwd, false);
            if let Some((s, _)) = found {
                at = s;
            }
        }
        match found {
            Some(m) => {
                self.search.current = Some(m);
                self.show_count();
                Some(m.0)
            }
            None => {
                let p = self.search.last.as_ref().map(|l| l.0.clone()).unwrap_or_default();
                self.notify.error(format!("E486: パターンは見つかりませんでした: {p}"));
                None
            }
        }
    }

    fn replace_key(&mut self, key: Key) {
        match key {
            Key::Esc | Key::Ctrl('c') => {
                self.search.origin = None;
                self.set_state(State::Normal);
                self.clamp_normal();
            }
            Key::Tab | Key::Down | Key::Up => self.replace_field = 1 - self.replace_field,
            Key::Alt('r') => {
                self.search.regex = !self.search.regex;
                self.update_incsearch();
            }
            // 今の 1 件を置き換えて次へ
            Key::Enter => {
                let Some((s, e)) = self.search.current else { return };
                let rep = self.replacement(s, e);
                self.hist.begin();
                self.edit(s, e, &rep);
                self.hist.close();
                self.refresh_matches();
                let after = s + rep.len();
                self.search.current = self.find_next(after, true, true);
                match self.search.current {
                    Some((s, _)) => self.set_abs(s),
                    None => {
                        self.notify.info("置換が終わりました");
                        self.set_state(State::Normal);
                        self.set_abs(s);
                        self.clamp_normal();
                    }
                }
            }
            // すべて置き換える（1 回の u で戻る）
            Key::Alt('a') => {
                // 全件置換は全件が要る。裏で数え終えていなければ、ここで数える
                let ms = match (self.match_count(), self.active_pattern()) {
                    (Some(_), _) => self.search.matches.clone(),
                    (None, Some((p, r))) => find_matches_in(&self.buf, &p, r),
                    (None, None) => Vec::new(),
                };
                if ms.is_empty() {
                    return;
                }
                let reps: Vec<String> = ms.iter().map(|&(s, e)| self.replacement(s, e)).collect();
                self.hist.begin();
                for (&(s, e), rep) in ms.iter().zip(&reps).rev() {
                    self.edit(s, e, rep);
                }
                self.hist.close();
                self.notify.info(format!("{} 箇所を置換しました", ms.len()));
                self.set_state(State::Normal);
                self.search.origin = None;
                self.set_abs(ms[0].0);
                self.clamp_normal();
            }
            k => {
                let field = self.replace_field;
                let changed = if field == 0 { self.query.key(k) } else { self.repl.key(k) };
                if changed && field == 0 {
                    self.update_incsearch();
                }
            }
        }
    }

    /// 一致 [s, e) の置換後の文字列（正規表現なら $1 などを展開する）。予告表示にも使う
    pub fn replacement(&self, s: usize, e: usize) -> String {
        if !self.search.regex {
            return self.repl.text.clone();
        }
        let matched = self.buf.slice_string(s, e);
        let (pat, _) = self.case_pattern(self.query.text.clone(), true);
        match compile_search(&pat, true) {
            Some(re) => re.replace(&matched, self.repl.text.as_str()).into_owned(),
            None => self.repl.text.clone(),
        }
    }

    // ---------------------------------------------------------------- Ex コマンド

    /// `:w 名前` / `:saveas 名前`。名前の無いバッファ（と :saveas）はその名前になり、名前のあるバッファは
    /// 別のファイルへ書き出すだけ（Vim と同じ）。ほかの既にあるファイルを上書きするには `!` が要る
    /// 打ったパスを、この窓の作業フォルダ（cwd）から見た絶対パスにする（プロセスの作業フォルダは窓をまたいで 1 つなので使わない）
    pub(crate) fn resolve(&self, p: impl AsRef<std::path::Path>) -> PathBuf {
        let p = p.as_ref();
        let p = if p.is_relative() && !p.has_root() { self.cwd.join(p) } else { p.to_path_buf() };
        std::path::absolute(&p).unwrap_or(p)
    }

    pub(crate) fn write_as(&mut self, name: &str, force: bool, rename: bool) -> bool {
        let target = self.resolve(name.trim().trim_matches('"'));
        let same = self.buf.path.as_ref().is_some_and(|p| std::path::absolute(p).ok() == std::path::absolute(&target).ok());
        if same {
            return self.write();
        }
        if target.exists() && !force {
            self.notify.error(format!("E13: ファイルが既にあります（:w! {name} で上書き）"));
            return false;
        }
        if self.buf.path.is_none() || rename {
            self.buf.path = Some(target.clone());
            self.buf.disk = None;
            self.watch_file(&target);
            crate::finder::remember_recent(&target);
            if self.syntax.is_none() {
                self.start_syntax();
            }
            return self.write();
        }
        // 名前はそのまま、中身を別のファイルへ（変更ありの印も変えない）
        let (path, dirty, disk) = (self.buf.path.replace(target.clone()), self.buf.dirty, self.buf.disk);
        let ok = self.write();
        self.buf.path = path;
        self.buf.dirty = dirty;
        self.buf.disk = disk;
        ok
    }

    pub(crate) fn write(&mut self) -> bool {
        self.tidy_before_save();
        match self.buf.save() {
            Ok(n) => {
                let name = self.buf.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                self.notify.info(format!("\"{name}\" {}L, {n}B 書き込み", self.buf.line_count()));
                // 保存したファイルの定義を索引し直す（裏で）。git の状態も取り直す
                self.reindex_saved();
                self.refresh_status();
                if self.blame_active() {
                    self.load_blame();
                }
                true
            }
            Err(e) => {
                self.notify.error(format!("E212: 書き込めません: {e}"));
                false
            }
        }
    }

    /// `g-` / `g+` / `:earlier` / `:later`
    pub(crate) fn undo_time(&mut self, later: bool, count: usize) {
        match self.hist.travel(&mut self.buf, later, count) {
            Some(at) => {
                self.set_abs(at.min(self.buf.len_bytes()));
                self.clamp_normal();
            }
            None => self.notify.info(if later { "これ以上は進めません" } else { "これ以上は戻せません" }),
        }
    }

    /// Ex コマンドを実行する（コマンドパレットから）
    pub(crate) fn run_ex(&mut self, cmd: &str) -> Effect {
        self.ex(cmd)
    }

    /// `:` の行を動かす（コマンドは commands.rs の表）
    fn ex(&mut self, cmd: &str) -> Effect {
        crate::commands::run(self, cmd)
    }

    /// IdeaVim の `:action <IntelliJ のアクション名>` を Nagi の機能に振り分ける
    pub(crate) fn action(&mut self, name: &str) {
        match nagi_vim::action::ideavim(name) {
            // 対応するコマンドがあればコマンドの表を通して動かす（commands::ACTIONS）
            Some((Some(id), desc)) => match crate::commands::ACTIONS.iter().find(|(a, _)| *a == id) {
                Some((_, cmd)) => {
                    crate::commands::run(self, cmd);
                }
                None => self.notify.info(format!("未実装: {desc}（{id}）")),
            },
            Some((None, desc)) => self.notify.info(format!("Nagi には無い機能: {desc}")),
            None => self.notify.info(format!("不明なアクション: {name}")),
        }
    }

    pub(crate) fn jump_last_change(&mut self) {
        match self.last_change {
            Some(at) => {
                self.set_abs(at.min(self.buf.len_bytes()));
                self.clamp_normal();
            }
            None => self.notify.error("E664: 変更履歴がありません"),
        }
    }

    /// 現在行の下に空行を足す（ideavimrc の `O` = `:call append(expand('.'), '')`）。カーソルは動かさない
    pub(crate) fn append_blank_line(&mut self) {
        let at = self.buf.line_start(self.cursor.line) + self.buf.line(self.cursor.line).len();
        let nl = self.buf.newline();
        self.hist.begin();
        self.edit(at, at, nl);
        self.hist.close();
    }

    // ---------------------------------------------------------------- IME・マウス

    /// IME の変換中の文字列（winit の Ime::Preedit）
    pub fn ime_preedit(&mut self, text: String, cursor: Option<(usize, usize)>) {
        // winit 0.30.13 の preedit カーソルは UTF-16 単位を char 数として返すことがある（#4772）。境界に丸める
        let fix = |i: usize| {
            let mut i = i.min(text.len());
            while !text.is_char_boundary(i) {
                i -= 1;
            }
            i
        };
        self.preedit_cursor = cursor.map(|(a, b)| (fix(a), fix(b)));
        self.preedit = text;
    }

    /// パネルの入力欄（フォーム・絞り込み・パスワード・WHERE / ORDER BY）にいるか。いる間は <leader> の割り当てを通さない
    pub fn in_text_field(&self) -> bool {
        (self.right_focused() && self.db.as_ref().is_some_and(|p| p.form.is_some() || p.filter.is_some() || p.password.is_some()))
            || (self.state() == State::List && self.list_side == Side::Bottom && (self.db_results.editing.is_some() || self.db_results.value_edit.is_some()))
    }

    /// 貼り付けを受ける入力欄にいるか（Vim の Insert の Ctrl-V は別）
    fn paste_target(&self) -> bool {
        self.has_prompt()
            || matches!(self.state(), State::Branches)
            || (self.state() == State::Log && self.log.as_ref().is_some_and(|l| l.filter.is_some()))
            || (self.right_focused() && self.db.as_ref().is_some_and(|p| p.password.is_some()))
    }

    /// クリップボードの文字を、今の入力欄に入れる（コミットメッセージ以外は 1 行にする）
    pub(crate) fn paste_clipboard(&mut self) {
        let text = match &self.clipboard {
            Some(cb) => (cb.get)(),
            None => crate::clipboard::get(),
        };
        let Some(text) = text.filter(|t| !t.is_empty()) else {
            self.notify.info("クリップボードが空です");
            return;
        };
        let text = if self.state() == State::CommitMsg {
            text.replace("\r\n", "\n")
        } else {
            text.trim_end_matches(['\r', '\n']).replace("\r\n", " ").replace(['\n', '\r'], " ")
        };
        if self.right_focused() {
            if let Some((_, pr)) = self.db.as_mut().and_then(|p| p.password.as_mut()) {
                pr.insert(&text);
                return;
            }
        }
        self.ime_commit(&text);
    }

    /// IME の確定（winit の Ime::Commit）。Insert なら挿入、入力欄なら欄に入れる
    pub fn ime_commit(&mut self, text: &str) {
        self.finish_load();
        self.preedit.clear();
        self.preedit_cursor = None;
        if !self.replaying {
            if let Some(rec) = &mut self.dot_rec {
                rec.extend(text.chars().map(Key::Char));
            }
        }
        if self.macro_depth == 0 {
            if let Some((_, ks)) = &mut self.recording {
                ks.extend(text.chars().map(Key::Char));
            }
        }
        if let Some(c) = crate::component::focused(self) {
            c.text(self, text);
            return;
        }
        match self.state() {
            State::Insert => {
                // IME で確定した文字は補完の語ではないので、一覧は閉じる
                if self.completion.as_ref().is_some_and(|c| c.smart.is_some()) {
                    self.completion = None;
                }
                self.insert_text(text)
            }
            State::Search => {
                self.query.insert(text);
                self.update_incsearch();
            }
            State::Replace => {
                if self.replace_field == 0 {
                    self.query.insert(text);
                    self.update_incsearch();
                } else {
                    self.repl.insert(text);
                }
            }

            _ => {}
        }
    }

    /// 下の欄の結果の表で、WHERE・ORDER BY の欄を直している間の文字
    pub(crate) fn bottom_text(&mut self, text: &str) {
        if let Some(f) = &mut self.db_results.value_edit {
            f.insert_text(text);
        } else if let Some((_, f)) = &mut self.db_results.editing {
            f.insert_text(text);
        }
    }

    /// 画面上の (行, セル列) にあたるカーソル位置。行は表示中のスクロール位置から数える
    pub fn pos_at(&self, row: usize, col: usize) -> Cursor {
        // 折り返している: 上の行から使う画面の行を数えて、押した行とその中の位置を求める
        if let Some(w) = self.wrap_width().filter(|_| self.folds.is_empty()) {
            let n = self.buf.line_count();
            let (mut r, mut ln) = (0, self.top.min(n.saturating_sub(1)));
            loop {
                let lr = self.line_rows(ln, w);
                if row < r + lr || ln + 1 >= n {
                    let seg = row.saturating_sub(r).min(lr - 1);
                    let text = self.buf.line(ln);
                    let c = seg * w + col.saturating_sub(self.gutter()).min(w.saturating_sub(1));
                    let byte = if self.state() == State::Insert && c >= display_col(&text, text.len(), false) { text.len() } else { byte_at_col(&text, c, false) };
                    return Cursor { line: ln, byte, want_col: display_col(&text, byte, false) };
                }
                r += lr;
                ln += 1;
            }
        }
        let line = if self.folds.is_empty() {
            (self.top + row).min(self.buf.line_count().saturating_sub(1))
        } else {
            let shown = self.folds.visible_from(self.top, row + 1, self.buf.line_count());
            shown.last().copied().unwrap_or(0)
        };
        let text = self.buf.line(line);
        let col = col.saturating_sub(self.gutter()) + self.left;
        let byte = if self.state() == State::Insert && col >= display_col(&text, text.len(), false) {
            text.len()
        } else {
            byte_at_col(&text, col, false)
        };
        Cursor { line, byte, want_col: display_col(&text, byte, false) }
    }

    /// パネル（木・検索の結果・右の欄など）にいたら本文の操作に戻る（本文をクリックしたとき）
    /// 本文を押した: 向いている部品（欄・差分など、どのペインでも）から離れる。画面を覆う小窓・ダイアログは
    /// 自分でクリックを受け取るので、ここへは来ない（来ても離れない）
    pub(crate) fn leave_panel(&mut self) {
        let overlay = matches!(self.focus, Some(Panel::Finder | Panel::Launcher | Panel::RunDialog | Panel::Settings | Panel::Branches | Panel::GrepPrompt | Panel::Menu));
        if self.focus.is_some() && !overlay {
            self.set_state(State::Normal);
            self.pending_keys.clear();
        }
    }

    /// 本文を押しても移らないか。最下行の入力欄（`:`・検索など）と画面を覆う部品の間は移らない。
    /// 欄の入力欄（コミットメッセージ・絞り込み・セルの値など）は、Insert・Visual のままでもすぐ本文へ移れる（Normal と同じ）
    pub(crate) fn text_click_blocked(&self) -> bool {
        self.has_prompt() && !matches!(self.state(), State::CommitMsg | State::Log | State::DbTable | State::List)
    }

    /// マウスのクリック: 画面上の (行, セル列) へカーソルを移す
    pub fn click(&mut self, row: usize, col: usize) {
        if self.text_click_blocked() {
            return;
        }
        self.leave_panel();
        if matches!(self.state(), State::Visual { .. }) {
            self.set_state(State::Normal);
        }
        self.cursor = self.pos_at(row, col);
        if self.state() == State::Normal {
            self.clamp_normal();
        }
        self.scroll_into_view();
    }

    /// ドラッグ: 押した位置（絶対 byte）を起点に Visual で選ぶ。Insert 中なら先に抜ける
    pub fn drag(&mut self, anchor: usize, row: usize, col: usize) {
        if self.text_click_blocked() {
            return;
        }
        self.leave_panel();
        if self.state() == State::Insert {
            self.key(Key::Esc);
        }
        if !matches!(self.state(), State::Visual { .. }) {
            self.anchor = anchor;
            self.set_state(State::Visual { line: false, block: false });
        }
        self.cursor = self.pos_at(row, col);
        self.clamp_normal();
    }

    /// ダブルクリックで単語、トリプルクリックで行を選ぶ（viw / V と同じ）
    pub fn select_at(&mut self, row: usize, col: usize, clicks: u32) {
        if self.text_click_blocked() {
            return;
        }
        self.leave_panel();
        if self.plain() {
            self.plain_select_at(row, col, clicks);
            return;
        }
        if self.state() == State::Insert {
            self.key(Key::Esc);
        }
        self.set_state(State::Normal);
        self.cursor = self.pos_at(row, col);
        self.clamp_normal();
        if clicks == 2 {
            self.key(Key::Char('v'));
            self.key(Key::Char('i'));
            self.key(Key::Char('w'));
        } else {
            self.key(Key::Char('V'));
        }
    }

    /// ホイール: 表示を動かし、カーソルは画面内に留める（Vim と同じ）
    pub fn scroll(&mut self, lines: isize) {
        let pos = self.top as f64 + lines as f64;
        self.scroll_to(pos);
        self.frac = 0.0;
    }

    /// 表示位置を行単位の小数で決める（スムーズスクロール）。カーソルは見えている行に留める
    pub fn scroll_to(&mut self, pos: f64) {
        let rows = self.rows.max(1);
        let max = self.buf.line_count().saturating_sub(1) as f64;
        let pos = pos.clamp(0.0, max);
        self.top = pos.floor() as usize;
        self.frac = (pos - pos.floor()) as f32;
        // 上端が欠けている行には置かない
        let first = self.top + usize::from(self.frac > 0.0);
        let last = (self.top + rows).saturating_sub(1).max(first);
        let mut c = self.cursor;
        if c.line < first {
            let d = (first - c.line) as isize;
            self.motion().vertical(&mut c, d);
        } else if c.line > last {
            let d = -((c.line - last) as isize);
            self.motion().vertical(&mut c, d);
        }
        self.cursor = c;
        if self.state() == State::Normal {
            self.clamp_normal();
        }
    }

    /// 横のホイール: 左端の列を `cols` 列送る（見えている行のいちばん長いものまで。カーソルは動かさない）
    pub fn hscroll(&mut self, cols: isize) {
        if self.wrap_width().is_some() {
            return;
        }
        let w = self.active_rect().w.saturating_sub(self.gutter()).max(1);
        let total = self.buf.line_count();
        // 長い行も、今の左端から少し先までを見れば足りる（数 MB の 1 行を毎回全部数えない）
        let widest = (self.top..(self.top + self.rows.max(1)).min(total))
            .map(|l| {
                let (h, _) = self.buf.line_head(l, (self.left + w * 2) * 4);
                display_col(&h, h.len(), false)
            })
            .max()
            .unwrap_or(0);
        crate::list::hwheel(&mut self.left, widest + 1, w, cols);
        self.left_at = (self.cursor.line, self.cursor.byte);
    }

    /// カーソルが動いたら、カーソルの列が見えるように左端を送る（折り返すときは 0）
    fn follow_left(&mut self) {
        if self.wrap_width().is_some() {
            self.left = 0;
            return;
        }
        let at = (self.cursor.line, self.cursor.byte);
        if at == self.left_at {
            return;
        }
        self.left_at = at;
        let w = self.active_rect().w.saturating_sub(self.gutter());
        if w < 2 {
            return;
        }
        let (head, _) = self.buf.line_head(self.cursor.line, self.cursor.byte + 4);
        let col = display_col(&head, self.cursor.byte.min(head.len()), false);
        // 端に着いたら少し余分に送る（1 文字ずつ送り続けないように）
        let step = (w / 4).max(1);
        if col < self.left {
            self.left = col.saturating_sub(step);
        } else if col >= self.left + w {
            self.left = col + 1 + step - w;
        }
    }

    /// 今の表示位置（行単位の小数）
    pub fn scroll_pos(&self) -> f64 {
        self.top as f64 + self.frac as f64
    }

    pub(crate) fn scroll_into_view(&mut self) {
        // 横（折りたたみがあっても。縦の送りは折りたたみのときに途中で返る）
        self.follow_left();
        let rows = self.rows.max(1);
        if !self.folds.is_empty() {
            // 閉じた折りたたみは 1 行として数える
            self.top = self.folds.snap(self.top);
            let cur = self.folds.snap(self.cursor.line);
            if cur < self.top {
                self.top = cur;
                self.frac = 0.0;
            } else {
                let total = self.buf.line_count();
                let shown = self.folds.visible_from(self.top, rows, total);
                if !shown.contains(&cur) {
                    let mut t = cur;
                    for _ in 1..rows {
                        match self.folds.prev_visible(t) {
                            Some(p) => t = p,
                            None => break,
                        }
                    }
                    self.top = t;
                    self.frac = 0.0;
                }
            }
            return;
        }
        if self.frac > 0.0 && self.cursor.line <= self.top {
            self.frac = 0.0;
        }
        if self.cursor.line < self.top {
            self.top = self.cursor.line;
        } else if self.cursor.line >= self.top + rows {
            self.top = self.cursor.line + 1 - rows;
        }
        // 折り返していれば、カーソルの行（の終わり）まで画面に入るように上の行を送る
        if let Some(w) = self.wrap_width() {
            while self.top < self.cursor.line && (self.top..=self.cursor.line).map(|l| self.line_rows(l, w)).sum::<usize>() > rows {
                self.top += 1;
                self.frac = 0.0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(text: &str) -> Editor {
        let mut e = Editor::new(Buffer::from_str(text), None);
        e.rows = 3;
        e
    }

    /// 大きなファイルの incsearch: 近く（1MB 以内）に無い一致へは、裏で数えた全件が届いてから移動する。
    /// 件数も届いてから出る
    #[test]
    fn incsearch_far_match_waits_for_count() {
        let mut text = "filler line without the word\n".repeat(80_000); // 約 2.3MB
        text.push_str("needle here\n");
        let mut e = ed(&text);
        e.waker = Some(std::sync::Arc::new(|| {}));
        for k in "/needle".chars() {
            e.key(Key::Char(k));
        }
        assert_eq!(e.search.current, None, "近くには無い");
        assert!(e.search.move_when_counted);
        let t = std::time::Instant::now();
        while !e.poll_search() {
            assert!(t.elapsed() < std::time::Duration::from_secs(10));
            std::thread::yield_now();
        }
        assert_eq!(e.cursor.line, 80_000);
        assert_eq!(e.match_count(), Some((Some(0), 1)));
        e.key(Key::Enter);
        assert_eq!(e.state(), State::Normal);
        assert_eq!(e.notify.text, "/needle  [1/1]");
        // n は近くから探す（全件はもう数えてある）
        e.key(Key::Char('g'));
        e.key(Key::Char('g'));
        e.key(Key::Char('n'));
        assert_eq!(e.cursor.line, 80_000);
    }

    #[test]
    fn drag_selects_from_press() {
        let mut e = ed("hello world\nsecond line\n");
        e.click(0, GUTTER + 2);
        let anchor = e.abs();
        e.drag(anchor, 1, GUTTER + 3);
        assert_eq!(e.state(), State::Visual { line: false, block: false });
        assert_eq!((e.cursor.line, e.cursor.byte), (1, 3));
        assert_eq!(e.visual_range(), (2, 16, false));
    }

    #[test]
    fn drag_leaves_insert() {
        let mut e = ed("abc\n");
        e.key(Key::Char('i'));
        e.drag(0, 0, GUTTER + 2);
        assert_eq!(e.state(), State::Visual { line: false, block: false });
    }

    #[test]
    fn double_and_triple_click() {
        let mut e = ed("let name = 1;\n");
        e.select_at(0, GUTTER + 5, 2);
        assert_eq!(e.visual_range(), (4, 8, false), "単語 name");
        e.select_at(0, GUTTER + 5, 3);
        assert_eq!(e.state(), State::Visual { line: true, block: false });
    }

    #[test]
    fn smooth_scroll_keeps_cursor_on_full_rows() {
        let text: String = (0..20).map(|i| format!("{i}\n")).collect();
        let mut e = ed(&text);
        e.scroll_to(2.5);
        assert_eq!((e.top, e.frac), (2, 0.5));
        // 上端が半分欠けた 2 行目ではなく、3 行目に置く
        assert_eq!(e.cursor.line, 3);
        e.scroll_to(100.0);
        assert_eq!(e.top, 19, "最終行より先へは行かない");
        assert_eq!(e.cursor.line, 19);
        e.scroll_to(0.0);
        assert_eq!(e.cursor.line, 2, "画面の最下段に留まる");
    }

    /// 折り返さない長い行: カーソルに付いて左端を送る・押した位置は左端から数える・横のホイールはカーソルを動かさない
    #[test]
    fn horizontal_scroll_follows_cursor() {
        let text = format!("{}\nshort\n", "x".repeat(100));
        let mut e = ed(&text);
        e.set_screen(40, 10);
        let w = e.active_rect().w - e.gutter();
        e.scroll_into_view();
        assert_eq!(e.left, 0);
        e.key(Key::Char('$'));
        e.scroll_into_view();
        assert!(e.left > 0 && 99 >= e.left && 99 < e.left + w, "行末が見える: left {}", e.left);
        let left = e.left;
        e.click(0, e.gutter());
        assert_eq!(e.cursor.byte, left, "押した位置は左端から");
        e.hscroll(-1000);
        e.scroll_into_view();
        assert_eq!((e.left, e.cursor.byte), (0, left), "ホイールではカーソルは動かず、左端も戻らない");
        e.key(Key::Char('h'));
        e.scroll_into_view();
        assert!(e.left > 0 && left - 1 < e.left + w, "カーソルが動いたら、また付いていく");
        e.key(Key::Char('j'));
        e.scroll_into_view();
        assert_eq!(e.left, 0, "短い行へ移ると左端へ");
        e.hscroll(1000);
        assert!(e.left <= 101, "見えている行のいちばん長いものまで: {}", e.left);
    }

    /// 閉じた折りたたみがあっても、行末へ動けば横に送る（縦の送りが途中で返っても）
    #[test]
    fn horizontal_scroll_with_closed_fold() {
        let text = format!("{}\na\nb\nc\n", "x".repeat(100));
        let mut e = ed(&text);
        e.set_screen(40, 10);
        for k in "jVjzf".chars() {
            e.key(Key::Char(k));
        }
        assert!(!e.folds.is_empty(), "{}", e.notify.text);
        for k in "gg$".chars() {
            e.key(Key::Char(k));
        }
        e.scroll_into_view();
        assert!(e.left > 0, "left {}", e.left);
    }
}
