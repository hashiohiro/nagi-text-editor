//! 画面の部品の型（ADR 0016 の 6。ADR 0011 で予定していた View）。
//!
//! キー・IME で確定した文字・画面を覆う部品のクリックは、「今向いている部品」（`focused`）へ 1 か所で渡す。
//! 部品の状態は Editor のフィールド（`self.launcher` など）に置いたまま、振る舞いを Component の実装 1 つにまとめる。
//!
//! 新しい部品（パネル・ダイアログ）の作り方:
//! 1. 状態の構造体を Editor のフィールドに置き、操作を Editor のメソッドとして書く（そのパネルのファイルに）。
//! 2. 同じファイルに `pub struct XxxComponent;` と `impl Component for XxxComponent` を書く（key は必須、text・click は要るものだけ）。
//! 3. 入力の状態（Mode）を 1 つ足し、下の `focused` に 1 行足す。描き方はそのファイルの `draw` モジュールに。
//!
//! Vim の編集の状態（Normal・Visual・Insert・検索・置換）は Editor そのものが受け取る（ここには載せない）。
//! 欄（木・下の欄・右の欄）の上でのクリックは位置で決まるので、main.rs が画面の配置から振り分ける。

use nagi_vim::Key;

use crate::editor::{Editor, Effect, Mode, Side};
use crate::result_grid::BottomKind;

pub trait Component: Sync {
    /// キーを受け取る
    fn key(&self, ed: &mut Editor, key: Key) -> Effect;

    /// IME で確定した文字（文字を打てない部品は何もしない）
    fn text(&self, ed: &mut Editor, text: &str) {
        let _ = (ed, text);
    }

    /// 押した（画面の行・列と、連続して押した回数）。扱ったら true（main は描き直して終わる）。
    /// 画面を覆う部品（ダイアログ）は、どこを押しても true を返す
    fn click(&self, ed: &mut Editor, row: usize, col: usize, clicks: u32) -> bool {
        let _ = (ed, row, col, clicks);
        false
    }
}

/// 今向いている部品（Vim の編集の状態なら None）
pub fn focused(ed: &Editor) -> Option<&'static dyn Component> {
    Some(match ed.mode {
        Mode::Normal | Mode::Visual { .. } | Mode::Insert | Mode::Search | Mode::Replace => return None,
        Mode::Command => &crate::cmdline_complete::CommandLineComponent,
        Mode::Finder => &crate::finder::FinderComponent,
        Mode::Launcher => &crate::launcher::LauncherComponent,
        Mode::RunDialog => &crate::run_dialog::RunDialogComponent,
        Mode::Settings => &crate::settings_dialog::SettingsComponent,
        Mode::GrepPrompt => &crate::grep_panel::GrepPromptComponent,
        Mode::List => match ed.list_side {
            Side::Left => &crate::tree_panel::TreeComponent,
            Side::Bottom if ed.bottom_kind == BottomKind::Result => &crate::result_grid::ResultComponent,
            Side::Bottom if ed.bottom_kind == BottomKind::Problems => &crate::bottom_panel::ProblemsComponent,
            Side::Bottom => &crate::grep_panel::ListComponent,
            Side::Commit => &crate::commit_panel::CommitListComponent,
            Side::Right => &crate::right_panel::RightComponent,
        },
        Mode::CommitMsg => &crate::commit_panel::CommitMsgComponent,
        Mode::Diff => &crate::diff_view::DiffComponent,
        Mode::Log => &crate::log_view::LogComponent,
        Mode::Terminal => &crate::terminal::TerminalComponent,
        Mode::Branches => &crate::branches::BranchesComponent,
        Mode::DbTable => &crate::db_table::DbTableComponent,
    })
}
