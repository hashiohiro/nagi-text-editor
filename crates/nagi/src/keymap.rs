//! キーマップ（設定の「キーマップ」）。`Ctrl+Alt+S` のような書き方のキーの組み合わせと、それに付けた操作。
//!
//! main.rs の on_key で、押したキーを組み合わせの文字に直して `action_for` で引き、当たればその操作をする。
//! 操作はコマンドの表（commands.rs）の名前（Editor::shortcut で動かす）か、そのまま流すキー（入力補完の Ctrl+Space）。

use nagi_vim::Key;

use crate::settings::Settings;

/// キーの組み合わせ（修飾キーと、キーの名前。名前は大文字の英字・数字・F1〜F24・Space・Enter・Tab など）
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: String,
}

impl Chord {
    /// 画面と設定のファイルに書く形（Ctrl+Alt+Shift+名前）
    pub fn label(&self) -> String {
        let mut s = String::new();
        for (on, name) in [(self.ctrl, "Ctrl+"), (self.alt, "Alt+"), (self.shift, "Shift+")] {
            if on {
                s.push_str(name);
            }
        }
        s.push_str(&self.key);
        s
    }
}

/// キーの名前をそろえる（f12 → F12、space → Space、a → A）
fn normalize(name: &str) -> Option<String> {
    let n = name.trim();
    if n.is_empty() {
        return None;
    }
    let lower = n.to_ascii_lowercase();
    if let Some(num) = lower.strip_prefix('f').and_then(|x| x.parse::<u8>().ok()) {
        return (1..=24).contains(&num).then(|| format!("F{num}"));
    }
    let named = [("space", "Space"), ("enter", "Enter"), ("return", "Enter"), ("tab", "Tab"), ("esc", "Esc"), ("escape", "Esc"), ("backspace", "Backspace"), ("delete", "Delete"), ("del", "Delete"), ("insert", "Insert"), ("home", "Home"), ("end", "End"), ("pageup", "PageUp"), ("pagedown", "PageDown"), ("up", "Up"), ("down", "Down"), ("left", "Left"), ("right", "Right")];
    if let Some((_, v)) = named.iter().find(|(k, _)| *k == lower) {
        return Some(v.to_string());
    }
    let mut chars = n.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_graphic() => Some(c.to_ascii_uppercase().to_string()),
        _ => None,
    }
}

/// `Ctrl+Alt+S` を読む（大文字小文字・空白は問わない。読めなければ None）
pub fn parse_chord(s: &str) -> Option<Chord> {
    let parts: Vec<&str> = s.split('+').map(str::trim).collect();
    // 「Ctrl++」のような + そのものは扱わない
    let (last, mods) = parts.split_last()?;
    let mut c = Chord { ctrl: false, alt: false, shift: false, key: normalize(last)? };
    for m in mods {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => c.ctrl = true,
            "alt" => c.alt = true,
            "shift" => c.shift = true,
            _ => return None,
        }
    }
    Some(c)
}

/// 組み合わせに付ける操作: コマンドの表（commands.rs）の名前か、そのまま流すキー（入力補完のように、その場に合わせて動くもの）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Cmd(&'static str),
    Key(Key),
}

/// 操作（設定のキー, 操作）。既定は IntelliJ（Windows）の既定のキーマップと同じ。ただし定義へは F12、
/// デバッグは Visual Studio と同じ（F5 続ける・F9 ブレークポイント・F10 次へ・F11 中へ・Shift+F11 外へ・Shift+F5 止める）。
/// ここは Vim のキー操作を使うときも効くので、Vim と重ならないもの（F キー・Alt・Ctrl+Alt・Ctrl+Shift）だけ
pub const ACTIONS: [(&str, Action); 52] = [
    ("keymap.settings", Action::Cmd("Settings")),
    ("keymap.complete", Action::Key(Key::Ctrl(' '))),
    ("keymap.run", Action::Cmd("Run")),
    ("keymap.run_choose", Action::Cmd("RunConfigs")),
    ("keymap.run_stop", Action::Cmd("RunStop")),
    ("keymap.run_again", Action::Cmd("RunAgain")),
    ("keymap.run_test", Action::Cmd("RunTest")),
    ("keymap.goto_def", Action::Cmd("GotoDefinition")),
    ("keymap.terminal", Action::Cmd("ToolTerminal")),
    ("keymap.project", Action::Cmd("ToolProject")),
    ("keymap.commit", Action::Cmd("ToolCommit")),
    ("keymap.run_panel", Action::Cmd("ToolRun")),
    ("keymap.git", Action::Cmd("ToolGit")),
    ("keymap.find", Action::Cmd("ToolFind")),
    ("keymap.problems", Action::Cmd("ToolProblems")),
    ("keymap.log", Action::Cmd("ToolLog")),
    ("keymap.structure", Action::Cmd("Outline")),
    ("keymap.hide_all", Action::Cmd("HideAll")),
    ("keymap.select_in", Action::Cmd("SelectInTree")),
    ("keymap.debug", Action::Cmd("Debug")),
    ("keymap.debug_test", Action::Cmd("DebugTest")),
    ("keymap.breakpoint", Action::Cmd("Breakpoint")),
    ("keymap.debug_resume", Action::Cmd("DebugResume")),
    ("keymap.step_over", Action::Cmd("StepOver")),
    ("keymap.step_into", Action::Cmd("StepInto")),
    ("keymap.step_out", Action::Cmd("StepOut")),
    ("keymap.debug_stop", Action::Cmd("DebugStop")),
    ("keymap.debug_panel", Action::Cmd("ToolDebug")),
    ("keymap.palette", Action::Cmd("Commands")),
    ("keymap.goto_file", Action::Cmd("Files")),
    ("keymap.goto_symbol", Action::Cmd("Symbols")),
    ("keymap.file_structure", Action::Cmd("Outline")),
    ("keymap.goto_impl", Action::Cmd("GotoImplementation")),
    ("keymap.find_usages", Action::Cmd("Refs")),
    ("keymap.find_in_files", Action::Cmd("Grep")),
    ("keymap.recent_locations", Action::Cmd("RecentLocations")),
    ("keymap.back", Action::Cmd("JumpBack")),
    ("keymap.forward", Action::Cmd("JumpForward")),
    ("keymap.last_edit", Action::Cmd("JumpLastChange")),
    ("keymap.error_next", Action::Cmd("ErrorNext")),
    ("keymap.error_prev", Action::Cmd("ErrorPrev")),
    ("keymap.method_next", Action::Cmd("MethodNext")),
    ("keymap.method_prev", Action::Cmd("MethodPrev")),
    ("keymap.reformat", Action::Cmd("Format")),
    ("keymap.switcher", Action::Cmd("Buffers")),
    ("keymap.close_tab", Action::Cmd("bd")),
    ("keymap.next_tab", Action::Cmd("bn")),
    ("keymap.prev_tab", Action::Cmd("bp")),
    ("keymap.hunk_next", Action::Cmd("HunkNext")),
    ("keymap.hunk_prev", Action::Cmd("HunkPrev")),
    ("keymap.rollback", Action::Cmd("GitRevert")),
    ("keymap.push", Action::Cmd("Gpush")),
];

/// Vim のキー操作を使わないときだけ効く組み合わせ（Ctrl+C・V・Z・N など。Vim では Ctrl+V は矩形選択、Ctrl+R はやり直しなので）。既定は IntelliJ と同じ
pub const PLAIN_ACTIONS: [(&str, Action); 17] = [
    ("keymap.undo", Action::Cmd("Undo")),
    ("keymap.redo", Action::Cmd("Redo")),
    ("keymap.copy", Action::Cmd("Copy")),
    ("keymap.cut", Action::Cmd("Cut")),
    ("keymap.paste", Action::Cmd("Paste")),
    ("keymap.select_all", Action::Cmd("SelectAll")),
    ("keymap.save", Action::Cmd("w")),
    ("keymap.find_text", Action::Cmd("Find")),
    ("keymap.replace", Action::Cmd("Replace")),
    ("keymap.duplicate", Action::Cmd("DuplicateLine")),
    ("keymap.delete_line", Action::Cmd("DeleteLine")),
    ("keymap.comment", Action::Cmd("CommentLine")),
    ("keymap.goto_line", Action::Cmd("GotoLine")),
    ("keymap.goto_class", Action::Cmd("Types")),
    ("keymap.recent", Action::Cmd("Recent")),
    ("keymap.vcs_commit", Action::Cmd("Git")),
    ("keymap.vcs_update", Action::Cmd("Gpull")),
];

/// 本文（エディタ）にいるときだけ効く操作。下の欄の表の F2（値を直す）、ターミナルの Alt+←→ などはその欄のものにする
const BODY_ONLY: [&str; 18] = [
    "keymap.run_test",
    "keymap.debug_test",
    "keymap.goto_impl",
    "keymap.find_usages",
    "keymap.back",
    "keymap.forward",
    "keymap.last_edit",
    "keymap.error_next",
    "keymap.error_prev",
    "keymap.method_next",
    "keymap.method_prev",
    "keymap.reformat",
    "keymap.close_tab",
    "keymap.next_tab",
    "keymap.prev_tab",
    "keymap.hunk_next",
    "keymap.hunk_prev",
    "keymap.rollback",
];

/// Vim のときも効く表と、Vim を使わないときだけの表
fn tables(vim: bool) -> impl Iterator<Item = &'static (&'static str, Action)> {
    ACTIONS.iter().chain(PLAIN_ACTIONS.iter().filter(move |_| !vim))
}

/// 今の場所で効く操作（`body` は本文にいるか）
fn live(vim: bool, body: bool) -> impl Iterator<Item = &'static (&'static str, Action)> {
    tables(vim).filter(move |(k, _)| body || !BODY_ONLY.contains(k))
}

/// すべての操作（設定の項目を作るとき）
pub fn all_actions() -> impl Iterator<Item = &'static (&'static str, Action)> {
    tables(false)
}

/// そのキーを Vim の側が持つか（vimrc の割り当て・IdeaVim の sethandler）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Claim {
    /// どちらでもない（既定どおり）
    None,
    /// vimrc で割り当てた・`sethandler … :vim`: 既定のキーマップより Vim を先に
    Vim,
    /// `sethandler … :ide`: Vim のときも IntelliJ のキー（Ctrl+B など、Vim を使わないときだけの表）を効かせる
    Ide,
}

/// 押したキーの組み合わせに付けた操作（設定で付け替えたものを含む）。どれにも付いていなければ None
#[cfg(test)]
pub fn action_for(settings: &Settings, chord: &Chord) -> Option<Action> {
    action_in(settings, chord, Claim::None, true)
}

/// `claim`（vimrc）を見て引く。強いものから: 利用者が設定で決めたキー → vimrc → IntelliJ と同じ既定
pub fn action_in(settings: &Settings, chord: &Chord, claim: Claim, body: bool) -> Option<Action> {
    let vim = settings.bool("vim.enabled") && claim != Claim::Ide;
    let hit = |k: &str| parse_chord(&settings.get(k)).as_ref() == Some(chord);
    if let Some(a) = live(vim, body).find(|(k, _)| settings.user_set(k) && hit(k)) {
        return Some(a.1);
    }
    // コマンドごとの項目（設定の画面のキーマップの下の方。keymap.cmd.名前。決まった項目ができたコマンドのものも）
    for k in settings.user_keys(crate::settings::CMD_KEY) {
        if hit(&k) {
            if let Some((c, _)) = crate::commands::find(&k[crate::settings::CMD_KEY.len()..]) {
                return Some(Action::Cmd(c.names[0]));
            }
        }
    }
    if claim == Claim::Vim {
        return None;
    }
    live(vim, body).find(|(k, _)| !settings.user_set(k) && hit(k)).map(|a| a.1)
}

/// IntelliJ のキーの組み合わせを Vim のキーに（vimrc の左辺・sethandler と比べる。Vim で書けないもの（Ctrl+Shift+A など）は None）
pub fn vim_key(chord: &Chord) -> Option<Key> {
    let one = {
        let mut cs = chord.key.chars();
        match (cs.next(), cs.next()) {
            (Some(c), None) => Some(c.to_ascii_lowercase()),
            _ => None,
        }
    };
    let nav = match chord.key.as_str() {
        "Up" => Some(Key::Up),
        "Down" => Some(Key::Down),
        "Left" => Some(Key::Left),
        "Right" => Some(Key::Right),
        "Home" => Some(Key::Home),
        "End" => Some(Key::End),
        "PageUp" => Some(Key::PageUp),
        "PageDown" => Some(Key::PageDown),
        "Backspace" => Some(Key::Backspace),
        "Delete" => Some(Key::Delete),
        "Tab" => Some(Key::Tab),
        _ => None,
    };
    if let Some(k) = nav {
        return k.with_mods(nagi_vim::Mods { shift: chord.shift, ctrl: chord.ctrl, alt: chord.alt });
    }
    match (chord.ctrl, chord.alt, chord.shift) {
        (true, false, false) if chord.key == "Space" => Some(Key::Ctrl(' ')),
        (true, false, false) => one.map(Key::Ctrl),
        (false, true, false) => one.map(Key::Alt),
        (false, false, false) => chord.key.strip_prefix('F').and_then(|n| n.parse().ok()).map(Key::F),
        _ => None,
    }
}

impl crate::editor::Editor {
    /// 今のモードで、そのキーを Vim の側が持つか（vimrc の割り当ての最初のキー・sethandler。後に書いたものが勝つ）
    pub fn vim_claim(&self, chord: &Chord) -> Claim {
        use crate::editor::State;
        if !self.vim_on {
            return Claim::None;
        }
        let mode = match self.state() {
            State::Normal => nagi_vim::Mode::Normal,
            State::Visual { .. } => nagi_vim::Mode::Visual,
            State::Insert if !self.in_text_field() => nagi_vim::Mode::Insert,
            _ => return Claim::None,
        };
        let Some(k) = vim_key(chord) else { return Claim::None };
        match self.vim_claims.iter().rev().find(|(m, x, _)| *m == mode && *x == k) {
            Some((_, _, true)) => Claim::Ide,
            Some((_, _, false)) => Claim::Vim,
            None => Claim::None,
        }
    }

    /// 本文（エディタ）にいるか（欄・入力欄・一覧ではない）
    pub fn in_body(&self) -> bool {
        use crate::editor::State;
        crate::component::focused(self).is_none() && matches!(self.state(), State::Normal | State::Visual { .. } | State::Insert) && !self.in_text_field()
    }

    /// 押した組み合わせの操作と、既定のキーから移ったか（設定で決めたキー → vimrc → 既定。本文だけの操作は本文で）
    pub fn chord_action(&self, chord: &Chord) -> (Option<Action>, bool) {
        let (claim, body) = (self.vim_claim(chord), self.in_body());
        (action_in(&self.settings, chord, claim, body), default_moved(&self.settings, chord, claim, body))
    }
}

/// vimrc から、Vim の側が持つキーの表（割り当ての最初のキー、その後に sethandler。Editor::vim_claim が引く）
pub fn vim_claims(rc: &nagi_vim::Rc) -> Vec<(nagi_vim::Mode, Key, bool)> {
    let mut v: Vec<(nagi_vim::Mode, Key, bool)> = Vec::new();
    for m in nagi_vim::Mode::ALL {
        // 既定の割り当て（git の <leader>…）は数えない
        v.extend(rc.keymap.list(m).iter().filter(|x| x.from != nagi_vim::DEFAULT_FROM).filter_map(|x| x.lhs.first()).map(|k| (m, *k, false)));
    }
    v.extend(rc.handlers.iter().map(|(k, m, ide)| (*m, *k, *ide)));
    v
}

/// エディタのキー（再生・マクロ・試験で届くもの）を組み合わせに（Alt+1・Ctrl+X・F12 など。文字そのものは None）
pub fn chord_of_key(key: Key) -> Option<Chord> {
    let c = |ctrl, alt, key: String| Some(Chord { ctrl, alt, shift: false, key });
    match key {
        Key::Alt(ch) if ch.is_ascii_graphic() => c(false, true, ch.to_ascii_uppercase().to_string()),
        Key::Ctrl(' ') => c(true, false, "Space".into()),
        Key::Ctrl(ch) if ch.is_ascii_graphic() => c(true, false, ch.to_ascii_uppercase().to_string()),
        Key::F(n) if (1..=24).contains(&n) => c(false, false, format!("F{n}")),
        _ => None,
    }
}

/// 既定のキーの組み合わせが、付け替えで別の操作に移った・外れたか（そのときは元のキーとしては扱わない。Vim の側が持つキーは除く）
pub fn default_moved(settings: &Settings, chord: &Chord, claim: Claim, body: bool) -> bool {
    let vim = settings.bool("vim.enabled") && claim != Claim::Ide;
    claim != Claim::Vim && live(vim, body).any(|(k, _)| {
        let def = crate::settings::schema(k).map(|i| i.default).unwrap_or("");
        parse_chord(def).as_ref() == Some(chord) && settings.get(k) != def
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// どのコマンドにもキーを付けられ、重なりは分かる
    #[test]
    fn any_command_gets_a_key() {
        let mut s = Settings::default();
        assert!(crate::settings::schema("keymap.cmd.Glog").is_some(), "コマンドごとの項目がある");
        assert!(crate::settings::schema("keymap.cmd.Run").is_none(), "決まった項目のあるものは作らない");
        s.set("keymap.cmd.Glog", "Ctrl+Shift+O", false);
        assert_eq!(action_for(&s, &parse_chord("ctrl+shift+o").unwrap()), Some(Action::Cmd("Glog")));
        s.set("keymap.cmd.Gdiff", "Shift+F10", false);
        let other: Vec<&str> = crate::settings::key_conflicts(&s, "keymap.cmd.Gdiff").iter().map(|i| i.key).collect();
        assert_eq!(other, ["keymap.run"]);
    }

    #[test]
    fn chords() {
        let c = parse_chord("ctrl + alt + s").unwrap();
        assert_eq!(c.label(), "Ctrl+Alt+S");
        assert_eq!(parse_chord("Shift+f10").unwrap().label(), "Shift+F10");
        assert_eq!(parse_chord("Ctrl+Space").unwrap().key, "Space");
        assert!(parse_chord("Hyper+X").is_none());
        assert!(parse_chord("F99").is_none());
        let mut s = Settings::default();
        assert_eq!(action_for(&s, &parse_chord("Shift+F10").unwrap()), Some(Action::Cmd("Run")));
        s.set("keymap.run", "Ctrl+R", false);
        assert_eq!(action_for(&s, &parse_chord("Ctrl+R").unwrap()), Some(Action::Cmd("Run")));
        // 操作の名前はどれもコマンドの表にある
        for (k, a) in all_actions() {
            if let Action::Cmd(c) = a {
                assert!(crate::commands::find(c).is_some(), ":{c} がコマンドの表に無い");
            }
            assert!(crate::settings::schema(k).is_some_and(|i| parse_chord(i.default).is_some()), "{k} の項目・既定のキーが無い");
        }
        // 既定のキーは重ならない
        let mut seen = std::collections::HashMap::new();
        for (k, _) in all_actions() {
            let d = crate::settings::schema(k).unwrap().default;
            assert!(seen.insert(parse_chord(d).unwrap(), *k).is_none(), "{d} が {k} と {:?} で重なる", seen.get(&parse_chord(d).unwrap()));
        }
        // 定義へは F12、デバッグは Visual Studio と同じ（Vim のときも）
        s.set("vim.enabled", "true", false);
        for (k, c) in [("F12", "GotoDefinition"), ("F5", "DebugResume"), ("F9", "Breakpoint"), ("F10", "StepOver"), ("F11", "StepInto"), ("Shift+F11", "StepOut"), ("Shift+F5", "DebugStop")] {
            assert_eq!(action_for(&s, &parse_chord(k).unwrap()), Some(Action::Cmd(c)), "{k}");
        }
        assert_eq!(action_for(&s, &parse_chord("Ctrl+N").unwrap()), None, "Vim のときは Vim の Ctrl-n");
        s.set("vim.enabled", "false", false);
        assert_eq!(action_for(&s, &parse_chord("Ctrl+N").unwrap()), Some(Action::Cmd("Types")));
        assert_eq!(action_for(&s, &parse_chord("Ctrl+Alt+L").unwrap()), Some(Action::Cmd("Format")));
        assert_eq!(chord_of_key(Key::Alt('1')).unwrap().label(), "Alt+1");
        assert!(default_moved(&s, &parse_chord("Shift+F10").unwrap(), Claim::None, true));
    }

    /// 設定で決めたキー → vimrc → 既定 の順
    #[test]
    fn user_and_vimrc_win() {
        let mut s = Settings::default();
        let f2 = parse_chord("F2").unwrap();
        assert_eq!(action_in(&s, &f2, Claim::None, true), Some(Action::Cmd("ErrorNext")));
        // vimrc で <F2> を割り当てていれば、既定より Vim
        assert_eq!(action_in(&s, &f2, Claim::Vim, true), None);
        assert!(!default_moved(&s, &f2, Claim::Vim, true));
        // 本文の外（下の欄の表など）では F2 はその欄のもの
        assert_eq!(action_in(&s, &f2, Claim::None, false), None);
        // 設定で決めたキーは vimrc より強い
        s.set("keymap.cmd.Glog", "F2", false);
        assert_eq!(action_in(&s, &f2, Claim::Vim, true), Some(Action::Cmd("Glog")));
        assert_eq!(action_in(&s, &f2, Claim::None, true), Some(Action::Cmd("Glog")), "既定の F2 より設定で決めたもの");
        // 決まった項目ができたコマンドの、前の keymap.cmd.名前 も効く
        s.set("keymap.cmd.Outline", "Ctrl+Shift+O", false);
        assert_eq!(action_for(&s, &parse_chord("Ctrl+Shift+O").unwrap()), Some(Action::Cmd("Outline")));
        // sethandler <C-n> a:ide: Vim のときも Ctrl+N は型へ
        s.set("vim.enabled", "true", false);
        let cn = parse_chord("Ctrl+N").unwrap();
        assert_eq!(action_in(&s, &cn, Claim::None, true), None);
        assert_eq!(action_in(&s, &cn, Claim::Ide, true), Some(Action::Cmd("Types")));
        assert_eq!(vim_key(&cn), Some(Key::Ctrl('n')));
        assert_eq!(vim_key(&parse_chord("Alt+Down").unwrap()).map(|k| k.label()).as_deref(), Some("<A-Down>"));
        assert_eq!(vim_key(&parse_chord("Ctrl+Shift+A").unwrap()), None);
        // vimrc の割り当て・sethandler から
        let rc = nagi_vim::parse_rc("nnoremap <F2> :echo<CR>\nsethandler <C-b> a:ide\n");
        let c = vim_claims(&rc);
        assert!(c.contains(&(nagi_vim::Mode::Normal, Key::F(2), false)));
        assert!(c.contains(&(nagi_vim::Mode::Insert, Key::Ctrl('b'), true)));
    }
}
