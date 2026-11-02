//! git の画面の 4 段目: blame（行ごとに最後に変えたコミット）。
//!
//! `git blame --porcelain --contents -` にバッファの中身を渡すので、保存していない変更の行も合う（「未コミット」）。
//! 作業中の窓の行番号の左に欄を足し、コミットが変わる行にだけ「ハッシュ・いつ・誰」を出す。新しいほど明るい文字。
//! カーソルの行のコミットは、メッセージ付きで枠に出す。保存したときに作り直す。

use std::collections::HashMap;
use std::path::PathBuf;

use crate::editor::Editor;

/// blame の欄の幅（セル）の下限・上限。中身に合わせて決め（IntelliJ の注釈と同じ）、右の線のドラッグで変えられる（大きさの表の blame_width）
const BLAME_MIN: usize = 10;
const BLAME_MAX: usize = 80;

/// 表示の幅（全角は 2）
fn width(s: &str) -> usize {
    nagi_core::display_col(s, s.len(), false)
}

/// 表示の幅で右に空白を足す
fn pad(s: &str, w: usize) -> String {
    format!("{s}{}", " ".repeat(w.saturating_sub(width(s))))
}

#[derive(Clone, Debug, Default)]
pub struct CommitInfo {
    pub hash: String,
    pub author: String,
    pub time: i64,
    pub summary: String,
}

impl CommitInfo {
    pub fn uncommitted(&self) -> bool {
        self.hash.starts_with("0000000")
    }
}

pub struct Blame {
    pub path: PathBuf,
    /// 行ごとのコミット（commits の番号）
    pub lines: Vec<usize>,
    pub commits: Vec<CommitInfo>,
    rx: Option<nagi_core::task::Task<nagi_core::Result<(Vec<usize>, Vec<CommitInfo>)>>>,
    /// 中身に合わせた欄の幅（読み終えたときに決める）
    pub width: usize,
}

/// `git blame --porcelain` を読む
pub fn parse_porcelain(out: &str) -> (Vec<usize>, Vec<CommitInfo>) {
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut commits: Vec<CommitInfo> = Vec::new();
    let mut lines: Vec<(usize, usize)> = Vec::new();
    let mut cur: Option<usize> = None;
    for l in out.lines() {
        if let Some(rest) = l.strip_prefix('\t') {
            let _ = rest;
            continue;
        }
        let mut parts = l.split(' ');
        let first = parts.next().unwrap_or("");
        if first.len() == 40 && first.chars().all(|c| c.is_ascii_hexdigit()) {
            let _orig = parts.next();
            let fin: usize = parts.next().and_then(|n| n.parse().ok()).unwrap_or(1);
            let id = *index.entry(first.to_string()).or_insert_with(|| {
                commits.push(CommitInfo { hash: first.to_string(), ..Default::default() });
                commits.len() - 1
            });
            lines.push((fin - 1, id));
            cur = Some(id);
            continue;
        }
        let Some(c) = cur else { continue };
        let val = l.split_once(' ').map_or("", |(_, v)| v);
        match first {
            "author" => commits[c].author = val.to_string(),
            "author-time" => commits[c].time = val.parse().unwrap_or(0),
            "summary" => commits[c].summary = val.to_string(),
            _ => {}
        }
    }
    let n = lines.iter().map(|(l, _)| l + 1).max().unwrap_or(0);
    let mut out = vec![0; n];
    for (l, id) in lines {
        out[l] = id;
    }
    (out, commits)
}

/// 「3 日前」の形（`now` も `t` も UNIX 時刻の秒）
pub fn relative_time(t: i64, now: i64) -> String {
    let d = (now - t).max(0);
    match d {
        0..60 => "たった今".into(),
        60..3600 => format!("{} 分前", d / 60),
        3600..86400 => format!("{} 時間前", d / 3600),
        86400..604800 => format!("{} 日前", d / 86400),
        604800..2592000 => format!("{} 週間前", d / 604800),
        2592000..31536000 => format!("{} か月前", d / 2592000),
        _ => format!("{} 年前", d / 31536000),
    }
}

/// 欄の 1 行（ハッシュ・いつ・誰。いつは表示の幅でそろえる）
fn label(c: &CommitInfo, now: i64) -> String {
    if c.uncommitted() {
        return format!("{}未コミット", " ".repeat(9));
    }
    let name: String = c.author.chars().take(16).collect();
    format!("{}  {}  {name}", &c.hash[..7.min(c.hash.len())], pad(&relative_time(c.time, now), 9))
}

/// 中身に合わせた欄の幅（前の空白 1 と右の線の前の 1 を足す）。まだ読んでいなければ 30
fn keep_width(lines: &[usize], commits: &[CommitInfo]) -> usize {
    if lines.is_empty() {
        return 30;
    }
    let now = now_secs();
    commits.iter().map(|c| width(&label(c, now)) + 2).max().unwrap_or(30).clamp(BLAME_MIN, BLAME_MAX)
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

impl Editor {
    /// 作業中の窓に blame を出しているか
    pub fn blame_active(&self) -> bool {
        self.panels.blame.as_ref().is_some_and(|b| self.core.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok()).as_ref() == Some(&b.path))
    }

    /// 作業中の窓の、行番号と blame の欄の幅
    pub fn gutter(&self) -> usize {
        crate::editor::GUTTER + if self.blame_active() { self.blame_width() } else { 0 }
    }

    /// blame の欄の幅（ドラッグで変えていればその幅、でなければ中身に合わせた幅）
    pub fn blame_width(&self) -> usize {
        let auto = self.panels.blame.as_ref().map_or(30, |b| b.width);
        self.svc.sizes.get("blame_width").unwrap_or(auto).clamp(BLAME_MIN, BLAME_MAX)
    }

    /// `:Gblame`（Annotate）: 出す・消す
    pub(crate) fn toggle_blame(&mut self) {
        if self.blame_active() {
            self.panels.blame = None;
            return;
        }
        self.load_blame();
    }

    /// 今のバッファの blame を裏で作る
    pub(crate) fn load_blame(&mut self) {
        if self.svc.git.root.is_none() {
            self.start_git();
        }
        let (Some(root), Some(path)) = (self.svc.git.root.clone(), self.core.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok())) else {
            self.svc.notify.info("git のリポジトリのファイルではありません");
            return;
        };
        let Ok(rel) = path.strip_prefix(&root) else { return };
        let rel = rel.to_string_lossy().replace('\\', "/");
        let text = self.core.buf.text();
        let rx = nagi_core::task::spawn("git-blame", self.waker(), move |ctx| {
            let r = nagi_workers::git::git_input(&root, &["blame", "--porcelain", "--contents", "-", "--", &rel], Some(text.as_bytes())).map(|b| parse_porcelain(&String::from_utf8_lossy(&b)));
            ctx.send(r);
        });
        let keep = self.panels.blame.take().filter(|b| b.path == path);
        let (lines, commits) = keep.map(|b| (b.lines, b.commits)).unwrap_or_default();
        let width = keep_width(&lines, &commits);
        self.panels.blame = Some(Blame { path, lines, commits, rx: Some(rx), width });
    }

    /// 裏で作った blame を受け取る。変わったら true
    pub fn poll_blame(&mut self) -> bool {
        let Some(b) = &mut self.panels.blame else { return false };
        let Some(rx) = &b.rx else { return false };
        let Ok(r) = rx.try_recv() else { return false };
        b.rx = None;
        match r {
            Ok((lines, commits)) => {
                b.width = keep_width(&lines, &commits);
                b.lines = lines;
                b.commits = commits;
            }
            Err(e) => {
                self.svc.notify.info(format!("blame: {}", e.first_line()));
                self.panels.blame = None;
            }
        }
        true
    }

    /// 描く用: 行 `l` の blame（コミットが変わる行だけ文字を出す）。(文字, 色の明るさ 0〜3, 未コミットか)
    pub fn blame_cell(&self, l: usize, now: i64) -> Option<(String, u8, bool)> {
        let b = self.panels.blame.as_ref()?;
        let &id = b.lines.get(l)?;
        let c = &b.commits[id];
        let age = now - c.time;
        let level = if c.uncommitted() || age < 7 * 86400 {
            3
        } else if age < 30 * 86400 {
            2
        } else if age < 365 * 86400 {
            1
        } else {
            0
        };
        let first = l == 0 || b.lines.get(l - 1) != Some(&id);
        if !first {
            return Some((String::new(), level, c.uncommitted()));
        }
        Some((label(c, now), level, c.uncommitted()))
    }

    /// カーソルの行のコミット（枠に出す）
    pub fn blame_at_cursor(&self) -> Option<&CommitInfo> {
        let b = self.panels.blame.as_ref()?;
        b.lines.get(self.core.cursor.line).map(|&i| &b.commits[i])
    }
}



/// 行のコミットの小窓（layout.rs の View）
pub struct BlamePopupView;

impl crate::layout::View for BlamePopupView {
    fn rect(&self, ed: &crate::editor::Editor, s: crate::layout::Screen) -> Option<crate::panes::Rect> {
        let _ = s;
        (ed.svc.git.popup.is_none() && ed.blame_active() && ed.idle()).then_some(crate::panes::Rect { x: 0, y: 0, w: s.cols, h: s.rows })
    }

    fn draw(&self, ed: &crate::editor::Editor, p: &mut crate::view::Painter, r: crate::panes::Rect, s: crate::layout::Screen) {
        let _ = (r, s);
        draw::draw_blame_popup(p, ed, s.rows);
    }
}

/// 描き方（view.rs の Painter で描く）
pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    /// blame: カーソルの行のコミットの詳しい内容（カーソルの行の下に枠で）
    pub(crate) fn draw_blame_popup(p: &mut Painter, ed: &Editor, rows: usize) {
        let Some(c) = ed.blame_at_cursor() else { return };
        if c.uncommitted() {
            return;
        }
        let cell = p.fonts.cell();
        let rect = ed.active_rect();
        if ed.core.cursor.line < ed.core.top {
            return;
        }
        let row = rect.y + (ed.core.cursor.line - ed.core.top) + 1;
        let lines = [
            format!(" {}   {}   {}", &c.hash[..7], c.author, crate::blame::relative_time(c.time, crate::blame::now_secs())),
            format!(" {}", c.summary),
            " :Gblame で閉じる".to_string(),
        ];
        let h = lines.len();
        let top = if row + h <= rows { row } else { row.saturating_sub(h + 1) };
        let bw = ed.blame_width();
        let x0 = rect.x + bw + 2;
        let w = lines.iter().map(|l| p.width(l)).max().unwrap_or(10) + 2;
        let w = w.min(rect.w.saturating_sub(bw + 3));
        let (x, y) = (x0 as f32 * cell.w, top as f32 * cell.h);
        popup_frame(p, x, y, w as f32 * cell.w, h as f32 * cell.h);
        let saved = p.cols;
        p.cols = x0 + w;
        for (k, l) in lines.iter().enumerate() {
            let color = if k == 1 { rgb(0xe8e8e8) } else { STATUS_FG };
            p.text(l, x0, y + k as f32 * cell.h, color);
        }
        p.cols = saved;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 欄の幅は中身（日本語の名前・「10 か月前」も）に合わせ、右の線のドラッグで変えられる。ダブルクリックで中身に合わせた幅へ戻る
    #[test]
    fn width_fits_and_drags() {
        let dir = std::env::temp_dir().join(format!("nagi-blame-w-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        std::fs::write(&path, "x\ny\n").unwrap();
        let mut buf = nagi_core::Buffer::open(&path).unwrap();
        buf.path = Some(path.clone());
        let mut e = Editor::new(buf, None);
        e.set_screen(160, 40);
        let now = now_secs();
        let commits = vec![
            CommitInfo { hash: "a".repeat(40), author: "山田 太郎（開発チーム）".into(), time: now - 300 * 86400, summary: String::new() },
            CommitInfo { hash: "b".repeat(40), author: "bob".into(), time: now - 60, summary: String::new() },
        ];
        let lines = vec![0, 1];
        let w = keep_width(&lines, &commits);
        e.panels.blame = Some(Blame { path: std::path::absolute(&path).unwrap(), lines, commits, rx: None, width: w });
        assert!(e.blame_active());
        let (t, ..) = e.blame_cell(0, now).unwrap();
        assert!(width(&t) + 2 <= e.blame_width(), "見切れない: {t:?} {}", e.blame_width());
        // 右の線をつかんで広げる・ダブルクリックで戻す
        let r = e.active_rect();
        let bw = e.blame_width();
        let sep = e.sep_at(r.y + 1, r.x + bw - 1).expect("注釈の欄の右の線");
        e.drag_sep(&sep, r.y + 1, r.x + bw + 9);
        assert_eq!(e.blame_width(), bw + 10);
        assert_eq!(e.gutter(), crate::editor::GUTTER + bw + 10);
        e.reset_sep(&sep);
        assert_eq!(e.blame_width(), bw);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn porcelain() {
        let h1 = "a".repeat(40);
        let h2 = "0".repeat(40);
        let out = format!(
            "{h1} 1 1 2\nauthor 自分\nauthor-time 1700000000\nsummary 最初\nfilename a.txt\n\tline1\n{h1} 2 2\n\tline2\n{h2} 3 3 1\nauthor Not Committed Yet\nauthor-time 1700000100\nsummary Version of a.txt from -\n\tline3\n"
        );
        let (lines, commits) = parse_porcelain(&out);
        assert_eq!(lines, vec![0, 0, 1]);
        assert_eq!((commits[0].author.as_str(), commits[0].summary.as_str(), commits[0].time), ("自分", "最初", 1700000000));
        assert!(commits[1].uncommitted());
        assert_eq!(relative_time(0, 3 * 86400), "3 日前");
        assert_eq!(relative_time(0, 70 * 86400), "2 か月前");
    }
}
