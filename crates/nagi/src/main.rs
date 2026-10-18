//! nagi（凪）: 静かで、余計なものがない、速いエディタ。
//!
//! 使い方: nagi [FILE] [--startup-report] [--exit-after-first-frame] [--keys KEYS] [--replay KEYS] [--replay-file PATH]
//!
//! `--keys` は起動直後にキー列（Vim の表記。`/foo<CR>` など）を流し込み、最後に 1 回だけ描く。画面の確認やデモ用。
//! `--replay` はキー列を 1 打ずつ、キー入力と同じ道筋（処理 → 描画 → 計測の記録）で流す。`:NagiStats` の確認用。
//! `--replay-file` は同じキー列を UTF-8 のファイルから読む（シェルの引用符や文字コードを通さない。末尾の改行は無視）。
//!
//! 起動は並列（architecture.md §6.4）: GPU・フォント・ファイル読込を別スレッドで進め、
//! メインスレッドはその間にイベントループと非表示のウィンドウを作る。最初のフレームを描いてから表示する。
//!
//! 画面のアプリとして作る（起動してもコンソールの窓を開かない）。書き出す先（標準エラー）がつながっていなければ、
//! 記録（落ちたときの理由・調べ物用の出力）は `%LOCALAPPDATA%\nagi\nagi.log` に書く。

// 試験の実行ファイルは結果を書き出すので、コンソールのアプリのまま
#![cfg_attr(not(test), windows_subsystem = "windows")]

mod angular;
mod autoread;
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
mod vim_field;
mod panes;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;

use editor::{Clipboard, Editor, Effect, Mode};
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
const FONT_PX: f64 = 14.0;

struct Opts {
    file: Option<PathBuf>,
    keys: Option<String>,
    replay: Option<String>,
    startup_report: bool,
    exit_after_first_frame: bool,
}

impl Opts {
    fn parse() -> Self {
        let mut o = Opts { file: None, keys: None, replay: None, startup_report: false, exit_after_first_frame: false };
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
const SCROLL_TIME: Duration = Duration::from_millis(110);
/// タッチパッドで止まってから行境界へ寄せるまでの時間
const SNAP_AFTER: Duration = Duration::from_millis(120);
/// `--replay` で 1 打ずつ流す間隔（速く打つ人の打鍵間隔くらい。詰めすぎるとフレームの待ちが毎回乗る）
const REPLAY_EVERY: Duration = Duration::from_millis(30);
/// `--replay` の間隔。`NAGI_REPLAY_MS` で変えられる（打鍵の間隔で速さが変わるかを調べる用）
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
}

impl ScrollAnim {
    /// 今の位置と、動ききったか
    fn at(&self, now: Instant) -> (f64, bool) {
        let t = (now - self.start).as_secs_f64() / SCROLL_TIME.as_secs_f64();
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

struct App {
    /// 開いている「名前を付けて保存」のダイアログ
    save_dialog: Option<std::sync::mpsc::Receiver<Option<std::path::PathBuf>>>,
    /// 題名に出しているパス（バッファを切り替えたら題名を変える）
    /// 題名に出しているもの（ファイル, プロジェクトの根）
    title_path: (Option<std::path::PathBuf>, Option<std::path::PathBuf>),
    opts: Opts,
    pending: Option<Pending>,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    fonts: Option<Fonts>,
    ed: Option<Editor>,
    modifiers: ModifiersState,
    scale: f64,
    mouse: (f64, f64),
    /// IME を許可しているか（案1: Insert だけ許可。ADR 0006）
    ime_allowed: bool,
    rc: Rc,
    matcher: Matcher,
    /// マッピングの続きを待つ期限
    map_deadline: Option<Instant>,
    scroll_anim: Option<ScrollAnim>,
    snap_deadline: Option<Instant>,
    press: Option<Press>,
    /// 直前のクリック（時刻, 画面上のセル, 連続回数）。ダブル・トリプルクリックの判定用
    last_click: Option<(Instant, (usize, usize), u32)>,
    autoscroll_deadline: Option<Instant>,
    /// ドラッグしている区切り
    sep_drag: Option<resize::Sep>,
    /// 今のマウスのカーソルの形（区切りの上では ↔ / ↕）
    cursor_icon: winit::window::CursorIcon,
    stats: stats::Stats,
    /// `--replay` の残りのキー（1 回のイベント処理で 1 打ずつ流す）
    replay: std::collections::VecDeque<VKey>,
    replay_deadline: Option<Instant>,
    /// 上下の先読みを済ませた表示位置
    prefetched_top: Option<usize>,
    /// 裏の処理（構文解析）が終わったことを知らせる
    proxy: Option<winit::event_loop::EventLoopProxy<()>>,
}

impl App {
    fn startup(&mut self, el: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title(title_for(None, None))
            .with_visible(false)
            // タイトルバーも暗くして、ターミナルと見分けがつかないようにする
            .with_theme(Some(winit::window::Theme::Dark))
            .with_inner_size(winit::dpi::LogicalSize::new(1100.0, 720.0));
        let window = Arc::new(el.create_window(attrs).unwrap());
        clock::mark("window created (hidden)");
        self.scale = window.scale_factor();
        let hwnd = {
            let RawWindowHandle::Win32(h) = window.window_handle().unwrap().as_raw() else { unreachable!() };
            windows::Win32::Foundation::HWND(h.hwnd.get() as *mut _)
        };

        // GPU の準備（最も遅い）を待つ間に、フォントとファイルを受け取って最初の画面のグリフをラスタライズしておく
        let p = self.pending.take().unwrap();
        let chain = p.fonts.recv().unwrap();
        let (buf, partial) = p.file.recv().unwrap();
        self.rc = p.rc.recv().unwrap();
        let mut fonts = Fonts::new(chain, (FONT_PX * self.scale).round() as f32);
        // 題名は最初の描画で（プロジェクトの根が決まってから）付ける
        window.set_title(&title_for(buf.path.as_deref(), None));
        self.title_path = (None, None);
        self.ed = Some(Editor::new(buf, None));
        self.apply_rc();
        // 構文解析は裏で行い、終わったらイベントループを起こして色を付ける（大きなファイルでも表示を待たせない）
        if let (Some(ed), Some(proxy)) = (&mut self.ed, self.proxy.take()) {
            ed.waker = Some(Arc::new(move || {
                let _ = proxy.send_event(());
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
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(hwnd, windows::Win32::UI::WindowsAndMessaging::SW_SHOWNOACTIVATE);
        }
        clock::mark("window visible");
        window.set_visible(true);
        window.focus_window();
        clock::mark("window activated");
        if let (Some(r), Some(f)) = (&mut self.renderer, &mut self.fonts) {
            view::prefetch_ascii(f, r);
            clock::mark("ascii prefetched");
        }
        // 外部での変更と設定ファイルの見張りは、表示の後に始める（起動を待たせない）
        let rc_files = self.rc.sources.clone();
        if let Some(ed) = &mut self.ed {
            ed.start_watch(&rc_files);
            clock::mark("watch started");
            // 引数がフォルダなら、プロジェクトとして開く（IntelliJ・Eclipse の設定を読み、前に開いていたファイルを復元）
            if let Some(dir) = self.opts.file.clone().filter(|p| p.is_dir()) {
                ed.open_project(dir);
                clock::mark("project opened");
            } else if self.opts.file.is_none() {
                // 引数なしで起動したら、ランチャー（最近のプロジェクトから選ぶ）
                ed.open_launcher();
            }
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
            el.exit();
        }
        if let Some(keys) = self.opts.keys.take() {
            for k in nagi_vim::parse_keys(&keys, &self.rc.leader.clone()) {
                self.feed(k, el);
            }
            self.sync_ime();
            self.draw();
        }
        if let Some(keys) = self.opts.replay.take() {
            self.replay = nagi_vim::parse_keys(&keys, &self.rc.leader.clone()).into();
            self.replay_deadline = Some(Instant::now() + replay_every());
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
            f.set_px((FONT_PX * scale).round() as f32);
            r.clear_glyphs();
            view::prefetch_ascii(f, r);
            self.prefetched_top = None;
        }
        self.draw();
    }

    fn draw(&mut self) -> [Duration; 4] {
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
        if let Err(e) = r.draw(&insts, view::BG) {
            eprintln!("draw: {e}");
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
        let dir = path.as_ref().and_then(|p| p.parent().map(|d| d.to_path_buf())).or_else(|| std::env::current_dir().ok());
        let name = path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
        self.save_dialog = Some(save_dialog::open(owner, dir, name, ed.waker.clone()));
    }

    /// 設定（vimrc）を読み直して反映する（`:source` と、設定ファイルが書き換わったとき）
    fn reload_rc(&mut self) {
        let t = Instant::now();
        self.rc = nagi_vim::load_rc();
        self.apply_rc();
        let n = self.rc.keymap.normal.len();
        let us = t.elapsed().as_micros();
        if let Some(ed) = &mut self.ed {
            ed.message = format!("設定を読み直しました（マッピング {n} 件、{us}µs）");
        }
    }

    /// 設定（vimrc）の内容をエディタに反映する
    fn apply_rc(&mut self) {
        let Some(ed) = &mut self.ed else { return };
        let clip = self.rc.has("clipboard=unnamedplus") || self.rc.has("clipboard=unnamed");
        ed.clipboard = clip.then_some(Clipboard { get: clipboard::get, set: clipboard::set });
    }

    fn on_key(&mut self, ev: KeyEvent, el: &ActiveEventLoop) {
        if ev.state != ElementState::Pressed {
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
        let keys: Vec<VKey> = match &ev.logical_key {
            Key::Character(c) if alt && !ctrl => c.chars().next().map(|ch| VKey::Alt(ch.to_ascii_lowercase())).into_iter().collect(),
            Key::Character(c) if ctrl => c.chars().next().map(|ch| VKey::Ctrl(ch.to_ascii_lowercase())).into_iter().collect(),
            Key::Character(_) => ev.text.as_deref().unwrap_or("").chars().map(VKey::Char).collect(),
            Key::Named(n) => match n {
                // Ctrl+Space は候補を出す（IntelliJ と同じ）
                NamedKey::Space if ctrl => vec![VKey::Ctrl(' ')],
                NamedKey::Space => vec![VKey::Char(' ')],
                NamedKey::Escape => vec![VKey::Esc],
                // Ctrl-Enter はコミット（Ctrl-m として渡す）
                NamedKey::Enter if ctrl => vec![VKey::Ctrl('m')],
                NamedKey::Enter => vec![VKey::Enter],
                NamedKey::Tab => vec![VKey::Tab],
                NamedKey::Backspace => vec![VKey::Backspace],
                NamedKey::Delete => vec![VKey::Delete],
                // Shift+Insert は貼り付け（入力欄では Ctrl+V と同じ）
                NamedKey::Insert if self.modifiers.shift_key() => vec![VKey::Paste],
                NamedKey::ArrowUp => vec![VKey::Up],
                NamedKey::ArrowDown => vec![VKey::Down],
                NamedKey::ArrowLeft => vec![VKey::Left],
                NamedKey::ArrowRight => vec![VKey::Right],
                NamedKey::Home => vec![VKey::Home],
                NamedKey::End => vec![VKey::End],
                NamedKey::PageUp => vec![VKey::PageUp],
                NamedKey::PageDown => vec![VKey::PageDown],
                // F12: 定義へ（IntelliJ・VS Code と同じ）
                // Alt+F12: ターミナル（IntelliJ と同じ。F24 として渡す）
                NamedKey::F12 if alt => vec![VKey::F(24)],
                NamedKey::F12 => vec![VKey::F(12)],
                _ => return,
            },
            _ => return,
        };
        if keys == [VKey::Ctrl('q')] {
            el.exit();
            return;
        }
        self.handle_keys(keys, el);
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
                self.ed.as_ref().map(|e| e.mode)
            );
        }
        self.stats.push(sample);
    }

    /// 1 打をマッピングに通してからエディタに流す。終了したら true
    fn feed(&mut self, k: VKey, el: &ActiveEventLoop) -> bool {
        // マッピングは Normal モードだけ（ideavimrc は nnoremap だけを使っている）。
        // リストモード（ツリー・検索結果）では leader で始まるものだけを通す（j/k などは置き換えない。architecture.md §7.1）
        let mode = self.ed.as_ref().map(|e| e.mode);
        let leader = nagi_vim::parse_keys(&self.rc.leader, &self.rc.leader).first().copied();
        // パネルの入力欄にいる間は割り当てを通さない（Space などがそのまま文字になるように）
        let in_field = self.ed.as_ref().is_some_and(|e| e.in_text_field());
        let list_leader = mode == Some(Mode::List) && !in_field && (!self.matcher.pending().is_empty() || Some(k) == leader);
        let out = if mode == Some(Mode::Normal) || list_leader {
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
            match ed.key(k) {
                Effect::None => {}
                Effect::Quit => {
                    el.exit();
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
                        ed.message = msg;
                    }
                }
                Effect::ReloadRc => self.reload_rc(),
                Effect::SaveDialog => self.open_save_dialog(),
            }
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
        let want = ed.mode == Mode::Insert || ed.has_prompt();
        if want != self.ime_allowed {
            w.set_ime_allowed(want);
            self.ime_allowed = want;
        }
        if want && matches!(ed.mode, Mode::CommitMsg | Mode::Launcher | Mode::Terminal) {
            // コミットメッセージの欄は、描いた縦棒の位置を draw で IME に知らせる
        } else if want {
            let cell = f.cell();
            let (col, row) = if ed.has_prompt() {
                // 最下行の入力欄のカーソル位置
                let (p, row) = match ed.mode {
                    Mode::Replace if ed.replace_field == 1 => (&ed.repl, rows + 1),
                    Mode::Command => (&ed.cmdline, rows),
                    Mode::Finder => match &ed.finder {
                        Some(f) => (&f.query, rows),
                        None => (&ed.query, rows),
                    },
                    Mode::GrepPrompt => match &ed.grep {
                        Some(g) => (&g.query, rows),
                        None => (&ed.query, rows),
                    },
                    _ => (&ed.query, rows),
                };
                // 入力欄の前の印（`:` `/` は 1 セル、ファインダーの `> ` は 2 セル）
                let lead = match ed.mode {
                    Mode::Finder => 2,
                    Mode::GrepPrompt => nagi_core::display_col(grep_panel::GREP_LEAD, grep_panel::GREP_LEAD.len(), false),
                    _ => 1,
                };
                (lead + nagi_core::display_col(&p.text, p.cursor, false), row)
            } else {
                // 画面の幅より右は見えないので、そこまでの幅だけ数える（数 MB の行で全体を数えない）
                let line = ed.buf.line_head(ed.cursor.line, ed.cursor.byte.min(4096) + 4).0;
                let rect = ed.active_rect();
                (rect.x + ed.gutter() + nagi_core::display_col(&line, ed.cursor.byte.min(line.len()), false), rect.y + ed.cursor.line.saturating_sub(ed.top))
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
        let redge = if ed.stripe == Some(editor::Side::Right) { STRIPE_W as f64 } else { 1.0 };
        // 左のストライプは出さない（左の欄の 1 行目にタブがあるため）
        let _ = edge;
        let want = if col >= cols - redge && row < ed.windows_rows() {
            Some(editor::Side::Right)
        } else if row >= rows && ed.status_rows() == 1 {
            Some(editor::Side::Bottom)
        } else if ed.stripe == Some(editor::Side::Bottom) && row + 1 == rows && col < 60.0 {
            Some(editor::Side::Bottom)
        } else {
            None
        };
        // 区切りの上ではカーソルを ↔ / ↕ に
        let icon = match ed.sep_at(row, col as usize) {
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
        let bar = if row == 0 && ed.windows_top() > 0 {
            let (x, w) = ed.windows_x();
            let c = col as usize;
            ed.console_bar(w).into_iter().find(|b| c >= x + b.1 && c < x + b.1 + b.2).map(|b| b.3)
        } else {
            None
        };
        let bar_changed = bar != ed.hover_bar;
        ed.hover_bar = bar;
        let mut hover = ed.button_at(row, col as usize);
        let mut want = want;
        if want.is_some() && want == ed.stripe {
            hover = None;
        } else if hover.is_some() {
            want = None;
        }
        if want != ed.stripe || hover != ed.hover_button || bar_changed {
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
        // ランチャー: 一覧の行を押すと選び、2 回で開く
        if let (Some(ed), Some(f)) = (&mut self.ed, &self.fonts) {
            if ed.mode == Mode::Launcher {
                let row = (self.mouse.1 as f32 / f.cell().h).floor().max(0.0) as usize;
                let now = Instant::now();
                let clicks = match self.last_click {
                    Some((t, c, n)) if now - t <= double_click_time() && c == (row, usize::MAX - 2) => n % 3 + 1,
                    _ => 1,
                };
                self.last_click = Some((now, (row, usize::MAX - 2), clicks));
                let hit = ed.launcher.as_ref().and_then(|l| l.row_map.borrow().iter().find(|(r, _)| *r == row).map(|x| x.1));
                if let Some(i) = hit {
                    ed.launcher_click(i, clicks);
                }
                self.sync_ime();
                self.draw();
                return;
            }
        }
        self.sync_screen();
        if self.press_stripe() {
            return;
        }
        // 区切りをつかむ（ダブルクリックで元の大きさに）。下のパネルの見出しは、つかみつつ従来どおりパネルへ移る
        if let (Some(f), Some(ed)) = (&self.fonts, &mut self.ed) {
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
        if let (Some(f), Some(ed)) = (&self.fonts, &mut self.ed) {
            let cell = f.cell();
            let (row, col) = ((self.mouse.1 as f32 / cell.h) as usize, (self.mouse.0.max(0.0) as f32 / cell.w) as usize);
            if let Some(b) = ed.button_at(row, col) {
                ed.press_button(b);
                ed.hover_button = None;
                self.sync_ime();
                self.draw();
                return;
            }
        }
        // パネルの上なら、パネルの行を選ぶ（ダブルクリックで開く）
        if let (Some(f), Some(ed)) = (&self.fonts, &mut self.ed) {
            let cell = f.cell();
            let row = (self.mouse.1 as f32 / cell.h) as usize;
            if let Some(r) = ed.panel_rect().filter(|r| row >= r.y && row < r.y + r.h) {
                let now = Instant::now();
                let clicks = match self.last_click {
                    Some((t, c, n)) if now - t <= double_click_time() && c == (row, usize::MAX) => n % 3 + 1,
                    _ => 1,
                };
                self.last_click = Some((now, (row, usize::MAX), clicks));
                // 1 行目はタブ、2 行目から中身
                let col = (self.mouse.0.max(0.0) as f32 / cell.w) as usize;
                ed.bottom_click(row - r.y, col.saturating_sub(r.x), clicks);
                self.sync_ime();
                self.draw();
                return;
            }
        }
        // SQL コンソールの上の帯
        if let (Some(f), Some(ed)) = (&self.fonts, &mut self.ed) {
            let cell = f.cell();
            let (row, col) = ((self.mouse.1 as f32 / cell.h) as usize, (self.mouse.0.max(0.0) as f32 / cell.w) as usize);
            if row == 0 && ed.windows_top() > 0 {
                let (x, w) = ed.windows_x();
                if col >= x && col < x + w {
                    if let Some(item) = ed.console_bar(w).into_iter().find(|b| col >= x + b.1 && col < x + b.1 + b.2).map(|b| b.3) {
                        ed.console_bar_press(item);
                    }
                    self.sync_ime();
                    self.draw();
                    return;
                }
            }
        }
        // 右の欄（データベース・Maven）
        if let (Some(f), Some(ed)) = (&self.fonts, &mut self.ed) {
            let cell = f.cell();
            let (row, col) = ((self.mouse.1 as f32 / cell.h) as usize, (self.mouse.0.max(0.0) as f32 / cell.w) as usize);
            if let Some(r) = ed.right_rect().filter(|r| r.contains(row, col)) {
                let now = Instant::now();
                let key = (row, usize::MAX - 4);
                let clicks = match self.last_click {
                    Some((t, c, n)) if now - t <= double_click_time() && c == key => n % 3 + 1,
                    _ => 1,
                };
                self.last_click = Some((now, key, clicks));
                ed.right_click(row - r.y, col - r.x, clicks);
                self.sync_ime();
                self.draw();
                return;
            }
        }
        // 左の欄の 1 行目ならタブを切り替える。コミットのパネルなら、その行を押す
        if let (Some(f), Some(ed)) = (&self.fonts, &mut self.ed) {
            let cell = f.cell();
            let (row, col) = ((self.mouse.1 as f32 / cell.h) as usize, (self.mouse.0.max(0.0) as f32 / cell.w) as usize);
            if let Some(r) = ed.left_rect().filter(|r| r.contains(row, col)) {
                let handled = if row == r.y {
                    if let Some(tab) = tree_panel::tab_at(col - r.x) {
                        ed.select_left_tab(tab);
                    }
                    true
                } else if ed.commit_rect().is_some() {
                    let now = Instant::now();
                    let clicks = match self.last_click {
                        Some((t, c, n)) if now - t <= double_click_time() && c == (row, usize::MAX - 2) => n % 3 + 1,
                        _ => 1,
                    };
                    self.last_click = Some((now, (row, usize::MAX - 2), clicks));
                    ed.commit_click(row - r.y, col - r.x, clicks);
                    true
                } else {
                    false
                };
                if handled {
                    self.sync_ime();
                    self.draw();
                    return;
                }
            }
        }
        // ツリーの上なら、ツリーの行を選ぶ（フォルダは開閉、ファイルはダブルクリックで開く）
        if let (Some(f), Some(ed)) = (&self.fonts, &mut self.ed) {
            let cell = f.cell();
            let (row, col) = ((self.mouse.1 as f32 / cell.h) as usize, (self.mouse.0.max(0.0) as f32 / cell.w) as usize);
            if ed.tree_rect().is_some_and(|r| r.contains(row, col)) {
                let now = Instant::now();
                let clicks = match self.last_click {
                    Some((t, c, n)) if now - t <= double_click_time() && c == (row, usize::MAX - 1) => n % 3 + 1,
                    _ => 1,
                };
                self.last_click = Some((now, (row, usize::MAX - 1), clicks));
                ed.tree_click(row, clicks);
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
            if ed.mode == Mode::Insert {
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
        let before = (ed.cursor, ed.mode);
        let dragging = matches!(ed.mode, Mode::Visual { .. }) || (ed.top + r, col) != start;
        if dragging {
            ed.drag(anchor, r, col);
        }
        let outside = row < 0 || row as usize >= rows;
        if outside && self.autoscroll_deadline.is_none() {
            self.autoscroll_deadline = Some(Instant::now());
        }
        if before != (ed.cursor, ed.mode) {
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
        // ターミナルの上ならさかのぼる
        let (row, col) = ((self.mouse.1 as f32 / f.cell().h) as usize, (self.mouse.0.max(0.0) as f32 / f.cell().w) as usize);
        if ed.bottom_kind == result_grid::BottomKind::Terminal && ed.bottom_content_rect().is_some_and(|r| r.contains(row, col)) {
            let lines = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => (y * WHEEL_LINES as f32).round() as isize,
                winit::event::MouseScrollDelta::PixelDelta(p) => (p.y / f.cell().h as f64).round() as isize,
            };
            ed.terminal_scroll(lines);
            self.draw();
            return;
        }
        let now = Instant::now();
        match delta {
            // マウスのホイール: 目盛りごとに 3 行を、短い時間で滑らかに動かす
            winit::event::MouseScrollDelta::LineDelta(_, y) => {
                let base = self.scroll_anim.as_ref().map_or(ed.scroll_pos(), |a| a.to);
                let max = ed.buf.line_count().saturating_sub(1) as f64;
                let to = (base - y as f64 * WHEEL_LINES).round().clamp(0.0, max);
                self.scroll_anim = Some(ScrollAnim { from: ed.scroll_pos(), to, start: now });
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
            self.scroll_anim = Some(ScrollAnim { from: pos, to: pos.round(), start: Instant::now() });
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

impl ApplicationHandler for App {
    /// 裏の処理（構文解析か、検索の全件数え）が終わった。描き直すと受け取る
    fn user_event(&mut self, _el: &ActiveEventLoop, _event: ()) {
        if let Some(ed) = &mut self.ed {
            if ed.poll_load() {
                clock::mark_once("file: full buffer swapped in");
            }
            ed.poll_search();
            ed.poll_finder();
            ed.poll_launcher();
            ed.poll_grep();
            ed.poll_tree();
            ed.poll_index();
            ed.poll_make();
            ed.poll_cmd_files();
            ed.poll_git();
            ed.poll_commit();
            ed.poll_blame();
            ed.poll_log();
            ed.poll_branches();
            ed.poll_db();
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
            if self.replay_deadline.is_some_and(|d| now >= d) {
                self.replay_deadline = None;
                if let Some(k) = self.replay.pop_front() {
                    self.handle_keys(vec![k], el);
                    if !self.replay.is_empty() {
                        self.replay_deadline = Some(Instant::now() + replay_every());
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
        let deadline = [self.map_deadline, self.snap_deadline, self.autoscroll_deadline, self.replay_deadline].into_iter().flatten().min();
        el.set_control_flow(if self.scroll_anim.is_some() {
            ControlFlow::Poll
        } else {
            match deadline {
                Some(d) => ControlFlow::WaitUntil(d),
                None => ControlFlow::Wait,
            }
        });
    }

    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_none() {
            self.startup(el);
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
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
            WindowEvent::KeyboardInput { event, .. } => self.on_key(event, el),
            WindowEvent::Ime(ime) => self.on_ime(ime),
            WindowEvent::CursorMoved { position, .. } => {
                if std::env::var_os("NAGI_DEBUG_MOUSE").is_some() {
                    eprintln!("[{:.1}] move {:.0},{:.0}", clock::since_process_start_ms(), position.x, position.y);
                }
                self.mouse = (position.x, position.y);
                if self.sep_drag.is_some() {
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
                ElementState::Pressed => self.on_press(),
                ElementState::Released => {
                    self.press = None;
                    self.sep_drag = None;
                    self.autoscroll_deadline = None;
                }
            },
            WindowEvent::MouseWheel { delta, .. } => self.on_wheel(delta),
            WindowEvent::RedrawRequested => {
                self.draw();
            }
            _ => {}
        }
    }
}

/// 標準エラーがつながっていなければ（エクスプローラーやショートカットから起動したとき）、ログのファイルへ向ける。
/// ターミナルや他のプログラムからつないで起動したときは、そのまま（--startup-report などはそこへ出る）
fn route_stderr_to_log() {
    use windows::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, SetStdHandle};
    let connected = unsafe { GetStdHandle(STD_ERROR_HANDLE) }.is_ok_and(|h| !h.is_invalid() && !h.0.is_null());
    if connected {
        return;
    }
    let Some(dir) = std::env::var_os("LOCALAPPDATA").map(|d| std::path::PathBuf::from(d).join("nagi")) else { return };
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("nagi.log");
    // 大きくなったら 1 つ前の分として残して作り直す
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 1 << 20) {
        let _ = std::fs::rename(&path, dir.join("nagi.old.log"));
    }
    let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) else { return };
    use std::io::Write;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let _ = writeln!(file, "--- 起動 {now}（UNIX 秒）pid {}", std::process::id());
    use std::os::windows::io::IntoRawHandle;
    let h = windows::Win32::Foundation::HANDLE(file.into_raw_handle());
    let _ = unsafe { SetStdHandle(STD_ERROR_HANDLE, h) };
}

fn main() {
    route_stderr_to_log();
    clock::mark("main");
    let opts = Opts::parse();
    let spawn = |name: &str| std::thread::Builder::new().name(name.into());
    let (gtx, gpu) = mpsc::channel();
    spawn("gpu")
        .spawn(move || {
            let g = Gpu::new();
            clock::mark("gpu: device + pipeline");
            gtx.send(g).unwrap();
        })
        .unwrap();
    let (ftx, fonts) = mpsc::channel();
    spawn("font")
        .spawn(move || {
            // 起動時に読むのは主フォントと日本語フォントだけ（S2）。残りは必要になったとき MapCharacters で解決する。
            let chain: Vec<FontData> = nagi_text::DEFAULT_FONTS[..2].iter().filter_map(|n| nagi_text::load_by_name(n)).collect();
            clock::mark(format!("font: {} faces", chain.len()));
            ftx.send(chain).unwrap();
        })
        .unwrap();
    let (xtx, file) = mpsc::channel();
    let (rest_tx, rest) = mpsc::channel();
    let path = opts.file.clone();
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
                    eprintln!("{}: {e}", p.display());
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
            let rc = nagi_vim::load_rc();
            clock::mark(format!("rc: {} mappings from {:?}", rc.keymap.normal.len(), rc.sources.first()));
            rtx.send(rc).unwrap();
        })
        .unwrap();
    let event_loop = EventLoop::new().unwrap();
    let proxy = event_loop.create_proxy();
    clock::mark("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App {
        opts,
        pending: Some(Pending { gpu, fonts, file, rest, rc }),
        proxy: Some(proxy),
        window: None,
        renderer: None,
        fonts: None,
        ed: None,
        modifiers: ModifiersState::empty(),
        scale: 1.0,
        mouse: (0.0, 0.0),
        ime_allowed: false,
        rc: Rc::default(),
        matcher: Matcher::default(),
        map_deadline: None,
        scroll_anim: None,
        snap_deadline: None,
        press: None,
        last_click: None,
        sep_drag: None,
        cursor_icon: winit::window::CursorIcon::Default,
        autoscroll_deadline: None,
        stats: stats::Stats::default(),
        replay: Default::default(),
        replay_deadline: None,
        prefetched_top: None,
        title_path: (None, None),
        save_dialog: None,
    };
    event_loop.run_app(&mut app).unwrap();
}
