//! 下の欄の「ターミナル」（IntelliJ の Terminal、Alt+F12）。ConPTY でシェルを動かす。窓（セッション）を
//! いくつも開け、それぞれ PowerShell・コマンドプロンプト・Git Bash・WSL から選ぶ。
//!
//! 1 行目に窓の並び（「1 PowerShell │ 2 コマンドプロンプト │ ＋」。押して切り替え、＋で新しい窓）、2 行目から画面。
//! 欄に移ると、キーをそのままシェルへ送る（Vim の割り当ては通さない）。`Ctrl-\` の後に:
//! n 本文へ（Vim のターミナルと同じ。シェルが Esc を使うため）、c 新しい窓（シェルを選ぶ）、1〜9 切り替え、x 閉じる。
//! 最後に選んだシェルを覚え、次からはそれで開く。日本語は IME で打てる。シェルが終わったら Enter で起動し直す。

use nagi_vim::Key;
use nagi_workers::term::Term;

use crate::editor::{Editor, State};
use crate::panes::Rect;
use crate::result_grid::BottomKind;

pub struct TermView {
    pub term: Term,
    /// 窓の名前（PowerShell・コマンドプロンプトなど）
    pub name: String,
    /// 起動したコマンド（起動し直す用）
    pub cmd: String,
    pub exited: bool,
}

/// ターミナルの窓たち
#[derive(Default)]
pub struct Terminals {
    pub list: Vec<TermView>,
    pub active: usize,
    /// Ctrl-\ を押した（次のキーで操作）
    pending: bool,
    /// シェルを選んでいる（選んでいる行）
    pub chooser: Option<usize>,
    /// マウスで選んでいる範囲（画面の (行, 列) の 2 点。始点, 終点）
    pub sel: Option<((u16, u16), (u16, u16))>,
    /// ドラッグの始点
    anchor: Option<(u16, u16)>,
}

impl Terminals {
    /// 選んでいる範囲（前から後ろの順）
    pub fn sel_ordered(&self) -> Option<((u16, u16), (u16, u16))> {
        let (a, b) = self.sel?;
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    /// (行, 列) が選んでいる範囲に入るか
    pub fn selected(&self, row: u16, col: u16) -> bool {
        self.sel_ordered().is_some_and(|(a, b)| (row, col) >= a && (row, col) <= b)
    }
}

impl Terminals {
    pub fn current(&self) -> Option<&TermView> {
        self.list.get(self.active)
    }

    fn current_mut(&mut self) -> Option<&mut TermView> {
        self.list.get_mut(self.active)
    }
}

/// 使えるシェル（名前, コマンド）
pub fn available_shells() -> Vec<(String, String)> {
    let in_path = |exe: &str| std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(exe)).find(|f| f.is_file()));
    let mut v = Vec::new();
    let pf = std::env::var_os("ProgramFiles").map(std::path::PathBuf::from).unwrap_or_else(|| "C:\\Program Files".into());
    if let Some(p) = in_path("pwsh.exe").or_else(|| Some(pf.join("PowerShell").join("7").join("pwsh.exe")).filter(|p| p.is_file())) {
        v.push(("PowerShell 7".into(), format!("\"{}\"", p.display())));
    }
    v.push(("Windows PowerShell".into(), "powershell.exe".into()));
    v.push(("コマンドプロンプト".into(), "cmd.exe".into()));
    let git_bash = pf.join("Git").join("bin").join("bash.exe");
    if git_bash.is_file() {
        v.push(("Git Bash".into(), format!("\"{}\" --login -i", git_bash.display())));
    }
    // WSL はディストリビューションごと（一覧が取れなければ既定のもの）
    if in_path("wsl.exe").is_some() {
        let ds = nagi_workers::wsl::distros();
        if ds.is_empty() {
            v.push(("WSL".into(), "wsl.exe".into()));
        }
        for d in ds {
            v.push((format!("WSL: {d}"), format!("wsl.exe -d {d}")));
        }
    }
    v
}

/// `:Terminal 名前` の名前からシェルを選ぶ（cmd・powershell・pwsh・bash・wsl。前方一致）
pub fn shell_by_name(name: &str) -> Option<(String, String)> {
    let n = name.trim().to_ascii_lowercase();
    let key = |label: &str| -> &'static str {
        match label {
            "PowerShell 7" => "pwsh",
            "Windows PowerShell" => "powershell",
            "コマンドプロンプト" => "cmd",
            "Git Bash" => "bash",
            _ => "wsl",
        }
    };
    available_shells().into_iter().find(|(l, _)| key(l).starts_with(&n) || l.to_ascii_lowercase().starts_with(&n) || (n == "ps" && key(l).contains("powershell")))
}

/// 最後に選んだシェルの記録（`%LOCALAPPDATA%\nagi\terminal-shell.txt`）
fn shell_file() -> Option<std::path::PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| std::path::PathBuf::from(d).join("nagi").join("terminal-shell.txt"))
}

fn remember_shell(label: &str) {
    if cfg!(test) {
        return;
    }
    if let Some(f) = shell_file() {
        if let Some(d) = f.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(f, label);
    }
}

/// 既定のシェル（NAGI_SHELL、最後に選んだもの、無ければ一番上）
fn default_shell(setting: Option<String>) -> (String, String) {
    // 設定の「既定のシェル」（名前で選べなければコマンドとして）
    if let Some(s) = setting {
        return shell_by_name(&s).unwrap_or_else(|| (s.split_whitespace().next().unwrap_or("shell").trim_end_matches(".exe").to_string(), s.clone()));
    }
    if let Some(s) = std::env::var_os("NAGI_SHELL") {
        let s = s.to_string_lossy().into_owned();
        return (s.split_whitespace().next().unwrap_or("shell").trim_end_matches(".exe").to_string(), s);
    }
    let shells = available_shells();
    let last = shell_file().and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default();
    shells.iter().find(|(l, _)| *l == last.trim()).cloned().unwrap_or_else(|| shells[0].clone())
}

/// キーを端末に送る並びに直す（`app_cursor` は矢印を ESC O の形にする）
pub fn key_bytes(key: Key, app_cursor: bool) -> Option<Vec<u8>> {
    let s = |x: &str| Some(x.as_bytes().to_vec());
    let arrow = |c: char| Some(if app_cursor { format!("\x1bO{c}") } else { format!("\x1b[{c}") }.into_bytes());
    match key {
        Key::Char(c) => Some(c.to_string().into_bytes()),
        Key::Enter | Key::Ctrl('m') => s("\r"),
        Key::Tab => s("\t"),
        Key::Backspace => s("\x7f"),
        Key::Esc => s("\x1b"),
        Key::Delete => s("\x1b[3~"),
        Key::Up => arrow('A'),
        Key::Down => arrow('B'),
        Key::Right => arrow('C'),
        Key::Left => arrow('D'),
        Key::Home => s("\x1b[H"),
        Key::End => s("\x1b[F"),
        Key::PageUp => s("\x1b[5~"),
        Key::PageDown => s("\x1b[6~"),
        Key::Ctrl(c) => {
            let b = match c {
                'a'..='z' => c as u8 - b'a' + 1,
                '[' => 0x1b,
                '\\' => 0x1c,
                ']' => 0x1d,
                '^' => 0x1e,
                '_' => 0x1f,
                ' ' | '@' => 0,
                _ => return None,
            };
            Some(vec![b])
        }
        Key::Alt(c) => Some(format!("\x1b{c}").into_bytes()),
        Key::F(n @ 1..=4) => Some(format!("\x1bO{}", (b'P' + n - 1) as char).into_bytes()),
        Key::F(n @ 5..=12) => {
            let code = [15, 17, 18, 19, 20, 21, 23, 24][(n - 5) as usize];
            Some(format!("\x1b[{code}~").into_bytes())
        }
        _ => None,
    }
}

/// 窓の並びの項目（表示, 押したら: Some(窓の番号) か None（＋））
pub fn strip_items(t: &Terminals) -> Vec<(String, Option<usize>)> {
    let mut v: Vec<(String, Option<usize>)> = t.list.iter().enumerate().map(|(i, s)| (format!(" {} {}{} ", i + 1, s.name, if s.exited { "（終了）" } else { "" }), Some(i))).collect();
    v.push((" ＋ ".into(), None));
    v
}

impl Editor {
    /// シェルの画面の矩形（下の欄の中身の 2 行目から。1 行目は窓の並び）
    pub fn terminal_rect(&self) -> Option<Rect> {
        let r = self.bottom_content_rect()?;
        Some(Rect { x: r.x, y: r.y + 1, w: r.w, h: r.h.saturating_sub(1) })
    }

    /// ターミナルのタブを出して移る（窓が無ければ既定のシェルで開く）
    pub(crate) fn open_terminal(&mut self) {
        self.bottom_hidden = false;
        self.bottom_kind = BottomKind::Terminal;
        self.set_state(State::Terminal);
        self.pending_keys.clear();
        self.rows = self.text_rows(self.active_rect());
        if self.terminals.list.is_empty() {
            // WSL のプロジェクトなら、そのディストリビューションのシェルをプロジェクトのフォルダから
            let (name, cmd) = match nagi_workers::exec::Exec::in_dir(&self.nav_root()).login_shell() {
                Some(s) if std::env::var_os("NAGI_SHELL").is_none() => s,
                _ => default_shell(self.settings.text("terminal.shell")),
            };
            self.new_terminal(&name, &cmd);
        }
    }

    /// `:Terminal 名前`: そのシェルで新しい窓を開く（名前が無ければタブを出すだけ）
    pub(crate) fn terminal_command(&mut self, arg: &str) {
        if arg.trim().is_empty() {
            self.select_bottom_tab(BottomKind::Terminal);
            return;
        }
        match shell_by_name(arg) {
            Some((name, cmd)) => {
                self.bottom_hidden = false;
                self.bottom_kind = BottomKind::Terminal;
                self.set_state(State::Terminal);
                self.rows = self.text_rows(self.active_rect());
                self.new_terminal_chosen(&name, &cmd);
            }
            None => {
                let names: Vec<String> = available_shells().into_iter().map(|s| s.0).collect();
                self.notify.warn(format!("シェルが見つかりません: {arg}（使えるもの: {}）", names.join("・")));
            }
        }
    }

    /// 新しい窓を開いて移る（人が選んだシェルなら `chosen`。次からの既定として覚える）
    pub(crate) fn new_terminal_chosen(&mut self, name: &str, cmd: &str) {
        if self.new_terminal(name, cmd) {
            remember_shell(name);
        }
    }

    /// 新しい窓を開いて移る（開けたら true）
    pub(crate) fn new_terminal(&mut self, name: &str, cmd: &str) -> bool {
        let (rows, cols) = self.terminal_rect().map_or((10, 80), |r| (r.h as u16, r.w.saturating_sub(1) as u16));
        // 設定の「始めるフォルダ」
        let cwd = match self.settings.get("terminal.start_dir").as_str() {
            "file" => self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok()).and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_else(|| self.nav_root()),
            "home" => std::env::var_os("USERPROFILE").map(std::path::PathBuf::from).unwrap_or_else(|| self.nav_root()),
            _ => self.nav_root(),
        };
        let waker = self.waker();
        match Term::spawn(cmd, &cwd, rows, cols, waker) {
            Ok(term) => {
                self.terminals.list.push(TermView { term, name: name.to_string(), cmd: cmd.to_string(), exited: false });
                self.terminals.active = self.terminals.list.len() - 1;
                self.terminals.chooser = None;
                true
            }
            Err(e) => {
                self.notify.error(format!("{name} を起動できません（{cmd}）: {e}"));
                false
            }
        }
    }

    /// 欄の大きさに合わせ、シェルが終わったかを見る（描く前に呼ぶ）
    pub(crate) fn sync_terminal(&mut self) {
        let r = (self.bottom_kind == BottomKind::Terminal).then(|| self.terminal_rect()).flatten();
        for t in &mut self.terminals.list {
            if let Some(r) = r {
                t.term.resize(r.h as u16, r.w.saturating_sub(1) as u16);
            }
            if !t.exited && t.term.exited() {
                t.exited = true;
            }
        }
    }

    pub(crate) fn terminal_key(&mut self, key: Key) {
        // 実行の構成の窓（▶）は、IntelliJ の実行の窓と同じく Esc でエディタへ戻る（シェルの窓では Esc はシェルへ）
        if key == Key::Esc && self.terminals.chooser.is_none() && self.terminals.current().is_some_and(|t| t.name.starts_with(crate::run_configs::RUN_PREFIX)) {
            self.terminals.sel = None;
            self.set_state(State::Normal);
            return;
        }
        // シェルを選んでいる
        if let Some(sel) = self.terminals.chooser {
            let shells = available_shells();
            let last = shells.len().saturating_sub(1);
            match key {
                Key::Char('j') | Key::Down | Key::Tab => self.terminals.chooser = Some((sel + 1).min(last)),
                Key::Char('k') | Key::Up => self.terminals.chooser = Some(sel.saturating_sub(1)),
                Key::Char(c @ '1'..='9') if (c as usize - '1' as usize) <= last => {
                    let (n, c) = shells[c as usize - '1' as usize].clone();
                    self.new_terminal_chosen(&n, &c);
                }
                Key::Enter => {
                    let (n, c) = shells[sel.min(last)].clone();
                    self.new_terminal_chosen(&n, &c);
                }
                Key::Esc | Key::Char('q') | Key::Ctrl('c') => {
                    self.terminals.chooser = None;
                    if self.terminals.list.is_empty() {
                        self.set_state(State::Normal);
                    }
                }
                _ => {}
            }
            return;
        }
        if self.terminals.list.is_empty() {
            match key {
                Key::Enter => self.open_terminal(),
                Key::Char('c') => self.terminals.chooser = Some(0),
                _ => self.set_state(State::Normal),
            }
            return;
        }
        // Ctrl-\ の後: n 本文へ、c 新しい窓、1〜9 切り替え、x 閉じる、Ctrl-\ そのもの
        if std::mem::take(&mut self.terminals.pending) {
            match key {
                Key::Char('n') | Key::Ctrl('n') | Key::Char('k') | Key::Ctrl('k') => {
                    self.set_state(State::Normal);
                    return;
                }
                Key::Char('c') => {
                    self.terminals.chooser = Some(0);
                    return;
                }
                Key::Char(c @ '1'..='9') => {
                    let i = c as usize - '1' as usize;
                    if i < self.terminals.list.len() {
                        self.terminals.active = i;
                    }
                    return;
                }
                Key::Char('x') => {
                    self.close_terminal();
                    return;
                }
                Key::Ctrl('\\') => {
                    if let Some(t) = self.terminals.current_mut() {
                        t.term.write(&[0x1c]);
                    }
                    return;
                }
                _ => {}
            }
        } else if key == Key::Ctrl('\\') {
            self.terminals.pending = true;
            return;
        }
        // Ctrl+C: 選んでいればコピー（シェルには送らない）、選んでいなければシェルへ（止める）。Windows Terminal と同じ
        if key == Key::Ctrl('c') && self.terminals.sel.is_some() {
            self.terminal_copy();
            return;
        }
        // Ctrl+V・Shift+Insert: 貼り付け
        let key = if key == Key::Ctrl('v') { Key::Paste } else { key };
        if key != Key::Paste {
            self.terminals.sel = None;
        }
        let clip = self.clipboard;
        let Some(t) = self.terminals.current_mut() else { return };
        if t.exited {
            if key == Key::Enter {
                let (name, cmd) = (t.name.clone(), t.cmd.clone());
                let i = self.terminals.active;
                self.terminals.list.remove(i);
                self.new_terminal(&name, &cmd);
                // 同じ場所に置き直す
                let new = self.terminals.list.pop();
                if let Some(n) = new {
                    self.terminals.list.insert(i, n);
                    self.terminals.active = i;
                }
            }
            return;
        }
        // 打ったら一番下へ
        if t.term.scrollback() > 0 {
            t.term.set_scrollback(0);
        }
        if key == Key::Paste {
            let text = match &clip {
                Some(cb) => (cb.get)(),
                None => crate::clipboard::get(),
            };
            if let Some(text) = text {
                self.terminal_input(&text);
            }
            return;
        }
        let app = t.term.with_screen(|s| s.application_cursor());
        if let Some(b) = key_bytes(key, app) {
            t.term.write(&b);
        }
    }

    /// 選んでいる範囲をクリップボードへ（各行の行末の空白は落とす）。コピーしたら選びを消す
    pub(crate) fn terminal_copy(&mut self) -> bool {
        let Some(((r0, c0), (r1, c1))) = self.terminals.sel_ordered() else { return false };
        let Some(t) = self.terminals.current() else { return false };
        let text = t.term.with_screen(|s| s.contents_between(r0, c0, r1, c1 + 1));
        let text: String = text.lines().map(str::trim_end).collect::<Vec<_>>().join("\r\n");
        match &self.clipboard {
            Some(cb) => (cb.set)(&text),
            None => crate::clipboard::set(&text),
        }
        self.terminals.sel = None;
        self.notify.info(format!("コピーしました（{} 文字）", text.chars().count()));
        true
    }

    /// 画面の (行, 列) → シェルの画面のセル
    fn terminal_cell(&self, row: usize, col: usize) -> Option<(u16, u16)> {
        let r = self.terminal_rect()?;
        if row < r.y || row >= r.y + r.h {
            return None;
        }
        Some(((row - r.y) as u16, col.saturating_sub(r.x + 1) as u16))
    }

    /// マウスを押した（シェルの画面の上なら選び始める）。当たれば true
    pub(crate) fn terminal_press(&mut self, row: usize, col: usize) -> bool {
        let Some(cell) = self.terminal_cell(row, col) else { return false };
        self.terminals.sel = None;
        self.terminals.anchor = Some(cell);
        self.set_state(State::Terminal);
        true
    }

    /// ドラッグ（範囲を伸ばす。画面の外は端で止める）
    pub(crate) fn terminal_drag(&mut self, row: usize, col: usize) {
        let Some(a) = self.terminals.anchor else { return };
        let Some(r) = self.terminal_rect() else { return };
        let row = row.clamp(r.y, r.y + r.h.saturating_sub(1));
        if let Some(cell) = self.terminal_cell(row, col) {
            if cell != a || self.terminals.sel.is_some() {
                self.terminals.sel = Some((a, cell));
            }
        }
    }

    pub(crate) fn terminal_release(&mut self) {
        self.terminals.anchor = None;
    }

    /// 右クリック: 選んでいればコピー、選んでいなければ貼り付け（Windows Terminal と同じ）
    pub(crate) fn terminal_right_click(&mut self) {
        self.set_state(State::Terminal);
        if !self.terminal_copy() {
            self.terminal_key(Key::Paste);
        }
    }

    /// 今の窓を閉じる（シェルを止める）
    pub(crate) fn close_terminal(&mut self) {
        let i = self.terminals.active;
        if i < self.terminals.list.len() {
            let mut t = self.terminals.list.remove(i);
            t.term.kill();
        }
        self.terminals.active = self.terminals.active.min(self.terminals.list.len().saturating_sub(1));
        if self.terminals.list.is_empty() {
            self.set_state(State::Normal);
        }
    }

    /// IME の確定・貼り付け（貼り付けは括弧付きの形に対応）
    pub(crate) fn terminal_input(&mut self, text: &str) {
        let Some(t) = self.terminals.current_mut() else { return };
        if t.exited {
            return;
        }
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        if text.chars().count() > 1 && t.term.with_screen(|s| s.bracketed_paste()) {
            t.term.write(format!("\x1b[200~{text}\x1b[201~").as_bytes());
        } else {
            t.term.write(text.as_bytes());
        }
    }

    /// 欄の中を押した（中身の行・列。0 行目は窓の並び）
    pub(crate) fn terminal_click(&mut self, row: usize, col: usize) {
        self.set_state(State::Terminal);
        if row == 0 {
            let mut x = 0;
            for (label, target) in strip_items(&self.terminals) {
                let w = nagi_core::display_col(&label, label.len(), false) + 1;
                if col >= x && col < x + w {
                    match target {
                        Some(i) => self.terminals.active = i,
                        None => self.terminals.chooser = Some(0),
                    }
                    return;
                }
                x += w;
            }
            return;
        }
        if let Some(sel) = self.terminals.chooser {
            // 選択肢の行（画面の 2 行目から）
            let n = available_shells().len();
            let i = row.saturating_sub(2);
            if row >= 2 && i < n {
                if i == sel {
                    self.terminal_key(Key::Enter);
                } else {
                    self.terminals.chooser = Some(i);
                }
            }
            return;
        }
        if self.terminals.list.is_empty() {
            self.open_terminal();
        }
    }

    /// ホイール: さかのぼる（正で上へ）
    pub(crate) fn terminal_scroll(&mut self, lines: isize) {
        if let Some(t) = self.terminals.current() {
            let now = t.term.scrollback() as isize;
            t.term.set_scrollback((now + lines).max(0) as usize);
        }
    }
}



/// この部品の入口（component.rs）
pub struct TerminalComponent;

impl crate::component::Component for TerminalComponent {
    fn status(&self, ed: &crate::editor::Editor) -> Option<crate::component::Status> {
        if ed.terminals.current().is_some_and(|t| t.name.starts_with(crate::run_configs::RUN_PREFIX)) {
            crate::component::Status::new("-- 実行 --", "Esc エディタへ   Ctrl+C 止める   Ctrl+F5 もう一度   Ctrl-\\ x 閉じる   Alt+F12 隠す ")
        } else {
            crate::component::Status::new("-- ターミナル --", "Ctrl-\\ の後: n 本文へ  c 新しい窓（シェルを選ぶ）  1〜9 切り替え  x 閉じる   Alt+F12 隠す ")
        }
    }

    fn key(&self, ed: &mut crate::editor::Editor, key: nagi_vim::Key) -> crate::editor::Effect {
        ed.terminal_key(key);
        crate::editor::Effect::None
    }

    fn text(&self, ed: &mut crate::editor::Editor, text: &str) {
        ed.terminal_input(text);
    }
}

/// 描き方（view.rs の Painter で描く）
pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    /// ターミナル（下の欄）。1 行目に窓の並び、2 行目からシェルの画面。移っていればカーソル（四角）と IME の位置
    pub(crate) fn draw_terminal(p: &mut Painter, ed: &Editor, r: Rect, focused: bool) {
        let cell = p.fonts.cell();
        let ts = &ed.terminals;
        let saved = p.cols;
        p.cols = r.x + r.w;
        // 窓の並び（今の窓は明るく下線、＋で新しい窓）
        let y0 = r.y as f32 * cell.h;
        let mut x = r.x;
        for (label, target) in crate::terminal::strip_items(ts) {
            let on = target == Some(ts.active) && !ts.list.is_empty();
            let end = p.text(&label, x, y0, if on { FG } else { STATUS_FG });
            if on {
                p.out.push(Instance::fill(x as f32 * cell.w, y0 + cell.h - 2.0, (end - x) as f32 * cell.w, 2.0, if focused { GIT_MOD } else { STATUS_FG }));
            }
            // 区切り: 罫線の文字は行の上下いっぱいに伸びて上のタブの線とつながるので、上下を空けた細い線にする
            let h = (cell.h * 0.5).round();
            p.out.push(Instance::fill((end as f32 + 0.5) * cell.w, y0 + (cell.h - h) / 2.0, 1.0, h, LINE_NO));
            x = end + 1;
        }
        let r = Rect { x: r.x, y: r.y + 1, w: r.w, h: r.h.saturating_sub(1) };
        // シェルを選んでいる
        if let Some(sel) = ts.chooser {
            p.text(" 新しい窓のシェル（j/k・数字で選んで Enter、Esc でやめる）", r.x, r.y as f32 * cell.h, STATUS_FG);
            for (k, (label, cmd)) in crate::terminal::available_shells().iter().enumerate() {
                let y = (r.y + 1 + k) as f32 * cell.h;
                if k == sel {
                    p.out.push(Instance::fill(r.x as f32 * cell.w, y, r.w as f32 * cell.w, cell.h, if focused { FG } else { MATCH_BG }));
                }
                let rev = k == sel && focused;
                let c = p.text(&format!("  {} {label}", k + 1), r.x, y, if rev { BG } else { FG });
                p.text(&format!("   {cmd}"), c, y, if rev { BG } else { LINE_NO });
            }
            p.cols = saved;
            return;
        }
        let Some(t) = ts.current() else {
            p.text(" 窓がありません（Enter で開く、c でシェルを選ぶ）", r.x, r.y as f32 * cell.h, STATUS_FG);
            p.cols = saved;
            return;
        };
        let x0 = r.x + 1;
        t.term.with_screen(|s| {
            let (rows, cols) = s.size();
            let scrolled = s.scrollback() > 0;
            for row in 0..rows.min(r.h as u16) {
                let y = (r.y + row as usize) as f32 * cell.h;
                let mut items: Vec<Item> = Vec::with_capacity(cols as usize);
                for col in 0..cols {
                    let Some(c) = s.cell(row, col) else { continue };
                    if c.is_wide_continuation() {
                        continue;
                    }
                    let (mut fg, mut bg) = (term_color(c.fgcolor(), FG), term_color(c.bgcolor(), BG));
                    if c.inverse() {
                        std::mem::swap(&mut fg, &mut bg);
                    }
                    // マウスで選んでいる範囲は反転
                    if ts.selected(row, col) {
                        fg = BG;
                        bg = FG;
                    }
                    if c.dim() {
                        fg = [fg[0] * 0.6, fg[1] * 0.6, fg[2] * 0.6, fg[3]];
                    }
                    let text = if c.has_contents() { c.contents().to_string() } else { " ".to_string() };
                    let width = if c.is_wide() { 2 } else { 1 };
                    let bg = (bg != BG).then_some(bg);
                    items.push(Item { g: Cow::Owned(text), width, fg, bg, underline: c.underline() });
                }
                p.draw(&items, x0, y);
            }
            // カーソル（遡っている間と、シェルが隠しているときは出さない）
            let (cr, cc) = s.cursor_position();
            if focused && !scrolled && !s.hide_cursor() && (cr as usize) < r.h {
                let y = (r.y + cr as usize) as f32 * cell.h;
                let col = x0 + cc as usize;
                p.out.push(Instance::fill(col as f32 * cell.w, y, cell.w, cell.h, CURSOR));
                let ch = s.cell(cr, cc).map(|c| c.contents().to_string()).filter(|c| !c.is_empty()).unwrap_or_default();
                if !ch.is_empty() {
                    p.text(&ch, col, y, BG);
                }
                // IME の候補の窓はカーソルの位置へ（変換中の文字は下線で重ねる）
                if !ed.preedit.is_empty() {
                    let mut pre = p.items(&ed.preedit, FG);
                    pre.iter_mut().for_each(|i| {
                        i.underline = true;
                        i.bg = Some(BG);
                    });
                    p.draw(&pre, col, y);
                }
                p.caret_at = Some((col, y));
            }
            if scrolled {
                let tag = format!(" ↑ {} 行さかのぼっています（打つと戻る） ", s.scrollback());
                let w = p.width(&tag);
                let x = (r.x + r.w).saturating_sub(w + 1);
                p.out.push(Instance::fill(x as f32 * cell.w, r.y as f32 * cell.h, w as f32 * cell.w, cell.h, MATCH_BG));
                p.text(&tag, x, r.y as f32 * cell.h, FG);
            }
        });
        if t.exited {
            let y = (r.y + r.h.saturating_sub(1)) as f32 * cell.h;
            p.out.push(Instance::fill(r.x as f32 * cell.w, y, r.w as f32 * cell.w, cell.h, MATCH_BG));
            let back = if t.name.starts_with(crate::run_configs::RUN_PREFIX) { "Esc でエディタへ、" } else { "" };
            p.text(&format!(" {} が終わりました。Enter で起動し直す（{back}Ctrl-\\ x で閉じる） ", t.name), r.x, y, FG);
        }
        p.cols = saved;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    #[test]
    fn keys_to_bytes() {
        assert_eq!(key_bytes(Key::Char('あ'), false).unwrap(), "あ".as_bytes());
        assert_eq!(key_bytes(Key::Ctrl('c'), false).unwrap(), vec![3]);
        assert_eq!(key_bytes(Key::Up, false).unwrap(), b"\x1b[A");
        assert_eq!(key_bytes(Key::Up, true).unwrap(), b"\x1bOA");
        assert_eq!(key_bytes(Key::Enter, false).unwrap(), b"\r");
        assert_eq!(key_bytes(Key::F(5), false).unwrap(), b"\x1b[15~");
        assert_eq!(key_bytes(Key::Alt('b'), false).unwrap(), b"\x1bb");
    }

    #[test]
    fn shells_list() {
        let s = available_shells();
        assert!(s.iter().any(|(l, _)| l == "コマンドプロンプト"));
        assert!(s.iter().any(|(l, _)| l == "Windows PowerShell"));
        assert_eq!(shell_by_name("cmd").map(|s| s.0).as_deref(), Some("コマンドプロンプト"));
        // PowerShell 7 が入っていればそちら
        assert!(matches!(shell_by_name("powershell").map(|s| s.0).as_deref(), Some("Windows PowerShell" | "PowerShell 7")));
    }

    fn wait_text(e: &mut Editor, want: &str) {
        let start = std::time::Instant::now();
        loop {
            e.sync_terminal();
            let text = e.terminals.current().unwrap().term.with_screen(|s| s.contents());
            if text.matches(want).count() >= 2 {
                return;
            }
            assert!(start.elapsed().as_secs() < 20, "{text:?}");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// 既定のシェルで開いて打ち、Ctrl-\ c でコマンドプロンプトの窓を足し、Ctrl-\ 1 で戻り、Ctrl-\ n で本文へ
    #[test]
    fn terminal_tab_and_sessions() {
        unsafe { std::env::set_var("NAGI_SHELL", "cmd.exe /Q /K prompt $G") };
        let mut e = Editor::new(Buffer::from_str("x\n"), None);
        e.set_screen(120, 40);
        e.toggle_bottom_tab(BottomKind::Terminal);
        assert_eq!(e.state(), State::Terminal);
        assert_eq!(e.terminals.list.len(), 1, "{}", e.notify.text);
        for k in nagi_vim::parse_keys("echo nagi-tab-ok<CR>", " ") {
            e.key(k);
        }
        wait_text(&mut e, "nagi-tab-ok");
        // 欄の大きさに合わせている（1 行目は窓の並び）
        let r = e.terminal_rect().unwrap();
        assert_eq!(e.terminals.current().unwrap().term.with_screen(|s| s.size()), (r.h as u16, (r.w - 1) as u16));
        // 新しい窓: シェルを選ぶ（コマンドプロンプト）
        e.key(Key::Ctrl('\\'));
        e.key(Key::Char('c'));
        assert!(e.terminals.chooser.is_some());
        let i = available_shells().iter().position(|(l, _)| l == "コマンドプロンプト").unwrap();
        e.terminals.chooser = Some(i);
        e.key(Key::Enter);
        assert_eq!((e.terminals.list.len(), e.terminals.active), (2, 1));
        assert_eq!(e.terminals.list[1].name, "コマンドプロンプト");
        for k in nagi_vim::parse_keys("echo second-ok<CR>", " ") {
            e.key(k);
        }
        wait_text(&mut e, "second-ok");
        // 1 番へ戻る、2 番を閉じる
        e.key(Key::Ctrl('\\'));
        e.key(Key::Char('2'));
        e.key(Key::Ctrl('\\'));
        e.key(Key::Char('x'));
        assert_eq!(e.terminals.list.len(), 1);
        assert_eq!(e.terminals.current().unwrap().name, "cmd");
        // 範囲を選んでコピー（Ctrl+C はシェルに送らない）、選びは消える
        let r = e.terminal_rect().unwrap();
        let row = e.terminals.current().unwrap().term.with_screen(|s| (0..s.size().0).find(|&i| s.contents_between(i, 0, i, 40).contains("echo nagi-tab-ok")).unwrap());
        assert!(e.terminal_press(r.y + row as usize, r.x + 1));
        e.terminal_drag(r.y + row as usize, r.x + 60);
        e.terminal_release();
        let copied = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        thread_local! { static CLIP: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) }; }
        fn set(t: &str) { CLIP.with(|c| *c.borrow_mut() = t.to_string()); }
        fn get() -> Option<String> { CLIP.with(|c| Some(c.borrow().clone())) }
        e.clipboard = Some(crate::editor::Clipboard { set, get });
        e.key(Key::Ctrl('c'));
        let got = get().unwrap();
        assert!(got.contains("echo nagi-tab-ok"), "{got:?}");
        assert!(e.terminals.sel.is_none());
        let _ = copied;
        // 貼り付け（Ctrl+V）。貼り付けの中の改行では実行しない（括弧付きの貼り付け）ので、Enter は別に押す
        set("echo pasted-ok");
        e.key(Key::Ctrl('v'));
        e.key(Key::Enter);
        wait_text(&mut e, "pasted-ok");
        // 本文へ
        e.key(Key::Ctrl('\\'));
        e.key(Key::Char('n'));
        assert_eq!(e.state(), State::Normal);
        e.key(Key::Char('x'));
        assert_eq!(e.buf.line(0), "");
        // :Terminal cmd で開く
        for k in nagi_vim::parse_keys(":Terminal cmd<CR>", " ") {
            e.key(k);
        }
        assert_eq!(e.terminals.list.len(), 2);
        assert_eq!(e.state(), State::Terminal);
    }
}
