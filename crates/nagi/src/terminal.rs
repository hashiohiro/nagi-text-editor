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
    /// 選ぶシェルの一覧（選び始めに 1 度だけ取る。WSL の一覧は wsl.exe を動かすので、描くたびに取ると止まる）
    pub shells: Vec<(String, String)>,
    /// マウスで選んでいる範囲（中身の (行, 列) の 2 点。始点, 終点）。行はさかのぼっていないときの画面の行で、
    /// さかのぼった行は負（さかのぼっても選んだ文字に付いて動く）
    pub sel: Option<((i64, u16), (i64, u16))>,
    /// ドラッグの始点（中身の (行, 列)）
    anchor: Option<(i64, u16)>,
    /// ドラッグしているマウスの位置（画面の (行, 列)。ドラッグしたままホイールで送ったら、終点をここに合わせ直す）
    drag_at: Option<(u16, u16)>,
}

impl Terminals {
    /// 選んでいる範囲（前から後ろの順）
    pub fn sel_ordered(&self) -> Option<((i64, u16), (i64, u16))> {
        let (a, b) = self.sel?;
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    /// 中身の (行, 列) が選んでいる範囲に入るか
    pub fn selected(&self, line: i64, col: u16) -> bool {
        self.sel_ordered().is_some_and(|(a, b)| (line, col) >= a && (line, col) <= b)
    }
}

impl Terminals {
    /// シェルを選び始める
    pub fn open_chooser(&mut self) {
        self.shells = available_shells();
        self.chooser = Some(0);
    }

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
            let shells = self.terminals.shells.clone();
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
                Key::Char('c') => self.terminals.open_chooser(),
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
                    self.terminals.open_chooser();
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

    /// 選んでいる範囲をクリップボードへ（`copy_text`）。コピーしたら選びを消す
    pub(crate) fn terminal_copy(&mut self) -> bool {
        let Some((a, b)) = self.terminals.sel_ordered() else { return false };
        let Some(t) = self.terminals.current() else { return false };
        let text = t.term.with_screen_mut(|s| copy_lines(s, a, b));
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

    /// シェルの画面のセル → 中身の (行, 列)（さかのぼっている分を引く）
    fn terminal_line(&self, (row, col): (u16, u16)) -> (i64, u16) {
        let back = self.terminals.current().map_or(0, |t| t.term.scrollback());
        (row as i64 - back as i64, col)
    }

    /// マウスを押した（シェルの画面の上なら選び始める）。当たれば true
    pub(crate) fn terminal_press(&mut self, row: usize, col: usize) -> bool {
        let Some(cell) = self.terminal_cell(row, col) else { return false };
        self.terminals.sel = None;
        self.terminals.anchor = Some(self.terminal_line(cell));
        self.terminals.drag_at = Some(cell);
        self.set_state(State::Terminal);
        true
    }

    /// ドラッグ（範囲を伸ばす。画面の外は端で止める）
    pub(crate) fn terminal_drag(&mut self, row: usize, col: usize) {
        let Some(r) = self.terminal_rect() else { return };
        let row = row.clamp(r.y, r.y + r.h.saturating_sub(1));
        if let Some(cell) = self.terminal_cell(row, col) {
            self.terminals.drag_at = Some(cell);
            self.terminal_extend();
        }
    }

    /// 選んでいる範囲の終点をマウスの位置に合わせる（ドラッグ中）
    fn terminal_extend(&mut self) {
        let (Some(a), Some(at)) = (self.terminals.anchor, self.terminals.drag_at) else { return };
        let end = self.terminal_line(at);
        if end != a || self.terminals.sel.is_some() {
            self.terminals.sel = Some((a, end));
        }
    }

    /// 離した。ドラッグせずに（範囲を選ばずに）離したところが URL なら開く（IntelliJ のターミナルと同じ）
    pub(crate) fn terminal_release(&mut self) {
        let (anchor, at) = (self.terminals.anchor.take(), self.terminals.drag_at.take());
        let at = at.filter(|_| anchor.is_some() && self.terminals.sel.is_none());
        let url = at.and_then(|cell| self.terminals.current().and_then(|t| t.term.with_screen(|s| link_at(s, cell))));
        if let Some(url) = url {
            crate::run_configs::open_url(&url);
            self.notify.info(format!("開きました: {url}"));
        }
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
                        None => self.terminals.open_chooser(),
                    }
                    return;
                }
                x += w;
            }
            return;
        }
        if let Some(sel) = self.terminals.chooser {
            // 選択肢の行（画面の 2 行目から）
            let n = self.terminals.shells.len();
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

    /// ホイール（正で上へ。画面の (行, 列) の上で回した）。アプリがマウスを求めていればホイールの操作を送り
    /// （Claude Code・vim など）、全画面（代替画面）のアプリには ↑↓ を送り、それ以外はさかのぼる
    pub(crate) fn terminal_wheel(&mut self, lines: isize, row: usize, col: usize) {
        let cell = self.terminal_cell(row, col).unwrap_or((0, 0));
        let Some(t) = self.terminals.current_mut() else { return };
        if t.exited || lines == 0 {
            return;
        }
        let (mode, enc, alt, app) = t.term.with_screen(|s| (s.mouse_protocol_mode(), s.mouse_protocol_encoding(), s.alternate_screen(), s.application_cursor()));
        let key = if lines > 0 { Key::Up } else { Key::Down };
        let one = if mode != vt100::MouseProtocolMode::None {
            wheel_bytes(lines > 0, cell, enc)
        } else if alt {
            key_bytes(key, app).unwrap_or_default()
        } else {
            let now = t.term.scrollback() as isize;
            t.term.set_scrollback((now + lines).max(0) as usize);
            // ドラッグしたまま送ったら、範囲を送った先まで伸ばす（始点は選んだ文字のまま）
            self.terminal_extend();
            return;
        };
        t.term.write(&one.repeat(lines.unsigned_abs()));
    }
}

/// 画面の 1 行（セルごとの (文字, 全角, 全角の続き) と、次の行へ折り返したか）。何も書かれていないセルの文字は空
struct Row {
    cells: Vec<(String, bool, bool)>,
    wrapped: bool,
}

fn read_row(s: &vt100::Screen, row: u16) -> Row {
    let cells = (0..s.size().1).map(|col| s.cell(row, col).map_or((String::new(), false, false), |c| (c.contents().to_string(), c.is_wide(), c.is_wide_continuation()))).collect();
    Row { cells, wrapped: s.row_wrapped(row) }
}

/// 画面の (行, 列) の `a` から `b` まで（`b` の列を含む）の文字（試験用。コピーは copy_lines）
#[cfg(test)]
pub fn copy_text(s: &vt100::Screen, (r0, c0): (u16, u16), (r1, c1): (u16, u16)) -> String {
    join_rows(&(r0..=r1).map(|r| read_row(s, r)).collect::<Vec<_>>(), c0, c1)
}

/// 中身の (行, 列) の `a` から `b` までの文字（`Terminals::sel` の行。画面の外のさかのぼった行も、送って読む。読んだら元の位置へ）
pub fn copy_lines(s: &mut vt100::Screen, (r0, c0): (i64, u16), (r1, c1): (i64, u16)) -> String {
    let back = s.scrollback();
    // さかのぼれるいちばん上（古い行は捨てられているので、そこから）
    s.set_scrollback(usize::MAX);
    let top = -(s.scrollback() as i64);
    let (r0, c0) = if r0 < top { (top, 0) } else { (r0, c0) };
    let r1 = r1.min(s.size().0 as i64 - 1);
    let mut rows = Vec::new();
    for line in r0..=r1 {
        s.set_scrollback((-line).max(0) as usize);
        rows.push(read_row(s, (line + s.scrollback() as i64) as u16));
    }
    s.set_scrollback(back);
    join_rows(&rows, c0, c1)
}

/// 行を 1 つの文字にする（最初の行は `c0` の列から、最後の行は `c1` の列まで）。シェルが出したとおりに戻す:
/// 折り返した行はつなぎ（長いコマンドが途中で切れない）、改行は `\n`（シェルの出したまま。貼ったスクリプトに `\r` が混ざらない）、
/// 書かれた空白は行末でも残し、何も書かれていない行末だけを落とす。
/// 全角の文字が行の端の 1 列に入らずに次の行へ送られたときの、端の空き（ConPTY は空白を書く）も落とす
fn join_rows(rows: &[Row], c0: u16, c1: u16) -> String {
    let mut out = String::new();
    let last = rows.len().saturating_sub(1);
    for (i, r) in rows.iter().enumerate() {
        let cols = r.cells.len();
        let (from, to) = (if i == 0 { c0 as usize } else { 0 }, if i == last { (c1 as usize + 1).min(cols) } else { cols });
        let wrapped = i < last && r.wrapped;
        let pad = wrapped && rows[i + 1].cells.first().is_some_and(|c| c.1) && r.cells.last().is_some_and(|c| !c.2 && c.0.trim().is_empty());
        // 折り返していない行は、書かれたいちばん後ろのセルまで
        let to = if wrapped { to } else { (from..to).rev().find(|&c| !r.cells[c].0.is_empty() || r.cells[c].2).map_or(from, |c| c + 1) };
        for col in from..to.max(from) {
            let (text, _, cont) = &r.cells[col];
            if *cont || (pad && col == cols - 1) {
                continue;
            }
            // 書かれていないセル（カーソルを動かして飛ばしたところ）は空白
            out.push_str(if text.is_empty() { " " } else { text });
        }
        if !wrapped && i < last {
            out.push('\n');
        }
    }
    out
}

/// 画面に見えている URL（URL, 載っているセルの (行, 列)）。折り返した行はつないで探す（長い URL が 2 行にまたがっても 1 つ）
pub fn links(s: &vt100::Screen) -> Vec<(String, Vec<(u16, u16)>)> {
    let (rows, cols) = s.size();
    let mut out = Vec::new();
    let mut row = 0;
    while row < rows {
        // 折り返しでつながった行をまとめる（文字と、文字の頭の byte → セル）
        let mut text = String::new();
        let mut at: Vec<(usize, (u16, u16))> = Vec::new();
        loop {
            for col in 0..cols {
                let Some(c) = s.cell(row, col) else { break };
                if c.is_wide_continuation() {
                    continue;
                }
                at.push((text.len(), (row, col)));
                text.push_str(if c.has_contents() { c.contents() } else { " " });
            }
            row += 1;
            if !s.row_wrapped(row - 1) || row >= rows {
                break;
            }
        }
        for (start, end) in url_spans(&text) {
            let cells = at.iter().filter(|(b, _)| *b >= start && *b < end).map(|(_, c)| *c).collect();
            out.push((text[start..end].to_string(), cells));
        }
    }
    out
}

/// 画面のセルに載っている URL
pub fn link_at(s: &vt100::Screen, cell: (u16, u16)) -> Option<String> {
    links(s).into_iter().find(|(_, cells)| cells.contains(&cell)).map(|(u, _)| u)
}

/// 文字の中の URL の範囲（byte）。空白・引用符・山括弧・ASCII でない文字（「https://… を開く」の日本語）で終わり、
/// 後ろに付いた句読点と対にならない閉じ括弧は外す
pub fn url_spans(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = ["https://", "http://", "file://"].iter().filter_map(|p| text[from..].find(p)).min() {
        let start = from + i;
        let rest = &text[start..];
        let mut end = start + rest.find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`') || !c.is_ascii()).unwrap_or(rest.len());
        loop {
            let url = &text[start..end];
            let trim = match url.chars().last() {
                Some('.' | ',' | ';' | ':' | '!' | '?') => true,
                Some(')') => url.matches(')').count() > url.matches('(').count(),
                Some(']') => url.matches(']').count() > url.matches('[').count(),
                _ => false,
            };
            if !trim {
                break;
            }
            end -= 1;
        }
        if text[start..end].split_once("://").is_some_and(|(_, host)| !host.is_empty()) {
            out.push((start, end));
        }
        from = end.max(start + 1);
    }
    out
}

/// ホイールの 1 回分のマウスの報告（上なら 64、下なら 65 のボタン。`cell` はシェルの画面の (行, 列)）
pub fn wheel_bytes(up: bool, (row, col): (u16, u16), enc: vt100::MouseProtocolEncoding) -> Vec<u8> {
    let b = if up { 64u32 } else { 65 };
    let (x, y) = (col as u32 + 1, row as u32 + 1);
    match enc {
        vt100::MouseProtocolEncoding::Sgr => format!("\x1b[<{b};{x};{y}M").into_bytes(),
        // 32 を足した値を 1 文字に（既定は 1 byte なので 223 まで、UTF-8 は 2047 まで）
        vt100::MouseProtocolEncoding::Utf8 => {
            let c = |v: u32| char::from_u32(32 + v.min(2015)).unwrap_or(' ');
            format!("\x1b[M{}{}{}", c(b), c(x), c(y)).into_bytes()
        }
        vt100::MouseProtocolEncoding::Default => {
            let c = |v: u32| (32 + v.min(223)) as u8;
            vec![0x1b, b'[', b'M', c(b), c(x), c(y)]
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
            crate::component::Status::new("-- ターミナル --", "クリックで URL を開く   Ctrl-\\ の後: n 本文へ  c 新しい窓（シェルを選ぶ）  1〜9 切り替え  x 閉じる   Alt+F12 隠す ")
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
            for (k, (label, cmd)) in ts.shells.iter().enumerate() {
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
            // URL は下線（押すと開く）
            let link_cells: std::collections::HashSet<(u16, u16)> = crate::terminal::links(s).into_iter().flat_map(|(_, c)| c).collect();
            let back = s.scrollback() as i64;
            let scrolled = back > 0;
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
                    if ts.selected(row as i64 - back, col) {
                        fg = BG;
                        bg = FG;
                    }
                    if c.dim() {
                        fg = [fg[0] * 0.6, fg[1] * 0.6, fg[2] * 0.6, fg[3]];
                    }
                    let text = if c.has_contents() { c.contents().to_string() } else { " ".to_string() };
                    let width = if c.is_wide() { 2 } else { 1 };
                    let bg = (bg != BG).then_some(bg);
                    items.push(Item { g: Cow::Owned(text), width, fg, bg, underline: c.underline() || link_cells.contains(&(row, col)) });
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
        // Ctrl+Enter・Shift+Enter（main.rs で Ctrl-j にする）は改行
        assert_eq!(key_bytes(Key::Ctrl('j'), false).unwrap(), b"\n");
        assert_eq!(key_bytes(Key::F(5), false).unwrap(), b"\x1b[15~");
        assert_eq!(key_bytes(Key::Alt('b'), false).unwrap(), b"\x1bb");
    }

    /// 折り返した行はつなぐ。全角の文字が入らずに空いた行の端は落とす（ConPTY は空白を書いて折り返す）
    #[test]
    fn copy_joins_wrapped_rows() {
        let copy = |bytes: &str| {
            let mut p = vt100::Parser::new(4, 9, 0);
            p.process(bytes.as_bytes());
            // 下の何も無い行も選んでいるので、その改行は落として比べる
            copy_text(p.screen(), (0, 0), (3, 8)).trim_end_matches('\n').to_string()
        };
        assert_eq!(copy("abcd日本 語x"), "abcd日本語x");
        // 行の端の本当の空白は残す
        assert_eq!(copy("abcdefgh ijk"), "abcdefgh ijk");
        assert_eq!(copy("abcdefghi jk"), "abcdefghi jk");
        // 折り返していない行は \n（シェルの出したまま）、書かれた空白は行末でも残す、全角は 1 文字
        assert_eq!(copy("ab  \r\n日本\r\n\r\nz"), "ab  \n日本\n\nz");
        // 長い 1 行のコマンド（ワンライナー）は折り返してもつながり、空白も崩れない
        assert_eq!(copy("for i in 1  2; do echo \"$i\"; done"), "for i in 1  2; do echo \"$i\"; done");
        // 字下げ、カーソルを動かして飛ばした空き（ConPTY は空白をそうして書くことがある）は空白、消した行末（ESC[K）は落とす
        assert_eq!(copy("  if x\r\n\x1b[4Cy\r\nab   \x1b[3D\x1b[K"), "  if x\n    y\nab");
        // 途中の列から
        let mut p = vt100::Parser::new(2, 9, 0);
        p.process("abcd日本 語x".as_bytes());
        assert_eq!(copy_text(p.screen(), (0, 2), (1, 0)), "cd日本語");
    }

    /// 文字の中の URL（後ろの句読点・閉じ括弧・日本語は入れない）。折り返して 2 行にまたがっても 1 つ
    #[test]
    fn finds_links() {
        let urls = |t: &str| url_spans(t).into_iter().map(|(a, b)| t[a..b].to_string()).collect::<Vec<_>>();
        assert_eq!(urls("see https://example.com/a?b=1&c=2."), ["https://example.com/a?b=1&c=2"]);
        assert_eq!(urls("(http://x.org/wiki/A_(b)) と https://y.jp/を開く"), ["http://x.org/wiki/A_(b)", "https://y.jp/"]);
        assert_eq!(urls("\"file:///C:/tmp/a.txt\", http://"), ["file:///C:/tmp/a.txt"]);
        assert!(urls("no links here").is_empty());
        let mut p = vt100::Parser::new(3, 12, 0);
        p.process(b"go https://example.com/x end");
        let l = links(p.screen());
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].0, "https://example.com/x");
        assert_eq!(link_at(p.screen(), (1, 2)).as_deref(), Some("https://example.com/x"), "2 行目のセルでも");
        assert_eq!(link_at(p.screen(), (0, 0)), None);
    }

    /// ドラッグせずにクリックして離すと、そこの URL を開く。ドラッグして範囲を選んだときは開かない
    #[test]
    fn click_opens_link() {
        unsafe { std::env::set_var("NAGI_SHELL", "cmd.exe /Q /K prompt $G") };
        let mut e = Editor::new(Buffer::from_str("x\n"), None);
        e.set_screen(120, 40);
        e.toggle_bottom_tab(BottomKind::Terminal);
        for k in nagi_vim::parse_keys("echo see https://example.com/nagi-ok<CR>", " ") {
            e.key(k);
        }
        wait_text(&mut e, "https://example.com/nagi-ok");
        let r = e.terminal_rect().unwrap();
        let row = e.terminals.current().unwrap().term.with_screen(|s| (0..s.size().0).find(|&i| s.contents_between(i, 0, i, 40).starts_with("see ")).unwrap()) as usize;
        // ドラッグして選んだら開かない
        assert!(e.terminal_press(r.y + row, r.x + 1 + 6));
        e.terminal_drag(r.y + row, r.x + 1 + 12);
        e.terminal_release();
        assert!(!e.notify.text.contains("開きました"), "{}", e.notify.text);
        e.terminals.sel = None;
        // クリックだけなら開く
        assert!(e.terminal_press(r.y + row, r.x + 1 + 10));
        e.terminal_release();
        assert_eq!(e.notify.text, "開きました: https://example.com/nagi-ok");
        e.close_terminal();
    }

    #[test]
    fn wheel_reports() {
        use vt100::MouseProtocolEncoding as E;
        assert_eq!(wheel_bytes(true, (4, 9), E::Sgr), b"\x1b[<64;10;5M");
        assert_eq!(wheel_bytes(false, (0, 0), E::Sgr), b"\x1b[<65;1;1M");
        assert_eq!(wheel_bytes(true, (4, 9), E::Default), [0x1b, b'[', b'M', 96, 42, 37]);
        assert_eq!(wheel_bytes(false, (0, 300), E::Default)[4], 255);
        assert_eq!(wheel_bytes(false, (0, 300), E::Utf8), format!("\x1b[Ma{}!", char::from_u32(32 + 301).unwrap()).into_bytes());
    }

    /// 全画面のアプリ（代替画面に移り、マウスを求める。Claude Code など）の出した要求が ConPTY を通って届き、
    /// ホイールはさかのぼらずにアプリへ送る
    #[test]
    fn wheel_goes_to_fullscreen_app() {
        unsafe { std::env::set_var("NAGI_SHELL", "cmd.exe /Q /K prompt $G") };
        let mut e = Editor::new(Buffer::from_str("x\n"), None);
        e.set_screen(120, 40);
        e.toggle_bottom_tab(BottomKind::Terminal);
        for k in nagi_vim::parse_keys("for /L %i in (1,1,60) do @echo l%i<CR>", " ") {
            e.key(k);
        }
        wait_text(&mut e, "l5");
        // ふつうの画面ではさかのぼる
        let r = e.terminal_rect().unwrap();
        e.terminal_wheel(3, r.y + 2, r.x + 5);
        assert_eq!(e.terminals.current().unwrap().term.scrollback(), 3);
        e.terminal_wheel(-3, r.y + 2, r.x + 5);
        assert_eq!(e.terminals.current().unwrap().term.scrollback(), 0);
        // 範囲を選んだままホイールで送っても、始点は選んだ文字に付いたまま。終点は送った先へ伸び、画面の外の行もコピーできる
        let row_of = |e: &Editor, want: &str| e.terminals.current().unwrap().term.with_screen(|s| (0..s.size().0).find(|&i| s.contents_between(i, 0, i, 10).trim_end() == want));
        let start = std::time::Instant::now();
        // l60 の後のプロンプトまで出きってから（出ると画面が送られる）
        while row_of(&e, "l60").is_none_or(|r| row_of(&e, ">").is_none_or(|p| p < r)) {
            assert!(start.elapsed().as_secs() < 20, "l60 が出ない");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let row = row_of(&e, "l60").unwrap() as usize;
        assert!(e.terminal_press(r.y + row, r.x + 1));
        e.terminal_drag(r.y + row, r.x + 3);
        let h = r.h as isize;
        e.terminal_wheel(h, r.y + row, r.x + 3);
        assert_eq!(e.terminals.current().unwrap().term.scrollback(), h as usize);
        assert_eq!(e.terminals.sel_ordered(), Some(((row as i64 - h as i64, 2), (row as i64, 0))), "始点は l60 のまま");
        e.terminal_release();
        let (a, b) = e.terminals.sel_ordered().unwrap();
        let got = e.terminals.current().unwrap().term.with_screen_mut(|s| copy_lines(s, a, b));
        let first = 60 - h;
        let want = format!("{}\n{}\nl", &format!("l{first}")[2..], (first + 1..60).map(|i| format!("l{i}")).collect::<Vec<_>>().join("\n"));
        assert_eq!(got, want);
        assert_eq!(e.terminals.current().unwrap().term.scrollback(), h as usize, "コピーしても見ている位置は変わらない");
        e.terminals.sel = None;
        e.terminal_wheel(-h, r.y + 2, r.x + 5);
        // アプリが代替画面とマウス（SGR）を求める。ConPTY がマウスの要求を通すのは、アプリが VT の入力
        // （ENABLE_VIRTUAL_TERMINAL_INPUT）にしているときだけ（Node の raw モードはそう）
        let ps1 = std::env::temp_dir().join(format!("nagi-vtin-{}.ps1", std::process::id()));
        std::fs::write(&ps1, VT_INPUT_APP).unwrap();
        e.terminal_input(&format!("powershell -NoProfile -ExecutionPolicy Bypass -File \"{}\"\r", ps1.display()));
        let start = std::time::Instant::now();
        loop {
            e.sync_terminal();
            let got = e.terminals.current().unwrap().term.with_screen(|s| (s.alternate_screen(), s.mouse_protocol_mode(), s.mouse_protocol_encoding()));
            if got.1 != vt100::MouseProtocolMode::None && got.2 == vt100::MouseProtocolEncoding::Sgr {
                break;
            }
            assert!(start.elapsed().as_secs() < 20, "{got:?}");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        e.terminal_wheel(3, r.y + 2, r.x + 5);
        assert_eq!(e.terminals.current().unwrap().term.scrollback(), 0);
        e.close_terminal();
        let _ = std::fs::remove_file(ps1);
    }

    /// VT の入力にして、代替画面とマウス（SGR）を求めて待つ
    const VT_INPUT_APP: &str = r#"Add-Type -Namespace W -Name K -MemberDefinition @'
[DllImport("kernel32.dll")] public static extern IntPtr GetStdHandle(int n);
[DllImport("kernel32.dll")] public static extern bool GetConsoleMode(IntPtr h, out uint m);
[DllImport("kernel32.dll")] public static extern bool SetConsoleMode(IntPtr h, uint m);
'@
$h = [W.K]::GetStdHandle(-10); $m = 0; [void][W.K]::GetConsoleMode($h, [ref]$m)
[void][W.K]::SetConsoleMode($h, ($m -bor 0x200) -band (-bnot 0x7))
$e = [char]27
[Console]::Write("$e[?1049h$e[?1000h$e[?1006h")
Start-Sleep 30
"#;

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
        let i = e.terminals.shells.iter().position(|(l, _)| l == "コマンドプロンプト").unwrap();
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

