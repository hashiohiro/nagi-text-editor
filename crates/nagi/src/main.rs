//! nagi（凪）: 静かで、余計なものがない、速いエディタ。
//!
//! 使い方: nagi [FILE] [--startup-report] [--exit-after-first-frame] [--keys KEYS]
//!
//! `--keys` は起動直後にキー列（Vim の表記。`/foo<CR>` など）を流し込む。画面の確認やデモ用。
//!
//! 起動は並列（architecture.md §6.4）: GPU・フォント・ファイル読込を別スレッドで進め、
//! メインスレッドはその間にイベントループと非表示のウィンドウを作る。最初のフレームを描いてから表示する。

mod clipboard;
mod clock;
#[cfg(test)]
mod difftest;
mod editor;
mod normal;
mod view;

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
    startup_report: bool,
    exit_after_first_frame: bool,
}

impl Opts {
    fn parse() -> Self {
        let mut o = Opts { file: None, keys: None, startup_report: false, exit_after_first_frame: false };
        let mut args = std::env::args().skip(1);
        while let Some(a) = args.next() {
            match a.as_str() {
                "--keys" => o.keys = args.next(),
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
    file: mpsc::Receiver<(Buffer, Option<nagi_syntax::Syntax>)>,
    rc: mpsc::Receiver<Rc>,
}

/// マッピングの続きを待つ時間（Vim の timeoutlen の既定値）
const TIMEOUT_LEN: Duration = Duration::from_millis(1000);

struct App {
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
}

impl App {
    fn startup(&mut self, el: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title(self.title_for(None))
            .with_visible(false)
            // タイトルバーも暗くして、ターミナルと見分けがつかないようにする
            .with_theme(Some(winit::window::Theme::Dark))
            .with_inner_size(winit::dpi::LogicalSize::new(1100.0, 720.0));
        let window = Arc::new(el.create_window(attrs).unwrap());
        clock::mark("window created (hidden)");
        self.scale = window.scale_factor();

        let p = self.pending.take().unwrap();
        let gpu = p.gpu.recv().unwrap().expect("D3D11 の初期化に失敗");
        let chain = p.fonts.recv().unwrap();
        let (buf, syntax) = p.file.recv().unwrap();
        self.rc = p.rc.recv().unwrap();
        clock::mark("init joined");

        let fonts = Fonts::new(chain, (FONT_PX * self.scale).round() as f32);
        clock::mark("fonts measured");
        let RawWindowHandle::Win32(h) = window.window_handle().unwrap().as_raw() else { unreachable!() };
        let size = window.inner_size();
        let renderer = Renderer::new(gpu, h.hwnd.get(), size.width, size.height).expect("スワップチェーンの作成に失敗");
        clock::mark("swap chain created");

        window.set_title(&self.title_for(buf.path.as_deref()));
        self.ed = Some(Editor::new(buf, syntax));
        self.apply_rc();
        self.fonts = Some(fonts);
        self.renderer = Some(renderer);
        self.window = Some(window.clone());
        if let (Some(r), Some(f), Some(ed)) = (&mut self.renderer, &mut self.fonts, &self.ed) {
            view::prefetch(ed, f, r);
            clock::mark("glyphs prefetched");
        }
        self.draw();
        clock::mark("first present");
        window.set_visible(true);
        clock::mark("window visible");
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
    }

    fn title_for(&self, path: Option<&std::path::Path>) -> String {
        match path {
            Some(p) => format!("nagi — {}", p.display()),
            None => "nagi".into(),
        }
    }

    /// 本文に使える行数（最下行の分を除く。置換中は 2 行）
    fn rows(&self) -> usize {
        let (Some(r), Some(f)) = (&self.renderer, &self.fonts) else { return 1 };
        let status = self.ed.as_ref().map_or(1, |e| e.status_rows());
        ((r.size().1 as f32 / f.cell().h) as usize).saturating_sub(status).max(1)
    }

    fn draw(&mut self) {
        let (Some(r), Some(f), Some(ed)) = (&mut self.renderer, &mut self.fonts, &mut self.ed) else { return };
        r.wait_frame();
        let status = ed.status_rows();
        ed.rows = ((r.size().1 as f32 / f.cell().h) as usize).saturating_sub(status).max(1);
        ed.scroll_into_view();
        ed.showcmd = self.matcher.pending().iter().map(|k| k.label()).collect();
        if let Some(s) = &mut ed.syntax {
            s.sync(&mut ed.buf);
        }
        let insts = view::build(ed, f, r);
        clock::mark_once("first frame built");
        if let Err(e) = r.draw(&insts, view::BG) {
            eprintln!("draw: {e}");
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
                NamedKey::Space => vec![VKey::Char(' ')],
                NamedKey::Escape => vec![VKey::Esc],
                NamedKey::Enter => vec![VKey::Enter],
                NamedKey::Tab => vec![VKey::Tab],
                NamedKey::Backspace => vec![VKey::Backspace],
                NamedKey::Delete => vec![VKey::Delete],
                NamedKey::ArrowUp => vec![VKey::Up],
                NamedKey::ArrowDown => vec![VKey::Down],
                NamedKey::ArrowLeft => vec![VKey::Left],
                NamedKey::ArrowRight => vec![VKey::Right],
                NamedKey::Home => vec![VKey::Home],
                NamedKey::End => vec![VKey::End],
                NamedKey::PageUp => vec![VKey::PageUp],
                NamedKey::PageDown => vec![VKey::PageDown],
                _ => return,
            },
            _ => return,
        };
        if keys == [VKey::Ctrl('q')] {
            el.exit();
            return;
        }
        for k in keys {
            if self.feed(k, el) {
                return;
            }
        }
        self.sync_ime();
        // request_redraw を経由せず、その場で描く（ADR 0005）
        self.draw();
    }

    /// 1 打をマッピングに通してからエディタに流す。終了したら true
    fn feed(&mut self, k: VKey, el: &ActiveEventLoop) -> bool {
        // マッピングは Normal モードだけ（ideavimrc は nnoremap だけを使っている）
        let normal = self.ed.as_ref().is_some_and(|e| e.mode == Mode::Normal);
        let out = if normal {
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
        let rows = self.rows();
        for k in keys {
            let Some(ed) = &mut self.ed else { return true };
            ed.rows = rows;
            match ed.key(k) {
                Effect::None => {}
                Effect::Quit => {
                    el.exit();
                    return true;
                }
                Effect::ReloadRc => {
                    let t = Instant::now();
                    self.rc = nagi_vim::load_rc();
                    self.apply_rc();
                    let n = self.rc.keymap.normal.len();
                    let us = t.elapsed().as_micros();
                    if let Some(ed) = &mut self.ed {
                        ed.message = format!("設定を読み直しました（マッピング {n} 件、{us}µs）");
                    }
                }
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
        if want {
            let cell = f.cell();
            let (col, row) = if ed.has_prompt() {
                // 最下行の入力欄のカーソル位置
                let (p, row) = match ed.mode {
                    Mode::Replace if ed.replace_field == 1 => (&ed.repl, rows + 1),
                    Mode::Command => (&ed.cmdline, rows),
                    _ => (&ed.query, rows),
                };
                (1 + nagi_core::display_col(&p.text, p.cursor, false), row)
            } else {
                let line = ed.buf.line(ed.cursor.line);
                (editor::GUTTER + nagi_core::display_col(&line, ed.cursor.byte.min(line.len()), false), ed.cursor.line.saturating_sub(ed.top))
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

    fn on_click(&mut self) {
        let rows = self.rows();
        let (Some(ed), Some(f)) = (&mut self.ed, &self.fonts) else { return };
        let cell = f.cell();
        let (row, col) = ((self.mouse.1 as f32 / cell.h) as usize, (self.mouse.0 as f32 / cell.w) as usize);
        if row >= rows {
            return;
        }
        ed.rows = rows;
        ed.click(row, col);
        self.sync_ime();
        self.draw();
    }

    fn on_wheel(&mut self, delta: winit::event::MouseScrollDelta) {
        let rows = self.rows();
        let (Some(ed), Some(f)) = (&mut self.ed, &self.fonts) else { return };
        let lines = match delta {
            winit::event::MouseScrollDelta::LineDelta(_, y) => -(y * 3.0) as isize,
            winit::event::MouseScrollDelta::PixelDelta(p) => -(p.y as f32 / f.cell().h) as isize,
        };
        if lines == 0 {
            return;
        }
        ed.rows = rows;
        ed.scroll(lines);
        self.sync_ime();
        self.draw();
    }
}

impl ApplicationHandler for App {
    fn new_events(&mut self, el: &ActiveEventLoop, cause: winit::event::StartCause) {
        if let winit::event::StartCause::ResumeTimeReached { .. } = cause {
            if self.map_deadline.is_some_and(|d| Instant::now() >= d) {
                self.on_map_timeout(el);
            }
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        el.set_control_flow(match self.map_deadline {
            Some(d) => ControlFlow::WaitUntil(d),
            None => ControlFlow::Wait,
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
                self.scale = scale_factor;
                // フォントを作り直すのは M1 の後続で（アトラスの作り直しが要る）
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::KeyboardInput { event, .. } => self.on_key(event, el),
            WindowEvent::Ime(ime) => self.on_ime(ime),
            WindowEvent::CursorMoved { position, .. } => self.mouse = (position.x, position.y),
            WindowEvent::MouseInput { state: ElementState::Pressed, button: winit::event::MouseButton::Left, .. } => self.on_click(),
            WindowEvent::MouseWheel { delta, .. } => self.on_wheel(delta),
            WindowEvent::RedrawRequested => self.draw(),
            _ => {}
        }
    }
}

fn main() {
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
    let path = opts.file.clone();
    spawn("file")
        .spawn(move || {
            let buf = match &path {
                Some(p) => Buffer::open(p).unwrap_or_else(|e| {
                    let mut b = Buffer::from_str("");
                    b.path = Some(p.clone());
                    eprintln!("{}: {e}", p.display());
                    b
                }),
                None => Buffer::from_str(""),
            };
            clock::mark(format!("file: {} lines", buf.line_count()));
            // 初回の構文解析も読み込みスレッドで済ませる
            let mut buf = buf;
            let syntax = buf.path.as_deref().and_then(nagi_syntax::Lang::detect).and_then(nagi_syntax::Syntax::new).map(|mut s| {
                s.parse(&mut buf);
                clock::mark(format!("syntax: {} parsed", s.lang.name()));
                s
            });
            xtx.send((buf, syntax)).unwrap();
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
    clock::mark("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App {
        opts,
        pending: Some(Pending { gpu, fonts, file, rc }),
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
    };
    event_loop.run_app(&mut app).unwrap();
}
