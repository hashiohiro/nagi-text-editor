//! UI スレッドの外で行う仕事（architecture.md §3）。結果はチャネルで返し、届くたびに `waker` で UI を起こす。

pub mod files;

/// UI のイベントループを起こす関数
pub type Waker = std::sync::Arc<dyn Fn() + Send + Sync>;
