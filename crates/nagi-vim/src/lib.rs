//! キーの表記、マッピング（nnoremap など）、vimrc の読み込み（architecture.md §8）。
//!
//! マッピングは Vim と同じく「右辺をキー列として、再マップせずにエディタへ流し込む」方式で展開する。
//! `:<C-u>action Find<CR>` のような IdeaVim のアクション呼び出しは、Ex コマンド `action Find` として
//! エディタに届き、[`action::ideavim`] の対応表で Nagi の機能に振り分ける。

pub mod action;
pub mod cmd;
mod key;
mod map;
mod rc;

pub use key::{Key, parse_keys};
pub use map::{Feed, Keymap, Matcher};
pub use rc::{Rc, load_rc, load_rc_from, parse_rc};
