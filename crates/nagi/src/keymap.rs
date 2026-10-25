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

/// 操作（設定のキー, 操作）
pub const ACTIONS: [(&str, Action); 15] = [
    ("keymap.settings", Action::Cmd("Settings")),
    ("keymap.complete", Action::Key(Key::Ctrl(' '))),
    ("keymap.run", Action::Cmd("Run")),
    ("keymap.run_choose", Action::Cmd("RunConfigs")),
    ("keymap.run_stop", Action::Cmd("RunStop")),
    ("keymap.run_again", Action::Cmd("RunAgain")),
    ("keymap.goto_def", Action::Cmd("GotoDefinition")),
    ("keymap.terminal", Action::Cmd("ToolTerminal")),
    ("keymap.project", Action::Cmd("ToolProject")),
    ("keymap.commit", Action::Cmd("ToolCommit")),
    ("keymap.run_panel", Action::Cmd("ToolRun")),
    ("keymap.git", Action::Cmd("ToolGit")),
    ("keymap.find", Action::Cmd("ToolFind")),
    ("keymap.problems", Action::Cmd("ToolProblems")),
    ("keymap.log", Action::Cmd("ToolLog")),
];

/// Vim のキー操作を使わないときだけ効く組み合わせ（Ctrl+C・V・Z など。Vim では Ctrl+V は矩形選択、Ctrl+R はやり直しなので）。
/// 既定は IntelliJ と同じ
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
    ("keymap.palette", Action::Cmd("Commands")),
    ("keymap.goto_file", Action::Cmd("Files")),
    ("keymap.recent", Action::Cmd("Recent")),
    ("keymap.find_in_files", Action::Cmd("Grep")),
];

/// 押したキーの組み合わせに付けた操作（設定で付け替えたものを含む）。どれにも付いていなければ None。
/// `vim` が false（Vim のキー操作を使わない）なら PLAIN_ACTIONS も見る
pub fn action_for(settings: &Settings, chord: &Chord) -> Option<Action> {
    let vim = settings.bool("vim.enabled");
    if let Some(a) = ACTIONS.iter().chain(PLAIN_ACTIONS.iter().filter(|_| !vim)).find(|(k, _)| parse_chord(&settings.get(k)).as_ref() == Some(chord)) {
        return Some(a.1);
    }
    // コマンドごとの項目（設定の画面のキーマップの下の方。keymap.cmd.名前）
    crate::settings::items().iter().filter_map(|i| Some((i, i.key.strip_prefix(crate::settings::CMD_KEY)?))).find(|(i, _)| parse_chord(&settings.get(i.key)).as_ref() == Some(chord)).map(|(_, name)| Action::Cmd(name))
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

/// 既定のキーの組み合わせが、付け替えで別の操作に移った・外れたか（そのときは元のキーとしては扱わない）
pub fn default_moved(settings: &Settings, chord: &Chord) -> bool {
    let vim = settings.bool("vim.enabled");
    ACTIONS.iter().chain(PLAIN_ACTIONS.iter().filter(|_| !vim)).any(|(k, _)| {
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
        assert!(crate::settings::schema("keymap.cmd.Outline").is_some(), "コマンドごとの項目がある");
        assert!(crate::settings::schema("keymap.cmd.Run").is_none(), "決まった項目のあるものは作らない");
        s.set("keymap.cmd.Outline", "Ctrl+Shift+O", false);
        assert_eq!(action_for(&s, &parse_chord("ctrl+shift+o").unwrap()), Some(Action::Cmd("Outline")));
        s.set("keymap.cmd.Symbols", "Shift+F10", false);
        let other: Vec<&str> = crate::settings::key_conflicts(&s, "keymap.cmd.Symbols").iter().map(|i| i.key).collect();
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
        for (_, a) in ACTIONS {
            if let Action::Cmd(c) = a {
                assert!(crate::commands::find(c).is_some(), ":{c} がコマンドの表に無い");
            }
        }
        assert_eq!(chord_of_key(Key::Alt('1')).unwrap().label(), "Alt+1");
        assert!(default_moved(&s, &parse_chord("Shift+F10").unwrap()));
    }
}
