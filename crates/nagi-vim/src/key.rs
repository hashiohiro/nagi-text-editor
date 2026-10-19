//! キーと、Vim のキー表記（`<C-s>` `<CR>` `<leader>` など）の解釈

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Key {
    /// 文字（Shift 込み。Space は ' '）
    Char(char),
    /// Ctrl＋文字（小文字に正規化）
    Ctrl(char),
    /// Alt＋文字（小文字に正規化）
    Alt(char),
    Esc,
    Enter,
    Tab,
    Backspace,
    Delete,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
    /// Shift+Insert（貼り付け）
    Paste,
    /// Shift・Ctrl・Alt を付けた移動・削除のキー（Shift+→ で選ぶ、Ctrl+← で単語へ など。Vim を使わないときの編集）。
    /// 修飾の無いものは Up などのまま
    Mod(Nav, Mods),
}

/// 修飾を付けられる移動・削除のキー
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Nav {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Backspace,
    Delete,
    Tab,
}

/// 修飾キー
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

impl Nav {
    /// 修飾の無いキー
    pub fn key(self) -> Key {
        match self {
            Nav::Up => Key::Up,
            Nav::Down => Key::Down,
            Nav::Left => Key::Left,
            Nav::Right => Key::Right,
            Nav::Home => Key::Home,
            Nav::End => Key::End,
            Nav::PageUp => Key::PageUp,
            Nav::PageDown => Key::PageDown,
            Nav::Backspace => Key::Backspace,
            Nav::Delete => Key::Delete,
            Nav::Tab => Key::Tab,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Nav::Up => "Up",
            Nav::Down => "Down",
            Nav::Left => "Left",
            Nav::Right => "Right",
            Nav::Home => "Home",
            Nav::End => "End",
            Nav::PageUp => "PageUp",
            Nav::PageDown => "PageDown",
            Nav::Backspace => "BS",
            Nav::Delete => "Del",
            Nav::Tab => "Tab",
        }
    }
}

impl Key {
    /// 修飾を外したキー（Vim の操作・パネルは修飾を見ない）
    pub fn base(self) -> Key {
        match self {
            Key::Mod(n, _) => n.key(),
            k => k,
        }
    }

    /// 修飾を付ける（どれも付かなければ元のキー。移動・削除のキーでなければ None）
    pub fn with_mods(self, m: Mods) -> Option<Key> {
        let n = match self {
            Key::Up => Nav::Up,
            Key::Down => Nav::Down,
            Key::Left => Nav::Left,
            Key::Right => Nav::Right,
            Key::Home => Nav::Home,
            Key::End => Nav::End,
            Key::PageUp => Nav::PageUp,
            Key::PageDown => Nav::PageDown,
            Key::Backspace => Nav::Backspace,
            Key::Delete => Nav::Delete,
            Key::Tab => Nav::Tab,
            _ => return None,
        };
        Some(if m == Mods::default() { self } else { Key::Mod(n, m) })
    }

    /// `parse_keys` で読み戻せる表記（マクロをレジスタに文字列で入れるため）
    pub fn notation(&self) -> String {
        match self {
            Key::Char('<') => "<lt>".into(),
            Key::Char(c) => c.to_string(),
            Key::Ctrl(c) => format!("<C-{c}>"),
            k => k.label(),
        }
    }

    /// 画面表示用（showcmd）
    pub fn label(&self) -> String {
        match self {
            Key::Char(' ') => "<Space>".into(),
            Key::Char(c) => c.to_string(),
            Key::Ctrl(c) => format!("^{}", c.to_ascii_uppercase()),
            Key::Alt(c) => format!("<M-{c}>"),
            Key::Esc => "<Esc>".into(),
            Key::Enter => "<CR>".into(),
            Key::Tab => "<Tab>".into(),
            Key::Backspace => "<BS>".into(),
            Key::Delete => "<Del>".into(),
            Key::Up => "<Up>".into(),
            Key::Down => "<Down>".into(),
            Key::Left => "<Left>".into(),
            Key::Right => "<Right>".into(),
            Key::Home => "<Home>".into(),
            Key::End => "<End>".into(),
            Key::PageUp => "<PageUp>".into(),
            Key::PageDown => "<PageDown>".into(),
            Key::F(n) => format!("<F{n}>"),
            Key::Paste => "<S-Insert>".into(),
            Key::Mod(n, m) => format!("<{}{}{}{}>", if m.ctrl { "C-" } else { "" }, if m.alt { "A-" } else { "" }, if m.shift { "S-" } else { "" }, n.name()),
        }
    }
}

fn named(name: &str, leader: &str) -> Option<Vec<Key>> {
    let lower = name.to_ascii_lowercase();
    let k = match lower.as_str() {
        "cr" | "enter" | "return" => Key::Enter,
        "esc" => Key::Esc,
        "tab" => Key::Tab,
        "bs" | "backspace" => Key::Backspace,
        "del" | "delete" => Key::Delete,
        "space" => Key::Char(' '),
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "s-insert" => Key::Paste,
        "lt" => Key::Char('<'),
        "bar" => Key::Char('|'),
        "bslash" => Key::Char('\\'),
        "leader" => return Some(parse_keys(leader, leader)),
        "nop" => return Some(Vec::new()),
        "c-space" | "c-@" => Key::Ctrl(' '),
        _ => {
            // 修飾つきの移動・削除（<S-Right>・<C-Left>・<C-S-Home> など）
            let (mut m, mut rest) = (Mods::default(), lower.as_str());
            loop {
                if let Some(r) = rest.strip_prefix("s-") {
                    m.shift = true;
                    rest = r;
                } else if let Some(r) = rest.strip_prefix("c-").filter(|r| r.len() > 1) {
                    m.ctrl = true;
                    rest = r;
                } else if let Some(r) = rest.strip_prefix("a-").or(rest.strip_prefix("m-")).filter(|r| r.len() > 1) {
                    m.alt = true;
                    rest = r;
                } else {
                    break;
                }
            }
            if m != Mods::default() {
                if let Some(k) = named(rest, leader).and_then(|v| v.first().copied()).and_then(|k| k.with_mods(m)) {
                    return Some(vec![k]);
                }
            }
            if let Some(rest) = lower.strip_prefix("m-").or(lower.strip_prefix("a-")) {
                let mut cs = rest.chars();
                let c = cs.next()?;
                if cs.next().is_none() {
                    return Some(vec![Key::Alt(c)]);
                }
                return None;
            }
            if let Some(rest) = lower.strip_prefix("c-") {
                let mut cs = rest.chars();
                let c = cs.next()?;
                if cs.next().is_none() {
                    return Some(vec![Key::Ctrl(c)]);
                }
                return None;
            }
            if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                return Some(vec![Key::F(n)]);
            }
            return None;
        }
    };
    Some(vec![k])
}

/// Vim のキー表記をキー列にする。解釈できない `<...>` は文字どおりに扱う（Vim と同じ）。
pub fn parse_keys(s: &str, leader: &str) -> Vec<Key> {
    let mut out = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '<' {
            if let Some(end) = chars[i + 1..].iter().position(|&c| c == '>') {
                let name: String = chars[i + 1..i + 1 + end].iter().collect();
                if let Some(keys) = named(&name, leader) {
                    out.extend(keys);
                    i += end + 2;
                    continue;
                }
            }
        }
        // `\|` はマッピングの右辺の中の | （コマンドの区切りではない）
        if c == '\\' && chars.get(i + 1) == Some(&'|') {
            out.push(Key::Char('|'));
            i += 2;
            continue;
        }
        out.push(Key::Char(c));
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notation() {
        assert_eq!(parse_keys("<leader>ff", " "), vec![Key::Char(' '), Key::Char('f'), Key::Char('f')]);
        assert_eq!(parse_keys(":<C-u>w<CR>", " "), vec![Key::Char(':'), Key::Ctrl('u'), Key::Char('w'), Key::Enter]);
        assert_eq!(parse_keys("<UP>", " "), vec![Key::Up]);
        assert_eq!(parse_keys("<leader><CR>", " "), vec![Key::Char(' '), Key::Enter]);
        assert_eq!(parse_keys("a\\|b", " "), vec![Key::Char('a'), Key::Char('|'), Key::Char('b')]);
        let sr = Key::Mod(Nav::Right, Mods { shift: true, ..Default::default() });
        assert_eq!(parse_keys("<S-Right>", " "), vec![sr]);
        assert_eq!(parse_keys(&sr.label(), " "), vec![sr]);
        let csh = Key::Mod(Nav::Home, Mods { shift: true, ctrl: true, alt: false });
        assert_eq!(parse_keys("<C-S-Home>", " "), vec![csh]);
        assert_eq!(parse_keys("<C-u>", " "), vec![Key::Ctrl('u')], "C-文字は今まで通り");
    }
}
