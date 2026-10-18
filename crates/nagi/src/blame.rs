//! git の画面の 4 段目: blame（行ごとに最後に変えたコミット）。
//!
//! `git blame --porcelain --contents -` にバッファの中身を渡すので、保存していない変更の行も合う（「未コミット」）。
//! 作業中の窓の行番号の左に欄を足し、コミットが変わる行にだけ「ハッシュ・いつ・誰」を出す。新しいほど明るい文字。
//! カーソルの行のコミットは、メッセージ付きで枠に出す。保存したときに作り直す。

use std::collections::HashMap;
use std::path::PathBuf;

use crate::editor::Editor;

/// blame の欄の幅（セル）
pub const BLAME_W: usize = 30;

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
    rx: Option<nagi_core::task::Task<Result<(Vec<usize>, Vec<CommitInfo>), String>>>,
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

pub fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

impl Editor {
    /// 作業中の窓に blame を出しているか
    pub fn blame_active(&self) -> bool {
        self.blame.as_ref().is_some_and(|b| self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok()).as_ref() == Some(&b.path))
    }

    /// 作業中の窓の、行番号と blame の欄の幅
    pub fn gutter(&self) -> usize {
        crate::editor::GUTTER + if self.blame_active() { BLAME_W } else { 0 }
    }

    /// `:Gblame`（Annotate）: 出す・消す
    pub(crate) fn toggle_blame(&mut self) {
        if self.blame_active() {
            self.blame = None;
            return;
        }
        self.load_blame();
    }

    /// 今のバッファの blame を裏で作る
    pub(crate) fn load_blame(&mut self) {
        if self.git.root.is_none() {
            self.start_git();
        }
        let (Some(root), Some(path)) = (self.git.root.clone(), self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok())) else {
            self.notify.info("git のリポジトリのファイルではありません");
            return;
        };
        let Ok(rel) = path.strip_prefix(&root) else { return };
        let rel = rel.to_string_lossy().replace('\\', "/");
        let text = self.buf.text();
        let rx = nagi_core::task::spawn("git-blame", self.waker(), move |ctx| {
            let r = nagi_workers::git::git_input(&root, &["blame", "--porcelain", "--contents", "-", "--", &rel], Some(text.as_bytes())).map(|b| parse_porcelain(&String::from_utf8_lossy(&b)));
            ctx.send(r);
        });
        let keep = self.blame.take().filter(|b| b.path == path);
        let (lines, commits) = keep.map(|b| (b.lines, b.commits)).unwrap_or_default();
        self.blame = Some(Blame { path, lines, commits, rx: Some(rx) });
    }

    /// 裏で作った blame を受け取る。変わったら true
    pub fn poll_blame(&mut self) -> bool {
        let Some(b) = &mut self.blame else { return false };
        let Some(rx) = &b.rx else { return false };
        let Ok(r) = rx.try_recv() else { return false };
        b.rx = None;
        match r {
            Ok((lines, commits)) => {
                b.lines = lines;
                b.commits = commits;
            }
            Err(e) => {
                self.notify.info(format!("blame: {}", e.lines().next().unwrap_or("")));
                self.blame = None;
            }
        }
        true
    }

    /// 描く用: 行 `l` の blame（コミットが変わる行だけ文字を出す）。(文字, 色の明るさ 0〜3, 未コミットか)
    pub fn blame_cell(&self, l: usize, now: i64) -> Option<(String, u8, bool)> {
        let b = self.blame.as_ref()?;
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
        let text = if c.uncommitted() {
            "         未コミット".to_string()
        } else {
            let name: String = c.author.chars().take(10).collect();
            format!("{}  {:<8}  {name}", &c.hash[..7], relative_time(c.time, now))
        };
        Some((text, level, c.uncommitted()))
    }

    /// カーソルの行のコミット（枠に出す）
    pub fn blame_at_cursor(&self) -> Option<&CommitInfo> {
        let b = self.blame.as_ref()?;
        b.lines.get(self.cursor.line).map(|&i| &b.commits[i])
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
        if ed.cursor.line < ed.top {
            return;
        }
        let row = rect.y + (ed.cursor.line - ed.top) + 1;
        let lines = [
            format!(" {}   {}   {}", &c.hash[..7], c.author, crate::blame::relative_time(c.time, crate::blame::now_secs())),
            format!(" {}", c.summary),
            " :Gblame で閉じる".to_string(),
        ];
        let h = lines.len();
        let top = if row + h <= rows { row } else { row.saturating_sub(h + 1) };
        let x0 = rect.x + crate::blame::BLAME_W + 2;
        let w = lines.iter().map(|l| p.width(l)).max().unwrap_or(10) + 2;
        let w = w.min(rect.w.saturating_sub(crate::blame::BLAME_W + 3));
        let (x, y) = (x0 as f32 * cell.w, top as f32 * cell.h);
        p.out.push(Instance::fill(x - 1.0, y - 1.0, w as f32 * cell.w + 2.0, h as f32 * cell.h + 2.0, STATUS_FG));
        p.out.push(Instance::fill(x, y, w as f32 * cell.w, h as f32 * cell.h, rgb(0x141414)));
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
