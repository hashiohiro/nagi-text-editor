//! 画面の部品の共通の型（ADR 0018）。
//!
//! Editor・描画・OS を知らない。部品（欄・一覧・木・表）が共通に使う「形」と「振る舞い」だけを持ち、単体で試験する。
//!
//! - `Sizes`・`Splitter`: ドラッグで変える大きさ（欄の幅・高さ、列の幅）と、その区切り（段 2）

mod sizes;

pub use sizes::{Axis, Sizes, Splitter, hit};
