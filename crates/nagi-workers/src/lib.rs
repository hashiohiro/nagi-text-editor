//! UI スレッドの外で行う仕事（architecture.md §3）。結果はチャネルで返し、届くたびに `waker` で UI を起こす。

pub mod config;
pub mod db;
pub mod drivers;
pub mod exec;
pub mod files;
pub mod git;
pub mod grammars;
pub mod grep;
pub mod index;
pub mod jdk;
pub mod jobs;
pub mod maven;
pub mod maven_deps;
pub mod project;
pub mod runconfig;
pub mod salesforce;
pub mod sys;
pub mod term;
pub mod toolchains;
pub mod wsl;
pub mod watch;

/// UI のイベントループを起こす関数
pub use nagi_core::task::Waker;
