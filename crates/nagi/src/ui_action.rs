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
    [&crate::db_panel::ACTIONS[..], &crate::tree_panel::ACTIONS, &crate::maven_panel::ACTIONS, &crate::md_preview::ACTIONS, &crate::commit_panel::ACTIONS, &crate::log_view::ACTIONS, &crate::diff_view::ACTIONS, &crate::grep_panel::ACTIONS, &crate::bottom_panel::ACTIONS].into_iter().flatten()
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
