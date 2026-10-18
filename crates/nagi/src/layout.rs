//! 画面の配置の部品（View。ADR 0016 の 6）。
//!
//! パネル・小窓・ダイアログは「今画面のどこを持つか（rect）」「描き方（draw）」「そこを押されたとき（click）」を
//! View の実装 1 つにまとめ、`PANELS`（欄・小窓）と `OVERLAYS`（画面を覆うダイアログ）に下から順に並べる。view.rs は表の順に描き、main.rs は押した位置を上から
//! 当てて渡す（前は描く順と当てる順を view.rs と main.rs に別々に書いていた）。
//!
//! Component（component.rs）との違い: Component は「今向いている部品」へキーを渡す口、View は「画面に出ている部品」の
//! 描画とマウスの口。木は向いていなくても出ていれば描くし、押されれば受け取る。
//!
//! 新しい部品の作り方: そのファイルの draw モジュールに描く関数、同じファイルに `pub struct XxxView;` と
//! `impl View for XxxView`（rect は出していなければ None）、下の `PANELS` か `OVERLAYS` に 1 行。

use crate::editor::Editor;
use crate::panes::Rect;
use crate::view::Painter;

/// 画面の大きさ（文字の行と列。rows は最下行を除いた行数）
#[derive(Clone, Copy, Debug)]
pub struct Screen {
    pub rows: usize,
    pub cols: usize,
}

/// 押したときの結果
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Click {
    /// 受け取らない（下の部品・エディタへ）
    Pass,
    /// 受け取った
    Done,
    /// 受け取って、そのままドラッグで行を選ぶ（検索・ログ）
    SelectList,
    /// 受け取って、そのままドラッグでターミナルの範囲を選ぶ
    SelectTerminal,
}

pub trait View: Sync {
    /// 今画面のどこを持つか（出していなければ None）
    fn rect(&self, ed: &Editor, s: Screen) -> Option<Rect>;

    /// 描く（rect を返したときだけ呼ばれる）
    fn draw(&self, ed: &Editor, p: &mut Painter, r: Rect, s: Screen) {
        let _ = (ed, p, r, s);
    }

    /// 押された（行・列は画面の位置、`clicks` は連続して押した回数）
    fn click(&self, ed: &mut Editor, r: Rect, row: usize, col: usize, clicks: u32) -> Click {
        let _ = (ed, r, row, col, clicks);
        Click::Pass
    }
}

/// 状態の行より下に描く層（欄・小窓）と、上に描く層（画面を覆うダイアログ）
pub enum Layer {
    Panels,
    Overlays,
}

/// 下から順（描く順。押したときは上から当てる）
pub static PANELS: &[&dyn View] = &[
    &crate::tree_panel::TreeView,
    &crate::tree_panel::LeftTabsView,
    &crate::commit_panel::CommitView,
    &crate::log_view::LogScreenView,
    &crate::diff_view::DiffScreenView,
    &crate::right_panel::RightView,
    &crate::db_table::DbTableView,
    &crate::db_panel::ConsoleBarView,
    &crate::bottom_panel::BottomView,
    &crate::finder::FinderView,
    &crate::branches::BranchPopupView,
    &crate::git_ui::HunkPopupView,
    &crate::blame::BlamePopupView,
    &crate::complete::CompletionView,
    &crate::cmdline_complete::CmdCompleteView,
];

pub static OVERLAYS: &[&dyn View] = &[&crate::run_dialog::RunDialogView, &crate::settings_dialog::SettingsView, &crate::launcher::LauncherView];

pub fn layer(l: Layer) -> &'static [&'static dyn View] {
    match l {
        Layer::Panels => PANELS,
        Layer::Overlays => OVERLAYS,
    }
}

/// 層の部品を下から描く
pub fn draw(l: Layer, ed: &Editor, p: &mut Painter, s: Screen) {
    for v in layer(l) {
        if let Some(r) = v.rect(ed, s) {
            v.draw(ed, p, r, s);
        }
    }
}

/// 押した位置の部品（欄の層の上から）と、その番号（連続の回数を部品ごとに数える）
pub fn hit(ed: &Editor, s: Screen, row: usize, col: usize) -> Option<(usize, &'static dyn View, Rect)> {
    PANELS.iter().enumerate().rev().find_map(|(i, v)| v.rect(ed, s).filter(|r| r.contains(row, col)).map(|r| (i, *v, r)))
}
