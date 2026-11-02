//! 画面の部品の共通の型（ADR 0018）。
//!
//! Editor・描画・OS を知らない。部品（欄・一覧・木・表）が共通に使う「形」と「振る舞い」だけを持ち、単体で試験する。
//!
//! - `Sizes`・`Splitter`: ドラッグで変える大きさ（欄の幅・高さ、列の幅）と、その区切り（段 2）
//! - `Rect`・`At`・`Ui`: 部品の中の座標と、描き方と押した位置で共有する割り付け（段 3）
//! - `Folds`・`TreeRow`・`left`・`right`・`path_rows`: 畳める木の状態と ← → の動き（段 4）
//! - `list`: 一覧の移動・見える範囲・押した行・ホイール（段 4）
//! - `speed`: 打って探すスピードサーチ（木・一覧）
//! - `cells`: 表のセルの範囲の選び方（Shift+矢印・Ctrl+A）とコピーの形

pub mod cells;
pub mod list;
mod sizes;
pub mod speed;
pub mod tree;
mod ui;

pub use sizes::{Axis, Sizes, Splitter, hit};
pub use tree::{Folds, PathRow, Step, TreeRow};
pub use speed::SpeedSearch;
pub use ui::{At, Rect, Ui};
