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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
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

/// 裏で全件を数えている検索。落とすと（検索語が変わると）数えるのをやめる
struct CountJob {
    key: SearchKey,
    rx: std::sync::mpsc::Receiver<Vec<(usize, usize)>>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for CountJob {
    fn drop(&mut self) {
        self.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
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
    pub mode: Mode,
    pub message: String,
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
    /// マッピングの続きを待っているキー（showcmd。App が描画前に入れる）
    pub showcmd: String,
    /// 構文木（対応言語のときだけ）
    pub syntax: Option<nagi_syntax::Syntax>,
    /// 大きなファイルの全体（先頭部分だけを先に表示している間。届いたら `poll_load` で差し替える）
    loading: Option<std::sync::mpsc::Receiver<Buffer>>,
    /// 裏で解析中の構文木（届いたら `poll_syntax` で受け取る）
    pub(crate) pending_syntax: Option<std::sync::mpsc::Receiver<nagi_syntax::Syntax>>,
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
    /// 窓（作業中のものは `WinSlot::Active` で、カーソルと表示位置はこの構造体の欄にある）
    pub(crate) wins: Vec<crate::panes::WinSlot>,
    pub(crate) cur_win: usize,
    /// 直前の窓（`Ctrl-w p`）
    pub(crate) prev_win: Option<usize>,
    pub(crate) layout: crate::panes::Layout,
    /// 画面の本文の範囲（列, 行）。最下行は含まない（App が描画前に入れる）
    pub(crate) screen: (usize, usize),
    pub finder: Option<crate::finder::Finder>,
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
    pub bottom_hidden: bool,
    /// 右の欄（データベース・Maven）と、下のパネルのクエリの結果
    pub db: Option<crate::db_panel::DbPanel>,
    pub maven: Option<crate::maven_panel::MavenPanel>,
    pub db_results: crate::result_grid::DbResults,
    /// テーブルのデータの編集（開いている間だけ）
    pub db_table: Option<crate::db_table::TableEditor>,
    pub bottom_kind: crate::result_grid::BottomKind,
    pub right_tab: crate::right_panel::RightTab,
    pub right_hidden: bool,
    pub right_w: Option<usize>,
    pub(crate) right_pending_w: bool,
    /// ドラッグで変えた左の欄の幅・下のパネルの高さ（None は既定の大きさ）
    pub left_w: Option<usize>,
    pub bottom_h: Option<usize>,
    /// 最後に描いた入力欄の縦棒カーソル（列, y の px）。IME の候補の窓の位置に使う
    pub prompt_caret: std::cell::Cell<Option<(usize, f32)>>,
    /// マウスを載せている SQL コンソールの帯のボタン
    pub hover_bar: Option<crate::db_panel::BarItem>,
    /// マウスを載せているボタン
    pub hover_button: Option<crate::toggles::Button>,
    /// 左の欄で出しているタブ（ツリーとコミットのパネルが両方開いているとき）
    pub left_tab: crate::tree_panel::LeftTab,
    /// ブランチのポップアップ（開いている間だけ）
    pub branch_popup: Option<crate::branches::BranchPopup>,
    /// 裏で動かしているフェッチ・pull（見出し, 結果）
    pub git_job: Option<(String, std::sync::mpsc::Receiver<Result<String, String>>)>,
    /// 履歴の画面（開いている間だけ）
    pub log: Option<crate::log_view::LogView>,
    /// blame（出している間だけ）
    pub blame: Option<crate::blame::Blame>,
    /// コミットのパネル（開いている間だけ）
    pub commit: Option<crate::commit_panel::CommitPanel>,
    /// 左右並べの差分（開いている間だけ）と、閉じたときに戻るモード
    pub diff: Option<crate::diff_view::DiffView>,
    pub(crate) diff_return: Option<Mode>,
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
    pub fn new(buf: Buffer, syntax: Option<nagi_syntax::Syntax>) -> Self {
        Self {
            buf,
            hist: History::default(),
            cursor: Cursor::default(),
            top: 0,
            frac: 0.0,
            mode: Mode::Normal,
            message: String::new(),
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
            wins: vec![crate::panes::WinSlot::Active],
            cur_win: 0,
            prev_win: None,
            layout: crate::panes::Layout::Leaf(0),
            screen: (80, 24),
            finder: None,
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
            blame: None,
            log: None,
            branch_popup: None,
            left_tab: Default::default(),
            left_hidden: false,
            bottom_hidden: false,
            hover_button: None,
            hover_bar: None,
            prompt_caret: Default::default(),
            left_w: None,
            db: None,
            maven: None,
            db_results: Default::default(),
            db_table: None,
            bottom_kind: Default::default(),
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
        let (tx, rx2) = std::sync::mpsc::channel();
        let waker = self.waker.clone();
        std::thread::spawn(move || {
            if let Ok(b) = rx.recv() {
                if tx.send(b).is_ok() {
                    if let Some(w) = waker {
                        w();
                    }
                }
            }
        });
        self.loading = Some(rx2);
        self.message = LOADING.into();
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
        if self.message == LOADING {
            self.message.clear();
        }
        self.start_syntax();
    }

    /// 構文解析を裏のスレッドで始める。大きなファイルでは数百 ms〜秒かかるので、表示や入力を待たせない。
    /// 解析は今の内容の複製に対して行い、その間の編集は届いた後の `sync` で差分として反映する
    pub fn start_syntax(&mut self) {
        let Some(lang) = self.buf.path.as_deref().and_then(nagi_syntax::Lang::detect) else { return };
        let mut snap = self.buf.snapshot();
        let (tx, rx) = std::sync::mpsc::channel();
        let waker = self.waker.clone();
        let spawned = std::thread::Builder::new().name("syntax".into()).spawn(move || {
            let Some(mut s) = nagi_syntax::Syntax::new(lang) else { return };
            s.parse(&mut snap);
            // ハイライトのクエリのコンパイル（大きい言語で数十 ms）もここで済ませる
            let _ = lang.highlights();
            if tx.send(s).is_ok() {
                if let Some(w) = waker {
                    w();
                }
            }
        });
        if spawned.is_ok() {
            self.pending_syntax = Some(rx);
        }
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
        if self.mode == Mode::Replace { 2 } else { 1 }
    }

    /// 入力欄（IME の確定先）があるモードか
    pub fn has_prompt(&self) -> bool {
        matches!(self.mode, Mode::Command | Mode::Search | Mode::Replace | Mode::Finder | Mode::GrepPrompt)
            || (self.mode == Mode::CommitMsg && self.commit.as_ref().is_some_and(|c| c.msg.is_insert()))
            || (self.mode == Mode::Log && self.log.as_ref().is_some_and(|l| l.filter.as_ref().is_some_and(|f| f.is_insert())))
            || (self.mode == Mode::Branches && self.branch_popup.as_ref().is_some_and(|b| b.naming.as_ref().is_none_or(|n| n.1.is_insert())))
            || self.db_typing()
            || self.db_table_typing()
            || (self.mode == Mode::List && self.list_side == Side::Bottom && self.db_results.editing.as_ref().is_some_and(|e| e.1.is_insert()))
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
    }

    pub(crate) fn enter_insert(&mut self) {
        self.hist.begin();
        self.mode = Mode::Insert;
        self.message.clear();
    }

    fn leave_insert(&mut self) {
        self.completion = None;
        // 矩形の I A c: 打った文字列をほかの行へ（同じ undo のグループに入れる）
        let placed = self.finish_block_insert();
        self.hist.close();
        self.mode = Mode::Normal;
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

    fn backspace(&mut self) {
        let line = self.buf.line(self.cursor.line);
        if self.cursor.byte > 0 {
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
        // 先頭部分だけを表示している間は、全体が届くのを待ってから処理する（先頭部分を編集してしまわない）
        self.finish_load();
        // 入力欄での Ctrl+V（Shift+Insert）は貼り付け。ファインダーの Ctrl+V は縦に分割して開く
        if (key == Key::Paste || (key == Key::Ctrl('v') && self.mode != Mode::Finder)) && self.paste_target() {
            self.paste_clipboard();
            return Effect::None;
        }
        // データを変える SQL の確認（y で実行）
        if self.db_confirm_key(key) {
            return Effect::None;
        }
        // SQL コンソールの Ctrl+Enter（Ctrl-m として届く）: カーソルのある文を流す
        if key == Key::Ctrl('m') && matches!(self.mode, Mode::Normal | Mode::Insert | Mode::Visual { .. }) && self.console_source().is_some() {
            self.db_run_at_cursor();
            return Effect::None;
        }
        // Alt-1 プロジェクト、Alt-0 コミット（IntelliJ と同じ。どのモードからでも）
        if matches!(key, Key::Alt('1') | Key::Alt('0')) && matches!(self.mode, Mode::Normal | Mode::Insert | Mode::List | Mode::CommitMsg) {
            if self.mode == Mode::Insert {
                self.key(Key::Esc);
            }
            self.toggle_left_tab(if key == Key::Alt('1') { crate::tree_panel::LeftTab::Project } else { crate::tree_panel::LeftTab::Commit });
            return Effect::None;
        }
        // マクロの記録を止める q（Normal と Visual で、ほかのキーの途中でないとき）
        if self.recording.is_some()
            && self.macro_depth == 0
            && key == Key::Char('q')
            && matches!(self.mode, Mode::Normal | Mode::Visual { .. })
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
        if self.mode == Mode::Normal && self.hunk_popup_key(key) {
            self.scroll_into_view();
            return Effect::None;
        }
        let effect = match self.mode {
            Mode::Normal | Mode::Visual { .. } => self.normal_key(key),
            Mode::Insert => {
                self.insert(key);
                Effect::None
            }
            Mode::Command => self.command(key),
            Mode::Search => {
                self.search_key(key);
                Effect::None
            }
            Mode::Replace => {
                self.replace_key(key);
                Effect::None
            }
            Mode::Finder => self.finder_key(key),
            Mode::GrepPrompt => {
                self.grep_prompt_key(key);
                Effect::None
            }
            Mode::List => match self.list_side {
                Side::Left => self.tree_key(key),
                Side::Bottom if self.bottom_kind == crate::result_grid::BottomKind::Result => self.result_key(key),
                Side::Bottom => self.list_key(key),
                Side::Commit => self.commit_list_key(key),
                Side::Right => self.right_key(key),
            },
            Mode::CommitMsg => {
                self.commit_msg_key(key);
                Effect::None
            }
            Mode::Diff => {
                self.diff_key(key);
                Effect::None
            }
            Mode::Log => {
                self.log_key(key);
                Effect::None
            }
            Mode::Branches => {
                self.branches_key(key);
                Effect::None
            }
            Mode::DbTable => {
                self.db_table_key(key);
                Effect::None
            }
        };
        // 検索や :{行} で閉じた折りたたみの途中へ跳んだら開く
        if !self.folds.is_empty() && self.mode == Mode::Normal {
            self.open_folds_at_cursor();
        }
        self.scroll_into_view();
        effect
    }

    fn insert(&mut self, key: Key) {
        // 補完（Ctrl-n / Ctrl-p）の途中なら先に扱う
        // Ctrl+Space は Ctrl-n と同じく単語の候補を出す（IntelliJ の基本の補完と同じキー）
        let key = if key == Key::Ctrl(' ') { Key::Ctrl('n') } else { key };
        if (self.completion.is_some() || matches!(key, Key::Ctrl('n') | Key::Ctrl('p'))) && self.completion_key(key) {
            return;
        }
        match key {
            Key::Esc | Key::Ctrl('c') => self.leave_insert(),
            // 新しい行のインデントは今の行に合わせる（autoindent。Neovim の既定と同じ）
            Key::Enter => {
                let line = self.buf.line(self.cursor.line);
                let indent: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
                let indent = &indent[..indent.len().min(self.cursor.byte)];
                self.insert_text(&format!("\n{indent}"));
            }
            Key::Backspace => self.backspace(),
            Key::Tab => self.insert_text("\t"),
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
                self.insert_text(c.encode_utf8(&mut [0; 4]));
                // SQL コンソールでは「別名.」の後に列の候補を出す
                if c == '.' && self.console_source().is_some() {
                    self.start_sql_completion(true);
                }
            }
            _ => {}
        }
    }

    fn command(&mut self, key: Key) -> Effect {
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
                self.mode = Mode::Normal;
                self.cmd_complete = None;
            }
            Key::Backspace if self.cmdline.text.is_empty() => {
                self.mode = Mode::Normal;
                self.cmd_complete = None;
            }
            Key::Enter => {
                self.cmd_complete = None;
                let line = std::mem::take(&mut self.cmdline).text;
                self.mode = Mode::Normal;
                // `|` で区切った複数のコマンド（`source ~/.ideavimrc | echo "..."`）。:s の中の | は区切らない
                let cmds: Vec<String> = if self.keeps_bar(&line) { vec![line] } else { line.split('|').map(str::to_string).collect() };
                let mut effect = Effect::None;
                for cmd in cmds {
                    match self.ex(cmd.trim()) {
                        Effect::None => {}
                        e => effect = e,
                    }
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
        match self.mode {
            Mode::Search | Mode::Replace if !self.query.text.is_empty() => Some((self.query.text.clone(), self.search.regex)),
            Mode::Search | Mode::Replace => None,
            _ => self.search.last.as_ref().map(|(p, r, _)| (p.clone(), *r)),
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
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (pattern, regex, waker, stop) = (key.1.clone(), key.2, self.waker.clone(), cancel.clone());
        std::thread::spawn(move || {
            let Some(ms) = find_matches_cancellable(&snap, &pattern, regex, &stop) else { return };
            if tx.send(ms).is_ok() {
                if let Some(w) = waker {
                    w();
                }
            }
        });
        // 前の検索語の数えは、ここで落とすと止まる
        self.search.job = Some(CountJob { key, rx, cancel });
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
        self.mode = if replace { Mode::Replace } else { Mode::Search };
        self.message.clear();
        self.update_incsearch();
    }

    /// 入力のたびに、最寄りの一致へカーソルを動かす（incsearch）
    pub(crate) fn update_incsearch(&mut self) {
        let Some((origin, top)) = self.search.origin else { return };
        self.refresh_matches();
        let at = self.buf.line_start(origin.line) + origin.byte;
        let include_at = self.mode == Mode::Replace;
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
        self.mode = Mode::Normal;
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
                self.mode = Mode::Normal;
                self.search.last = Some((pattern.clone(), self.search.regex, self.search.forward));
                self.search.highlight = true;
                if let Some((from, _)) = self.search.origin.take() {
                    if self.search.current.is_some() {
                        self.push_jump_from(from);
                    }
                }
                if self.search.current.is_none() {
                    self.message = format!("E486: パターンは見つかりませんでした: {pattern}");
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
                self.message = format!("{lead}{p}  [{}/{n}]", i + 1);
                self.search.count_message = false;
            }
            _ => {
                // 全件はまだ数えている途中。届いたら出し直す
                self.message = format!("{lead}{p}  [?/…]");
                self.search.count_message = true;
            }
        }
    }

    /// n / N の移動先。`reverse` は検索の向きを逆にする
    pub(crate) fn search_next(&mut self, reverse: bool, count: usize) -> Option<usize> {
        let Some((_, _, forward)) = self.search.last.clone() else {
            self.message = "E35: 前回の検索パターンがありません".into();
            return None;
        };
        self.search.highlight = true;
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
                self.message = format!("E486: パターンは見つかりませんでした: {p}");
                None
            }
        }
    }

    fn replace_key(&mut self, key: Key) {
        match key {
            Key::Esc | Key::Ctrl('c') => {
                self.search.origin = None;
                self.mode = Mode::Normal;
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
                        self.message = "置換が終わりました".into();
                        self.mode = Mode::Normal;
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
                self.message = format!("{} 箇所を置換しました", ms.len());
                self.mode = Mode::Normal;
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
        match compile_search(&self.query.text, true) {
            Some(re) => re.replace(&matched, self.repl.text.as_str()).into_owned(),
            None => self.repl.text.clone(),
        }
    }

    // ---------------------------------------------------------------- Ex コマンド

    /// `:w 名前` / `:saveas 名前`。名前の無いバッファ（と :saveas）はその名前になり、名前のあるバッファは
    /// 別のファイルへ書き出すだけ（Vim と同じ）。ほかの既にあるファイルを上書きするには `!` が要る
    pub(crate) fn write_as(&mut self, name: &str, force: bool, rename: bool) -> bool {
        let target = PathBuf::from(name.trim().trim_matches('"'));
        let same = self.buf.path.as_ref().is_some_and(|p| std::path::absolute(p).ok() == std::path::absolute(&target).ok());
        if same {
            return self.write();
        }
        if target.exists() && !force {
            self.message = format!("E13: ファイルが既にあります（:w! {name} で上書き）");
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

    fn write(&mut self) -> bool {
        match self.buf.save() {
            Ok(n) => {
                let name = self.buf.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                self.message = format!("\"{name}\" {}L, {n}B 書き込み", self.buf.line_count());
                // 保存したファイルの定義を索引し直す（裏で）。git の状態も取り直す
                self.reindex_saved();
                self.refresh_status();
                if self.blame_active() {
                    self.load_blame();
                }
                true
            }
            Err(e) => {
                self.message = format!("E212: 書き込めません: {e}");
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
            None => self.message = if later { "これ以上は進めません".into() } else { "これ以上は戻せません".into() },
        }
    }

    /// Ex コマンドを実行する（コマンドパレットから）
    pub(crate) fn run_ex(&mut self, cmd: &str) -> Effect {
        self.ex(cmd)
    }

    fn ex(&mut self, cmd: &str) -> Effect {
        let cmd = cmd.trim_start_matches(':').trim();
        // 範囲を取るコマンド（:s :g :d :normal など）と :{行}
        if let Some(e) = self.ex_ranged(cmd) {
            return e;
        }
        let (name, arg) = cmd.split_once(char::is_whitespace).map_or((cmd, ""), |(n, a)| (n, a.trim()));
        // 大文字小文字を区別せずにコマンド名を探す（:gblame → Gblame）
        let name = crate::cmdline_complete::canonical_cmd(name);
        match name {
            "" => {}
            "w" | "write" | "w!" | "write!" if !arg.is_empty() => {
                self.write_as(arg, name.ends_with('!'), false);
            }
            "sav" | "saveas" | "sav!" | "saveas!" if !arg.is_empty() => {
                self.write_as(arg, name.ends_with('!'), true);
            }
            // 名前を書かない :saveas と、名前の無いバッファの :w はダイアログで選ぶ
            "sav" | "saveas" | "sav!" | "saveas!" => return Effect::SaveDialog,
            "w" | "write" | "w!" | "write!" | "wq" | "x" if self.buf.path.is_none() => return Effect::SaveDialog,
            "browse" if arg.starts_with('w') || arg.starts_with("sav") => return Effect::SaveDialog,
            "w" | "write" | "w!" | "write!" => {
                self.write();
            }
            // 分割中の :q は今の窓だけを閉じる（バッファは隠れたまま残る）
            "q" | "quit" | "q!" | "quit!" | "clo" | "close" if self.is_split() => self.close_window(self.cur_win),
            // :wq 名前 は名前を付けて保存してから
            "wq" | "x" | "xit" | "wq!" if !arg.is_empty() && !self.write_as(arg, name.ends_with('!'), false) => {}
            "wq" | "x" | "xit" if self.is_split() => {
                if self.write() {
                    self.close_window(self.cur_win);
                }
            }
            "clo" | "close" => self.message = "E444: 最後のウィンドウは閉じられません".into(),
            "on" | "only" => self.only_window(),
            "sp" | "split" | "vs" | "vsplit" => {
                self.split(name.starts_with('v'));
                if !arg.is_empty() {
                    self.edit_file(PathBuf::from(arg));
                }
            }
            "new" | "vne" | "vnew" => {
                self.split(name.starts_with('v'));
                self.new_empty_buffer();
            }
            "q" | "quit" | "qa" | "qall" | "quitall" if self.buf.dirty => self.message = "E37: 変更が保存されていません（:q! で破棄して終了）".into(),
            "q" | "quit" | "qa" | "qall" | "quitall" if !self.dirty_docs().is_empty() => {
                let i = self.dirty_docs()[0];
                self.message = format!("E162: バッファ {} の変更が保存されていません（:qa! で破棄して終了）", i + 1);
            }
            "q" | "quit" | "q!" | "quit!" | "qa" | "qall" | "quitall" | "qa!" | "qall!" | "quitall!" => return Effect::Quit,
            "wq" | "x" | "xit" => {
                if self.write() && self.dirty_docs().is_empty() {
                    return Effect::Quit;
                }
                if let Some(&i) = self.dirty_docs().first() {
                    self.message = format!("E162: バッファ {} の変更が保存されていません", i + 1);
                }
            }
            "wa" | "wall" => {
                self.write_all();
            }
            "wqa" | "wqall" | "xa" | "xall" => {
                if self.write_all() {
                    return Effect::Quit;
                }
            }
            "e" | "edit" if !arg.is_empty() => self.edit_file(PathBuf::from(arg)),
            "e!" | "edit!" => self.reload(),
            "ls" | "buffers" | "files" => self.list_buffers(),
            "Files" => self.open_finder(crate::finder::Source::Files),
            "Buffers" => self.open_finder(crate::finder::Source::Buffers),
            "Recent" | "History" => self.open_finder(crate::finder::Source::Recent),
            "Commands" | "Palette" => self.open_finder(crate::finder::Source::Commands),
            "Symbols" => self.symbol_search(false),
            "Types" | "Classes" => self.symbol_search(true),
            "Outline" => self.outline(),
            "Refs" | "References" => self.find_references(),
            "Idea" | "IntelliJ" => self.open_in_intellij(),
            "Make" | "make" => self.make(arg),
            "GitHunk" => self.open_hunk_popup(),
            "Git" | "G" | "Gcommit" => self.open_commit_panel(),
            "Gdiff" | "Gd" => self.diff_current(),
            "Gblame" | "Gb" => self.toggle_blame(),
            "Glog" | "Gl" => self.open_log(false),
            "Ghistory" | "Gfilelog" => self.open_log(true),
            "Gbranch" => self.git_new_branch(arg),
            "Gbranches" | "Gbr" => self.open_branches(),
            "Maven" | "Mvn" if arg.is_empty() => self.open_maven(),
            "Maven" | "Mvn" => self.maven_run(arg),
            "Db" | "Database" => self.open_db_panel(),
            "DbAdd" => self.db_add(arg),
            "DbRun" => self.db_run_at_cursor(),
            "DbSchema" => self.db_set_schema(arg),
            "DbExport" => self.db_export(arg),
            "DbCommit" => self.db_tx_command("commit"),
            "DbRollback" => self.db_tx_command("rollback"),
            "DbTx" => self.db_tx_command("toggle"),
            "DbLogout" => self.db_logout(),
            "DbNew" | "DbQuery" => self.db_new_query_command(),
            "Gpull" => self.run_git_job("pull ", &["pull", "--no-edit"]),
            "Gfetch" => self.run_git_job("フェッチ", &["fetch", "--all", "--prune"]),
            "Gpush" => self.push_from_anywhere(),
            "GitStage" => self.stage_hunk(),
            "GitRevert" => self.revert_hunk(),
            "eslint" | "Lint" => self.make("npx eslint -f unix ."),
            "Grep" | "grep" | "Rg" | "rg" => self.open_grep(arg),
            "cn" | "cnext" => self.next_hit(true, arg.parse().unwrap_or(1)),
            "cp" | "cprevious" | "cN" | "cNext" => self.next_hit(false, arg.parse().unwrap_or(1)),
            "ccl" | "cclose" => self.close_grep(),
            "Tree" | "Explore" | "Ex" | "NERDTreeToggle" => self.toggle_tree(),
            "cope" | "copen" => self.focus_panel(),
            "b" | "buffer" if !arg.is_empty() => self.goto_buffer(arg),
            "bn" | "bnext" => self.cycle_buffer(true, arg.parse().unwrap_or(1)),
            "bp" | "bprevious" | "bN" | "bNext" => self.cycle_buffer(false, arg.parse().unwrap_or(1)),
            "bd" | "bdelete" | "bw" | "bwipeout" => self.delete_buffer(arg, false),
            "bd!" | "bdelete!" | "bw!" | "bwipeout!" => self.delete_buffer(arg, true),
            "action" => self.action(arg),
            "source" | "so" => return Effect::ReloadRc,
            "NagiStats" => return Effect::Stats { reset: false },
            "NagiStats!" => return Effect::Stats { reset: true },
            "echo" => self.message = arg.trim_matches(['"', '\'']).to_string(),
            "noh" | "nohlsearch" => self.search.highlight = false,
            // ideavimrc の `O`: `:call append(expand('.'), '')`（現在行の下に空行を足す）
            "call" if arg.starts_with("append(") => self.append_blank_line(),
            "marks" => self.list_marks(),
            "earlier" | "ea" => self.undo_time(false, arg.parse().unwrap_or(1)),
            "later" | "lat" => self.undo_time(true, arg.parse().unwrap_or(1)),
            "delm" | "delmarks" => self.delete_marks(arg, false),
            "delm!" | "delmarks!" => self.delete_marks(arg, true),
            n if n.starts_with('!') => self.message = format!("未実装: 外部コマンド（{cmd}）"),
            _ => self.message = format!("E492: エディタのコマンドではありません: {cmd}"),
        }
        Effect::None
    }

    /// IdeaVim の `:action <IntelliJ のアクション名>` を Nagi の機能に振り分ける
    fn action(&mut self, name: &str) {
        match nagi_vim::action::ideavim(name) {
            Some((Some("jump.last_change"), _)) => self.jump_last_change(),
            Some((Some("search.buffer"), _)) => self.start_search(true, false, ""),
            Some((Some("replace.buffer"), _)) => self.start_search(true, true, ""),
            Some((Some("finder.files" | "finder.everything"), _)) => self.open_finder(crate::finder::Source::Files),
            Some((Some("finder.recent"), _)) => self.open_finder(crate::finder::Source::Recent),
            Some((Some("buffer.close"), _)) => self.delete_buffer("", false),
            Some((Some("search.project"), _)) => self.open_grep(""),
            Some((Some("tool.project"), _)) => self.toggle_tree(),
            Some((Some("palette"), _)) => self.open_finder(crate::finder::Source::Commands),
            Some((Some("finder.symbols"), _)) => self.symbol_search(false),
            Some((Some("finder.types"), _)) => self.symbol_search(true),
            Some((Some("outline"), _)) => self.outline(),
            Some((Some("nav.definition"), _)) => self.goto_definition(),
            Some((Some("nav.references"), _)) => self.find_references(),
            Some((Some("nav.method_prev"), _)) => self.method_jump(false),
            Some((Some("nav.method_next"), _)) => self.method_jump(true),
            Some((Some("jump.back"), _)) => self.jump(true, 1),
            Some((Some("nav.error_next"), _)) => self.next_error(true),
            Some((Some("git.hunk_next"), _)) => self.hunk_jump(true, 1),
            Some((Some("git.commit" | "git.changes"), _)) => self.open_commit_panel(),
            Some((Some("git.blame"), _)) => self.toggle_blame(),
            Some((Some("git.file_history"), _)) => self.open_log(true),
            Some((Some("git.log"), _)) => self.open_log(false),
            Some((Some("git.branches"), _)) => self.open_branches(),
            Some((Some("tool.maven"), _)) => self.toggle_right_tab(crate::right_panel::RightTab::Maven),
            Some((Some("tool.database"), _)) => self.toggle_right_tab(crate::right_panel::RightTab::Database),
            Some((Some("db.new_query"), _)) => self.db_new_query_command(),
            Some((Some("git.pull"), _)) => self.run_git_job("pull ", &["pull", "--no-edit"]),
            Some((Some("git.fetch"), _)) => self.run_git_job("フェッチ", &["fetch", "--all", "--prune"]),
            Some((Some("git.push"), _)) => self.push_from_anywhere(),
            Some((Some("git.hunk_prev"), _)) => self.hunk_jump(false, 1),
            Some((Some("git.hunk_popup"), _)) => self.open_hunk_popup(),
            Some((Some("git.hunk_revert"), _)) => self.revert_hunk(),
            Some((Some("nav.error_prev"), _)) => self.next_error(false),
            Some((Some("jump.forward"), _)) => self.jump(false, 1),
            Some((Some(cmd), desc)) => self.message = format!("未実装: {desc}（{cmd}）"),
            Some((None, desc)) => self.message = format!("Nagi には無い機能: {desc}"),
            None => self.message = format!("不明なアクション: {name}"),
        }
    }

    pub(crate) fn jump_last_change(&mut self) {
        match self.last_change {
            Some(at) => {
                self.set_abs(at.min(self.buf.len_bytes()));
                self.clamp_normal();
            }
            None => self.message = "E664: 変更履歴がありません".into(),
        }
    }

    /// 現在行の下に空行を足す（ideavimrc の `O` = `:call append(expand('.'), '')`）。カーソルは動かさない
    fn append_blank_line(&mut self) {
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
            || (self.mode == Mode::List && self.list_side == Side::Bottom && self.db_results.editing.is_some())
    }

    /// 貼り付けを受ける入力欄にいるか（Vim の Insert の Ctrl-V は別）
    fn paste_target(&self) -> bool {
        self.has_prompt()
            || matches!(self.mode, Mode::Branches)
            || (self.mode == Mode::Log && self.log.as_ref().is_some_and(|l| l.filter.is_some()))
            || (self.right_focused() && self.db.as_ref().is_some_and(|p| p.password.is_some()))
    }

    /// クリップボードの文字を、今の入力欄に入れる（コミットメッセージ以外は 1 行にする）
    pub(crate) fn paste_clipboard(&mut self) {
        let text = match &self.clipboard {
            Some(cb) => (cb.get)(),
            None => crate::clipboard::get(),
        };
        let Some(text) = text.filter(|t| !t.is_empty()) else {
            self.message = "クリップボードが空です".into();
            return;
        };
        let text = if self.mode == Mode::CommitMsg {
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
        match self.mode {
            Mode::Insert => self.insert_text(text),
            Mode::Search => {
                self.query.insert(text);
                self.update_incsearch();
            }
            Mode::Replace => {
                if self.replace_field == 0 {
                    self.query.insert(text);
                    self.update_incsearch();
                } else {
                    self.repl.insert(text);
                }
            }
            Mode::Command => {
                self.cmdline.insert(text);
                self.update_cmd_complete();
            }
            Mode::Finder => self.finder_insert(text),
            Mode::GrepPrompt => self.grep_insert(text),
            Mode::CommitMsg => self.commit_msg_insert(text),
            Mode::Branches => self.branches_insert(text),
            Mode::DbTable => self.db_table_insert(text),
            Mode::List if self.list_side == Side::Right => {
                self.db_insert(text);
            }
            Mode::List if self.list_side == Side::Bottom => {
                if let Some((_, f)) = &mut self.db_results.editing {
                    f.insert_text(text);
                }
            }
            Mode::Log => {
                if let Some(l) = &mut self.log {
                    if let Some(f) = &mut l.filter {
                        f.insert_text(text);
                        l.filter_text = f.text();
                        l.refilter_pub();
                    }
                }
            }
            _ => {}
        }
    }

    /// 画面上の (行, セル列) にあたるカーソル位置。行は表示中のスクロール位置から数える
    pub fn pos_at(&self, row: usize, col: usize) -> Cursor {
        let line = if self.folds.is_empty() {
            (self.top + row).min(self.buf.line_count().saturating_sub(1))
        } else {
            let shown = self.folds.visible_from(self.top, row + 1, self.buf.line_count());
            shown.last().copied().unwrap_or(0)
        };
        let text = self.buf.line(line);
        let col = col.saturating_sub(self.gutter());
        let byte = if self.mode == Mode::Insert && col >= display_col(&text, text.len(), false) {
            text.len()
        } else {
            byte_at_col(&text, col, false)
        };
        Cursor { line, byte, want_col: display_col(&text, byte, false) }
    }

    /// マウスのクリック: 画面上の (行, セル列) へカーソルを移す
    pub fn click(&mut self, row: usize, col: usize) {
        if self.has_prompt() {
            return;
        }
        if matches!(self.mode, Mode::Visual { .. }) {
            self.mode = Mode::Normal;
        }
        self.cursor = self.pos_at(row, col);
        if self.mode == Mode::Normal {
            self.clamp_normal();
        }
        self.scroll_into_view();
    }

    /// ドラッグ: 押した位置（絶対 byte）を起点に Visual で選ぶ。Insert 中なら先に抜ける
    pub fn drag(&mut self, anchor: usize, row: usize, col: usize) {
        if self.has_prompt() {
            return;
        }
        if self.mode == Mode::Insert {
            self.key(Key::Esc);
        }
        if !matches!(self.mode, Mode::Visual { .. }) {
            self.anchor = anchor;
            self.mode = Mode::Visual { line: false, block: false };
        }
        self.cursor = self.pos_at(row, col);
        self.clamp_normal();
    }

    /// ダブルクリックで単語、トリプルクリックで行を選ぶ（viw / V と同じ）
    pub fn select_at(&mut self, row: usize, col: usize, clicks: u32) {
        if self.has_prompt() {
            return;
        }
        if self.mode == Mode::Insert {
            self.key(Key::Esc);
        }
        self.mode = Mode::Normal;
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
        if self.mode == Mode::Normal {
            self.clamp_normal();
        }
    }

    /// 今の表示位置（行単位の小数）
    pub fn scroll_pos(&self) -> f64 {
        self.top as f64 + self.frac as f64
    }

    pub(crate) fn scroll_into_view(&mut self) {
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
        assert_eq!(e.mode, Mode::Normal);
        assert_eq!(e.message, "/needle  [1/1]");
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
        assert_eq!(e.mode, Mode::Visual { line: false, block: false });
        assert_eq!((e.cursor.line, e.cursor.byte), (1, 3));
        assert_eq!(e.visual_range(), (2, 16, false));
    }

    #[test]
    fn drag_leaves_insert() {
        let mut e = ed("abc\n");
        e.key(Key::Char('i'));
        e.drag(0, 0, GUTTER + 2);
        assert_eq!(e.mode, Mode::Visual { line: false, block: false });
    }

    #[test]
    fn double_and_triple_click() {
        let mut e = ed("let name = 1;\n");
        e.select_at(0, GUTTER + 5, 2);
        assert_eq!(e.visual_range(), (4, 8, false), "単語 name");
        e.select_at(0, GUTTER + 5, 3);
        assert_eq!(e.mode, Mode::Visual { line: true, block: false });
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
}
