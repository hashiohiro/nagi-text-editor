//! git（`git` コマンドを呼ぶ）。gix の代わりに Windows の git を使うのは、コミット・プッシュ・blame・グラフ付きの
//! ログ・パッチの適用まで同じ道具で確実にそろうため。呼ぶのは裏のスレッドからだけ（UI スレッドでは呼ばない）。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// git を `root` で動かし、標準出力を返す。失敗したら標準エラー（無ければ終了コード）
pub fn git(root: &Path, args: &[&str]) -> nagi_core::Result<Vec<u8>> {
    git_input(root, args, None)
}

/// 標準入力を渡して git を動かす（パッチの適用・コミットのメッセージ）
pub fn git_input(root: &Path, args: &[&str], input: Option<&[u8]>) -> nagi_core::Result<Vec<u8>> {
    // 出力を英語と UTF-8 の決まった形にする（読み取りがずれないように）
    let base = ["-c", "core.quotepath=false", "-c", "color.ui=false"];
    // WSL のプロジェクトは WSL の中の git で（Windows の git だと遅く、safe.directory で断られる）。設定で切り替えられる
    let cfg = crate::config::get();
    let ex = if cfg.git_wsl { crate::exec::Exec::in_dir(root) } else { crate::exec::Exec::windows(root) };
    let ex = ex.env("LANG", "C").env("LC_ALL", "C").env("GIT_TERMINAL_PROMPT", "0");
    // 設定の「git の場所」（Windows のときだけ）
    let git = if ex.is_wsl() { "git" } else { cfg.git.as_deref().unwrap_or("git") };
    let mut cmd = ex.program(git, &[base.as_slice(), args].concat());
    if input.is_some() {
        cmd.stdin(Stdio::piped());
    }
    let mut child = cmd.spawn().map_err(|e| format!("git を起動できません: {e}"))?;
    if let (Some(data), Some(mut si)) = (input, child.stdin.take()) {
        let _ = si.write_all(data);
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() { format!("git が失敗しました（{}）", out.status).into() } else { err.into() })
    }
}

fn text(b: Vec<u8>) -> String {
    String::from_utf8_lossy(&b).into_owned()
}

/// 作業ツリーの根（`.git` のある所）
pub fn repo_root(path: &Path) -> Option<PathBuf> {
    crate::files::project_root(path)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileState {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
}

impl FileState {
    pub fn letter(self) -> char {
        match self {
            FileState::Modified => 'M',
            FileState::Added => 'A',
            FileState::Deleted => 'D',
            FileState::Renamed => 'R',
            FileState::Untracked => '?',
            FileState::Conflicted => 'U',
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// 根からの相対パス（`/` 区切り）
    pub path: String,
    pub state: FileState,
    /// ステージ済みの変更がある
    pub staged: bool,
    /// 作業ツリーに（ステージしていない）変更がある
    pub unstaged: bool,
    /// 名前を変えたときの元の名前
    pub orig: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub branch: String,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub changes: Vec<Change>,
}

/// `git status --porcelain=v2 --branch -z` を読む
pub fn parse_status(out: &str) -> Status {
    let mut st = Status::default();
    let mut it = out.split('\0').peekable();
    while let Some(rec) = it.next() {
        if rec.is_empty() {
            continue;
        }
        if let Some(h) = rec.strip_prefix("# ") {
            if let Some(b) = h.strip_prefix("branch.head ") {
                st.branch = b.to_string();
            } else if let Some(u) = h.strip_prefix("branch.upstream ") {
                st.upstream = Some(u.to_string());
            } else if let Some(ab) = h.strip_prefix("branch.ab ") {
                let mut p = ab.split(' ');
                st.ahead = p.next().and_then(|a| a.trim_start_matches('+').parse().ok()).unwrap_or(0);
                st.behind = p.next().and_then(|b| b.trim_start_matches('-').parse().ok()).unwrap_or(0);
            }
            continue;
        }
        let f: Vec<&str> = rec.splitn(if rec.starts_with('2') { 10 } else if rec.starts_with('u') { 11 } else { 9 }, ' ').collect();
        match f.first().copied() {
            Some("1") | Some("2") if f.len() >= 9 => {
                let xy: Vec<char> = f[1].chars().collect();
                let (x, y) = (xy[0], xy[1]);
                let path = f.last().unwrap().to_string();
                let renamed = f[0] == "2";
                let orig = if renamed { it.next().map(String::from) } else { None };
                let state = if renamed {
                    FileState::Renamed
                } else if x == 'A' {
                    FileState::Added
                } else if x == 'D' || y == 'D' {
                    FileState::Deleted
                } else {
                    FileState::Modified
                };
                st.changes.push(Change { path, state, staged: x != '.', unstaged: y != '.', orig });
            }
            Some("u") => {
                let path = f.last().unwrap().to_string();
                st.changes.push(Change { path, state: FileState::Conflicted, staged: false, unstaged: true, orig: None });
            }
            Some("?") => {
                let path = rec[2..].to_string();
                st.changes.push(Change { path, state: FileState::Untracked, staged: false, unstaged: true, orig: None });
            }
            _ => {}
        }
    }
    st
}

pub fn status(root: &Path) -> nagi_core::Result<Status> {
    git(root, &["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all"]).map(|b| parse_status(&text(b)))
}

/// HEAD の版のファイルの中身（HEAD に無ければ None）
pub fn head_blob(root: &Path, rel: &str) -> Option<String> {
    git(root, &["show", &format!("HEAD:{rel}")]).ok().map(text)
}

/// ある版のファイルの中身（`rev` は `HEAD` やハッシュ。無ければ None）
pub fn blob_at(root: &Path, rev: &str, rel: &str) -> Option<String> {
    git(root, &["show", &format!("{rev}:{rel}")]).ok().map(text)
}

/// パッチを index に当てる（変更の単位のステージ）
pub fn apply_cached(root: &Path, patch: &str) -> nagi_core::Result<()> {
    git_input(root, &["apply", "--cached", "--unidiff-zero", "-"], Some(patch.as_bytes())).map(|_| ())
}

/// `rel` の部分 `old_lines` を `new_lines` に変えるパッチ（変更の単位のステージ用。前後の文脈は付けない）
pub fn hunk_patch(rel: &str, old_start: usize, old_lines: &[&str], new_start: usize, new_lines: &[&str], new_file: bool) -> String {
    let mut p = String::new();
    p.push_str(&format!("diff --git a/{rel} b/{rel}\n"));
    if new_file {
        p.push_str("new file mode 100644\n--- /dev/null\n");
    } else {
        p.push_str(&format!("--- a/{rel}\n"));
    }
    p.push_str(&format!("+++ b/{rel}\n"));
    // 0 行のときの開始行は、その手前の行（unified diff の決まり）
    let os = if old_lines.is_empty() { old_start } else { old_start + 1 };
    let ns = if new_lines.is_empty() { new_start } else { new_start + 1 };
    p.push_str(&format!("@@ -{os},{} +{ns},{} @@\n", old_lines.len(), new_lines.len()));
    for l in old_lines {
        p.push_str(&format!("-{l}\n"));
    }
    for l in new_lines {
        p.push_str(&format!("+{l}\n"));
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_parse() {
        let out = "# branch.oid abc\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -1\0\
1 .M N... 100644 100644 100644 a a src/a.rs\0\
1 A. N... 000000 100644 100644 0 b src/new.rs\0\
1 .D N... 100644 100644 000000 c c gone.rs\0\
2 R. N... 100644 100644 100644 d d R100 renamed.rs\0old.rs\0\
? memo.md\0";
        let s = parse_status(out);
        assert_eq!((s.branch.as_str(), s.upstream.as_deref(), s.ahead, s.behind), ("main", Some("origin/main"), 2, 1));
        let v: Vec<(&str, FileState, bool, bool)> = s.changes.iter().map(|c| (c.path.as_str(), c.state, c.staged, c.unstaged)).collect();
        assert_eq!(
            v,
            vec![
                ("src/a.rs", FileState::Modified, false, true),
                ("src/new.rs", FileState::Added, true, false),
                ("gone.rs", FileState::Deleted, false, true),
                ("renamed.rs", FileState::Renamed, true, false),
                ("memo.md", FileState::Untracked, false, true),
            ]
        );
        assert_eq!(s.changes[3].orig.as_deref(), Some("old.rs"));
    }

    /// 一時フォルダに本物のリポジトリを作って、status・HEAD の中身・変更の単位のステージを確かめる
    #[test]
    fn real_repo() {
        let dir = std::env::temp_dir().join(format!("nagi-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| git(&dir, args).unwrap();
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "test@example.com"]);
        g(&["config", "user.name", "test"]);
        std::fs::write(dir.join("a.txt"), "1\n2\n3\n").unwrap();
        g(&["add", "a.txt"]);
        g(&["commit", "-q", "-m", "first"]);
        std::fs::write(dir.join("a.txt"), "1\nTWO\n3\n4\n").unwrap();
        std::fs::write(dir.join("b.txt"), "new\n").unwrap();
        let s = status(&dir).unwrap();
        assert_eq!(s.branch, "main");
        assert_eq!(s.changes.len(), 2);
        assert_eq!(head_blob(&dir, "a.txt").as_deref(), Some("1\n2\n3\n"));
        assert_eq!(head_blob(&dir, "b.txt"), None);
        // 2 行目の変更だけをステージする
        apply_cached(&dir, &hunk_patch("a.txt", 1, &["2"], 1, &["TWO"], false)).unwrap();
        let staged = text(g(&["diff", "--cached"]));
        assert!(staged.contains("-2\n+TWO"), "{staged}");
        assert!(!staged.contains("+4"), "4 行目はステージしない");
        // 足しただけの変更（0 行の置き換え）もステージできる
        apply_cached(&dir, &hunk_patch("a.txt", 3, &[], 3, &["4"], false)).unwrap();
        assert!(text(g(&["diff", "--cached"])).contains("+4"));
    }
}
