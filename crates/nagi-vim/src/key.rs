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
}

impl Key {
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
        "lt" => Key::Char('<'),
        "bar" => Key::Char('|'),
        "bslash" => Key::Char('\\'),
        "leader" => return Some(parse_keys(leader, leader)),
        "nop" => return Some(Vec::new()),
        _ => {
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
    }
}
