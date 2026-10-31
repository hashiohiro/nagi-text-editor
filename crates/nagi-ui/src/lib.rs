//! 画面の部品の共通の型（ADR 0018）。
//!
//! Editor・描画・OS を知らない。部品（欄・一覧・木・表）が共通に使う「形」と「振る舞い」だけを持ち、単体で試験する。
//!
//! - `Sizes`・`Splitter`: ドラッグで変える大きさ（欄の幅・高さ、列の幅）と、その区切り（段 2）
//! - `Rect`・`At`・`Ui`: 部品の中の座標と、描き方と押した位置で共有する割り付け（段 3）

mod sizes;
mod ui;

pub use sizes::{Axis, Sizes, Splitter, hit};
pub use ui::{At, Rect, Ui};
