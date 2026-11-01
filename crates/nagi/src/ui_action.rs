//! 部品の操作（UI アクション。ADR 0019）。
//!
//! 欄・一覧・ダイアログの操作は、名前の付いたアクションとして部品ごとの表（`db_panel::ACTIONS` など）に登録する。
//! 右クリックのメニュー（`Menu::action`）・欄のボタン・ショートカット・コマンドパレットは、どれもこの表を引く。
//! ショートカットは設定の `keymap.ui.<id>` で付け替えられ、既定は付けない（空）。付けたキーは、その部品にフォーカスがあるときだけ効く。
//!
//! 部品の中で Vim のキー（j・k・q・: など）は受けない。Vim のキーは VimField（本文と入力欄）の中だけ。
//! 部品が付け替えずに受けるのは標準のキー（矢印・Home・End・PageUp・PageDown・Enter・Esc・Tab・Space・F2・Delete・Ctrl+C・Ctrl+Z・Ctrl+Enter）だけ。

use crate::editor::Editor;
use crate::keymap::{parse_chord, Chord};

/// どの部品の操作か（ショートカットはその部品にフォーカスがあるときだけ効く。同じキーを別の部品で使ってもぶつからない）
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    Database,
    ProjectTree,
    Maven,
    Preview,
    Commit,
    Log,
    Diff,
    Grep,
    Problems,
    Run,
    Results,
    Table,
    Settings,
    RunConfig,
    Launcher,
}

impl Scope {
    /// 設定の画面・コマンドパレットに出す部品の名前
    pub fn name(self) -> &'static str {
        match self {
            Scope::Database => "データベース",
            Scope::ProjectTree => "プロジェクト",
            Scope::Maven => "Maven",
            Scope::Preview => "プレビュー",
            Scope::Commit => "コミット",
            Scope::Log => "履歴",
            Scope::Diff => "差分",
            Scope::Grep => "検索",
            Scope::Problems => "問題",
            Scope::Run => "実行",
            Scope::Results => "クエリの結果",
            Scope::Table => "テーブル",
            Scope::Settings => "設定",
            Scope::RunConfig => "実行の構成",
            Scope::Launcher => "ランチャー",
        }
    }
}

pub struct UiAction {
    /// 名前（設定の項目は keymap.ui.<id>）
    pub id: &'static str,
    /// メニュー・パレット・設定の画面に出す名前
    pub label: &'static str,
    pub scope: Scope,
    /// 今できるか（できないときはメニューで薄く出し、キーとパレットでは動かさない）
    pub enabled: fn(&Editor) -> bool,
    pub run: fn(&mut Editor),
}

/// いつでもできる
pub fn always(_: &Editor) -> bool {
    true
}

/// 設定の項目の頭
pub const KEY: &str = "keymap.ui.";

/// すべてのアクション（部品の表をつなげたもの）
pub fn all() -> impl Iterator<Item = &'static UiAction> {
    [&crate::db_panel::ACTIONS[..], &crate::tree_panel::ACTIONS, &crate::maven_panel::ACTIONS, &crate::md_preview::ACTIONS, &crate::commit_panel::ACTIONS, &crate::log_view::ACTIONS, &crate::diff_view::ACTIONS, &crate::grep_panel::ACTIONS, &crate::bottom_panel::ACTIONS, &crate::run_panel::ACTIONS, &crate::result_grid::ACTIONS, &crate::db_table::ACTIONS, &crate::settings_dialog::ACTIONS, &crate::run_dialog::ACTIONS, &crate::launcher::UI_ACTIONS].into_iter().flatten()
}

pub fn find(id: &str) -> Option<&'static UiAction> {
    all().find(|a| a.id == id)
}

/// その項目（keymap.ui.<id>）の部品
pub fn scope_of_key(key: &str) -> Option<Scope> {
    find(key.strip_prefix(KEY)?).map(|a| a.scope)
}

impl Editor {
    /// 今フォーカスのある部品（アクションのショートカットを引く範囲）。入力欄に打っている間は None（キーは入力欄へ）
    pub fn action_scope(&self) -> Option<Scope> {
        if self.has_prompt() || self.in_text_field() {
            return None;
        }
        crate::component::focused(self)?.scope(self)
    }

    /// 押した組み合わせに利用者が付けた、今の部品のアクション
    pub fn ui_action_for(&self, chord: &Chord) -> Option<&'static UiAction> {
        let scope = self.action_scope()?;
        all().filter(|a| a.scope == scope).find(|a| parse_chord(&self.settings.get(&format!("{KEY}{}", a.id))).as_ref() == Some(chord))
    }

    /// アクションを行う。できないときは行わずに false
    pub fn run_ui_action(&mut self, id: &str) -> bool {
        let Some(a) = find(id) else { return false };
        if !(a.enabled)(self) {
            return false;
        }
        (a.run)(self);
        true
    }

    /// アクションに付けたショートカット（メニュー・状態の行に出す）
    pub fn ui_shortcut(&self, id: &str) -> Option<String> {
        parse_chord(&self.settings.get(&format!("{KEY}{id}"))).map(|c| c.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 試験の外のソース（src/*.rs の #[cfg(test)] より前）
    fn sources() -> Vec<(String, String)> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "rs"))
            .map(|p| {
                let s = std::fs::read_to_string(&p).unwrap();
                // 試験のモジュールより前（ファイルの途中の #[cfg(test)] の関数では切らない）
                let body = s.split("\n#[cfg(test)]\nmod tests").next().unwrap_or("").split("\n#[cfg(test)]\npub(crate) mod tests").next().unwrap_or("").to_string();
                (p.file_name().unwrap().to_string_lossy().into_owned(), body)
            })
            .collect()
    }

    /// すべてのアクションに、マウスの入口（右クリックのメニューの項目かボタン）がある（ADR 0019。ショートカットは既定で付けないので、
    /// 入口が無いとパレットからしか届かない）。メニューは Menu::action・action_as、ボタンは run_ui_action で呼ぶ
    #[test]
    fn every_action_has_mouse_entry() {
        let src: String = sources().into_iter().filter(|(n, _)| n != "ui_action.rs").map(|(_, b)| b).collect();
        let missing: Vec<&str> = all()
            .map(|a| a.id)
            .filter(|id| {
                let q = format!("\"{id}\"");
                !src.contains(&format!(".action(ed, {q})")) && !src.contains(&format!(".action(self, {q})")) && !src.contains(&format!("action_as(ed, {q}")) && !src.contains(&format!("action_as(self, {q}")) && !src.contains(&format!("run_ui_action({q})"))
            })
            .collect();
        assert!(missing.is_empty(), "メニューにもボタンにも無いアクション: {missing:?}");
    }

    /// 部品の上で印字できる文字を打っても、部品から抜けない・メニューも出ない・部品も閉じない（1 文字キーの操作は無い。ADR 0019）。
    /// 打った字は絞り込みの欄へ入ってよい。Vim を使うときも使わないときも
    #[test]
    fn printable_keys_do_nothing_in_panels() {
        use crate::editor::State;
        use nagi_vim::Key;
        let dir = std::env::temp_dir().join(format!("nagi-ui-letters-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(dir.join("b.txt"), "b\n").unwrap();
        type Open = fn(&mut Editor, &std::path::Path);
        let panels: [(&str, Open, fn(&Editor) -> bool); 5] = [
            ("プロジェクトの木", |e, _| e.open_tree(), |e| e.panels.tree.is_some()),
            ("問題", |e, d| {
                e.diags = vec![crate::diagnostics::Diag { path: d.join("src/a.rs"), line: 0, col: 0, error: true, msg: "m".into() }];
                e.toggle_bottom_tab(crate::result_grid::BottomKind::Problems);
            }, |e| e.bottom_visible()),
            ("差分", |e, _| e.show_diff("a".into(), "b".into(), "x\n", "y\n", None), |e| e.panels.diff.is_some()),
            ("設定", |e, _| e.open_settings(""), |e| e.panels.settings_dialog.is_some()),
            ("実行の構成", |e, _| e.open_run_dialog(None), |e| e.panels.run_dialog.is_some()),
        ];
        for vim in [false, true] {
            for (name, open, alive) in panels {
                let mut buf = nagi_core::Buffer::open(&dir.join("b.txt")).unwrap();
                buf.path = Some(dir.join("b.txt"));
                let mut e = Editor::new(buf, None);
                e.set_screen(140, 40);
                e.set_vim(vim);
                open(&mut e, &dir);
                let state = e.state();
                assert_ne!(state, State::Normal, "{name}: 部品にいる");
                for c in (0x21u8..0x7f).map(char::from) {
                    e.key(Key::Char(c));
                    assert!(alive(&e) && e.menu.is_none(), "{name} vim={vim}: {c} で閉じた・メニューが出た");
                    assert_eq!(e.state(), state, "{name} vim={vim}: {c} で部品から抜けた");
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ids_unique_and_unbound_by_default() {
        let ids: Vec<&str> = all().map(|a| a.id).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(ids.len(), sorted.len(), "名前がぶつかっている");
        // ショートカットの既定は付けない
        for id in ids {
            let item = crate::settings::schema(&format!("{KEY}{id}")).expect("設定の項目がある");
            assert_eq!(item.default, "", "{id}");
        }
    }
}
