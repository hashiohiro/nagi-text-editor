//! デバッグ（IntelliJ のデバッガと同じキー）。Java・Kotlin は JDWP（nagi-workers の debug.rs）、ブラウザの
//! TypeScript・JavaScript は Chrome DevTools Protocol（cdp.rs）、Python は debugpy の DAP（dap.rs）。画面とキーはどれも同じ。
//!
//! - ブレークポイント: 行番号の欄を押す・F9・`:Breakpoint`（ファイルごとの行。編集で行がずれれば一緒に動く）
//! - 始める: Shift+F9・`:Debug`（選んでいる実行の構成を、デバッグの口を開けて動かし、つなぐ。Java の構成・
//!   IntelliJ の JavaScript Debug（URL）の構成・Python）、`:DebugTest`（カーソルの位置のテスト。Maven・pytest）、
//!   `:DebugBrowser URL`（Chrome・Edge をデバッグの口を開けて起こす）、`:DebugAttach 5005`（動いている JVM）・
//!   `:DebugAttachBrowser 9222`（--remote-debugging-port で起こしたブラウザ）・`:DebugAttachPython 5678`（debugpy）
//! - 止まったら: その行へ移って行を目立たせ、下の欄の「デバッグ」にスタックと変数を出す
//! - F5 続ける、F10 次へ、F11 中へ、Shift+F11 外へ、Shift+F5・Ctrl+F2 止める（Visual Studio と同じ。キーマップで変えられる）
//!
//! 変数は木で、オブジェクト・配列は開くと中身を裏で読む。式の評価（Alt+F8）はコンパイラが要るので作らない。

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use nagi_vim::Key;
use nagi_workers::debug::{Bp, Cmd, Ev, Frame, Var};
use nagi_workers::jdwp::Depth;

use crate::editor::{Editor, Side, State};
use crate::result_grid::BottomKind;

/// デバッグする言語（ブレークポイントの置き方とつなぐ相手）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Java,
    Browser,
    Python,
}

/// ファイルの言語（デバッグできないものは None）
pub fn lang_of(path: &Path) -> Option<Lang> {
    match path.extension()?.to_string_lossy().to_ascii_lowercase().as_str() {
        "java" | "kt" => Some(Lang::Java),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "mts" | "cts" | "vue" | "svelte" => Some(Lang::Browser),
        "py" => Some(Lang::Python),
        _ => None,
    }
}

/// 変数の木の 1 行
#[derive(Clone, Debug)]
pub struct VarRow {
    pub depth: usize,
    pub var: Var,
    pub open: bool,
}

pub struct DebugUi {
    pub lang: Lang,
    tx: Option<Sender<Cmd>>,
    task: Option<nagi_core::task::Task<Ev>>,
    /// 何をデバッグしているか（構成の名前・つないだ先）
    pub name: String,
    /// 1 行目に出す今の様子
    pub status: String,
    pub stopped: bool,
    pub ended: bool,
    pub frames: Vec<Frame>,
    pub frame: usize,
    pub vars: Vec<VarRow>,
    /// 選んでいる行（スタックか変数か）
    pub sel_frame: usize,
    pub sel_var: usize,
    pub var_scroll: usize,
    pub on_vars: bool,
    /// 止まっている行（ファイル, 行は 0 始まり）
    pub at: Option<(PathBuf, usize)>,
    /// ソース（com/x/Foo.java）→ ファイル
    src: HashMap<String, Option<PathBuf>>,
    /// 動かしているターミナルの窓（止めるときに閉じる）
    pub terminal: Option<String>,
    /// 1 行目のボタン（列の始め, 終わり, キー）
    pub buttons: std::cell::RefCell<Vec<(usize, usize, Key)>>,
}

/// JVM に JDWP の口を開ける引数
fn agent(port: u16) -> String {
    format!("-agentlib:jdwp=transport=dt_socket,server=y,suspend=y,address=127.0.0.1:{port}")
}

/// 空いている TCP のポート
fn free_port() -> Option<u16> {
    std::net::TcpListener::bind("127.0.0.1:0").ok()?.local_addr().ok().map(|a| a.port())
}

/// 構成に JDWP の口を開ける引数を足す（足せない種類なら None）
pub fn with_agent(cfg: &nagi_workers::runconfig::RunConfig, agent: &str) -> Option<nagi_workers::runconfig::RunConfig> {
    use nagi_workers::runconfig::Kind;
    let mut c = cfg.clone();
    match &mut c.kind {
        // Spring Boot は別の JVM を起こすので jvmArguments に
        Kind::SpringBoot { vm, .. } => *vm = format!("{} {agent}", vm.trim()).trim().to_string(),
        // exec:java は Maven の JVM の中で動くので MAVEN_OPTS に
        Kind::Application { env, .. } => env.push(("MAVEN_OPTS".into(), agent.to_string())),
        Kind::Maven { goals, options, .. } => {
            let prop = if goals.iter().any(|g| g.contains("spring-boot:run")) { "spring-boot.run.jvmArguments" } else { "maven.surefire.debug" };
            *options = format!("{} \"-D{prop}={agent}\"", options.trim()).trim().to_string();
        }
        Kind::Shell { command, .. } if command.trim_start().starts_with("java ") => {
            *command = command.replacen("java ", &format!("java {agent} "), 1);
        }
        _ => return None,
    }
    c.name = format!("{}（デバッグ）", cfg.name);
    Some(c)
}

/// Java のファイルのクラス名（package の行とファイル名から。com.x.Foo）
pub fn class_of(path: &Path, text: &str) -> Option<String> {
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    if !matches!(ext.as_str(), "java" | "kt") {
        return None;
    }
    // Kotlin のファイルの関数は FooKt に入る
    let stem = if ext == "kt" && !text.contains(&format!("class {stem}")) { format!("{stem}Kt") } else { stem };
    let pkg = text.lines().map(str::trim).find_map(|l| l.strip_prefix("package ")).map(|p| p.trim_end_matches(';').trim().to_string());
    Some(match pkg {
        Some(p) if !p.is_empty() => format!("{p}.{stem}"),
        _ => stem,
    })
}

impl Editor {
    /// 今の行のブレークポイントを付ける・外す（F9・:Breakpoint・行番号の欄を押す）
    pub(crate) fn toggle_breakpoint(&mut self, line: Option<usize>) {
        let Some(path) = self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok()) else {
            self.notify.info("ファイル名がありません");
            return;
        };
        let line = line.unwrap_or(self.cursor.line);
        let set = self.breakpoints.entry(path.clone()).or_default();
        let on = if set.remove(&line) {
            false
        } else {
            set.insert(line);
            true
        };
        if set.is_empty() {
            self.breakpoints.remove(&path);
        }
        if lang_of(&path).is_none() {
            self.notify.info("ブレークポイントで止まるのは Java・Kotlin・TypeScript・JavaScript・Python のファイルだけです");
        } else {
            self.notify.info(format!("{} 行目のブレークポイントを{}", line + 1, if on { "付けました" } else { "外しました" }));
        }
        self.send_breakpoints();
    }

    /// `path` の行のブレークポイント
    pub(crate) fn breaks_for(&self, path: Option<&Path>) -> Vec<usize> {
        let Some(p) = path.and_then(|p| std::path::absolute(p).ok()) else { return Vec::new() };
        self.breakpoints.get(&p).map(|s| s.iter().copied().collect()).unwrap_or_default()
    }

    /// `path` の止まっている行
    pub(crate) fn exec_line_for(&self, path: Option<&Path>) -> Option<usize> {
        let (p, l) = self.debug.as_ref()?.at.as_ref()?;
        let here = std::path::absolute(path?).ok()?;
        (here == *p).then_some(*l)
    }

    /// 編集で行が増えた・減った: このファイルのブレークポイントを一緒に動かす（`start_line` より後の行。None はすべて）
    pub(crate) fn shift_breakpoints(&mut self, start_line: Option<usize>, removed: usize, inserted: usize) {
        if removed == inserted || self.breakpoints.is_empty() {
            return;
        }
        let Some(p) = self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok()) else { return };
        let Some(set) = self.breakpoints.get_mut(&p) else { return };
        let moved: BTreeSet<usize> = set
            .iter()
            .map(|&l| {
                let Some(start_line) = start_line else { return l + inserted - removed.min(l + inserted) };
                if l <= start_line {
                    l
                } else if l <= start_line + removed {
                    // 消えた行の中にあったものは、編集の行に
                    start_line
                } else {
                    l + inserted - removed
                }
            })
            .collect();
        *set = moved;
    }

    /// `lang` のブレークポイント（Java はクラスも。行は 1 始まり）
    fn all_bps(&self, lang: Lang) -> Vec<Bp> {
        let mut out = Vec::new();
        for (path, lines) in self.breakpoints.iter().filter(|(p, _)| lang_of(p) == Some(lang)) {
            let class = if lang == Lang::Java {
                let text = if self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok()).as_ref() == Some(path) { self.buf.text() } else { std::fs::read_to_string(path).unwrap_or_default() };
                let Some(c) = class_of(path, &text) else { continue };
                c
            } else {
                String::new()
            };
            out.extend(lines.iter().map(|l| Bp { class: class.clone(), file: path.clone(), line: l + 1 }));
        }
        out
    }

    fn send_breakpoints(&mut self) {
        let Some(lang) = self.debug.as_ref().map(|d| d.lang) else { return };
        let bps = self.all_bps(lang);
        if let Some(tx) = self.debug.as_ref().and_then(|d| d.tx.as_ref()) {
            let _ = tx.send(Cmd::Breakpoints(bps));
        }
    }

    /// つなぐ（`addr` に相手が起きるのを待つ。ブラウザなら `hint` を含む URL のページへ）
    fn start_debug(&mut self, name: String, lang: Lang, addr: String, terminal: Option<String>, hint: &str) {
        self.stop_debug_session();
        let bps = self.all_bps(lang);
        let (tx, task) = match lang {
            Lang::Java => nagi_workers::debug::start(addr.clone(), bps, true, self.waker()),
            Lang::Browser => nagi_workers::cdp::start(addr.clone(), hint.to_string(), self.nav_root(), bps, self.waker()),
            Lang::Python => nagi_workers::dap::start(addr.clone(), bps, self.waker()),
        };
        self.debug = Some(DebugUi {
            lang,
            tx: Some(tx),
            task: Some(task),
            name,
            status: format!("{addr} につないでいます…"),
            stopped: false,
            ended: false,
            frames: Vec::new(),
            frame: 0,
            vars: Vec::new(),
            sel_frame: 0,
            sel_var: 0,
            var_scroll: 0,
            on_vars: true,
            at: None,
            src: HashMap::new(),
            terminal,
            buttons: Default::default(),
        });
    }

    /// Shift+F9・`:Debug`: 選んでいる実行の構成（無ければ選ぶ）をデバッグで動かす
    pub(crate) fn debug_run(&mut self, name: &str) {
        let configs = self.load_run_configs();
        let want = if name.trim().is_empty() { self.run_selected.clone() } else { Some(name.trim().to_string()) };
        // 今のファイルが Python なら、構成が無くてもそのファイルを
        let here_py = self.buf.path.as_deref().is_some_and(|p| lang_of(p) == Some(Lang::Python));
        let picked = want.and_then(|w| configs.iter().find(|c| c.name == w).cloned()).or_else(|| if here_py { self.current_file_config() } else { None }).or_else(|| configs.first().cloned()).or_else(|| self.current_file_config());
        let Some(cfg) = picked else {
            self.notify.info("実行の構成がありません（:DebugTest でカーソルの位置のテスト、:DebugBrowser でブラウザ、:DebugAttach 5005 で動いている JVM へ）");
            return;
        };
        self.debug_config(cfg);
    }

    fn debug_config(&mut self, cfg: nagi_workers::runconfig::RunConfig) {
        use nagi_workers::runconfig::Kind;
        let Some(port) = free_port() else {
            self.notify.error("空いているポートがありません");
            return;
        };
        // IntelliJ の JavaScript Debug の構成（URL）: ブラウザを起こしてつなぐ
        if let Kind::Url { url } = &cfg.kind {
            let url = url.clone();
            self.debug_browser(&url);
            return;
        }
        let (dcfg, lang) = match with_agent(&cfg, &agent(port)) {
            Some(c) => (c, Lang::Java),
            None => match self.with_debugpy(&cfg, port) {
                Some(c) => (c, Lang::Python),
                None => {
                    self.notify.error(format!("「{}」（{}）はデバッグで動かせません（Java の Maven・Spring Boot・アプリケーション・java のコマンド、Python、ブラウザの URL）", cfg.name, cfg.kind_label()));
                    return;
                }
            },
        };
        if lang == Lang::Python && !self.has_debugpy() {
            return;
        }
        let name = dcfg.name.clone();
        self.run_config(dcfg);
        self.start_debug(name.clone(), lang, format!("127.0.0.1:{port}"), Some(format!("{}{name}", crate::run_configs::RUN_PREFIX)), "");
        self.notify.info(format!("デバッグ: {name}（起きたらつなぐ）"));
    }

    /// プロジェクトの Python（.venv・venv があればその中のもの）
    fn python_exe(&self) -> String {
        let root = self.nav_root();
        [".venv", "venv"].iter().map(|v| root.join(v).join("Scripts").join("python.exe")).find(|p| p.is_file()).map_or("python".into(), |p| p.display().to_string())
    }

    /// debugpy が入っているか（無ければ入れ方を知らせる）
    fn has_debugpy(&mut self) -> bool {
        let py = self.python_exe();
        let mut c = std::process::Command::new(&py);
        c.args(["-c", "import debugpy"]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        nagi_workers::sys::no_window(&mut c);
        let ok = c.status().is_ok_and(|s| s.success());
        if !ok {
            self.notify.error(format!("{py} に debugpy がありません（{py} -m pip install debugpy）"));
        }
        ok
    }

    /// Python の構成を debugpy の口を開けて動かす形に（python のコマンド・Python の構成）
    fn with_debugpy(&self, cfg: &nagi_workers::runconfig::RunConfig, port: u16) -> Option<nagi_workers::runconfig::RunConfig> {
        use nagi_workers::runconfig::Kind;
        let py = self.python_exe();
        let listen = format!("-m debugpy --listen 127.0.0.1:{port} --wait-for-client");
        let (command, workdir) = match &cfg.kind {
            Kind::Tool { tool, target, args, workdir, .. } if tool == "python" => (format!("\"{py}\" {listen} {} {}", target.trim(), args.trim()), workdir.clone()),
            Kind::Shell { command, workdir } => {
                let rest = command.trim_start().strip_prefix("python ").or_else(|| command.trim_start().strip_prefix("python3 "))?;
                (format!("\"{py}\" {listen} {rest}"), workdir.clone())
            }
            _ => return None,
        };
        Some(nagi_workers::runconfig::RunConfig { name: format!("{}（デバッグ）", cfg.name), kind: Kind::Shell { command: command.trim().to_string(), workdir }, from: "nagi" })
    }

    /// `:DebugBrowser [URL]`: Chrome（無ければ Edge）をデバッグの口を開けて、いつもとは別のプロフィールで起こしてつなぐ
    pub(crate) fn debug_browser(&mut self, url: &str) {
        let url = if url.trim().is_empty() { self.dev_server_url() } else { url.trim().to_string() };
        let Some(exe) = nagi_workers::cdp::find_browser() else {
            self.notify.error("Chrome も Edge も見つかりません");
            return;
        };
        let Some(port) = free_port() else { return };
        match nagi_workers::cdp::launch_browser(&exe, port, &url, false) {
            Ok(_) => {
                let hint = url.split("://").nth(1).unwrap_or(&url).to_string();
                self.start_debug(format!("ブラウザ（{url}）"), Lang::Browser, format!("127.0.0.1:{port}"), None, &hint);
                self.notify.info(format!("デバッグ: {} で {url} を開きました", exe.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
            }
            Err(e) => self.notify.error(e),
        }
    }

    /// 開発サーバーの URL（package.json から当てる。Vite は 5173、Angular は 4200、ほかは 3000）
    fn dev_server_url(&self) -> String {
        let pkg = std::fs::read_to_string(self.nav_root().join("package.json")).unwrap_or_default();
        let port = if pkg.contains("\"vite\"") { 5173 } else if pkg.contains("@angular/core") { 4200 } else { 3000 };
        format!("http://localhost:{port}")
    }

    /// `:DebugAttachBrowser [ポート]`: --remote-debugging-port で起こしてあるブラウザにつなぐ
    pub(crate) fn debug_attach_browser(&mut self, arg: &str) {
        let a = arg.trim();
        let addr = if a.is_empty() { "127.0.0.1:9222".to_string() } else if a.contains(':') { a.to_string() } else { format!("127.0.0.1:{a}") };
        self.start_debug(format!("ブラウザ（{addr}）"), Lang::Browser, addr, None, "");
        self.notify.info("ブラウザにつないでいます…（chrome --remote-debugging-port=9222 で起こしたもの）");
    }

    /// `:DebugAttachPython [ポート]`: debugpy（python -m debugpy --listen 5678 ...）につなぐ
    pub(crate) fn debug_attach_python(&mut self, arg: &str) {
        let a = arg.trim();
        let addr = if a.is_empty() { "127.0.0.1:5678".to_string() } else if a.contains(':') { a.to_string() } else { format!("127.0.0.1:{a}") };
        self.start_debug(format!("Python（{addr}）"), Lang::Python, addr, None, "");
        self.notify.info("debugpy につないでいます…");
    }

    /// `:DebugTest`: カーソルの位置のテストをデバッグで（Maven の surefire の口を開ける）
    pub(crate) fn debug_test(&mut self) {
        let Some(cfg) = self.test_config() else { return };
        self.debug_config(cfg);
    }

    /// `:DebugAttach [ホスト:]ポート`: 動いている JVM につなぐ（-agentlib:jdwp=...,server=y,address=5005 で起こしたもの）
    pub(crate) fn debug_attach(&mut self, arg: &str) {
        let a = arg.trim();
        let addr = match a {
            "" => "127.0.0.1:5005".to_string(),
            a if a.contains(':') => a.to_string(),
            a => format!("127.0.0.1:{a}"),
        };
        self.start_debug(format!("つないだ JVM（{addr}）"), Lang::Java, addr, None, "");
        self.notify.info("JVM につないでいます…");
    }

    fn debug_send(&mut self, c: Cmd) {
        match self.debug.as_ref().and_then(|d| d.tx.as_ref()) {
            Some(tx) if !self.debug.as_ref().is_some_and(|d| d.ended) => {
                let _ = tx.send(c);
            }
            _ => self.notify.info("デバッグしていません（Shift+F9 で始める）"),
        }
    }

    /// 続ける（F5。Visual Studio と同じく、デバッグしていなければ始める）
    pub(crate) fn debug_resume(&mut self) {
        if !self.debug.as_ref().is_some_and(|d| !d.ended && d.tx.is_some()) {
            self.debug_run("");
            return;
        }
        self.debug_send(Cmd::Resume);
    }

    pub(crate) fn debug_step(&mut self, d: Depth) {
        if !self.debug.as_ref().is_some_and(|x| x.stopped) {
            self.notify.info("止まっていないのでステップできません");
            return;
        }
        self.debug_send(Cmd::Step(d));
    }

    /// 止める（JVM を終わらせ、動かしていたターミナルも閉じる）
    pub(crate) fn debug_stop(&mut self) {
        if !self.debug.as_ref().is_some_and(|d| !d.ended) {
            self.notify.info("デバッグしていません");
            return;
        }
        self.debug_send(Cmd::Stop);
        if let Some(t) = self.debug.as_ref().and_then(|d| d.terminal.clone()) {
            self.close_terminal_named(&t);
        }
    }

    /// 前のセッションを捨てる
    fn stop_debug_session(&mut self) {
        if let Some(d) = &mut self.debug {
            if let Some(tx) = d.tx.take() {
                let _ = tx.send(Cmd::Detach);
            }
            d.task = None;
        }
        self.debug = None;
    }

    /// ソースのパス（com/x/Foo.java・/src/App.tsx・絶対のパス）のファイル（開いているもの → プロジェクトの中を探す）
    fn source_path(&mut self, rel: &str) -> Option<PathBuf> {
        if let Some(p) = self.debug.as_ref().and_then(|d| d.src.get(rel).cloned()) {
            return p;
        }
        // 絶対のパス（Python・file:// のページ）ならそのまま
        let abs = PathBuf::from(rel);
        if abs.is_absolute() && abs.is_file() {
            return Some(abs);
        }
        // ブラウザの /src/App.tsx はプロジェクトの根から
        let rel_path = PathBuf::from(rel.trim_start_matches('/'));
        let open = self.buffer_list().into_iter().filter_map(|(_, p, ..)| p).find(|p| p.ends_with(&rel_path));
        let found = open.or_else(|| find_under(&self.nav_root(), &rel_path));
        if let Some(d) = &mut self.debug {
            d.src.insert(rel.to_string(), found.clone());
        }
        found
    }

    /// 止まったフレームのファイルの行へ
    fn show_frame(&mut self, i: usize) {
        let Some(f) = self.debug.as_ref().and_then(|d| d.frames.get(i).cloned()) else { return };
        let (Some(src), Some(line)) = (f.source.as_deref(), f.line) else {
            if let Some(d) = &mut self.debug {
                d.at = None;
            }
            return;
        };
        match self.source_path(src) {
            Some(p) => {
                let p = std::path::absolute(&p).unwrap_or(p);
                self.jump_to(&p, line - 1, 0);
                self.with_motion(|m, c| m.first_non_blank(c));
                if let Some(d) = &mut self.debug {
                    d.at = Some((p, line - 1));
                }
            }
            None => self.notify.info(format!("{src} がプロジェクトに見つかりません（{}.{}:{line}）", f.class, f.method)),
        }
    }

    /// 裏のセッションから届いたもの。変わったら true
    pub(crate) fn poll_debug(&mut self) -> bool {
        let Some(evs) = self.debug.as_ref().and_then(|d| d.task.as_ref()).map(|t| t.drain()) else { return false };
        if evs.is_empty() {
            return false;
        }
        for e in evs {
            match e {
                Ev::Connected(v) => self.debug_status(format!("つながりました（{v}）")),
                Ev::Running => {
                    if let Some(d) = &mut self.debug {
                        d.stopped = false;
                        d.at = None;
                        d.status = "動いています".into();
                    }
                }
                Ev::Stopped { reason, thread, frames, frame, vars } => {
                    let lang = self.debug.as_ref().map_or(Lang::Java, |d| d.lang);
                    let top = frames.first().map(|f| format!("{}:{}", frame_label(lang, f), f.line.unwrap_or(0))).unwrap_or_default();
                    if let Some(d) = &mut self.debug {
                        d.stopped = true;
                        d.frames = frames;
                        d.frame = frame;
                        d.sel_frame = frame;
                        d.vars = vars.into_iter().map(|var| VarRow { depth: 0, var, open: false }).collect();
                        d.sel_var = 0;
                        d.var_scroll = 0;
                        d.status = format!("止まっています: {reason}（{thread}）  {top}");
                    }
                    self.show_frame(frame);
                    // 下の欄の「デバッグ」を出す（本文のカーソルはそのまま）
                    self.bottom_hidden = false;
                    self.bottom_kind = BottomKind::Debug;
                    self.rows = self.text_rows(self.active_rect());
                }
                Ev::Vars { frame, vars } => {
                    if let Some(d) = &mut self.debug {
                        d.frame = frame;
                        d.vars = vars.into_iter().map(|var| VarRow { depth: 0, var, open: false }).collect();
                        d.sel_var = 0;
                        d.var_scroll = 0;
                    }
                }
                Ev::Children { parent, vars } => {
                    if let Some(d) = &mut self.debug {
                        if let Some(i) = d.vars.iter().position(|r| r.var.children == Some(parent) && !r.open) {
                            let depth = d.vars[i].depth + 1;
                            d.vars[i].open = true;
                            let rows: Vec<VarRow> = vars.into_iter().map(|var| VarRow { depth, var, open: false }).collect();
                            d.vars.splice(i + 1..i + 1, rows);
                        }
                    }
                }
                Ev::Note(n) => self.notify.info(n),
                Ev::Ended(m) => {
                    if let Some(d) = &mut self.debug {
                        d.ended = true;
                        d.stopped = false;
                        d.at = None;
                        d.tx = None;
                        d.status = format!("終わりました: {m}");
                    }
                    self.notify.info(format!("デバッグ: {m}"));
                }
            }
        }
        true
    }

    fn debug_status(&mut self, s: String) {
        if let Some(d) = &mut self.debug {
            d.status = s;
        }
    }

    /// 変数の行を開く・閉じる
    pub(crate) fn debug_toggle_var(&mut self, i: usize) {
        let Some(d) = &mut self.debug else { return };
        let Some(r) = d.vars.get(i).cloned() else { return };
        if r.open {
            let end = (i + 1..d.vars.len()).find(|&j| d.vars[j].depth <= r.depth).unwrap_or(d.vars.len());
            d.vars.drain(i + 1..end);
            d.vars[i].open = false;
        } else if let Some((t, id)) = r.var.children {
            self.debug_send(Cmd::Expand(t, id));
        }
    }

    /// スタックのフレームを選ぶ（そのファイルの行へ移り、変数を出し直す）
    pub(crate) fn debug_select_frame(&mut self, i: usize) {
        if !self.debug.as_ref().is_some_and(|d| d.stopped && i < d.frames.len()) {
            return;
        }
        if let Some(d) = &mut self.debug {
            d.sel_frame = i;
        }
        self.debug_send(Cmd::Frame(i));
        self.show_frame(i);
    }

    /// 下の欄の「デバッグ」のキー
    pub(crate) fn debug_key(&mut self, key: Key) -> crate::editor::Effect {
        let Some(d) = &mut self.debug else {
            if matches!(key, Key::Esc | Key::Char('q')) {
                self.leave_panel();
            }
            return crate::editor::Effect::None;
        };
        let (n, sel) = if d.on_vars { (d.vars.len(), d.sel_var) } else { (d.frames.len(), d.sel_frame) };
        if let Some(to) = crate::list::step(key, sel, n, 10) {
            if d.on_vars {
                d.sel_var = to;
            } else {
                self.debug_select_frame(to);
            }
            return crate::editor::Effect::None;
        }
        match key {
            Key::Tab | Key::Char('h') if !d.on_vars || key == Key::Tab => d.on_vars = !d.on_vars,
            Key::Enter | Key::Char('l') | Key::Char(' ') if d.on_vars => {
                let i = d.sel_var;
                self.debug_toggle_var(i);
            }
            Key::Char('h') => {
                // 閉じる（開いた行の中なら親へ）
                let i = d.sel_var;
                if d.vars.get(i).is_some_and(|r| r.open) {
                    self.debug_toggle_var(i);
                } else if let Some(p) = d.vars.get(i).map(|r| r.depth).filter(|&dp| dp > 0).and_then(|dp| (0..i).rev().find(|&j| d.vars[j].depth < dp)) {
                    d.sel_var = p;
                }
            }
            Key::Char('c') => self.debug_resume(),
            Key::Char('n') => self.debug_step(Depth::Over),
            Key::Char('s') => self.debug_step(Depth::Into),
            Key::Char('o') => self.debug_step(Depth::Out),
            Key::Char('x') => self.debug_stop(),
            Key::Esc | Key::Char('q') => self.leave_panel(),
            _ => {}
        }
        crate::editor::Effect::None
    }

    /// 下の欄の「デバッグ」を押した（欄の中の行・列。0 行目は様子とボタン）
    pub(crate) fn debug_click(&mut self, row: usize, col: usize, w: usize, clicks: u32) {
        self.list_side = Side::Bottom;
        self.set_state(State::List);
        let Some(d) = &mut self.debug else { return };
        if row == 0 {
            let hit = d.buttons.borrow().iter().find(|(a, b, _)| col >= *a && col < *b).map(|x| x.2);
            if let Some(k) = hit {
                self.debug_key(k);
            }
            return;
        }
        let row = row - 1;
        let fw = (w * 2 / 5).max(20);
        if col < fw {
            d.on_vars = false;
            if row < d.frames.len() {
                self.debug_select_frame(row);
            }
        } else {
            d.on_vars = true;
            let i = d.var_scroll + row;
            if i < d.vars.len() {
                let again = d.sel_var == i;
                d.sel_var = i;
                if again || clicks >= 2 || d.vars[i].var.children.is_some() {
                    self.debug_toggle_var(i);
                }
            }
        }
    }
}

/// スタックの 1 行の名前（Java は Foo.main、ほかは main（a.ts））
pub(crate) fn frame_label(lang: Lang, f: &Frame) -> String {
    match lang {
        Lang::Java => format!("{}.{}", f.class.rsplit('.').next().unwrap_or(&f.class), f.method),
        _ => format!("{}（{}）", f.method, f.class),
    }
}

/// `root` の下で `rel`（com/x/Foo.java）で終わるファイルを探す（target・node_modules・.git などは見ない）
fn find_under(root: &Path, rel: &Path) -> Option<PathBuf> {
    let name = rel.file_name()?;
    let mut stack = vec![root.to_path_buf()];
    let mut seen = 0;
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            seen += 1;
            if seen > 200_000 {
                return None;
            }
            let p = e.path();
            let n = e.file_name();
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                let s = n.to_string_lossy();
                if !matches!(s.as_ref(), "target" | "node_modules" | ".git" | "build" | "out" | ".idea" | ".gradle") {
                    stack.push(p);
                }
            } else if n == name && p.ends_with(rel) {
                return Some(p);
            }
        }
    }
    None
}

/// この部品の入口（component.rs）
pub struct DebugComponent;

impl crate::component::Component for DebugComponent {
    fn status(&self, ed: &Editor) -> Option<crate::component::Status> {
        let _ = ed;
        let k = |key: &str| ed.settings.get(key);
        let (go, over, into, out, stop) = (k("keymap.debug_resume"), k("keymap.step_over"), k("keymap.step_into"), k("keymap.step_out"), k("keymap.debug_stop"));
        crate::component::Status::keys(
            "-- デバッグ --",
            format!("j/k 選ぶ  Tab スタック⇔変数  Enter 開く  c 続ける({go})  n 次へ({over})  s 中へ({into})  o 外へ({out})  x 止める  q 隠す "),
            format!("{go} 続ける  {over} 次へ  {into} 中へ  {out} 外へ  {stop} 止める "),
        )
    }

    fn key(&self, ed: &mut Editor, key: Key) -> crate::editor::Effect {
        ed.debug_key(key)
    }
}

/// 描き方（view.rs の Painter で描く）
pub(crate) mod draw {
    use super::*;
    use crate::panes::Rect;
    use crate::view::*;

    /// 下の欄の「デバッグ」: 1 行目に様子とボタン、左にスタック、右に変数の木
    pub(crate) fn draw_debug(p: &mut Painter, ed: &Editor, r: Rect, focused: bool) {
        let cell = p.fonts.cell();
        let y = |k: usize| (r.y + k) as f32 * cell.h;
        let Some(d) = &ed.debug else {
            p.text(" デバッグしていません（Shift+F9 で実行の構成を、:DebugTest でカーソルの位置のテストを、:DebugAttach 5005 で動いている JVM を）", r.x, y(0), STATUS_FG);
            p.text(&format!(" ブレークポイントは行番号の欄を押すか {}", ed.settings.get("keymap.breakpoint")), r.x, y(1), STATUS_FG);
            return;
        };
        let saved = p.cols;
        p.cols = r.x + r.w;
        // 1 行目: ボタンと様子
        let mut x = r.x + 1;
        let mut buttons = Vec::new();
        for (label, key, on) in [("▶ 続ける", Key::Char('c'), d.stopped), ("次へ", Key::Char('n'), d.stopped), ("中へ", Key::Char('s'), d.stopped), ("外へ", Key::Char('o'), d.stopped), ("■ 止める", Key::Char('x'), !d.ended)] {
            let w = p.width(label) + 2;
            p.out.push(Instance::fill(x as f32 * cell.w, y(0), w as f32 * cell.w, cell.h, if on { MATCH_BG } else { BG }));
            p.text(&format!(" {label} "), x, y(0), if on { FG } else { LINE_NO });
            buttons.push((x - r.x, x - r.x + w, key));
            x += w + 1;
        }
        let color = if d.stopped { DIAG_WARN } else if d.ended { STATUS_FG } else { GIT_ADD };
        p.text(&format!("  {}  {}", d.name, d.status), x, y(0), color);
        *d.buttons.borrow_mut() = buttons;
        // 左: スタック
        let fw = (r.w * 2 / 5).max(20).min(r.w);
        let h = r.h.saturating_sub(1);
        p.cols = r.x + fw - 1;
        if d.frames.is_empty() {
            p.text(if d.stopped { " （スタックがありません）" } else { " 止まるとスタックを出します" }, r.x, y(1), STATUS_FG);
        }
        for (k, f) in d.frames.iter().enumerate().take(h) {
            let sel = k == d.sel_frame;
            if sel {
                p.out.push(Instance::fill(r.x as f32 * cell.w, y(1 + k), fw.saturating_sub(1) as f32 * cell.w, cell.h, if focused && !d.on_vars { FG } else { MATCH_BG }));
            }
            let rev = sel && focused && !d.on_vars;
            let line = f.line.map_or(String::new(), |l| format!(":{l}"));
            let c = p.text(&format!(" {}", frame_label(d.lang, f)), r.x, y(1 + k), if rev { BG } else { FG });
            p.text(&line, c, y(1 + k), if rev { BG } else { LINE_NO });
        }
        p.out.push(Instance::fill((r.x + fw) as f32 * cell.w - cell.w * 0.5, y(1), 1.0, h as f32 * cell.h, LINE_NO));
        // 右: 変数
        p.cols = r.x + r.w;
        let vx = r.x + fw + 1;
        let top = d.var_scroll.max(d.sel_var.saturating_sub(h.saturating_sub(1))).min(d.sel_var);
        if d.vars.is_empty() && d.stopped {
            p.text(" （変数はありません）", vx, y(1), STATUS_FG);
        }
        for (k, row) in d.vars.iter().enumerate().skip(top).take(h) {
            let yy = y(1 + k - top);
            let sel = k == d.sel_var;
            if sel {
                p.out.push(Instance::fill(vx as f32 * cell.w, yy, (r.x + r.w).saturating_sub(vx) as f32 * cell.w, cell.h, if focused && d.on_vars { FG } else { MATCH_BG }));
            }
            let rev = sel && focused && d.on_vars;
            let mark = match (row.var.children.is_some(), row.open) {
                (true, true) => "▾ ",
                (true, false) => "▸ ",
                _ => "  ",
            };
            let c = p.text(&format!("{}{mark}", "  ".repeat(row.depth)), vx, yy, if rev { BG } else { STATUS_FG });
            let c = p.text(&row.var.name, c, yy, if rev { BG } else { FG });
            let c = p.text(" = ", c, yy, if rev { BG } else { LINE_NO });
            let c = p.text(&row.var.value, c, yy, if rev { BG } else { hl_value(&row.var.value) });
            if !row.var.ty.is_empty() {
                p.text(&format!("  {}", row.var.ty), c, yy, if rev { BG } else { LINE_NO });
            }
        }
        p.cols = saved;
    }

    /// 値の色（文字列・数・null）
    fn hl_value(v: &str) -> [f32; 4] {
        if v.starts_with('"') || v.starts_with('\'') {
            rgb(0x87af87)
        } else if v == "null" {
            rgb(0x8a8a8a)
        } else if v.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '-') || v == "true" || v == "false" {
            rgb(0x87afd7)
        } else {
            FG
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_and_agents() {
        assert_eq!(class_of(Path::new("x/Foo.java"), "package com.x;\n\nclass Foo {}"), Some("com.x.Foo".into()));
        assert_eq!(class_of(Path::new("x/Foo.java"), "class Foo {}"), Some("Foo".into()));
        assert_eq!(class_of(Path::new("x/util.kt"), "package a.b\nfun f() {}"), Some("a.b.utilKt".into()));
        assert_eq!(class_of(Path::new("x/a.ts"), ""), None);
        use nagi_workers::runconfig::{Kind, RunConfig};
        let sb = RunConfig { name: "App".into(), kind: Kind::SpringBoot { module: None, main_class: "a.App".into(), vm: "-Xmx1g".into(), args: String::new(), profiles: String::new(), env: Vec::new() }, from: "nagi" };
        let d = with_agent(&sb, "-agentlib:x").unwrap();
        assert!(matches!(&d.kind, Kind::SpringBoot { vm, .. } if vm == "-Xmx1g -agentlib:x"));
        let mv = RunConfig { name: "t".into(), kind: Kind::Maven { goals: vec!["test".into()], profiles: Vec::new(), options: String::new(), workdir: None, pom: None }, from: "nagi" };
        assert!(matches!(&with_agent(&mv, "-agentlib:x").unwrap().kind, Kind::Maven { options, .. } if options == "\"-Dmaven.surefire.debug=-agentlib:x\""));
        let npm = RunConfig { name: "n".into(), kind: Kind::Url { url: "http://x".into() }, from: "nagi" };
        assert!(with_agent(&npm, "-agentlib:x").is_none());
    }

    fn jdk_bin() -> Option<PathBuf> {
        let home = std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join(".jdks"));
        let mut cands: Vec<PathBuf> = std::env::var_os("JAVA_HOME").map(PathBuf::from).into_iter().collect();
        if let Some(h) = home {
            cands.extend(std::fs::read_dir(h).into_iter().flatten().flatten().map(|e| e.path()));
        }
        cands.into_iter().map(|d| d.join("bin")).find(|b| b.join("javac.exe").is_file())
    }

    /// 本物の JVM に画面の側からつなぐ: 行番号の欄のブレークポイントで止まり、その行を出し、変数・ステップ・続ける
    #[test]
    fn debugs_from_the_editor() {
        let Some(bin) = jdk_bin() else { return };
        let dir = std::env::temp_dir().join(format!("nagi-dbgui-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src/demo")).unwrap();
        let src = "package demo;\n\npublic class Main {\n  public static void main(String[] args) {\n    int n = 41;\n    n = n + 1;\n    System.out.println(n);\n  }\n}\n";
        let file = dir.join("src/demo/Main.java");
        std::fs::write(&file, src).unwrap();
        assert!(std::process::Command::new(bin.join("javac")).args(["-g", "-d", "out", "src/demo/Main.java"]).current_dir(&dir).status().unwrap().success());
        let port = free_port().unwrap();
        let mut jvm = std::process::Command::new(bin.join("java"))
            .args([&agent(port), "-cp", "out", "demo.Main"])
            .current_dir(&dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut e = Editor::new(nagi_core::Buffer::open(&file).unwrap(), None);
        e.buf.path = Some(file.clone());
        e.cwd = dir.clone();
        e.set_screen(120, 40);
        e.toggle_breakpoint(Some(5));
        e.debug_attach(&port.to_string());
        let wait = |e: &mut Editor, f: &dyn Fn(&DebugUi) -> bool| {
            let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while std::time::Instant::now() < until {
                e.poll_debug();
                if e.debug.as_ref().is_some_and(f) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            panic!("待ちきれない: {:?}", e.debug.as_ref().map(|d| d.status.clone()));
        };
        wait(&mut e, &|d| d.stopped);
        assert_eq!(e.exec_line_for(Some(&file)), Some(5), "{}", e.debug.as_ref().unwrap().status);
        assert_eq!(e.bottom_kind, BottomKind::Debug);
        let d = e.debug.as_ref().unwrap();
        assert_eq!(d.vars.iter().map(|r| (r.var.name.as_str(), r.var.value.as_str())).collect::<Vec<_>>(), [("args", "String[0]"), ("n", "41")]);
        e.debug_step(Depth::Over);
        wait(&mut e, &|d| d.stopped && d.at.as_ref().is_some_and(|a| a.1 == 6));
        assert_eq!(e.exec_line_for(Some(&file)), Some(6));
        assert!(e.debug.as_ref().unwrap().vars.iter().any(|r| r.var.name == "n" && r.var.value == "42"));
        e.debug_resume();
        wait(&mut e, &|d| d.ended);
        let _ = jvm.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn python_commands_get_debugpy() {
        use nagi_workers::runconfig::{Kind, RunConfig};
        let e = Editor::new(nagi_core::Buffer::from_str(""), None);
        let t = RunConfig { name: "t".into(), kind: Kind::Shell { command: "python -m pytest \"a.py::f\"".into(), workdir: None }, from: "nagi" };
        let d = e.with_debugpy(&t, 5678).unwrap();
        assert!(matches!(&d.kind, Kind::Shell { command, .. } if command.ends_with("-m debugpy --listen 127.0.0.1:5678 --wait-for-client -m pytest \"a.py::f\"")), "{:?}", d.kind);
        let f = RunConfig { name: "f".into(), kind: Kind::Tool { tool: "python".into(), target: "app.py".into(), args: "-v".into(), env: Vec::new(), workdir: None }, from: "file" };
        assert!(matches!(&e.with_debugpy(&f, 1).unwrap().kind, Kind::Shell { command, .. } if command.ends_with("--wait-for-client app.py -v")));
        assert_eq!(lang_of(Path::new("a/App.tsx")), Some(Lang::Browser));
        assert_eq!(frame_label(Lang::Browser, &Frame { class: "a.ts".into(), method: "add".into(), source: None, line: None }), "add（a.ts）");
    }

    fn wait_until(e: &mut Editor, f: &dyn Fn(&DebugUi) -> bool) {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(40);
        while std::time::Instant::now() < until {
            e.poll_debug();
            if e.debug.as_ref().is_some_and(f) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("待ちきれない: {:?}", e.debug.as_ref().map(|d| d.status.clone()));
    }

    /// 窓なしの Chrome に画面の側からつなぐ: TypeScript の行番号の欄のブレークポイントで止まり、その .ts の行を出す。
    /// ブラウザを起こすので普段は流さない
    #[test]
    #[ignore]
    fn debugs_browser_from_the_editor() {
        let exe = nagi_workers::cdp::find_browser().unwrap();
        let dir = std::env::temp_dir().join(format!("nagi-tsui-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let ts = "function add(a: number, b: number): number {\n  const c = a + b;\n  return c;\n}\nsetInterval(() => { document.title = String(add(2, 3)); }, 300);\n";
        std::fs::write(dir.join("src/a.ts"), ts).unwrap();
        let js = ts.replace(": number", "");
        let map = r#"{"version":3,"file":"a.js","sources":["a.ts"],"mappings":"AAAA;AACA;AACA;AACA;AACA"}"#;
        std::fs::write(dir.join("src/a.js"), format!("{js}//# sourceMappingURL=data:application/json;base64,{}\n", nagi_workers::ws::base64_encode(map.as_bytes()))).unwrap();
        std::fs::write(dir.join("index.html"), "<html><body><script src=\"src/a.js\"></script></body></html>").unwrap();
        let port = free_port().unwrap();
        let url = format!("file:///{}", dir.join("index.html").display().to_string().replace('\\', "/"));
        let mut child = nagi_workers::cdp::launch_browser(&exe, port, &url, true).unwrap();
        let file = dir.join("src/a.ts");
        let mut e = Editor::new(nagi_core::Buffer::open(&file).unwrap(), None);
        e.buf.path = Some(file.clone());
        e.cwd = dir.clone();
        e.set_screen(120, 40);
        e.toggle_breakpoint(Some(1));
        e.debug_attach_browser(&port.to_string());
        wait_until(&mut e, &|d| d.stopped);
        assert_eq!(e.exec_line_for(Some(&file)), Some(1), "{}", e.debug.as_ref().unwrap().status);
        assert!(e.debug.as_ref().unwrap().vars.iter().any(|r| r.var.name == "b" && r.var.value == "3"));
        e.debug_stop();
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// debugpy に画面の側からつなぐ（NAGI_DEBUGPY_PYTHON に debugpy の入った python）。普段は流さない
    #[test]
    #[ignore]
    fn debugs_python_from_the_editor() {
        let py = std::env::var("NAGI_DEBUGPY_PYTHON").unwrap_or_else(|_| "python".into());
        let dir = std::env::temp_dir().join(format!("nagi-pyui-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("app.py");
        std::fs::write(&file, "total = 0\nfor i in range(3):\n    total += i\nprint(total)\n").unwrap();
        let port = free_port().unwrap();
        let mut child = std::process::Command::new(&py)
            .args(["-m", "debugpy", "--listen", &format!("127.0.0.1:{port}"), "--wait-for-client", &file.display().to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut e = Editor::new(nagi_core::Buffer::open(&file).unwrap(), None);
        e.buf.path = Some(file.clone());
        e.cwd = dir.clone();
        e.set_screen(120, 40);
        e.toggle_breakpoint(Some(2));
        e.debug_attach_python(&port.to_string());
        wait_until(&mut e, &|d| d.stopped);
        assert_eq!(e.exec_line_for(Some(&file)), Some(2), "{}", e.debug.as_ref().unwrap().status);
        e.debug_resume();
        wait_until(&mut e, &|d| d.stopped && d.vars.iter().any(|r| r.var.name == "i" && r.var.value == "1"));
        e.toggle_breakpoint(Some(2));
        e.debug_resume();
        wait_until(&mut e, &|d| d.ended);
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ブレークポイントは編集で行がずれれば一緒に動く
    #[test]
    fn breakpoints_follow_edits() {
        let mut e = Editor::new(nagi_core::Buffer::from_str("a\nb\nc\nd\n"), None);
        e.buf.path = Some(std::env::temp_dir().join("nagi-bp/A.java"));
        e.toggle_breakpoint(Some(2));
        e.toggle_breakpoint(Some(3));
        assert_eq!(e.breaks_for(e.buf.path.as_deref()), [2, 3]);
        e.edit(0, 0, "x\ny\n");
        assert_eq!(e.breaks_for(e.buf.path.as_deref()), [4, 5]);
        let s = e.buf.line_start(1);
        let t = e.buf.line_start(5);
        e.edit(s, t, "");
        assert_eq!(e.breaks_for(e.buf.path.as_deref()), [1], "消えた行の中のものは、編集の行に");
        e.toggle_breakpoint(Some(1));
        assert!(e.breaks_for(e.buf.path.as_deref()).is_empty());
    }
}
