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

    /// 右クリック（メニューを開いたら true。`menu::Menu` を作って `open_menu` に渡す）
    fn context(&self, ed: &mut Editor, r: Rect, row: usize, col: usize) -> bool {
        let _ = (ed, r, row, col);
        false
    }

    /// ホイール（`dy` は下へ、`dx` は右へ動かす行・列の数。Shift か横のホイール・タッチパッドで横）。受け取ったら true
    fn wheel(&self, ed: &mut Editor, r: Rect, row: usize, col: usize, dy: isize, dx: isize) -> bool {
        let _ = (ed, r, row, col, dy, dx);
        false
    }

    /// 型の名前（試験で部品を見分ける。大きさ 0 の型は番地では見分けられない）
    #[cfg(test)]
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
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
    &crate::commit_panel::CommitView,
    // 左の欄の 1 行目は木・コミットの欄の矩形にも入るので、それより上に置いて先に当てる
    &crate::tree_panel::LeftTabsView,
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
    &crate::menu::MenuView,
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

/// 押した位置の部品へ、欄の層の上から渡す。受け取らなければ（小窓は画面全体を持つが押されても受け取らない）その下へ。
/// 受け取った部品の番号（連続して押した回数を部品ごとに数える）と結果。どれも受け取らなければ None（エディタへ）。
/// `clicks` は部品の番号から連続の回数を出す関数
pub fn press(ed: &mut Editor, s: Screen, row: usize, col: usize, clicks: impl Fn(usize) -> u32) -> Option<(usize, Click)> {
    for (i, v) in PANELS.iter().enumerate().rev() {
        let Some(r) = v.rect(ed, s).filter(|r| r.contains(row, col)) else { continue };
        match v.click(ed, r, row, col, clicks(i)) {
            Click::Pass => {}
            done => return Some((i, done)),
        }
    }
    None
}

/// 右クリックを、押した位置の部品へ欄の層の上から渡す。どれかがメニューを開いたら true
pub fn context(ed: &mut Editor, s: Screen, row: usize, col: usize) -> bool {
    if ed.menu.is_some() {
        return false;
    }
    PANELS.iter().rev().any(|v| v.rect(ed, s).filter(|r| r.contains(row, col)).is_some_and(|r| v.context(ed, r, row, col)))
}

/// ホイールを、マウスの下の部品へ上から渡す（画面を覆うダイアログ、欄・小窓の順）。どれかが受け取ったら true（受け取らなければ本文へ）
pub fn wheel(ed: &mut Editor, s: Screen, row: usize, col: usize, dy: isize, dx: isize) -> bool {
    OVERLAYS.iter().chain(PANELS.iter()).rev().any(|v| v.rect(ed, s).filter(|r| r.contains(row, col)).is_some_and(|r| v.wheel(ed, r, row, col, dy, dx)))
}

/// 画面を覆う小窓・ダイアログのホイール: 向いている部品へ ↓ / ↑ を行の数だけ送る（選びが動き、見える範囲が付いてくる）
pub fn wheel_keys(ed: &mut Editor, dy: isize) -> bool {
    let Some(c) = crate::component::focused(ed) else { return false };
    let key = if dy > 0 { nagi_vim::Key::Down } else { nagi_vim::Key::Up };
    for _ in 0..dy.unsigned_abs() {
        c.key(ed, key);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::State;
    use nagi_core::Buffer;

    #[test]
    fn press_goes_to_the_panel_under_popups() {
        let dir = std::env::temp_dir().join(format!("nagi-layout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        std::fs::write(dir.join("b.txt"), "two\n").unwrap();
        let mut buf = Buffer::open(&dir.join("a.txt")).unwrap();
        buf.path = Some(dir.join("a.txt"));
        let mut e = Editor::new(buf, None);
        e.set_screen(120, 40);
        e.open_tree();
        e.set_state(State::Normal);
        // 差分の小窓（画面全体を持つが受け取らない）が出ていても、木の行は木が受け取る
        e.git.popup = Some(0);
        let s = Screen { rows: 40, cols: 120 };
        let at = |name: &str| PANELS.iter().position(|v| v.name().ends_with(name)).unwrap();
        let tree = at("::TreeView");
        assert_eq!(press(&mut e, s, 2, 3, |_| 1).map(|p| p.0), Some(tree));
        assert_eq!(e.state(), State::List);
        // 1 行目はタブ（木の矩形にも入るが、タブが先に受け取る）
        let tabs = at("::LeftTabsView");
        assert_eq!(press(&mut e, s, 0, 1, |_| 1).map(|p| p.0), Some(tabs));
        // 本文はどれも受け取らない
        e.set_state(State::Normal);
        assert_eq!(press(&mut e, s, 5, 100, |_| 1), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
