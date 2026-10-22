//! nagi（凪）: 静かで、余計なものがない、速いエディタ。
//!
//! 使い方: nagi [FILE] [--startup-report] [--exit-after-first-frame] [--keys KEYS] [--replay KEYS] [--replay-file PATH] [--headless]
//!
//! `--keys` は起動直後にキー列（Vim の表記。`/foo<CR>` など）を流し込み、最後に 1 回だけ描く。画面の確認やデモ用。
//! `--replay` はキー列を 1 打ずつ、キー入力と同じ道筋（処理 → 描画 → 計測の記録）で流す。`:NagiStats` の確認用。
//! `--replay-file` は同じキー列を UTF-8 のファイルから読む（シェルの引用符や文字コードを通さない。末尾の改行は無視）。
//! `--headless` は窓を表に出さずに流す（試験用。ほかの作業の入力を取らない。replay.rs の `<Shot>` で画面を写せる）。
//!
//! 起動は並列（architecture.md §6.4）: GPU・フォント・ファイル読込を別スレッドで進め、
//! メインスレッドはその間にイベントループと非表示のウィンドウを作る。最初のフレームを描いてから表示する。
//!
//! 画面のアプリとして作る（起動してもコンソールの窓を開かない）。書き出す先（標準エラー）がつながっていなければ、
//! 記録（落ちたときの理由・調べ物用の出力）は `%LOCALAPPDATA%\nagi\nagi.log` に書く。

// 試験の実行ファイルは結果を書き出すので、コンソールのアプリのまま
#![cfg_attr(not(test), windows_subsystem = "windows")]

mod angular;
mod dialogs;
mod jumplist;
mod read_view;
mod run_configs;
mod run_panel;
mod run_dialog;
mod keymap;
mod settings;
mod settings_dialog;
mod editor_settings;
mod smart_complete;
mod autoread;
mod replay;
mod bottom_panel;
mod terminal;
mod launcher;
mod md_preview;
mod project_open;
mod blame;
mod db_panel;
mod db_table;
mod branches;
mod block;
mod buffers;
mod clipboard;
mod clock;
mod cmdline_complete;
mod commit_panel;
mod complete;
mod diagnostics;
mod diff_view;
#[cfg(test)]
mod difftest;
mod editor;
mod ex_cmds;
mod finder;
mod git_ui;
mod folds;
mod grep_panel;
mod log_view;
mod maven_panel;
mod macros;
mod marks;
mod nav;
mod resize;
mod result_grid;
mod right_panel;
mod save_dialog;
mod sql_complete;
mod toggles;
mod tree_panel;
mod normal;
mod stats;
mod view;
mod list;
mod menu;
mod buffer_tabs;
mod plain;
mod layout;
mod component;
mod commands;
mod notify;
mod ui;
mod grammars;
mod vim_field;
mod panes;
mod instance;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;

use editor::{Editor, Effect, State};
use nagi_vim::{Feed, Key as VKey, Matcher, Rc};
use std::time::{Duration, Instant};
use nagi_core::Buffer;
use nagi_render::{Gpu, Renderer};
use nagi_text::{FontData, Fonts};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Window, WindowId};

/// 論理ピクセルでのフォントサイズ（表示スケールを掛けて使う）
/// 文字の大きさの既定（px。設定の「文字の大きさ」が 0 のとき）
const FONT_PX: f64 = 14.0;

/// 設定の文字の大きさ（pt）を px に（0 なら既定）
fn font_px(s: &settings::Settings) -> f64 {
    match s.int("appearance.font_size") {
        0 => FONT_PX,
        pt => pt as f64 * 96.0 / 72.0,
    }
}

/// 設定のフォントの名前（英数字・日本語）
fn font_names(s: &settings::Settings) -> Vec<String> {
    vec![s.get("appearance.font"), s.get("appearance.font_ja")]
}

/// 名前のフォントを読む。英数字のフォントが読めなければ既定のもので
fn load_font_chain(names: &[String]) -> Vec<FontData> {
    let mut chain: Vec<FontData> = names.iter().filter_map(|n| nagi_text::load_by_name(n)).collect();
    if chain.is_empty() || nagi_text::load_by_name(&names[0]).is_none() {
        chain = nagi_text::DEFAULT_FONTS[..2].iter().filter_map(|n| nagi_text::load_by_name(n)).collect();
    }
    chain
}

struct Opts {
    file: Option<PathBuf>,
    /// 窓の作業フォルダ（2 回目の起動で足した窓は、その nagi を起動したフォルダ。None ならプロセスの作業フォルダ）
    cwd: Option<PathBuf>,
    keys: Option<String>,
    replay: Option<String>,
    startup_report: bool,
    exit_after_first_frame: bool,
    /// 窓を表に出さない（`--replay` の試験を、ほかの作業の入力を取らずに流す。描いたものは `<Shot>` で読み戻せる）
    headless: bool,
}

impl Opts {
    fn parse() -> Self {
        let mut o = Opts { file: None, cwd: None, keys: None, replay: None, startup_report: false, exit_after_first_frame: false, headless: false };
        let mut args = std::env::args().skip(1);
        while let Some(a) = args.next() {
            match a.as_str() {
                "--keys" => o.keys = args.next(),
                "--replay" => o.replay = args.next(),
                "--replay-file" => match args.next().map(|p| std::fs::read_to_string(&p).map_err(|e| format!("{p}: {e}"))) {
                    Some(Ok(k)) => o.replay = Some(k.trim_end_matches(['\r', '\n']).to_string()),
                    Some(Err(e)) => eprintln!("--replay-file: {e}"),
                    None => eprintln!("--replay-file: ファイルを指定してください"),
                },
                "--startup-report" => o.startup_report = true,
                "--headless" => o.headless = true,
                "--exit-after-first-frame" => {
                    o.exit_after_first_frame = true;
                    o.startup_report = true;
                }
                _ => o.file = Some(PathBuf::from(a)),
            }
        }
        o
    }
}

/// 起動時に別スレッドで進める初期化の受け口
struct Pending {
    gpu: mpsc::Receiver<windows::core::Result<Gpu>>,
    fonts: mpsc::Receiver<Vec<FontData>>,
    /// 開くファイル（大きなファイルは先頭部分だけで、true が付く）
    file: mpsc::Receiver<(Buffer, bool)>,
    /// 大きなファイルの全体
    rest: mpsc::Receiver<Buffer>,
    rc: mpsc::Receiver<Rc>,
}

/// マッピングの続きを待つ時間（Vim の timeoutlen の既定値）
const TIMEOUT_LEN: Duration = Duration::from_millis(1000);
/// ホイール 1 目盛りで動かす行数と、それを動かしきる時間
const WHEEL_LINES: f64 = 3.0;

/// タッチパッドで止まってから行境界へ寄せるまでの時間
const SNAP_AFTER: Duration = Duration::from_millis(120);
/// `--replay` で 1 打ずつ流す間隔（速く打つ人の打鍵間隔くらい。詰めすぎるとフレームの待ちが毎回乗る）
const REPLAY_EVERY: Duration = Duration::from_millis(30);
/// `--replay` の間隔。`NAGI_REPLAY_MS` で変えられる（打鍵の間隔で速さが変わるかを調べる用）
/// 移動のキーの押しっぱなし: 繰り返し始めるまで・繰り返しの間隔・速める間隔と、速めるまで
/// （Windows の最速は 約 500ms・約 33ms ごと）
/// 押しっぱなしで速めるまで（間隔の速さは設定の「エディタ」）
const REPEAT_FAST_AFTER: Duration = Duration::from_millis(1200);

fn replay_every() -> Duration {
    std::env::var("NAGI_REPLAY_MS").ok().and_then(|v| v.parse().ok()).map_or(REPLAY_EVERY, Duration::from_millis)
}
/// ドラッグで画面外に出たときの自動スクロールの間隔
const AUTOSCROLL_EVERY: Duration = Duration::from_millis(40);

/// スムーズスクロールの動き（行単位の小数で from → to）
struct ScrollAnim {
    from: f64,
    to: f64,
    start: Instant,
    /// かける時間（設定の「スムーズスクロールの時間」。0 ならすぐ）
    dur: Duration,
}

impl ScrollAnim {
    /// 今の位置と、動ききったか
    fn at(&self, now: Instant) -> (f64, bool) {
        let t = if self.dur.is_zero() { 1.0 } else { (now - self.start).as_secs_f64() / self.dur.as_secs_f64() };
        if t >= 1.0 {
            return (self.to, true);
        }
        // ease-out（最初に速く、最後にゆっくり止まる）
        let e = 1.0 - (1.0 - t).powi(3);
        (self.from + (self.to - self.from) * e, false)
    }
}

/// 左ボタンを押している間の状態
struct Press {
    /// 押した位置（絶対 byte）。ドラッグ選択の起点
    anchor: usize,
    /// 押したセル（本文の行, セル列）。ここから出たらドラッグ
    cell: (usize, usize),
    /// ダブル・トリプルクリックで選んだ後は、ドラッグで選び直さない
    selected: bool,
}

/// 窓の題名: 「nagi — プロジェクト名 — プロジェクトの中のパス」（プロジェクトの外ならフルパス）
fn title_for(path: Option<&std::path::Path>, root: Option<&std::path::Path>) -> String {
    let project = root.and_then(|r| r.file_name()).map(|n| n.to_string_lossy().into_owned());
    let file = path.map(|p| {
        let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
        match root.and_then(|r| abs.strip_prefix(r).ok()) {
            Some(rel) => rel.display().to_string(),
            None => abs.display().to_string(),
        }
    });
    match (project, file) {
        (Some(pr), Some(f)) => format!("nagi — {pr} — {f}"),
        (Some(pr), None) => format!("nagi — {pr}"),
        (None, Some(f)) => format!("nagi — {f}"),
        (None, None) => "nagi".into(),
    }
}

/// ストライプの幅（セル。全角 1 字がちょうど入る）
const STRIPE_W: usize = 2;

/// 窓 1 枚分（窓・描画・エディタと、その窓の入力の状態）。App が束ねる
struct Frame {
    /// 開いている「名前を付けて保存」のダイアログ
    save_dialog: Option<nagi_core::task::Task<Option<std::path::PathBuf>>>,
    /// 題名に出しているパス（バッファを切り替えたら題名を変える）
    /// 題名に出しているもの（ファイル, プロジェクトの根）
    title_path: (Option<std::path::PathBuf>, Option<std::path::PathBuf>),
    opts: Opts,
    pending: Option<Pending>,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    fonts: Option<Fonts>,
    ed: Option<Editor>,
    /// 裏の処理から起こされて、まだ受け取っていない（起こすのは受け取るまで 1 度だけ。
    /// ターミナルのように何度も起こすものがあっても、描くのが追いつかずにキーが待たされない）
    woken: Arc<std::sync::atomic::AtomicBool>,
    modifiers: ModifiersState,
    scale: f64,
    /// 文字の大きさ（px。表示倍率をかける前。設定の「文字の大きさ」）と、読んだフォントの名前
    font_px: f64,
    font_names: Vec<String>,
    mouse: (f64, f64),
    /// IME を許可しているか（案1: Insert だけ許可。ADR 0006）
    ime_allowed: bool,
    rc: Rc,
    matcher: Matcher,
    /// マッピングの続きを待つ期限
    map_deadline: Option<Instant>,
    scroll_anim: Option<ScrollAnim>,
    /// タッチパッドの、まだ 1 行・1 列に満たない動き（欄・小窓は行・列の単位で送る）
    wheel_acc: (f64, f64),
    snap_deadline: Option<Instant>,
    press: Option<Press>,
    /// 直前のクリック（時刻, 画面上のセル, 連続回数）。ダブル・トリプルクリックの判定用
    last_click: Option<(Instant, (usize, usize), u32)>,
    autoscroll_deadline: Option<Instant>,
    /// 押しっぱなしの移動のキー（OS の繰り返しより速く、自分で繰り返す）: (物理キー, 流すキー, 押した時刻, 次に流す時刻)
    held: Option<(winit::keyboard::PhysicalKey, Vec<VKey>, Instant, Instant)>,
    /// ドラッグしている区切り
    sep_drag: Option<resize::Sep>,
    /// ターミナルの中で範囲を選んでいる（左ボタンを押したまま）
    term_select: bool,
    /// 検索・ログの欄の中で行の範囲を選んでいる（左ボタンを押したまま）
    list_select: bool,
    /// 今のマウスのカーソルの形（区切りの上では ↔ / ↕）
    cursor_icon: winit::window::CursorIcon,
    stats: stats::Stats,
    /// `--replay` の残りの手順（1 回のイベント処理で 1 つずつ。キーと、試験用のマウス・待ち・確かめ。replay.rs）
    replay: std::collections::VecDeque<replay::Step>,
    replay_deadline: Option<Instant>,
    /// 上下の先読みを済ませた表示位置
    prefetched_top: Option<usize>,
    /// 裏の処理（構文解析）が終わったことを知らせる
    proxy: Option<winit::event_loop::EventLoopProxy<UserEvent>>,
    /// 閉じた（App が外す。最後の 1 枚なら終わる）
    closed: bool,
    /// この窓が次に起きたい時（about_to_wait で決め、App が全部の窓のうち早いものにする）
    flow: ControlFlow,
}

impl Frame {
    fn startup(&mut self, el: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title(title_for(None, None))
            .with_visible(false)
            // タイトルバーも暗くして、ターミナルと見分けがつかないようにする
            .with_theme(Some(winit::window::Theme::Dark))
            .with_inner_size(winit::dpi::LogicalSize::new(1100.0, 720.0));
        // exe に埋め込んだアイコン（build.rs。ID 1）を窓とタスクバーに
        let attrs = {
            use winit::platform::windows::{IconExtWindows, WindowAttributesExtWindows};
            match winit::window::Icon::from_resource(1, None) {
                Ok(icon) => attrs.with_window_icon(Some(icon.clone())).with_taskbar_icon(Some(icon)),
                Err(_) => attrs,
            }
        };
        // 試験（--headless）: 表に出さないうえ、万一出ても見えず前に出ない所に作る（画面の外・タスクバーに出さない）
        let attrs = if self.opts.headless {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs.with_position(winit::dpi::PhysicalPosition::new(-32000, -32000)).with_active(false).with_skip_taskbar(true)
        } else {
            attrs
        };
        let window = Arc::new(el.create_window(attrs).unwrap());
        clock::mark("window created (hidden)");
        self.scale = window.scale_factor();
        let hwnd = {
            let RawWindowHandle::Win32(h) = window.window_handle().unwrap().as_raw() else { unreachable!() };
            windows::Win32::Foundation::HWND(h.hwnd.get() as *mut _)
        };
        // 試験（--headless）: キー・マウスを受けない窓にする（操作は --replay が中から流す。ほかの作業の入力を吸わない）
        if self.opts.headless {
            unsafe {
                let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(hwnd, false);
            }
        }

        // GPU の準備（最も遅い）を待つ間に、フォントとファイルを受け取って最初の画面のグリフをラスタライズしておく
        let p = self.pending.take().unwrap();
        let chain = p.fonts.recv().unwrap();
        let (buf, partial) = p.file.recv().unwrap();
        self.rc = p.rc.recv().unwrap();
        let st = settings::Settings::load(None);
        self.font_px = font_px(&st);
        let mut fonts = Fonts::new(chain, (self.font_px * self.scale).round() as f32);
        fonts.set_line_height(st.int("appearance.line_height") as f32 / 100.0);
        self.font_names = font_names(&st);
        // 題名は最初の描画で（プロジェクトの根が決まってから）付ける
        window.set_title(&title_for(buf.path.as_deref(), None));
        self.title_path = (None, None);
        self.ed = Some(Editor::new(buf, None));
        if let Some(ed) = &mut self.ed {
            // ダイアログ（フォルダの選択）の持ち主にする
            ed.hwnd = hwnd.0 as isize;
            if let Some(d) = &self.opts.cwd {
                ed.cwd = d.clone();
            }
        }
        self.apply_rc();
        // 構文解析は裏で行い、終わったらイベントループを起こして色を付ける（大きなファイルでも表示を待たせない）
        if let (Some(ed), Some(proxy)) = (&mut self.ed, self.proxy.take()) {
            let woken = self.woken.clone();
            ed.waker = Some(Arc::new(move || {
                if !woken.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    let _ = proxy.send_event(UserEvent::Wake);
                }
            }));
            if partial {
                // 構文解析は全体が届いてから
                ed.wait_full_buffer(p.rest);
            } else {
                ed.start_syntax();
            }
        }
        let size = window.inner_size();
        let glyphs = view::rasterize_first_screen(self.ed.as_ref().unwrap(), &mut fonts, size.height);
        clock::mark("glyphs rasterized");
        let gpu = p.gpu.recv().unwrap().expect("D3D11 の初期化に失敗");
        clock::mark("gpu joined");

        let mut renderer = Renderer::new(gpu, hwnd.0 as isize, size.width, size.height).expect("スワップチェーンの作成に失敗");
        clock::mark("swap chain created");
        renderer.insert_rasterized(glyphs);
        clock::mark("glyphs uploaded");
        self.fonts = Some(fonts);
        self.renderer = Some(renderer);
        self.window = Some(window.clone());
        self.draw();
        clock::mark("first present");
        // 表示だけ先に済ませる。アクティブにする処理（フォーカスと IME の準備）は 20〜35ms かかるので、見えてから行う
        if !self.opts.headless {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(hwnd, windows::Win32::UI::WindowsAndMessaging::SW_SHOWNOACTIVATE);
            }
            clock::mark("window visible");
            window.set_visible(true);
            window.focus_window();
            clock::mark("window activated");
        }
        if let (Some(r), Some(f)) = (&mut self.renderer, &mut self.fonts) {
            view::prefetch_ascii(f, r);
            clock::mark("ascii prefetched");
        }
        // 外部での変更と設定ファイルの見張りは、表示の後に始める（起動を待たせない）
        let rc_files = self.rc.sources.clone();
        if let Some(ed) = &mut self.ed {
            ed.start_watch(&rc_files);
            clock::mark("watch started");
            // 開いたファイルのプロジェクトの設定の上書き
            let root = ed.nav_root();
            ed.settings.set_root(Some(&root));
            ed.apply_settings(None);
            // 引数がフォルダなら、プロジェクトとして開く（IntelliJ・Eclipse の設定を読み、前に開いていたファイルを復元）
            if let Some(dir) = self.opts.file.clone().filter(|p| p.is_dir()) {
                ed.open_project(dir);
                clock::mark("project opened");
            } else if self.opts.file.is_none() {
                // 引数なしで起動したら、設定の「起動時」: ランチャー（既定）・前回のプロジェクト・空のバッファ
                match ed.settings.get("project.startup").as_str() {
                    "last" => match project_open::recent_entries().into_iter().find(|e| !e.missing) {
                        Some(e) => ed.open_project(e.path),
                        None => ed.open_launcher(),
                    },
                    "empty" => {}
                    _ => ed.open_launcher(),
                }
            }
            // タスクバーのジャンプリスト（最近のプロジェクト）
            jumplist::refresh();
            // 定義の索引も表示の後に裏で作る（保存したものがあれば差分だけ）
            ed.start_index();
            // git の状態（行頭の変更マーカーと状態行のブランチ）
            ed.start_git();
        }
        // 試験用: 表示倍率が変わったときと同じ処理をこの倍率で行う（OS の設定を変えずに確かめる）
        if let Some(s) = std::env::var("NAGI_DEBUG_SCALE").ok().and_then(|v| v.parse().ok()) {
            self.set_scale(s);
        }
        if self.opts.startup_report {
            clock::report();
        }
        if self.opts.exit_after_first_frame {
            self.closed = true;
        }
        if let Some(keys) = self.opts.keys.take() {
            for k in nagi_vim::parse_keys(&keys, &self.rc.leader.clone()) {
                self.feed(k, el);
            }
            self.sync_ime();
            self.draw();
        }
        if let Some(keys) = self.opts.replay.take() {
            self.replay = replay::parse(&keys, &self.rc.leader.clone());
            self.replay_deadline = Some(Instant::now() + replay_every());
            // 文字の所を押す・文字が出るのを待つために、描いた文字を控える
            if let Some(ed) = &mut self.ed {
                ed.record_texts = true;
            }
        }
    }


    /// 本文に使える行数（最下行の分を除く。置換中は 2 行）
    /// 画面の本文の行数（最下行を除く。分割中は全部の窓を合わせた高さ）
    fn rows(&self) -> usize {
        let (Some(r), Some(f)) = (&self.renderer, &self.fonts) else { return 1 };
        let status = self.ed.as_ref().map_or(1, |e| e.status_rows());
        ((r.size().1 as f32 / f.cell().h) as usize).saturating_sub(status).max(1)
    }

    fn cols(&self) -> usize {
        let (Some(r), Some(f)) = (&self.renderer, &self.fonts) else { return 80 };
        (r.size().0 as f32 / f.cell().w) as usize
    }

    /// 画面の大きさをエディタに伝え、作業中の窓の本文の行数を返す（マウスの処理の前に呼ぶ）
    fn sync_screen(&mut self) -> usize {
        let (cols, rows) = (self.cols(), self.rows());
        let Some(ed) = &mut self.ed else { return rows };
        ed.set_screen(cols, rows);
        ed.rows
    }

    /// 1 フレーム描く。戻り値は内訳（構文木の更新, スワップチェーンの待ち, 組み立て, 送出）
    /// 表示倍率に文字の大きさを合わせ、アトラスを作り直す（ウィンドウの大きさは winit が続けて Resized で知らせる）
    fn set_scale(&mut self, scale: f64) {
        self.scale = scale;
        if let (Some(f), Some(r)) = (&mut self.fonts, &mut self.renderer) {
            f.set_px((self.font_px * scale).round() as f32);
            r.clear_glyphs();
            view::prefetch_ascii(f, r);
            self.prefetched_top = None;
        }
        self.draw();
    }

    /// 設定の外観（フォント・大きさ・行の高さ）が変わったら、フォントを作り直す（表示倍率が変わったときと同じ道筋）
    fn reload_fonts(&mut self) {
        let Some(ed) = &self.ed else { return };
        let names = font_names(&ed.settings);
        let px = font_px(&ed.settings);
        let line = ed.settings.int("appearance.line_height") as f32 / 100.0;
        if names != self.font_names {
            let chain = load_font_chain(&names);
            if !chain.is_empty() {
                self.fonts = Some(Fonts::new(chain, (px * self.scale).round() as f32));
                self.font_names = names;
            }
        }
        self.font_px = px;
        if let (Some(f), Some(r)) = (&mut self.fonts, &mut self.renderer) {
            f.set_line_height(line);
            f.set_px((px * self.scale).round() as f32);
            r.clear_glyphs();
            view::prefetch_ascii(f, r);
            self.prefetched_top = None;
        }
        self.sync_screen();
    }

    fn draw(&mut self) -> [Duration; 4] {
        // 設定を適用して外観が変わったら、フォントを作り直す
        if self.ed.as_mut().is_some_and(|e| std::mem::take(&mut e.appearance_changed)) {
            self.reload_fonts();
        }
        let (Some(r), Some(f), Some(ed)) = (&mut self.renderer, &mut self.fonts, &mut self.ed) else { return Default::default() };
        let t0 = Instant::now();
        // 構文木の更新は待ちの前に済ませる（待っている間に前のフレームが出る）
        if let Some(s) = &mut ed.syntax {
            s.sync(&mut ed.buf);
        }
        let t1 = Instant::now();
        r.wait_frame();
        let t2 = Instant::now();
        let status = ed.status_rows();
        let rows = ((r.size().1 as f32 / f.cell().h) as usize).saturating_sub(status).max(1);
        ed.set_screen((r.size().0 as f32 / f.cell().w) as usize, rows);
        ed.scroll_into_view();
        ed.showcmd = self.matcher.pending().iter().map(|k| k.label()).collect();
        let insts = view::build(ed, f, r);
        // 入力欄の縦棒の所に IME の候補の窓を出す
        if self.ime_allowed {
            if let (Some(w), Some((col, y))) = (&self.window, ed.prompt_caret.get()) {
                let cell = f.cell();
                w.set_ime_cursor_area(winit::dpi::PhysicalPosition::new((col as f32 * cell.w) as i32, y as i32), winit::dpi::PhysicalSize::new(cell.w as u32, cell.h as u32));
            }
        }
        clock::mark_once("first frame built");
        // バッファを切り替えたら題名も変える
        let key = (ed.buf.path.clone(), Some(ed.nav_root()));
        if self.title_path != key {
            self.title_path = key;
            if let Some(w) = &self.window {
                w.set_title(&title_for(self.title_path.0.as_deref(), self.title_path.1.as_deref()));
            }
        }
        let t3 = Instant::now();
        if let Err(e) = r.draw(&insts, view::theme_color(view::BG)) {
            nagi_core::log::error(&format!("描けません: {e}"));
        }
        [t1 - t0, t2 - t1, t3 - t2, t3.elapsed()]
    }

    /// 「名前を付けて保存」のダイアログを出す（別のスレッドで。選んだら user_event で保存する）
    fn open_save_dialog(&mut self) {
        let (Some(w), Some(ed)) = (&self.window, &self.ed) else { return };
        let owner = match w.window_handle().map(|h| h.as_raw()) {
            Ok(winit::raw_window_handle::RawWindowHandle::Win32(h)) => h.hwnd.get(),
            _ => 0,
        };
        let path = ed.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok());
        let dir = path.as_ref().and_then(|p| p.parent().map(|d| d.to_path_buf())).or_else(|| Some(ed.cwd.clone()));
        let name = path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
        self.save_dialog = Some(save_dialog::open(owner, dir, name, ed.waker.clone()));
    }

    /// 設定（vimrc）を読み直して反映する（`:source` と、設定ファイルが書き換わったとき）
    fn reload_rc(&mut self) {
        let t = Instant::now();
        self.rc = nagi_vim::load_rc_from(self.ed.as_ref().and_then(|e| e.settings.text("vim.rc_path")).as_deref());
        self.apply_rc();
        let n = self.rc.keymap.normal.len();
        let us = t.elapsed().as_micros();
        if let Some(ed) = &mut self.ed {
            ed.notify.info(format!("設定を読み直しました（マッピング {n} 件、{us}µs）"));
        }
    }

    /// 設定（vimrc）の内容をエディタに反映する
    fn apply_rc(&mut self) {
        let Some(ed) = &mut self.ed else { return };
        ed.rc_clipboard = self.rc.has("clipboard=unnamedplus") || self.rc.has("clipboard=unnamed");
        ed.sync_clipboard();
    }

    fn on_key(&mut self, ev: KeyEvent, el: &ActiveEventLoop) {
        if ev.state != ElementState::Pressed {
            if self.held.as_ref().is_some_and(|h| h.0 == ev.physical_key) {
                self.held = None;
            }
            return;
        }
        // 押しっぱなしの移動のキーは自分で繰り返すので、OS の繰り返しは捨てる
        if ev.repeat && self.held.as_ref().is_some_and(|h| h.0 == ev.physical_key) {
            return;
        }
        if std::env::var_os("NAGI_DEBUG_IME").is_some() {
            eprintln!("[{:.1}] key {:?} text={:?}", clock::since_process_start_ms(), ev.logical_key, ev.text);
        }
        // winit #4508: 変換中は VK_PROCESSKEY が KeyboardInput として届く。捨てる
        if ev.logical_key == Key::Named(NamedKey::Process) {
            return;
        }
        let Some(ed) = &self.ed else { return };
        // 変換中のキーは IME のもの（Esc も変換の取り消しであって、モードの切り替えではない）
        if !ed.preedit.is_empty() {
            return;
        }
        let ctrl = self.modifiers.control_key();
        let alt = self.modifiers.alt_key();
        let in_terminal = matches!(ed.state(), State::Terminal);
        // キーマップ（設定。Ctrl+Alt+S・Shift+F10 などの IntelliJ の組み合わせは既定値）: 付けた操作にする。
        // 元の組み合わせから別のキーへ移した操作は、元のキーでは動かさない
        if let Some(chord) = chord_of(&ev.logical_key, self.modifiers) {
            let (action, moved) = (keymap::action_for(&ed.settings, &chord), keymap::default_moved(&ed.settings, &chord));
            match action {
                Some(keymap::Action::Key(k)) => {
                    self.held = None;
                    self.handle_keys(vec![k], el);
                    return;
                }
                // コマンドの表の操作（今の状態で動かせないときは、キーとしてそのまま流す）
                Some(keymap::Action::Cmd(c)) => {
                    self.sync_screen();
                    if let Some(eff) = self.ed.as_mut().and_then(|ed| ed.shortcut(c)) {
                        self.held = None;
                        if !self.apply_effect(eff, el) {
                            self.draw();
                            self.sync_ime();
                        }
                        return;
                    }
                }
                None => {}
            }
            if moved {
                return;
            }
        }
        let keys: Vec<VKey> = match &ev.logical_key {
            Key::Character(c) if alt && !ctrl => c.chars().next().map(|ch| VKey::Alt(ch.to_ascii_lowercase())).into_iter().collect(),
            Key::Character(c) if ctrl => c.chars().next().map(|ch| VKey::Ctrl(ch.to_ascii_lowercase())).into_iter().collect(),
            Key::Character(_) => ev.text.as_deref().unwrap_or("").chars().map(VKey::Char).collect(),
            Key::Named(n) => match n {
                // Ctrl+Space は候補を出す（IntelliJ と同じ）
                NamedKey::Space if ctrl => vec![VKey::Ctrl(' ')],
                NamedKey::Space => vec![VKey::Char(' ')],
                NamedKey::Escape => vec![VKey::Esc],
                // ターミナルでは Ctrl+Enter・Shift+Enter は改行（Ctrl-j の \n。Claude Code などで送らずに改行する）
                NamedKey::Enter if (ctrl || self.modifiers.shift_key()) && in_terminal => vec![VKey::Ctrl('j')],
                // Ctrl-Enter はコミット（Ctrl-m として渡す）
                NamedKey::Enter if ctrl => vec![VKey::Ctrl('m')],
                NamedKey::Enter => vec![VKey::Enter],
                // Shift+Insert は貼り付け（入力欄では Ctrl+V と同じ）
                NamedKey::Insert if self.modifiers.shift_key() => vec![VKey::Paste],
                // 移動・削除のキーは修飾（Shift で選ぶ・Ctrl で単語など）を付けて渡す。Vim の操作とパネルは修飾を外して見る
                n => {
                    let k = match n {
                        NamedKey::Tab => VKey::Tab,
                        NamedKey::Backspace => VKey::Backspace,
                        NamedKey::Delete => VKey::Delete,
                        NamedKey::ArrowUp => VKey::Up,
                        NamedKey::ArrowDown => VKey::Down,
                        NamedKey::ArrowLeft => VKey::Left,
                        NamedKey::ArrowRight => VKey::Right,
                        NamedKey::Home => VKey::Home,
                        NamedKey::End => VKey::End,
                        NamedKey::PageUp => VKey::PageUp,
                        NamedKey::PageDown => VKey::PageDown,
                        // F12・Alt+F12・Shift+F10 などはキーマップ（上）で操作にする
                        _ => return,
                    };
                    let m = nagi_vim::Mods { shift: self.modifiers.shift_key(), ctrl, alt };
                    vec![k.with_mods(m).unwrap_or(k)]
                }
            },
            _ => return,
        };
        if keys == [VKey::Ctrl('q')] {
            self.closed = true;
            return;
        }
        // 移動のキーを押し始めたら、自分で繰り返す（ほかのキーを押したら止める。OS と同じ）
        self.held = None;
        if !ev.repeat && self.repeatable(&keys) {
            let now = Instant::now();
            let delay = self.ed.as_ref().map_or(250, |e| e.settings.int("editor.repeat_delay")) as u64;
            self.held = Some((ev.physical_key, keys.clone(), now, now + Duration::from_millis(delay)));
        }
        self.handle_keys(keys, el);
    }

    /// 自分で繰り返す移動のキーか（本文・ビジュアル・一覧で、数や演算子の途中でないとき）
    fn repeatable(&self, keys: &[VKey]) -> bool {
        let Some(ed) = &self.ed else { return false };
        let mode_ok = matches!(ed.state(), State::Normal | State::Visual { .. } | State::List | State::Log) && !ed.in_text_field();
        let key_ok = matches!(keys, [VKey::Char('h' | 'j' | 'k' | 'l' | 'w' | 'b' | 'e' | 'W' | 'B' | 'E') | VKey::Up | VKey::Down | VKey::Left | VKey::Right]);
        mode_ok && key_ok && ed.pending_keys.is_empty() && ed.preedit.is_empty() && self.matcher.pending().is_empty()
    }

    /// 押しっぱなしの繰り返しの 1 回
    fn on_held(&mut self, el: &ActiveEventLoop) {
        let Some((_, keys, since, next)) = self.held.clone() else { return };
        if !self.repeatable(&keys) {
            self.held = None;
            return;
        }
        self.handle_keys(keys, el);
        // 長く押すほど少し速く。遅れても溜めて一度に流さない
        let (fast, normal) = self.ed.as_ref().map_or((16, 25), |e| (e.settings.int("editor.repeat_fast"), e.settings.int("editor.repeat_interval")));
        let every = Duration::from_millis(if since.elapsed() > REPEAT_FAST_AFTER { fast } else { normal } as u64);
        let now = Instant::now();
        if let Some(h) = &mut self.held {
            h.3 = (next + every).max(now);
        }
    }

    /// 1 回のキー入力を処理して描き、かかった時間を記録する
    fn handle_keys(&mut self, keys: Vec<VKey>, el: &ActiveEventLoop) {
        let t0 = Instant::now();
        let keys_label: String = keys.iter().map(|k| k.label()).collect();
        self.stop_scroll();
        for k in keys {
            if self.feed(k, el) {
                return;
            }
        }
        let edit = t0.elapsed();
        // request_redraw を経由せず、その場で描く（ADR 0005）
        let [sync, wait, build, present] = self.draw();
        // IME の有効・無効の切り替えは OS の呼び出しが重い（1〜20ms）ので、描いてから行う。
        // 次のキーはこの後に処理されるので、日本語入力の始まりには間に合う
        let t_ime = Instant::now();
        self.sync_ime();
        let ime = t_ime.elapsed();
        let sample = stats::Sample { edit: stats::us(edit + sync), wait: stats::us(wait), build: stats::us(build), present: stats::us(present) };
        if std::env::var_os("NAGI_DEBUG_STATS").is_some() {
            let (rt, rn) = self.renderer.as_mut().map_or(Default::default(), |r| r.take_raster_stats());
            eprintln!(
                "stat {:?} total={} edit={} wait={} build={} present={} ime_after={} raster={}us/{rn} mode={:?}",
                keys_label,
                sample.total(),
                sample.edit,
                sample.wait,
                sample.build,
                sample.present,
                ime.as_micros(),
                rt.as_micros(),
                self.ed.as_ref().map(|e| e.state())
            );
        }
        self.stats.push(sample);
    }

    /// 1 打をマッピングに通してからエディタに流す。終了したら true
    fn feed(&mut self, k: VKey, el: &ActiveEventLoop) -> bool {
        // マッピングは Normal モードだけ（ideavimrc は nnoremap だけを使っている）。
        // リストモード（ツリー・検索結果）では leader で始まるものだけを通す（j/k などは置き換えない。architecture.md §7.1）
        let mode = self.ed.as_ref().map(|e| e.state());
        let leader = nagi_vim::parse_keys(&self.rc.leader, &self.rc.leader).first().copied();
        // パネルの入力欄にいる間は割り当てを通さない（Space などがそのまま文字になるように）
        let in_field = self.ed.as_ref().is_some_and(|e| e.in_text_field());
        let list_leader = mode == Some(State::List) && !in_field && (!self.matcher.pending().is_empty() || Some(k) == leader);
        // Vim のキー操作を使わないときは .ideavimrc の割り当てを通さない
        let vim = self.ed.as_ref().is_some_and(|e| e.vim_on);
        let out = if vim && (mode == Some(State::Normal) || list_leader) {
            match self.matcher.feed(&self.rc.keymap, k) {
                Feed::Pending => {
                    self.map_deadline = Some(Instant::now() + TIMEOUT_LEN);
                    return false;
                }
                Feed::Emit(keys) => keys,
            }
        } else {
            vec![k]
        };
        self.map_deadline = None;
        self.run_keys(out, el)
    }

    /// マッピング展開後のキーをエディタに流す。終了したら true
    fn run_keys(&mut self, keys: Vec<VKey>, el: &ActiveEventLoop) -> bool {
        // 窓の行数は、分割や窓の移動のたびにエディタが合わせる
        self.sync_screen();
        for k in keys {
            let Some(ed) = &mut self.ed else { return true };
            let eff = ed.key(k);
            if self.apply_effect(eff, el) {
                return true;
            }
        }
        false
    }

    /// エディタが返した効果（終了・計測・設定の読み直し・保存のダイアログ）を行う。終了したら true
    fn apply_effect(&mut self, eff: Effect, _el: &ActiveEventLoop) -> bool {
        match eff {
            Effect::None => {}
            Effect::Quit => {
                self.closed = true;
                return true;
            }
            Effect::Stats { reset } => {
                let msg = if reset {
                    self.stats.clear();
                    "計測の記録を消しました".to_string()
                } else {
                    self.stats.summary(clock::at("window visible"), clock::memory_mb().0)
                };
                if let Some(ed) = &mut self.ed {
                    ed.notify.info(msg);
                }
            }
            Effect::ReloadRc => self.reload_rc(),
            Effect::SaveDialog => self.open_save_dialog(),
        }
        false
    }

    /// timeoutlen が切れた: 待っていたキーを確定させる
    fn on_map_timeout(&mut self, el: &ActiveEventLoop) {
        self.map_deadline = None;
        let keys = self.matcher.timeout(&self.rc.keymap);
        if keys.is_empty() || self.run_keys(keys, el) {
            return;
        }
        self.sync_ime();
        self.draw();
    }

    /// モードに合わせて IME を切り替え、候補ウィンドウの位置をカーソルに合わせる（案1）。
    /// Insert に加えて、最下行の入力欄（: / ? と置換）でも IME を使える（日本語で検索できるように）
    fn sync_ime(&mut self) {
        let rows = self.rows();
        let (Some(w), Some(ed), Some(f)) = (&self.window, &self.ed, &self.fonts) else { return };
        let want = ed.state() == State::Insert || ed.has_prompt();
        if want != self.ime_allowed {
            w.set_ime_allowed(want);
            self.ime_allowed = want;
        }
        if want && matches!(ed.state(), State::CommitMsg | State::Launcher | State::Terminal | State::RunDialog | State::Settings) {
            // コミットメッセージの欄は、描いた縦棒の位置を draw で IME に知らせる
        } else if want {
            let cell = f.cell();
            let (col, row) = if ed.has_prompt() {
                // 最下行の入力欄のカーソル位置
                let (p, row) = match ed.state() {
                    State::Replace if ed.replace_field == 1 => (&ed.repl, rows + 1),
                    State::Command => (&ed.cmdline, rows),
                    State::Finder => match &ed.finder {
                        Some(f) => (&f.query, rows),
                        None => (&ed.query, rows),
                    },
                    State::GrepPrompt => match &ed.grep {
                        Some(g) => (&g.query, rows),
                        None => (&ed.query, rows),
                    },
                    _ => (&ed.query, rows),
                };
                // 入力欄の前の印（`:` `/` は 1 セル、ファインダーの `> ` は 2 セル）
                let lead = match ed.state() {
                    State::Finder => 2,
                    State::GrepPrompt => nagi_core::display_col(grep_panel::GREP_LEAD, grep_panel::GREP_LEAD.len(), false),
                    _ => 1,
                };
                (lead + nagi_core::display_col(&p.text, p.cursor, false), row)
            } else {
                // 画面の幅より右は見えないので、そこまでの幅だけ数える（数 MB の行で全体を数えない）
                let line = ed.buf.line_head(ed.cursor.line, ed.cursor.byte.min(4096) + 4).0;
                let rect = ed.active_rect();
                (rect.x + ed.gutter() + nagi_core::display_col(&line, ed.cursor.byte.min(line.len()), false).saturating_sub(ed.left), rect.y + ed.cursor.line.saturating_sub(ed.top))
            };
            w.set_ime_cursor_area(
                winit::dpi::PhysicalPosition::new((col as f32 * cell.w) as i32, (row as f32 * cell.h) as i32),
                winit::dpi::PhysicalSize::new(cell.w as u32, cell.h as u32),
            );
        }
    }

    fn on_ime(&mut self, ime: winit::event::Ime) {
        if std::env::var_os("NAGI_DEBUG_IME").is_some() {
            eprintln!("[{:.1}] ime {ime:?}", clock::since_process_start_ms());
        }
        let Some(ed) = &mut self.ed else { return };
        match ime {
            winit::event::Ime::Preedit(text, cursor) => ed.ime_preedit(text, cursor),
            winit::event::Ime::Commit(text) => ed.ime_commit(&text),
            _ => return,
        }
        self.sync_ime();
        self.draw();
    }

    /// マウスの位置にあたる、作業中の窓の中の (本文の行, セル列)。行は負や窓の外にもなる
    fn mouse_cell(&self) -> Option<(isize, usize)> {
        let (f, ed) = (self.fonts.as_ref()?, self.ed.as_ref()?);
        let cell = f.cell();
        let rect = ed.active_rect();
        let row = (self.mouse.1 as f32 / cell.h - rect.y as f32 + ed.frac).floor() as isize;
        let col = (self.mouse.0.max(0.0) as f32 / cell.w) as usize;
        Some((row, col.saturating_sub(rect.x)))
    }

    /// マウスを画面の端に寄せている間だけストライプを出す（architecture.md §9 の hover）。
    /// 左端の 1 セル（出ている間は帯の 2 セル）で左のストライプ、最下行で下のストライプ
    fn update_stripe(&mut self) {
        let (Some(f), Some(ed)) = (&self.fonts, &mut self.ed) else { return };
        let cell = f.cell();
        let (col, row) = (self.mouse.0 / cell.w as f64, (self.mouse.1 / cell.h as f64) as usize);
        let edge = if ed.stripe == Some(editor::Side::Left) { STRIPE_W as f64 } else { 1.0 };
        let rows = ed.screen.1;
        let cols = ed.screen.0 as f64;
        // 左右のストライプは出さない（欄の 1 行目にタブがあり、隠した欄は端の » « のボタンで出すため）
        let _ = (edge, cols);
        // 下のストライプも出さない（下の欄の 1 行目にタブがあり、隠した欄は ▴ のボタンで出す）
        let _ = (rows, row >= rows);
        let want: Option<editor::Side> = None;
        // 区切りの上ではカーソルを ↔ / ↕ に（メニューが出ている間はつかめないので、そのまま）
        let icon = match ed.sep_at(row, col as usize).filter(|_| ed.menu.is_none()) {
            Some(s) if s.horizontal_drag() => winit::window::CursorIcon::ColResize,
            Some(_) => winit::window::CursorIcon::RowResize,
            None => winit::window::CursorIcon::Default,
        };
        if icon != self.cursor_icon {
            self.cursor_icon = icon;
            if let Some(w) = &self.window {
                w.set_cursor(icon);
            }
        }
        // 出す・隠すのボタンの上なら明るくして名前を出す。ボタンは左端にもあるので、
        // ストライプがまだ出ていなければボタンを優先し、もう出ていればストライプを優先する
        // SQL コンソールの帯のボタン
        let bar = if row == ed.tabs_row() && ed.console_row() > 0 {
            let (x, w) = ed.windows_x();
            let c = col as usize;
            ed.console_bar(w).into_iter().find(|b| c >= x + b.1 && c < x + b.1 + b.2).map(|b| b.3)
        } else {
            None
        };
        let bar_changed = bar != ed.hover_bar;
        ed.hover_bar = bar;
        // バッファのタブ（載せたタブの名前を明るく、× の上なら × を）
        let buftab = ed.buftab_at(row, col as usize);
        let bar_changed = bar_changed || buftab != ed.hover_buftab;
        ed.hover_buftab = buftab;
        let mut hover = ed.button_at(row, col as usize);
        let mut want = want;
        if want.is_some() && want == ed.stripe {
            hover = None;
        } else if hover.is_some() {
            want = None;
        }
        // タブのアイコンの上なら名前を出す
        let tab = ed.tab_hits.borrow().iter().find(|(r, c0, c1, _)| *r == row && (col as usize) >= *c0 && (col as usize) < *c1).map(|t| (t.0, t.1, t.3));
        let tab_changed = tab != ed.hover_tab;
        ed.hover_tab = tab;
        if want != ed.stripe || hover != ed.hover_button || bar_changed || tab_changed {
            ed.stripe = want;
            ed.hover_button = hover;
            self.draw();
        }
    }

    /// ストライプのラベルのクリック。押したら true
    fn press_stripe(&mut self) -> bool {
        let (Some(f), Some(ed)) = (&self.fonts, &mut self.ed) else { return false };
        let cell = f.cell();
        let (col, row) = ((self.mouse.0 / cell.w as f64) as usize, (self.mouse.1 / cell.h as f64) as usize);
        match ed.stripe {
            Some(editor::Side::Left) if col < STRIPE_W && row < ed.windows_rows() => match tree_panel::stripe_at(row) {
                Some(tree_panel::LeftTab::Project) => ed.toggle_tree(),
                Some(tree_panel::LeftTab::Commit) => ed.toggle_commit(),
                Some(tree_panel::LeftTab::Run) => ed.toggle_left_tab(tree_panel::LeftTab::Run),
                None => return false,
            },
            Some(editor::Side::Right) if col + STRIPE_W >= ed.screen.0 && row < ed.windows_rows() => match right_panel::rstripe_at(row) {
                Some(tab) => ed.toggle_right_tab(tab),
                None => return false,
            },
            // 下のストライプ: タブの名前を押すと、そのタブを出す・隠す
            Some(editor::Side::Bottom) if row + 1 == ed.screen.1 => match ed.btab_at(col) {
                Some(k) => ed.toggle_bottom_tab(k),
                None => return false,
            },
            _ => return false,
        }
        ed.stripe = None;
        self.sync_ime();
        self.draw();
        true
    }

    /// マウスの下の窓へ移る。窓の状態行や区切りの上なら false（本文のクリックとしては扱わない）
    fn focus_under_mouse(&mut self) -> bool {
        let Some(f) = &self.fonts else { return false };
        let cell = f.cell();
        let (row, col) = ((self.mouse.1 as f32 / cell.h) as usize, (self.mouse.0.max(0.0) as f32 / cell.w) as usize);
        let Some(ed) = &mut self.ed else { return false };
        let Some((w, rect)) = ed.window_at(row, col) else { return false };
        if w != ed.cur_win {
            ed.focus_window(w);
        }
        row < rect.y + ed.text_rows(rect)
    }

    fn on_press(&mut self) {
        // 今向いている部品（ダイアログ・ランチャー・コマンドの候補）が受け取るなら、そこへ（component.rs）
        if let (Some(ed), Some(f)) = (&mut self.ed, &self.fonts) {
            if let Some(c) = component::focused(ed) {
                let cell = f.cell();
                let (row, col) = ((self.mouse.1 as f32 / cell.h).floor().max(0.0) as usize, (self.mouse.0.max(0.0) as f32 / cell.w) as usize);
                let now = Instant::now();
                let clicks = match self.last_click {
                    Some((t, k, n)) if now - t <= double_click_time() && k == (row, usize::MAX - 2) => n % 3 + 1,
                    _ => 1,
                };
                if c.click(ed, row, col, clicks) {
                    self.last_click = Some((now, (row, usize::MAX - 2), clicks));
                    self.sync_ime();
                    self.draw();
                    return;
                }
            }
        }
        self.sync_screen();
        // メニュー（確認の窓を含む）が出ている間は、下にある区切り・ストライプ・ボタンをつかまない（メニューの項目が押せなくなる）
        let menu_open = self.ed.as_ref().is_some_and(|e| e.menu.is_some());
        if !menu_open && self.press_stripe() {
            return;
        }
        // 区切りをつかむ（ダブルクリックで元の大きさに）。下のパネルの見出しは、つかみつつ従来どおりパネルへ移る
        if let (Some(f), Some(ed), false) = (&self.fonts, &mut self.ed, menu_open) {
            let cell = f.cell();
            let (row, col) = ((self.mouse.1 as f32 / cell.h) as usize, (self.mouse.0.max(0.0) as f32 / cell.w) as usize);
            if ed.button_at(row, col).is_none() {
                if let Some(sep) = ed.sep_at(row, col) {
                    let now = Instant::now();
                    let key = (row, usize::MAX - 3);
                    let double = matches!(self.last_click, Some((t, c, _)) if now - t <= double_click_time() && c == key);
                    self.last_click = Some((now, key, if double { 2 } else { 1 }));
                    if double {
                        ed.reset_sep(&sep);
                        self.last_click = None;
                        self.draw();
                        return;
                    }
                    let bottom = sep == resize::Sep::Bottom;
                    self.sep_drag = Some(sep);
                    if !bottom {
                        return;
                    }
                }
            }
        }
        // 出す・隠すのボタン
        if let (Some(f), Some(ed), false) = (&self.fonts, &mut self.ed, menu_open) {
            let cell = f.cell();
            let (row, col) = ((self.mouse.1 as f32 / cell.h) as usize, (self.mouse.0.max(0.0) as f32 / cell.w) as usize);
            if let Some(b) = ed.button_at(row, col) {
                ed.press_button(b);
                ed.hover_button = None;
                self.sync_ime();
                self.draw();
                return;
            }
            // 状態行の「▶ 構成の名前」: 実行のメニュー
            if ed.on_run_label(row, col) {
                ed.run_menu(row, col);
                self.sync_ime();
                self.draw();
                return;
            }
        }
        // 欄・小窓の上なら、その部品へ（layout.rs。上の部品から当てる。連続して押した回数は部品ごとに数える）
        if let (Some(f), Some(ed)) = (&self.fonts, &mut self.ed) {
            let cell = f.cell();
            let (row, col) = ((self.mouse.1 as f32 / cell.h) as usize, (self.mouse.0.max(0.0) as f32 / cell.w) as usize);
            let screen = layout::Screen { rows: ed.screen.1, cols: ed.screen.0 };
            let now = Instant::now();
            let last = self.last_click;
            let clicks = |i: usize| match last {
                Some((t, c, n)) if now - t <= double_click_time() && c == (row, usize::MAX - 16 - i) => n % 3 + 1,
                _ => 1,
            };
            ed.click_mods = nagi_vim::Mods { shift: self.modifiers.shift_key(), ctrl: self.modifiers.control_key(), alt: self.modifiers.alt_key() };
            let hit = layout::press(ed, screen, row, col, clicks);
            ed.click_mods = Default::default();
            if let Some((i, done)) = hit {
                self.last_click = Some((now, (row, usize::MAX - 16 - i), clicks(i)));
                self.list_select = done == layout::Click::SelectList;
                self.term_select = done == layout::Click::SelectTerminal;
                self.sync_ime();
                self.draw();
                return;
            }
        }
        let on_text = self.focus_under_mouse();
        let rows = self.sync_screen();
        if !on_text {
            self.sync_ime();
            self.draw();
            return;
        }
        let Some((row, col)) = self.mouse_cell() else { return };
        // 最下行（入力欄・ルーラー）は対象外
        if row < 0 || row as usize >= rows {
            return;
        }
        let row = row as usize;
        let now = Instant::now();
        let clicks = match self.last_click {
            Some((t, cell, n)) if now - t <= double_click_time() && cell == (row, col) => n % 3 + 1,
            _ => 1,
        };
        self.last_click = Some((now, (row, col), clicks));
        self.stop_scroll();
        let Some(ed) = &mut self.ed else { return };
        ed.rows = rows;
        // Ctrl+クリックで定義へ、Ctrl+Alt+クリックで実装へ（IntelliJ と同じ）
        if clicks == 1 && self.modifiers.control_key() {
            if ed.state() == State::Insert {
                ed.key(VKey::Esc);
            }
            ed.click(row, col);
            if self.modifiers.alt_key() {
                ed.goto_implementation();
            } else {
                ed.goto_definition();
            }
            self.last_click = None;
            self.sync_ime();
            self.draw();
            return;
        }
        if clicks == 1 {
            ed.click(row, col);
        } else {
            ed.select_at(row, col, clicks);
        }
        let anchor = ed.abs();
        self.press = Some(Press { anchor, cell: (ed.top + row, col), selected: clicks > 1 });
        self.sync_ime();
        self.draw();
    }

    fn on_drag(&mut self) {
        let rows = self.sync_screen();
        let Some(press) = &self.press else { return };
        if press.selected {
            return;
        }
        let (anchor, start) = (press.anchor, press.cell);
        let Some((row, col)) = self.mouse_cell() else { return };
        let Some(ed) = &mut self.ed else { return };
        ed.rows = rows;
        // 画面の上下へはみ出したら端の行で選び、続きは自動スクロールで送る
        let r = row.clamp(0, rows as isize - 1) as usize;
        let before = (ed.cursor, ed.state());
        let dragging = matches!(ed.state(), State::Visual { .. }) || (ed.top + r, col) != start;
        if dragging {
            ed.drag(anchor, r, col);
        }
        let outside = row < 0 || row as usize >= rows;
        if outside && self.autoscroll_deadline.is_none() {
            self.autoscroll_deadline = Some(Instant::now());
        }
        if before != (ed.cursor, ed.state()) {
            self.sync_ime();
            self.draw();
        }
    }

    /// 区切りのドラッグ
    fn on_sep_drag(&mut self) {
        let (Some(f), Some(ed), Some(sep)) = (&self.fonts, &mut self.ed, &self.sep_drag) else { return };
        let cell = f.cell();
        // 区切りの線はセルの真ん中にあるので、マウスのあるセルに区切りを置く（当たり判定と同じ数え方）
        let row = (self.mouse.1.max(0.0) as f32 / cell.h) as usize;
        let col = (self.mouse.0.max(0.0) as f32 / cell.w) as usize;
        let before = (ed.left_width(), ed.bottom_height(), ed.window_rects());
        ed.drag_sep(sep, row, col);
        if before != (ed.left_width(), ed.bottom_height(), ed.window_rects()) {
            self.sync_ime();
            self.draw();
        }
    }

    /// ドラッグで画面外にいる間、一定間隔で 1 行ずつ送る
    fn on_autoscroll(&mut self) {
        let rows = self.sync_screen();
        let (Some(press), Some((row, col))) = (&self.press, self.mouse_cell()) else {
            self.autoscroll_deadline = None;
            return;
        };
        let anchor = press.anchor;
        let d = if row < 0 {
            -1
        } else if row as usize >= rows {
            1
        } else {
            self.autoscroll_deadline = None;
            return;
        };
        let Some(ed) = &mut self.ed else { return };
        ed.rows = rows;
        ed.scroll(d);
        ed.drag(anchor, if d < 0 { 0 } else { rows - 1 }, col);
        self.autoscroll_deadline = Some(Instant::now() + AUTOSCROLL_EVERY);
        self.draw();
    }

    fn on_wheel(&mut self, delta: winit::event::MouseScrollDelta) {
        let rows = self.sync_screen();
        let (Some(ed), Some(f)) = (&mut self.ed, &self.fonts) else { return };
        ed.rows = rows;
        // ターミナルの上ならシェル・アプリへ（さかのぼる・アプリに送る）
        let (row, col) = ((self.mouse.1 as f32 / f.cell().h) as usize, (self.mouse.0.max(0.0) as f32 / f.cell().w) as usize);
        if ed.bottom_kind == result_grid::BottomKind::Terminal && ed.bottom_content_rect().is_some_and(|r| r.contains(row, col)) {
            let lines = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => (y * WHEEL_LINES as f32).round() as isize,
                winit::event::MouseScrollDelta::PixelDelta(p) => (p.y / f.cell().h as f64).round() as isize,
            };
            ed.terminal_wheel(lines, row, col);
            self.draw();
            return;
        }
        // 行・列の数（下・右が正）。Shift を押していれば縦のホイールも横に
        let (mut dx, mut dy) = match delta {
            winit::event::MouseScrollDelta::LineDelta(x, y) => ((x * WHEEL_LINES as f32).round() as isize, (-y * WHEEL_LINES as f32).round() as isize),
            winit::event::MouseScrollDelta::PixelDelta(p) => {
                // 1 行・1 列に満たない分は貯めておく（ゆっくり動かしても欄が送られるように）
                let (cw, ch) = (f.cell().w as f64, f.cell().h as f64);
                self.wheel_acc.0 += p.x;
                self.wheel_acc.1 -= p.y;
                let (x, y) = ((self.wheel_acc.0 / cw).trunc(), (self.wheel_acc.1 / ch).trunc());
                self.wheel_acc.0 -= x * cw;
                self.wheel_acc.1 -= y * ch;
                (x as isize, y as isize)
            }
        };
        let shift = self.modifiers.shift_key();
        if shift && dx == 0 {
            (dx, dy) = (dy, 0);
        }
        // 欄・小窓・ダイアログの上なら、その部品へ（layout.rs）
        // （行に満たない動きでも、欄の上なら本文へは通さない）
        let screen = layout::Screen { rows: ed.screen.1, cols: ed.screen.0 };
        if layout::wheel(ed, screen, row, col, dy, dx) {
            self.draw();
            return;
        }
        // 分けた窓: マウスの下の窓を動かす（その窓へ移る）
        if let Some((w, _)) = ed.window_at(row, col).filter(|(w, _)| *w != ed.cur_win) {
            self.scroll_anim = None;
            ed.focus_window(w);
            let rows = self.sync_screen();
            if let Some(ed) = &mut self.ed {
                ed.rows = rows;
            }
        }
        let Some(ed) = &mut self.ed else { return };
        // 本文の横（折り返していないとき）
        if dx != 0 {
            ed.hscroll(dx);
            self.sync_ime();
            self.draw();
            return;
        }
        let Some(f) = &self.fonts else { return };
        let now = Instant::now();
        match delta {
            // マウスのホイール: 目盛りごとに 3 行を、短い時間で滑らかに動かす
            winit::event::MouseScrollDelta::LineDelta(_, y) => {
                let base = self.scroll_anim.as_ref().map_or(ed.scroll_pos(), |a| a.to);
                let max = ed.buf.line_count().saturating_sub(1) as f64;
                let to = (base - y as f64 * WHEEL_LINES).round().clamp(0.0, max);
                let dur = Duration::from_millis(ed.settings.int("editor.scroll_ms") as u64);
                self.scroll_anim = Some(ScrollAnim { from: ed.scroll_pos(), to, start: now, dur });
                self.snap_deadline = None;
                self.step_scroll(now);
            }
            // タッチパッド: 指の動きにそのまま付いていき、止まったら行の境目へ寄せる
            winit::event::MouseScrollDelta::PixelDelta(p) => {
                self.scroll_anim = None;
                ed.scroll_to(ed.scroll_pos() - p.y / f.cell().h as f64);
                self.snap_deadline = Some(now + SNAP_AFTER);
                self.sync_ime();
                self.draw();
            }
        }
    }

    /// スムーズスクロールを 1 コマ進めて描く
    fn step_scroll(&mut self, now: Instant) {
        let Some(a) = &self.scroll_anim else { return };
        let (pos, done) = a.at(now);
        if done {
            self.scroll_anim = None;
        }
        if std::env::var_os("NAGI_DEBUG_MOUSE").is_some() {
            eprintln!("[{:.1}] scroll {pos:.3}", clock::since_process_start_ms());
        }
        if let Some(ed) = &mut self.ed {
            ed.scroll_to(pos);
        }
        self.sync_ime();
        self.draw();
    }

    /// タッチパッドが止まった: 近い方の行の境目へ滑らせる
    fn snap_scroll(&mut self) {
        self.snap_deadline = None;
        let Some(ed) = &self.ed else { return };
        let pos = ed.scroll_pos();
        if pos.fract() != 0.0 {
            let dur = self.ed.as_ref().map_or(Duration::from_millis(110), |e| Duration::from_millis(e.settings.int("editor.scroll_ms") as u64));
            self.scroll_anim = Some(ScrollAnim { from: pos, to: pos.round(), start: Instant::now(), dur });
        }
    }

    /// キー操作が来たら、スクロールの途中でも行の境目に揃える
    fn stop_scroll(&mut self) {
        self.scroll_anim = None;
        self.snap_deadline = None;
        if let Some(ed) = &mut self.ed {
            ed.frac = 0.0;
        }
    }
}

/// ダブルクリックとみなす間隔（Windows の設定に従う）
fn double_click_time() -> Duration {
    // SAFETY: 引数なしで値を返すだけの API
    Duration::from_millis(unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() } as u64)
}

impl Frame {
    /// 裏の処理（構文解析か、検索の全件数え）が終わった。描き直すと受け取る
    fn user_event(&mut self) {
        // 受け取る前に下ろす（この後に届いた知らせは、次のイベントで受け取る）
        self.woken.store(false, std::sync::atomic::Ordering::SeqCst);
        if let Some(ed) = &mut self.ed {
            ed.poll_background();
        }
        // ダイアログで選んだ名前で保存する（ダイアログが上書きを確かめている）
        if let Some(r) = self.save_dialog.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.save_dialog = None;
            if let (Some(p), Some(ed)) = (r, &mut self.ed) {
                ed.write_as(&p.to_string_lossy(), true, true);
            }
        }
        if self.ed.as_mut().is_some_and(|ed| ed.poll_watch().1) {
            self.reload_rc();
        }
        if self.ed.as_mut().is_some_and(|ed| ed.poll_syntax()) {
            if let Some(s) = self.ed.as_ref().and_then(|ed| ed.syntax.as_ref()) {
                clock::mark_once(&format!("syntax: {} parsed", s.lang.name()));
            }
        }
        self.draw();
    }

    fn new_events(&mut self, el: &ActiveEventLoop, cause: winit::event::StartCause) {
        if let winit::event::StartCause::ResumeTimeReached { .. } = cause {
            let now = Instant::now();
            if self.map_deadline.is_some_and(|d| now >= d) {
                self.on_map_timeout(el);
            }
            if self.snap_deadline.is_some_and(|d| now >= d) {
                self.snap_scroll();
            }
            if self.autoscroll_deadline.is_some_and(|d| now >= d) {
                self.on_autoscroll();
            }
            if self.held.as_ref().is_some_and(|h| now >= h.3) {
                self.on_held(el);
            }
            // 入力補完を自動で出す（設定）
            if self.ed.as_mut().is_some_and(|e| e.fire_auto_complete(now)) {
                self.draw();
            }
            if self.replay_deadline.is_some_and(|d| now >= d) {
                self.replay_deadline = None;
                if let Some(step) = self.replay.pop_front() {
                    // 待つ手順（まだ出ていない）は少し後にもう一度。`<Wait>` は自分で期限を決める
                    let again = self.run_step(step, el);
                    if self.replay_deadline.is_none() && !self.replay.is_empty() {
                        self.replay_deadline = Some(Instant::now() + if again { Duration::from_millis(100) } else { replay_every() });
                    }
                }
            }
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        // スクロール中は毎フレーム描く（draw はスワップチェーンの待機で垂直同期に合わせて止まる）
        if self.scroll_anim.is_some() {
            self.step_scroll(Instant::now());
        }
        // 手が空いたら、画面の上下のグリフを先に載せておく（表示位置が変わったときだけ）
        if self.scroll_anim.is_none() {
            if let (Some(ed), Some(f), Some(r)) = (&self.ed, &mut self.fonts, &mut self.renderer) {
                if self.prefetched_top != Some(ed.top) {
                    self.prefetched_top = Some(ed.top);
                    view::prefetch_around(ed, f, r);
                }
            }
        }
        let auto = self.ed.as_ref().and_then(|e| e.auto_complete_at);
        let deadline = [self.map_deadline, self.snap_deadline, self.autoscroll_deadline, self.replay_deadline, self.held.as_ref().map(|h| h.3), auto].into_iter().flatten().min();
        let _ = el;
        self.flow = if self.scroll_anim.is_some() {
            ControlFlow::Poll
        } else {
            match deadline {
                Some(d) => ControlFlow::WaitUntil(d),
                None => ControlFlow::Wait,
            }
        };
    }

    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_none() {
            self.startup(el);
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.closed = true,
            WindowEvent::Resized(size) => {
                if let Some(r) = &mut self.renderer {
                    let _ = r.resize(size.width, size.height);
                }
                self.draw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.set_scale(scale_factor);
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            // 離したのが届かないことがあるので、ほかの窓へ移ったら繰り返しを止める
            WindowEvent::Focused(false) => self.held = None,
            // 前に出たら、ほかの窓が保存した設定を読み直す（窓ごとに設定を持っているので）。
            // git の状態も取り直す（.git を見張れない場所（WSL のフォルダなど）で、外から git を動かしたとき）
            WindowEvent::Focused(true) => {
                if let Some(e) = &mut self.ed {
                    e.refresh_status();
                }
                if self.ed.as_mut().is_some_and(|e| e.reload_settings()) {
                    self.draw();
                }
            }
            // エクスプローラーから落としたもの: フォルダはプロジェクトとして、ファイルはそのまま開く
            WindowEvent::DroppedFile(path) => {
                if let Some(ed) = &mut self.ed {
                    ed.open_dropped(path);
                }
                self.sync_ime();
                self.draw();
                if let Some(w) = &self.window {
                    w.focus_window();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => self.on_key(event, el),
            WindowEvent::Ime(ime) => self.on_ime(ime),
            WindowEvent::CursorMoved { position, .. } => {
                if std::env::var_os("NAGI_DEBUG_MOUSE").is_some() {
                    eprintln!("[{:.1}] move {:.0},{:.0}", clock::since_process_start_ms(), position.x, position.y);
                }
                self.mouse = (position.x, position.y);
                if self.list_select {
                    if let (Some(f), Some(ed)) = (&self.fonts, &mut self.ed) {
                        let row = (position.y.max(0.0) as f32 / f.cell().h) as usize;
                        if let Some(r) = ed.bottom_content_rect() {
                            let col = (position.x.max(0.0) as f32 / f.cell().w) as usize;
                            ed.panel_drag(row.clamp(r.y, r.y + r.h.saturating_sub(1)) - r.y, col.saturating_sub(r.x));
                        }
                    }
                    self.draw();
                } else if self.term_select {
                    if let (Some(f), Some(ed)) = (&self.fonts, &mut self.ed) {
                        let cell = f.cell();
                        ed.terminal_drag((position.y as f32 / cell.h) as usize, (position.x.max(0.0) as f32 / cell.w) as usize);
                    }
                    self.draw();
                } else if self.sep_drag.is_some() {
                    self.on_sep_drag();
                } else if self.press.is_some() {
                    self.on_drag();
                } else {
                    self.update_stripe();
                }
            }
            WindowEvent::MouseInput { state, button: winit::event::MouseButton::Left, .. } => match state {
                _ if std::env::var_os("NAGI_DEBUG_MOUSE").is_some() && {
                    eprintln!("[{:.1}] {state:?} at {:.0},{:.0}", clock::since_process_start_ms(), self.mouse.0, self.mouse.1);
                    false
                } => {}
                ElementState::Pressed => self.on_left(true, el),
                ElementState::Released => self.on_left(false, el),
            },
            WindowEvent::MouseInput { state: ElementState::Pressed, button: winit::event::MouseButton::Right, .. } => self.on_right_press(),
            WindowEvent::MouseWheel { delta, .. } => self.on_wheel(delta),
            WindowEvent::RedrawRequested => {
                self.draw();
            }
            _ => {}
        }
    }

    /// 左のボタンを押した・離した（`--replay` の `<Click>` も、ここを通る）
    fn on_left(&mut self, pressed: bool, el: &ActiveEventLoop) {
        match pressed {
            true => {
                self.on_press();
                // 押して決めた操作（ファインダーのダブルクリックでコマンドを流したなど）の、App がすること
                if let Some(eff) = self.ed.as_mut().and_then(|e| e.click_effect.take()) {
                    self.apply_effect(eff, el);
                }
            }
            false => {
                self.list_select = false;
                if std::mem::take(&mut self.term_select) {
                    if let Some(ed) = &mut self.ed {
                        ed.terminal_release();
                    }
                }
                self.press = None;
                // 区切りをドラッグし終えたら、欄の大きさを覚える（設定）
                if self.sep_drag.take().is_some() {
                    if let Some(ed) = &mut self.ed {
                        ed.remember_panel_sizes();
                    }
                }
                self.autoscroll_deadline = None;
            }
        }
    }

    /// 右クリック: 部品のメニュー。ターミナルの上なら、選んでいればコピー、いなければ貼り付け（`--replay` の `<RClick>` も、ここを通る）
    fn on_right_press(&mut self) {
        if let (Some(f), Some(ed)) = (&self.fonts, &mut self.ed) {
            let cell = f.cell();
            let (row, col) = ((self.mouse.1 as f32 / cell.h) as usize, (self.mouse.0.max(0.0) as f32 / cell.w) as usize);
            // 部品の右クリックのメニュー（layout.rs の View の context・menu.rs）
            if ed.on_run_label(row, col) {
                ed.run_menu(row, col);
                self.sync_ime();
                self.draw();
            } else if layout::context(ed, layout::Screen { rows: ed.screen.1, cols: ed.screen.0 }, row, col) {
                self.sync_ime();
                self.draw();
            } else if ed.window_at(row, col).is_some_and(|(_, r)| row < r.y + ed.text_rows(r)) && !ed.has_prompt() {
                // 本文: 切り取り・コピー・貼り付け・定義へ など
                if let Some((w, _)) = ed.window_at(row, col).filter(|(w, _)| *w != ed.cur_win) {
                    ed.focus_window(w);
                }
                // 窓の中の位置（mouse_cell と同じ基準）
                let rect = ed.active_rect();
                let lrow = (self.mouse.1 as f32 / cell.h - rect.y as f32 + ed.frac).floor().max(0.0) as usize;
                ed.text_context(lrow, col.saturating_sub(rect.x), (row, col));
                self.sync_ime();
                self.draw();
            } else if ed.bottom_kind == result_grid::BottomKind::Terminal && ed.terminal_rect().is_some_and(|r| r.contains(row, col)) {
                ed.terminal_right_click();
                self.sync_ime();
                self.draw();
            } else if matches!(ed.bottom_kind, result_grid::BottomKind::Grep | result_grid::BottomKind::Output) && ed.bottom_content_rect().is_some_and(|r| r.contains(row, col)) {
                // 検索・ログ: 選んでいる行（無ければ今の行）をコピー
                ed.panel_copy();
                self.draw();
            }
        }
    }
}

/// ログ（%LOCALAPPDATA%\nagi\nagi.log。知らせの warn・error、落ちたときの理由）を開く。上限は設定の「ログの上限」。
/// 標準エラーがつながっていなければ（エクスプローラーやショートカットから起動したとき）、標準エラーも同じファイルへ向ける。
/// ターミナルや他のプログラムからつないで起動したときは、そのまま（--startup-report などはそこへ出る）
fn start_log() {
    use windows::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, SetStdHandle};
    let Some(path) = nagi_core::log::default_path() else { return };
    let max = settings::Settings::load(None).int("other.log_max_kb") as u64 * 1024;
    nagi_core::log::init(path, max);
    nagi_core::log::info(&format!("起動 nagi {} pid {}", env!("CARGO_PKG_VERSION"), std::process::id()));
    let connected = unsafe { GetStdHandle(STD_ERROR_HANDLE) }.is_ok_and(|h| !h.is_invalid() && !h.0.is_null());
    if connected {
        return;
    }
    if let Some(file) = nagi_core::log::file_clone() {
        use std::os::windows::io::IntoRawHandle;
        let h = windows::Win32::Foundation::HANDLE(file.into_raw_handle());
        let _ = unsafe { SetStdHandle(STD_ERROR_HANDLE, h) };
    }
}

/// 押したキーをキーマップの組み合わせに直す。文字を打つのを妨げないよう、Ctrl・Alt を伴うものと F キーなどだけ
fn chord_of(key: &Key, m: winit::keyboard::ModifiersState) -> Option<keymap::Chord> {
    let (ctrl, alt, shift) = (m.control_key(), m.alt_key(), m.shift_key());
    let (name, special) = match key {
        Key::Character(c) => {
            let ch = c.chars().next().filter(|c| c.is_ascii_graphic())?;
            (ch.to_ascii_uppercase().to_string(), false)
        }
        Key::Named(n) => {
            let dbg = format!("{n:?}");
            let name = match n {
                NamedKey::Space => "Space".to_string(),
                NamedKey::Enter => "Enter".into(),
                NamedKey::Tab => "Tab".into(),
                NamedKey::Escape => "Esc".into(),
                NamedKey::Backspace => "Backspace".into(),
                NamedKey::Delete => "Delete".into(),
                NamedKey::Insert => "Insert".into(),
                NamedKey::Home => "Home".into(),
                NamedKey::End => "End".into(),
                NamedKey::PageUp => "PageUp".into(),
                NamedKey::PageDown => "PageDown".into(),
                NamedKey::ArrowUp => "Up".into(),
                NamedKey::ArrowDown => "Down".into(),
                NamedKey::ArrowLeft => "Left".into(),
                NamedKey::ArrowRight => "Right".into(),
                // F1〜F24
                _ if dbg.len() <= 3 && dbg.starts_with('F') && dbg[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)) => dbg.clone(),
                _ => return None,
            };
            let f = name.starts_with('F') && name.len() <= 3 && name != "F";
            (name, f)
        }
        _ => return None,
    };
    (ctrl || alt || special).then_some(keymap::Chord { ctrl, alt, shift, key: name })
}

fn main() {
    start_log();
    // タスクバーのボタンとジャンプリストを nagi のものとしてまとめる（窓を作る前に）
    jumplist::set_app_id();
    clock::mark("main");
    let opts = Opts::parse();
    // 2 回目の起動は、動いている nagi に窓を足してもらって終わる（instance.rs）。試験用の引数があるときと NAGI_NEW_PROCESS では自分で開く
    let single = opts.keys.is_none() && opts.replay.is_none() && !opts.startup_report && !opts.exit_after_first_frame && std::env::var_os("NAGI_NEW_PROCESS").is_none();
    if single && instance::hand_off(opts.file.as_deref()) {
        return;
    }
    let pending = spawn_pending(opts.file.clone(), None);
    let event_loop = EventLoop::<UserEvent>::with_user_event().build().unwrap();
    let proxy = event_loop.create_proxy();
    clock::mark("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    if single {
        let p = proxy.clone();
        instance::serve(move |file, cwd| {
            let _ = p.send_event(UserEvent::Open(file, cwd));
        });
    }
    let mut app = App { frames: vec![new_frame(opts, pending, proxy.clone())], proxy };
    event_loop.run_app(&mut app).unwrap();
}

/// 裏の処理からイベントループへの知らせ
#[derive(Debug)]
enum UserEvent {
    /// 構文解析・検索などが終わった（全部の窓が受け取りに行く）
    Wake,
    /// 2 回目の起動から: 窓を足してこのファイル（None なら起動時と同じ）を開く
    /// 2 回目の起動（開くファイル, 起動したフォルダ）
    Open(Option<PathBuf>, Option<PathBuf>),
}

/// 窓を束ねる。窓の数だけ Frame があり、GPU の装置は 1 枚目のものを使い回す
struct App {
    frames: Vec<Frame>,
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
}

impl App {
    /// 閉じた窓を外す。1 枚も無くなったら終わる
    fn sweep(&mut self, el: &ActiveEventLoop) {
        self.frames.retain(|f| !f.closed);
        if self.frames.is_empty() {
            el.exit();
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn user_event(&mut self, el: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Wake => self.frames.iter_mut().for_each(|f| f.user_event()),
            UserEvent::Open(file, cwd) => {
                let gpu = self.frames.iter().find_map(|f| f.renderer.as_ref().map(|r| r.gpu()));
                let opts = Opts { file, cwd, keys: None, replay: None, startup_report: false, exit_after_first_frame: false, headless: false };
                let pending = spawn_pending(opts.file.clone(), gpu);
                let mut f = new_frame(opts, pending, self.proxy.clone());
                f.startup(el);
                self.frames.push(f);
            }
        }
        self.sweep(el);
    }

    fn new_events(&mut self, el: &ActiveEventLoop, cause: winit::event::StartCause) {
        for f in &mut self.frames {
            f.new_events(el, cause);
        }
        self.sweep(el);
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let mut flow = ControlFlow::Wait;
        for f in &mut self.frames {
            f.about_to_wait(el);
            flow = match (flow, f.flow) {
                (ControlFlow::Poll, _) | (_, ControlFlow::Poll) => ControlFlow::Poll,
                (ControlFlow::WaitUntil(a), ControlFlow::WaitUntil(b)) => ControlFlow::WaitUntil(a.min(b)),
                (ControlFlow::WaitUntil(a), _) | (_, ControlFlow::WaitUntil(a)) => ControlFlow::WaitUntil(a),
                _ => ControlFlow::Wait,
            };
        }
        el.set_control_flow(flow);
    }

    fn resumed(&mut self, el: &ActiveEventLoop) {
        for f in &mut self.frames {
            f.resumed(el);
        }
        self.sweep(el);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if let Some(f) = self.frames.iter_mut().find(|f| f.window.as_ref().is_some_and(|w| w.id() == id)) {
            f.window_event(el, id, event);
        }
        self.sweep(el);
    }
}

/// 窓 1 枚を作る準備を裏で始める（フォント・開くファイル・.ideavimrc。GPU は渡されなければ作る）
fn spawn_pending(open: Option<PathBuf>, gpu: Option<Gpu>) -> Pending {
    let spawn = |name: &str| std::thread::Builder::new().name(name.into());
    let (gtx, gpu_rx) = mpsc::channel();
    match gpu {
        Some(g) => gtx.send(Ok(g)).unwrap(),
        None => {
            spawn("gpu")
                .spawn(move || {
                    let g = Gpu::new();
                    clock::mark("gpu: device + pipeline");
                    gtx.send(g).unwrap();
                })
                .unwrap();
        }
    }
    let (ftx, fonts) = mpsc::channel();
    spawn("font")
        .spawn(move || {
            // 起動時に読むのは主フォントと日本語フォントだけ（S2）。残りは必要になったとき MapCharacters で解決する。
            // 設定のフォント（英数字・日本語）。読めなければ既定のもの
            let chain = load_font_chain(&font_names(&settings::Settings::load(None)));
            clock::mark(format!("font: {} faces", chain.len()));
            ftx.send(chain).unwrap();
        })
        .unwrap();
    let (xtx, file) = mpsc::channel();
    let (rest_tx, rest) = mpsc::channel();
    let path = open;
    spawn("file")
        .spawn(move || {
            // 大きなファイルは、先頭部分を読んだところで先に渡して表示し、全体は後から渡す
            let mut sent_head = false;
            let buf = match &path {
                // フォルダはプロジェクトとして開く（表示の後に open_project）
                Some(p) if p.is_dir() => Buffer::from_str(""),
                Some(p) => Buffer::open_with_head(p, |head| {
                    clock::mark(format!("file: head {} lines", head.line_count()));
                    sent_head = xtx.send((head, true)).is_ok();
                })
                .unwrap_or_else(|e| {
                    let mut b = Buffer::from_str("");
                    b.path = Some(p.clone());
                    nagi_core::log::warn(&format!("{}: {e}", p.display()));
                    b
                }),
                None => Buffer::from_str(""),
            };
            clock::mark(format!("file: {} lines", buf.line_count()));
            if sent_head {
                let _ = rest_tx.send(buf);
            } else {
                let _ = xtx.send((buf, false));
            }
        })
        .unwrap();

    let (rtx, rc) = mpsc::channel();
    spawn("rc")
        .spawn(move || {
            let rc = nagi_vim::load_rc_from(settings::Settings::load(None).text("vim.rc_path").as_deref());
            clock::mark(format!("rc: {} mappings from {:?}", rc.keymap.normal.len(), rc.sources.first()));
            rtx.send(rc).unwrap();
        })
        .unwrap();
    Pending { gpu: gpu_rx, fonts, file, rest, rc }
}

fn new_frame(opts: Opts, pending: Pending, proxy: winit::event_loop::EventLoopProxy<UserEvent>) -> Frame {
    Frame {
        opts,
        pending: Some(pending),
        proxy: Some(proxy),
        closed: false,
        flow: ControlFlow::Wait,
        window: None,
        renderer: None,
        fonts: None,
        ed: None,
        woken: Default::default(),
        modifiers: ModifiersState::empty(),
        scale: 1.0,
        font_px: FONT_PX,
        font_names: Vec::new(),
        mouse: (0.0, 0.0),
        ime_allowed: false,
        rc: Rc::default(),
        matcher: Matcher::default(),
        map_deadline: None,
        scroll_anim: None,
        wheel_acc: (0.0, 0.0),
        snap_deadline: None,
        press: None,
        last_click: None,
        sep_drag: None,
        term_select: false,
        list_select: false,
        cursor_icon: winit::window::CursorIcon::Default,
        autoscroll_deadline: None,
        held: None,
        stats: stats::Stats::default(),
        replay: Default::default(),
        replay_deadline: None,
        prefetched_top: None,
        title_path: (None, None),
        save_dialog: None,
    }
}
