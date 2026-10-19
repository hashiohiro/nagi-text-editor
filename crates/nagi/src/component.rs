//! 画面の部品の型（ADR 0016 の 6。ADR 0011 で予定していた View）。
//!
//! キー・IME で確定した文字・画面を覆う部品のクリックは、「今向いている部品」（`focused`）へ 1 か所で渡す。
//! 部品の状態は Editor のフィールド（`self.launcher` など）に置いたまま、振る舞いを Component の実装 1 つにまとめる。
//!
//! 新しい部品（パネル・ダイアログ）の作り方:
//! 1. 状態の構造体を Editor のフィールドに置き、操作を Editor のメソッドとして書く（そのパネルのファイルに）。
//! 2. 同じファイルに `pub struct XxxComponent;` と `impl Component for XxxComponent` を書く（key は必須、text・click・status は要るものだけ）。
//! 3. editor.rs の Panel（と見え方の State）に 1 つ足し、下の `focused` に 1 行足す。描き方はそのファイルの `draw` モジュールに。
//!
//! Vim の編集の状態（Normal・Visual・Insert・検索・置換）は Editor そのものが受け取る（ここには載せない）。
//! 欄（木・下の欄・右の欄）の上でのクリックは位置で決まるので、main.rs が画面の配置から振り分ける。

use nagi_vim::Key;

use crate::editor::{Editor, Effect, Mode, Panel, Side};
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

    /// 状態行（左に部品の名前、右に操作の説明。知らせがあれば左は知らせ）。None なら view.rs が描く（入力欄を出す部品・Vim の状態）
    fn status(&self, ed: &Editor) -> Option<Status> {
        let _ = ed;
        None
    }
}

/// 部品の状態行
pub struct Status {
    /// 左（`-- 履歴 --` など）
    pub name: String,
    /// 左を注意の色で（送っていない変更があるなど）
    pub warn: bool,
    /// 右の操作の説明
    pub help: String,
    /// Vim を使わないときの説明（矢印・Enter・右クリックで書く。None なら help のまま）
    pub plain: Option<String>,
}

impl Status {
    pub fn new(name: impl Into<String>, help: impl Into<String>) -> Option<Status> {
        Some(Status { name: name.into(), warn: false, help: help.into(), plain: None })
    }

    /// Vim のときの説明と、使わないときの説明
    pub fn keys(name: impl Into<String>, help: impl Into<String>, plain: impl Into<String>) -> Option<Status> {
        Some(Status { name: name.into(), warn: false, help: help.into(), plain: Some(plain.into()) })
    }
}

/// 今向いている部品（Vim の編集の状態なら None）。`:` のコマンドラインは Vim の状態だが、候補の窓を持つので部品として扱う
pub fn focused(ed: &Editor) -> Option<&'static dyn Component> {
    if ed.vim == Mode::Command {
        return Some(&crate::cmdline_complete::CommandLineComponent);
    }
    Some(match ed.focus? {
        Panel::Finder => &crate::finder::FinderComponent,
        Panel::Launcher => &crate::launcher::LauncherComponent,
        Panel::RunDialog => &crate::run_dialog::RunDialogComponent,
        Panel::Settings => &crate::settings_dialog::SettingsComponent,
        Panel::GrepPrompt => &crate::grep_panel::GrepPromptComponent,
        Panel::List => match ed.list_side {
            Side::Left => &crate::tree_panel::TreeComponent,
            Side::Bottom if ed.bottom_kind == BottomKind::Result => &crate::result_grid::ResultComponent,
            Side::Bottom if ed.bottom_kind == BottomKind::Problems => &crate::bottom_panel::ProblemsComponent,
            Side::Bottom => &crate::grep_panel::ListComponent,
            Side::Commit => &crate::commit_panel::CommitListComponent,
            Side::Right => &crate::right_panel::RightComponent,
        },
        Panel::CommitMsg => &crate::commit_panel::CommitMsgComponent,
        Panel::Diff => &crate::diff_view::DiffComponent,
        Panel::Log => &crate::log_view::LogComponent,
        Panel::Terminal => &crate::terminal::TerminalComponent,
        Panel::Branches => &crate::branches::BranchesComponent,
        Panel::DbTable => &crate::db_table::DbTableComponent,
        Panel::Menu => &crate::menu::MenuComponent,
    })
}
