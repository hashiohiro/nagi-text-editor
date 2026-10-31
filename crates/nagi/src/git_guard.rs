//! 作業中の変更・未追跡のファイルが上書きされるので git が止めたとき（チェックアウト・マージ・チェリーピック・pull）、
//! IntelliJ と同じく「退避して続ける（スマート）・破棄して続ける（強制）・中止」を確かめてから、もう一度行う。
//! 退避・破棄するのは止めたファイルだけ（ほかの作業中の変更には触らない）

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use nagi_workers::git::{self, Blocked};

use crate::editor::Editor;

/// もう一度行える git の操作（裏のスレッドで動かす。成功したら知らせる文）
type GitOp = dyn Fn(&Path) -> nagi_core::Result<String> + Send + Sync;
type Then = Box<dyn FnOnce(&mut Editor, bool)>;

/// 止めたファイルをどうするか
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Resolve {
    Stash,
    Discard,
}

impl Editor {
    /// git の操作を裏で行い（UI スレッドでは git を動かさない。ADR 0018 の §4）、終わったら知らせて状態を読み直してから `then`（成功したか）。
    /// 上書きされるので止まったら、どうするか確かめてからもう一度裏で行う（中止したら `then` は呼ばない）
    pub(crate) fn git_guarded(&mut self, op: impl Fn(&Path) -> nagi_core::Result<String> + Send + Sync + 'static, then: impl FnOnce(&mut Editor, bool) + 'static) {
        let Some(root) = self.git.root.clone() else { return };
        let op: Arc<GitOp> = Arc::new(op);
        let (o, r) = (op.clone(), root.clone());
        self.notify.progress("git を動かしています…");
        self.job("git", move || o(&r), move |e, res| e.git_settle(root, op, res, Box::new(then)));
    }

    /// 裏で行った git の操作の結果を受け取る（上書きで止まったら、確かめてからもう一度）
    fn git_settle(&mut self, root: PathBuf, op: Arc<GitOp>, res: nagi_core::Result<String>, then: Then) {
        let Some(b) = res.as_ref().err().and_then(git::blocked) else {
            self.git_finish(res.map(|m| (m, None)), then);
            return;
        };
        self.refresh_git();
        let then = Cell::new(Some(then));
        self.ask_blocked(b, move |e, b, how| {
            let Some(t) = then.take() else { return };
            let (op, root, b) = (op.clone(), root.clone(), b.clone());
            e.notify.progress("git を動かしています…");
            e.job(
                "git",
                move || match how {
                    Resolve::Discard => git::discard(&root, &b).and_then(|_| op(&root)).map(|m| (m, None)),
                    Resolve::Stash => git::with_stash(&root, &b, || op(&root)),
                },
                move |e, r| e.git_finish(r, t),
            );
        });
    }

    fn git_finish(&mut self, r: nagi_core::Result<(String, Option<String>)>, then: Then) {
        let ok = r.is_ok();
        match r {
            Ok((m, None)) => self.notify.info(m),
            Ok((m, Some(w))) => self.notify.warn(format!("{m}。{w}")),
            Err(e) => self.notify.error(format!("git: {}", git::reason(&e))),
        }
        self.refresh_git();
        then(self, ok);
    }

    /// 上書きされるファイル `b` をどうするか確かめる（`b` が空なら、作業中の変更があるので始めなかったもの。破棄は出さない）
    pub(crate) fn ask_blocked(&mut self, b: Blocked, go: impl Fn(&mut Editor, &Blocked, Resolve) + 'static) {
        let what = match (b.changed.is_empty(), b.untracked.is_empty()) {
            (true, true) => "作業中の変更があるので止めました".to_string(),
            (false, true) => "作業中の変更が上書きされます".to_string(),
            (true, false) => "未追跡のファイルが上書きされます".to_string(),
            (false, false) => "作業中の変更と未追跡のファイルが上書きされます".to_string(),
        };
        let files: Vec<&String> = b.files().collect();
        let list = match files.len() {
            0 => String::new(),
            1..=3 => format!("（{}）", files.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("、")),
            n => format!("（{} ほか {} 件）", files[..3].iter().map(|s| s.as_str()).collect::<Vec<_>>().join("、"), n - 3),
        };
        let go = Rc::new(go);
        let b = Rc::new(b);
        let (g, x) = (go.clone(), b.clone());
        let mut m = crate::menu::Menu::new(0, 0).item_key("退避して続ける（S）", 's', move |e| g(e, &x, Resolve::Stash));
        if b.files().next().is_some() {
            let (g, x) = (go.clone(), b.clone());
            m = m.item_key("破棄して続ける（D）", 'd', move |e| g(e, &x, Resolve::Discard));
        }
        let cancel = m.items.len();
        m = m.item_key("中止（C）", 'c', |e| e.notify.info("やめました"));
        self.ask_menu(format!("{what}{list}"), m, cancel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// main の a（3 行目）を変え、未追跡の new を置いて、a と new のある side をチェックアウトする
    fn repo(name: &str) -> (std::path::PathBuf, Editor) {
        let dir = std::env::temp_dir().join(format!("nagi-guard-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| git::git(&dir, args).unwrap();
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "test@example.com"]);
        g(&["config", "user.name", "test"]);
        std::fs::write(dir.join("a"), "1\n2\n3\n").unwrap();
        std::fs::write(dir.join("other"), "1\n").unwrap();
        g(&["add", "."]);
        g(&["commit", "-q", "-m", "1"]);
        g(&["checkout", "-q", "-b", "side"]);
        std::fs::write(dir.join("a"), "S\n2\n3\n").unwrap();
        std::fs::write(dir.join("new"), "side\n").unwrap();
        g(&["add", "."]);
        g(&["commit", "-q", "-m", "2"]);
        g(&["checkout", "-q", "main"]);
        std::fs::write(dir.join("a"), "1\n2\nD\n").unwrap();
        std::fs::write(dir.join("new"), "mine\n").unwrap();
        // 止めたファイルでない変更は、どちらを選んでも残る
        std::fs::write(dir.join("other"), "keep\n").unwrap();
        let mut e = Editor::new(nagi_core::Buffer::from_str(""), None);
        e.git.root = Some(dir.clone());
        (dir, e)
    }

    fn checkout(e: &mut Editor) -> Rc<Cell<bool>> {
        let done = Rc::new(Cell::new(false));
        let d = done.clone();
        e.git_guarded(|r| git::git(r, &["checkout", "-q", "side"]).map(|_| "チェックアウトしました".into()), move |_, ok| d.set(ok));
        e.wait_jobs();
        done
    }

    fn head(dir: &Path) -> String {
        String::from_utf8_lossy(&git::git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap()).trim().to_string()
    }

    fn pick(e: &mut Editor, label: &str) {
        let i = e.menu.as_ref().expect("確かめる窓が出る").items.iter().position(|it| it.label.starts_with(label)).unwrap();
        e.menu_run(i);
        e.wait_jobs();
    }

    #[test]
    fn blocked_files() {
        let (dir, _) = repo("parse");
        let err = git::git(&dir, &["checkout", "side"]).unwrap_err();
        let b = git::blocked(&err).unwrap();
        assert_eq!((b.changed, b.untracked), (vec!["a".to_string()], vec!["new".to_string()]));
        // マージ・チェリーピックも同じ形
        let err = git::cherry_pick_many(&dir, &["side".to_string()]).unwrap_err();
        assert!(git::blocked(&err).is_some(), "{err}");
    }

    #[test]
    fn abort_keeps_everything() {
        let (dir, mut e) = repo("abort");
        let done = checkout(&mut e);
        assert!(e.menu.as_ref().unwrap().title.as_deref().unwrap().contains("作業中の変更と未追跡のファイルが上書きされます（a、new）"));
        pick(&mut e, "中止");
        assert!(!done.get());
        assert_eq!(head(&dir), "main");
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "1\n2\nD\n");
        assert_eq!(std::fs::read_to_string(dir.join("new")).unwrap(), "mine\n");
    }

    #[test]
    fn discard_and_continue() {
        let (dir, mut e) = repo("discard");
        let done = checkout(&mut e);
        pick(&mut e, "破棄して続ける");
        assert!(done.get(), "{}", e.notify.text);
        assert_eq!(head(&dir), "side");
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "S\n2\n3\n");
        assert_eq!(std::fs::read_to_string(dir.join("new")).unwrap(), "side\n");
        assert_eq!(std::fs::read_to_string(dir.join("other")).unwrap(), "keep\n", "止めたファイルでない変更は残す");
    }

    #[test]
    fn stash_and_continue() {
        let (dir, mut e) = repo("stash");
        std::fs::remove_file(dir.join("new")).unwrap();
        let done = checkout(&mut e);
        pick(&mut e, "退避して続ける");
        // a の変更は side の変更とぶつからないので、チェックアウトの後に戻る
        assert!(done.get(), "{}", e.notify.text);
        assert_eq!(head(&dir), "side");
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "S\n2\nD\n");
        assert!(git::git(&dir, &["stash", "list"]).unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(dir.join("other")).unwrap(), "keep\n");
    }

    #[test]
    fn stash_kept_when_restore_conflicts() {
        let (dir, mut e) = repo("conflict");
        let done = checkout(&mut e);
        pick(&mut e, "退避して続ける");
        // 未追跡の new は side にもあるので戻せない。チェックアウトはして、退避は残す
        assert!(done.get(), "{}", e.notify.text);
        assert_eq!(head(&dir), "side");
        assert!(e.notify.text.contains("stash@{0}"), "{}", e.notify.text);
        assert!(!git::git(&dir, &["stash", "list"]).unwrap().is_empty());
    }

    #[test]
    fn no_question_when_nothing_blocks() {
        let (dir, mut e) = repo("free");
        std::fs::remove_file(dir.join("new")).unwrap();
        git::git(&dir, &["checkout", "--", "a"]).unwrap();
        let done = checkout(&mut e);
        assert!(done.get());
        assert!(e.menu.is_none());
        assert_eq!(std::fs::read_to_string(dir.join("other")).unwrap(), "keep\n");
    }

    /// pull は裏で動かす。止まったら確かめ、選んだ方法でもう一度裏で動かす
    #[test]
    fn pull_asks_and_retries() {
        let (dir, mut e) = repo("pull");
        let wait = |e: &mut Editor, until: &dyn Fn(&Editor) -> bool| {
            for _ in 0..2000 {
                e.poll_branches();
                if until(e) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            panic!("pull が終わらない: {}", e.notify.text);
        };
        // 手元のリポジトリの side を取り込む（利用者の pull.rebase に左右されないように --no-rebase）
        e.run_git_job("pull ", &["pull", "--no-edit", "--no-rebase", ".", "side"]);
        wait(&mut e, &|e| e.menu.is_some());
        pick(&mut e, "破棄して続ける");
        wait(&mut e, &|e| e.git_job.is_none());
        assert_eq!(e.notify.text, "pull しました");
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "S\n2\n3\n");
        assert_eq!(std::fs::read_to_string(dir.join("other")).unwrap(), "keep\n");
    }
}
