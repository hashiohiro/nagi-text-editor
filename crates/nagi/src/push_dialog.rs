//! コミットのプッシュのダイアログ（IntelliJ の Push Commits と同じ形）。
//!
//! 上に送り先（ブランチ → リモート : ブランチ。上流が無ければ新しく作る）、左に送るコミット、右に選んだコミットで変えたファイル、
//! 下に「強制プッシュ」「プッシュ」「キャンセル」。Enter かプッシュで送り、強制プッシュは確かめてから --force-with-lease で送る。

use nagi_vim::Key;

use crate::editor::{Editor, Effect, State};
use crate::panes::Rect;

pub struct PushDialog {
    /// 送り先の見出し（dev → origin : dev）
    pub target: String,
    /// 上流が無い（origin に同じ名前で作る）
    pub new_upstream: bool,
    /// 送るコミット（「短いハッシュ 件名」）
    pub commits: Vec<String>,
    pub selected: usize,
    pub scroll: usize,
    /// 選んだコミットで変えたファイル（「M\tパス」）と、どのコミットのものか
    pub files: Vec<String>,
    files_for: Option<String>,
    /// 選んでいるボタン（0: プッシュ、1: キャンセル、2: 強制プッシュ。Tab で替える）
    pub button: usize,
}

/// ボタン（番号, 見出し）。右下に プッシュ・キャンセル、左下に 強制プッシュ
const BUTTONS: [(usize, &str); 3] = [(2, "強制プッシュ"), (0, "プッシュ"), (1, "キャンセル")];

/// ダイアログの矩形（画面の真ん中）
pub fn rect(cols: usize, rows: usize) -> Rect {
    let w = crate::metrics::cols(110).min(cols.saturating_sub(4));
    let h = 22.min(rows.saturating_sub(4));
    Rect { x: cols.saturating_sub(w) / 2, y: rows.saturating_sub(h) / 2, w, h }
}

/// 一覧の行の範囲（ダイアログの中の行。上 2 行は題と送り先、下 2 行はボタン）
fn list_rows(r: Rect) -> std::ops::Range<usize> {
    2..r.h.saturating_sub(2)
}

/// ボタンの位置（番号, 行, 始めの列, 終わりの列）
pub fn buttons(r: Rect) -> Vec<(usize, usize, usize, usize)> {
    let row = r.y + r.h - 2;
    let w = |s: &str| nagi_core::display_col(s, s.len(), false) + 4;
    let cancel = (1, row, r.x + r.w - 2 - w("キャンセル"), r.x + r.w - 2);
    let push = (0, row, cancel.2 - 1 - w("プッシュ"), cancel.2 - 1);
    let force = (2, row, r.x + 2, r.x + 2 + w("強制プッシュ"));
    vec![force, push, cancel]
}

impl Editor {
    /// 送るコミット（`log`）と上流（`up`。無ければ origin に作る）を見せるダイアログを開く
    pub(crate) fn open_push_dialog(&mut self, up: Option<String>, log: Vec<String>) {
        let branch = self.svc.git.status.as_ref().map(|s| s.branch.clone()).unwrap_or_default();
        let target = match &up {
            Some(u) => match u.split_once('/') {
                Some((remote, b)) => format!("{branch} → {remote} : {b}"),
                None => format!("{branch} → {u}"),
            },
            None => format!("{branch} → origin : {branch}（新しく作る）"),
        };
        self.panels.push = Some(PushDialog { target, new_upstream: up.is_none(), commits: log, selected: 0, scroll: 0, files: Vec::new(), files_for: None, button: 0 });
        self.set_state(State::Push);
        self.push_load_files();
    }

    /// 選んでいるコミットで変えたファイルを裏で読む
    fn push_load_files(&mut self) {
        let (Some(root), Some(d)) = (self.svc.git.root.clone(), &mut self.panels.push) else { return };
        let Some(hash) = d.commits.get(d.selected).and_then(|c| c.split_whitespace().next()).map(String::from) else { return };
        if d.files_for.as_deref() == Some(hash.as_str()) {
            return;
        }
        d.files_for = Some(hash.clone());
        d.files.clear();
        let h = hash.clone();
        self.job(
            "git-push-files",
            move || nagi_workers::git::git(&root, &["show", "--name-status", "--format=", &h]).map(|b| String::from_utf8_lossy(&b).lines().filter(|l| !l.is_empty()).map(String::from).collect::<Vec<_>>()).unwrap_or_default(),
            move |e, files| {
                // 届くまでに選びが変わっていたら捨てる
                if let Some(d) = e.panels.push.as_mut().filter(|d| d.files_for.as_deref() == Some(hash.as_str())) {
                    d.files = files;
                }
            },
        );
    }

    fn close_push_dialog(&mut self) {
        self.panels.push = None;
        self.set_state(State::Normal);
        self.svc.notify.info("プッシュをやめました");
    }

    /// プッシュ（`force` なら確かめてから --force-with-lease）
    fn push_accept(&mut self, force: bool) {
        let Some(d) = self.panels.push.take() else { return };
        self.set_state(State::Normal);
        let new_upstream = d.new_upstream;
        if force {
            let to = d.target.split(" → ").nth(1).unwrap_or("").to_string();
            self.ask_or(
                format!("{to} への強制プッシュを実行しようとしています。この操作により、リモートのコミットが上書きされる可能性があります。続行しますか？"),
                move |e| e.push(new_upstream, true),
                |e| e.svc.notify.info("プッシュをやめました"),
            );
        } else {
            self.push(new_upstream, false);
        }
    }

    fn push_press(&mut self, button: usize) {
        match button {
            0 => self.push_accept(false),
            2 => self.push_accept(true),
            _ => self.close_push_dialog(),
        }
    }

    pub(crate) fn push_dialog_key(&mut self, key: Key) {
        let Some(d) = &mut self.panels.push else {
            self.set_state(State::Normal);
            return;
        };
        let h = list_rows(rect(self.screen.0, self.screen.1)).len().max(1);
        if let Some(i) = nagi_ui::list::nav(key, d.selected, d.commits.len(), h) {
            d.selected = i;
            nagi_ui::list::fit(d.selected, &mut d.scroll, h);
            self.push_load_files();
            return;
        }
        match key {
            Key::Esc | Key::Ctrl('c') => self.close_push_dialog(),
            Key::Enter => {
                let b = d.button;
                self.push_press(b);
            }
            Key::Tab => d.button = (d.button + 1) % 3,
            _ => {}
        }
    }

    pub(crate) fn push_dialog_click(&mut self, row: usize, col: usize, clicks: u32) {
        let r = rect(self.screen.0, self.screen.1);
        if let Some(b) = buttons(r).into_iter().find(|b| b.1 == row && col >= b.2 && col < b.3).map(|b| b.0) {
            self.push_press(b);
            return;
        }
        // 左のコミットの一覧
        let Some(d) = &mut self.panels.push else { return };
        let rows = list_rows(r);
        if row >= r.y && row < r.y + rows.end && col < r.x + r.w / 2 && nagi_ui::list::pick_row(&mut d.selected, d.scroll, row - r.y, rows.start, d.commits.len(), clicks).is_some() {
            self.push_load_files();
        }
    }
}

/// この部品の入口（component.rs）
pub struct PushComponent;

impl crate::component::Component for PushComponent {
    fn key(&self, ed: &mut Editor, key: Key) -> Effect {
        ed.push_dialog_key(key);
        Effect::None
    }

    fn click(&self, ed: &mut Editor, row: usize, col: usize, clicks: u32) -> bool {
        ed.push_dialog_click(row, col, clicks);
        true
    }

    fn status(&self, ed: &Editor) -> Option<crate::component::Status> {
        let _ = ed;
        crate::component::Status::new("-- プッシュ --", "↑↓ コミット  Enter プッシュ  Tab ボタン  Esc やめる ")
    }
}

/// 描き方（layout.rs の View。画面全体を覆う）
pub struct PushView;

impl crate::layout::View for PushView {
    fn rect(&self, ed: &Editor, s: crate::layout::Screen) -> Option<Rect> {
        ed.panels.push.as_ref()?;
        Some(Rect { x: 0, y: 0, w: s.cols, h: s.rows })
    }

    fn draw(&self, ed: &Editor, p: &mut crate::view::Painter, r: Rect, s: crate::layout::Screen) {
        let _ = r;
        if let Some(d) = &ed.panels.push {
            draw::draw_push(p, d, s.cols, s.rows);
        }
    }
}

pub(crate) mod draw {
    use super::*;
    use crate::view::*;

    pub(crate) fn draw_push(p: &mut Painter, d: &PushDialog, cols: usize, rows: usize) {
        let cell = p.fonts.cell();
        let r = rect(cols, rows);
        let (x0, y0, w, h) = (r.x as f32 * cell.w, r.y as f32 * cell.h, r.w as f32 * cell.w, r.h as f32 * cell.h);
        popup_frame(p, x0, y0, w, h);
        let saved = p.cols;
        p.cols = r.x + r.w - 1;
        let y = |k: usize| (r.y + k) as f32 * cell.h;
        p.text(" コミットのプッシュ", r.x, y(0), FG_BRIGHT);
        p.text(&format!("  {}   {} 件", d.target, d.commits.len()), r.x, y(1), FG);
        // 左: コミット、右: 変えたファイル（間に 1px の線）
        let half = r.w / 2;
        let lr = list_rows(r);
        p.out.push(Instance::fill((r.x + half) as f32 * cell.w, y(lr.start), 1.0, (lr.len()) as f32 * cell.h, SEPARATOR));
        p.cols = r.x + half - 1;
        for (k, c) in d.commits.iter().enumerate().skip(d.scroll).take(lr.len()) {
            let yy = y(lr.start + k - d.scroll);
            if k == d.selected {
                selection(p, x0, yy, half as f32 * cell.w, cell.h, LIST_SEL_BG);
            }
            let (hash, subject) = c.split_once(' ').unwrap_or((c, ""));
            // 件名は右のハッシュの手前で切る
            let hw = nagi_core::display_col(hash, hash.len(), false);
            let hx = (r.x + half).saturating_sub(hw + 2);
            p.cols = hx.saturating_sub(1);
            p.text(&format!("  {subject}"), r.x, yy, FG);
            p.cols = r.x + half - 1;
            p.text(hash, hx, yy, STATUS_FG);
        }
        if d.commits.is_empty() {
            p.text("  送るコミットはありません", r.x, y(lr.start), STATUS_FG);
        }
        p.cols = r.x + r.w - 1;
        for (k, f) in d.files.iter().take(lr.len()).enumerate() {
            let (st, path) = f.split_once('\t').unwrap_or(("", f));
            let color = match st.chars().next() {
                Some('A') => GIT_ADD,
                Some('D') => GIT_DEL,
                Some('M' | 'R') => GIT_MOD,
                _ => FG,
            };
            p.text(&format!("  {}", path.replace('\t', " → ")), r.x + half + 1, y(lr.start + k), color);
        }
        for (i, row, c0, c1) in buttons(r) {
            let label = BUTTONS.iter().find(|b| b.0 == i).map_or("", |b| b.1);
            let on = i == d.button;
            p.out.push(Instance::rounded(c0 as f32 * cell.w, row as f32 * cell.h + crate::metrics::pad(), (c1 - c0) as f32 * cell.w, cell.h - 2.0 * crate::metrics::pad(), crate::metrics::radius(crate::metrics::Radius::Control), if on { ACCENT_FG } else { RAISED_BG }));
            let lw = p.width(label);
            p.text(label, c0 + (c1 - c0).saturating_sub(lw) / 2, row as f32 * cell.h, if on { FG_BRIGHT } else if i == 2 { DIAG_WARN } else { FG });
        }
        p.cols = saved;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    /// プッシュ: 送るコミットとファイルをダイアログで見せ、プッシュのボタン（Enter）で送る。Esc ならやめる
    #[test]
    fn shows_commits_and_pushes() {
        let base = std::env::temp_dir().join(format!("nagi-push-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (remote, dir) = (base.join("remote.git"), base.join("work"));
        std::fs::create_dir_all(&remote).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let git = |at: &std::path::Path, args: &[&str]| String::from_utf8_lossy(&nagi_workers::git::git(at, args).unwrap()).into_owned();
        git(&remote, &["init", "-q", "--bare", "-b", "main"]);
        git(&dir, &["init", "-q", "-b", "main"]);
        git(&dir, &["config", "user.email", "test@example.com"]);
        git(&dir, &["config", "user.name", "test"]);
        git(&dir, &["remote", "add", "origin", &remote.to_string_lossy()]);
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "first"]);
        git(&dir, &["push", "-q", "-u", "origin", "main"]);
        for (f, m) in [("b.txt", "add b"), ("c.txt", "add c")] {
            std::fs::write(dir.join(f), "x\n").unwrap();
            git(&dir, &["add", "."]);
            git(&dir, &["commit", "-q", "-m", m]);
        }
        let mut buf = Buffer::open(&dir.join("a.txt")).unwrap();
        buf.path = Some(dir.join("a.txt"));
        let mut e = Editor::new(buf, None);
        e.set_screen(160, 40);
        e.start_git();
        let settle = |e: &mut Editor| {
            for _ in 0..2000 {
                e.poll_git();
                e.poll_commit();
                e.wait_jobs();
                if e.svc.git.status.is_some() && !e.git_status_pending() && e.panels.commit.as_ref().is_none_or(|p| !p.busy()) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            panic!("git が終わらない");
        };
        settle(&mut e);
        e.push_from_anywhere();
        settle(&mut e);
        assert_eq!(e.state(), State::Push);
        let d = e.panels.push.as_ref().unwrap();
        assert_eq!(d.target, "main → origin : main");
        assert_eq!(d.commits.iter().map(|c| c.split_once(' ').unwrap().1).collect::<Vec<_>>(), ["add c", "add b"]);
        assert_eq!(d.files, ["A\tc.txt"], "選んだコミットで変えたファイル");
        // ↓ で次のコミット（ファイルも替わる）
        e.key(Key::Down);
        e.wait_jobs();
        assert_eq!(e.panels.push.as_ref().unwrap().files, ["A\tb.txt"]);
        // Esc でやめる
        e.key(Key::Esc);
        assert!(e.panels.push.is_none() && e.state() == State::Normal);
        // もう一度開いて、プッシュのボタンを押す
        e.push_from_anywhere();
        settle(&mut e);
        let r = rect(160, 40);
        let b = buttons(r).into_iter().find(|b| b.0 == 0).unwrap();
        e.push_dialog_click(b.1, b.2 + 1, 1);
        settle(&mut e);
        assert!(e.svc.notify.text.starts_with("プッシュしました"), "{}", e.svc.notify.text);
        assert_eq!(git(&remote, &["log", "--format=%s", "main"]).lines().collect::<Vec<_>>(), ["add c", "add b", "first"]);
        let _ = std::fs::remove_dir_all(&base);
    }
}
