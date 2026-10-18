//! UI スレッドの外で行う仕事（architecture.md §3）。結果はチャネルで返し、届くたびに `waker` で UI を起こす。

pub mod db;
pub mod drivers;
pub mod files;
pub mod git;
pub mod grep;
pub mod index;
pub mod jobs;
pub mod maven;
pub mod project;
pub mod salesforce;
pub mod term;
pub mod wsl;
pub mod watch;

/// UI のイベントループを起こす関数
pub type Waker = std::sync::Arc<dyn Fn() + Send + Sync>;
