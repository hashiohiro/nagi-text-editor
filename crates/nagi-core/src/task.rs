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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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
