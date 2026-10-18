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
pub const ACTIONS: [(&str, Action); 14] = [
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
    ("keymap.git", Action::Cmd("ToolGit")),
    ("keymap.find", Action::Cmd("ToolFind")),
    ("keymap.problems", Action::Cmd("ToolProblems")),
    ("keymap.log", Action::Cmd("ToolLog")),
];

/// 押したキーの組み合わせに付けた操作（設定で付け替えたものを含む）。どれにも付いていなければ None
pub fn action_for(settings: &Settings, chord: &Chord) -> Option<Action> {
    ACTIONS.iter().find(|(k, _)| parse_chord(&settings.get(k)).as_ref() == Some(chord)).map(|a| a.1)
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
    ACTIONS.iter().any(|(k, _)| {
        let def = crate::settings::schema(k).map(|i| i.default).unwrap_or("");
        parse_chord(def).as_ref() == Some(chord) && settings.get(k) != def
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
