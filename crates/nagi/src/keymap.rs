//! キーマップ（設定の「キーマップ」）。`Ctrl+Alt+S` のような書き方のキーの組み合わせと、それに付けた操作。
//!
//! main.rs の on_key で、押したキーを組み合わせの文字に直して `action_for` で引き、当たれば操作のキー（VKey）に置き換える。
//! 操作のキーは、エディタの中で操作を表す内部のキー（F(40) 実行 など）か、元からあるキー（Alt+1 など）。

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

/// 操作（設定のキー, 操作のキー）
pub const ACTIONS: [(&str, Key); 14] = [
    ("keymap.settings", Key::F(45)),
    ("keymap.complete", Key::Ctrl(' ')),
    ("keymap.run", Key::F(40)),
    ("keymap.run_choose", Key::F(41)),
    ("keymap.run_stop", Key::F(42)),
    ("keymap.run_again", Key::F(43)),
    ("keymap.goto_def", Key::F(12)),
    ("keymap.terminal", Key::F(24)),
    ("keymap.project", Key::Alt('1')),
    ("keymap.commit", Key::Alt('0')),
    ("keymap.git", Key::Alt('9')),
    ("keymap.find", Key::Alt('3')),
    ("keymap.problems", Key::Alt('6')),
    ("keymap.log", Key::Alt('4')),
];

/// 押したキーの組み合わせに付けた操作のキー（設定で付け替えたものを含む）。どれにも付いていなければ None
pub fn action_for(settings: &Settings, chord: &Chord) -> Option<Key> {
    ACTIONS.iter().find(|(k, _)| parse_chord(&settings.get(k)).as_ref() == Some(chord)).map(|a| a.1)
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
        assert_eq!(action_for(&s, &parse_chord("Shift+F10").unwrap()), Some(Key::F(40)));
        s.set("keymap.run", "Ctrl+R", false);
        assert_eq!(action_for(&s, &parse_chord("Ctrl+R").unwrap()), Some(Key::F(40)));
        assert!(default_moved(&s, &parse_chord("Shift+F10").unwrap()));
    }
}
