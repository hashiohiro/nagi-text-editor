//! 裏の処理の骨組み（ADR 0016 の 1）。
//!
//! ```ignore
//! let t = task::spawn("git-status", waker, move |ctx| {
//!     let r = git::status(&root);
//!     ctx.send(r);
//! });
//! // UI スレッド（poll）で
//! while let Ok(r) = t.try_recv() { … }
//! ```
//!
//! - `ctx.send` は結果を送って UI を起こす（waker）。受け手が無くなっていれば false（やめてよい合図）。
//! - `ctx.cancelled()` で取り消しを見る。`Task` を捨てると取り消しの合図が立つ（ADR 0009 の AtomicBool）。
//! - スレッドを作れなかったとき（ほぼ無い）は、何も届かない Task になる。
//! - release はパニックで即終了する設定（panic = "abort"）なので、パニックは捕まえない。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::Arc;

/// UI を起こす関数（イベントループに知らせる）
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// 何もしない Waker（試験・UI の無いところ）
pub fn no_waker() -> Waker {
    Arc::new(|| {})
}

/// 裏の処理の中から使うもの
pub struct Ctx<M> {
    tx: Sender<M>,
    waker: Waker,
    cancel: Arc<AtomicBool>,
}

impl<M> Ctx<M> {
    /// 結果を送って UI を起こす。受け手が無くなっていれば false
    pub fn send(&self, m: M) -> bool {
        let ok = self.tx.send(m).is_ok();
        (self.waker)();
        ok
    }

    /// 取り消されたか（Task を捨てた・cancel を呼んだ）
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// 取り消しの合図そのもの（取り消しを見る既存の関数に渡す）
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    pub fn waker(&self) -> Waker {
        self.waker.clone()
    }

    /// 同じ受け手へ送る口（処理の中でさらにスレッドを分けるとき）
    pub fn sender(&self) -> Sender<M> {
        self.tx.clone()
    }
}

/// 動いている裏の処理（UI が持つ）
pub struct Task<M> {
    rx: Receiver<M>,
    cancel: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
}

impl<M> Task<M> {
    /// 届いているものを 1 つ（mpsc と同じ形）
    pub fn try_recv(&self) -> Result<M, TryRecvError> {
        self.rx.try_recv()
    }

    /// 届いているものを全部
    pub fn drain(&self) -> Vec<M> {
        self.rx.try_iter().collect()
    }

    /// 届くまで待って 1 つ受け取る（処理が終わって何も届かなければ Err）
    pub fn recv(&self) -> Result<M, std::sync::mpsc::RecvError> {
        self.rx.recv()
    }

    /// 処理が終わるまで、届くものを順に（待つ）
    pub fn iter(&self) -> std::sync::mpsc::Iter<'_, M> {
        self.rx.iter()
    }

    /// 終わるまで待って 1 つ受け取る（試験用）
    pub fn recv_timeout(&self, d: std::time::Duration) -> Option<M> {
        self.rx.recv_timeout(d).ok()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// 処理が終わったか（届いたものを読み残していても true）
    pub fn finished(&self) -> bool {
        self.finished.load(Ordering::Relaxed)
    }
}

impl<M> Drop for Task<M> {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// `name` のスレッドで `f` を動かす
pub fn spawn<M: Send + 'static>(name: &str, waker: Waker, f: impl FnOnce(&Ctx<M>) + Send + 'static) -> Task<M> {
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let fin = finished.clone();
    let ctx = Ctx { tx, waker, cancel: cancel.clone() };
    let spawned = std::thread::Builder::new().name(name.into()).spawn(move || {
        f(&ctx);
        // 送り口を閉じてから起こす（受け取る側は「届き終えた」= Disconnected・finished で分かる）
        let waker = ctx.waker.clone();
        drop(ctx);
        fin.store(true, Ordering::Relaxed);
        waker();
    });
    if spawned.is_err() {
        finished.store(true, Ordering::Relaxed);
    }
    Task { rx, cancel, finished }
}

/// スレッドを作らず、手で送る口と組にした Task（試験で結果の届き方を真似る・別の仕組みの結果を Task として持つ）
pub fn manual<M>() -> (Sender<M>, Task<M>) {
    let (tx, rx) = channel();
    (tx, Task { rx, cancel: Arc::new(AtomicBool::new(false)), finished: Arc::new(AtomicBool::new(false)) })
}

/// 多くの処理が 1 つの受け口へ送る形（manual で作った口へ）: `f` の結果を `tx` へ送って UI を起こす
pub fn spawn_to<M: Send + 'static>(name: &str, tx: Sender<M>, waker: Waker, f: impl FnOnce() -> M + Send + 'static) {
    let _ = std::thread::Builder::new().name(name.into()).spawn(move || {
        if tx.send(f()).is_ok() {
            waker();
        }
    });
}

/// 結果を待たない後始末（保存・書き出し）を裏で動かす
pub fn detach(name: &str, f: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new().name(name.into()).spawn(f);
}

/// 裏の仕事と、終わったら UI スレッドで呼ぶ受け取り（ADR 0018 の §4）。受け口をフィールドに持って poll で受け取る代わりに使う。
///
/// ```ignore
/// ed.jobs.run("git-checkout", waker, move || git::git(&root, &args), |ed: &mut Editor, r| ed.notify(…));
/// // UI スレッドで（Editor::poll_background）
/// let mut jobs = std::mem::take(&mut ed.jobs);
/// jobs.poll(ed);   // 終わった仕事の受け取りを呼ぶ（受け取りの中で新しく run した仕事は ed.jobs に入る）
/// ed.jobs.merge(jobs);
/// ```
///
/// 同じ名前の仕事を始めると、前の仕事は取り消す（Task を捨てる。受け取りも呼ばない）。
pub struct Jobs<C> {
    list: Vec<Box<dyn Pending<C>>>,
}

impl<C> Default for Jobs<C> {
    fn default() -> Self {
        Jobs { list: Vec::new() }
    }
}

trait Pending<C> {
    fn name(&self) -> &str;
    /// 終わっていれば受け取りを返す（何も届かずに終わったら何もしない受け取り）。まだなら自分を返す
    fn finish(self: Box<Self>) -> Result<Box<dyn FnOnce(&mut C)>, Box<dyn Pending<C>>>;
}

struct One<C, R> {
    name: String,
    task: Task<R>,
    done: Box<dyn FnOnce(&mut C, R)>,
}

impl<C: 'static, R: 'static> Pending<C> for One<C, R> {
    fn name(&self) -> &str {
        &self.name
    }

    fn finish(self: Box<Self>) -> Result<Box<dyn FnOnce(&mut C)>, Box<dyn Pending<C>>> {
        match self.task.try_recv() {
            Ok(r) => {
                let done = self.done;
                Ok(Box::new(move |c: &mut C| done(c, r)))
            }
            Err(TryRecvError::Disconnected) => Ok(Box::new(|_: &mut C| {})),
            Err(TryRecvError::Empty) => Err(self),
        }
    }
}

impl<C: 'static> Jobs<C> {
    /// `name` のスレッドで `work` を動かし、終わったら UI スレッドで `done` に結果を渡す（同じ名前の前の仕事は取り消す）
    pub fn run<R: Send + 'static>(&mut self, name: &str, waker: Waker, work: impl FnOnce() -> R + Send + 'static, done: impl FnOnce(&mut C, R) + 'static) {
        self.list.retain(|j| j.name() != name);
        let task = spawn(name, waker, move |ctx| {
            ctx.send(work());
        });
        self.list.push(Box::new(One { name: name.to_string(), task, done: Box::new(done) }));
    }

    /// 終わった仕事の受け取りを呼ぶ。呼んだら true（描き直す）
    pub fn poll(&mut self, c: &mut C) -> bool {
        let mut called = false;
        for j in std::mem::take(&mut self.list) {
            match j.finish() {
                Ok(done) => {
                    done(c);
                    called = true;
                }
                Err(j) => self.list.push(j),
            }
        }
        called
    }

    /// 受け取りの中で始めた仕事（`other`）を足す。同じ名前のものは新しい方を残す
    pub fn merge(&mut self, other: Jobs<C>) {
        let newer: Vec<String> = self.list.iter().map(|j| j.name().to_string()).collect();
        self.list.extend(other.list.into_iter().filter(|j| !newer.iter().any(|n| n == j.name())));
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// その名前の仕事が動いているか
    pub fn running(&self, name: &str) -> bool {
        self.list.iter().any(|j| j.name() == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn jobs_call_back_on_ui_thread() {
        #[derive(Default)]
        struct Ui {
            got: Vec<i32>,
            jobs: Jobs<Ui>,
        }
        let mut ui = Ui::default();
        ui.jobs.run("a", no_waker(), || 1, |u: &mut Ui, r| u.got.push(r));
        // 同じ名前で始め直すと前の仕事は呼ばない
        ui.jobs.run("b", no_waker(), || 2, |u: &mut Ui, r| {
            u.got.push(r);
            // 受け取りの中で次の仕事
            u.jobs.run("c", no_waker(), || 3, |u: &mut Ui, r| u.got.push(r));
        });
        ui.jobs.run("b", no_waker(), || 20, |u: &mut Ui, r| {
            u.got.push(r);
            u.jobs.run("c", no_waker(), || 30, |u: &mut Ui, r| u.got.push(r));
        });
        for _ in 0..500 {
            let mut jobs = std::mem::take(&mut ui.jobs);
            jobs.poll(&mut ui);
            ui.jobs.merge(jobs);
            if ui.jobs.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        ui.got.sort();
        assert_eq!(ui.got, [1, 20, 30]);
    }

    #[test]
    fn sends_and_cancels() {
        let woke = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let w = woke.clone();
        let t = spawn("t", Arc::new(move || {
            w.fetch_add(1, Ordering::Relaxed);
        }), |ctx: &Ctx<u32>| {
            ctx.send(1);
            ctx.send(2);
        });
        assert_eq!(t.recv_timeout(Duration::from_secs(5)), Some(1));
        assert_eq!(t.recv_timeout(Duration::from_secs(5)), Some(2));
        while !t.finished() {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(woke.load(Ordering::Relaxed), 3, "送るたびと、終わったときに UI を起こす");
        assert!(matches!(t.try_recv(), Err(TryRecvError::Disconnected)), "終われば送り口が閉じる");
        // 捨てると取り消しの合図が立つ
        let (tx, rx) = channel();
        let t = spawn("t2", no_waker(), move |ctx: &Ctx<()>| {
            while !ctx.cancelled() {
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = tx.send(());
        });
        drop(t);
        assert!(rx.recv_timeout(Duration::from_secs(5)).is_ok());
    }
}
