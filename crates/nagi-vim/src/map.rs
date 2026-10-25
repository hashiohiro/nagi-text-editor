//! マッピングの照合（Vim の nnoremap / nmap と同じ規則）。
//!
//! - 入力中のキー列が、ある左辺の「途中まで」に一致している間は待つ（timeoutlen が切れたら確定）。
//! - 左辺に完全一致し、それより長い左辺の途中でもなければ、すぐに右辺へ置き換える。
//! - どの左辺にも続かなくなったら、先頭のキーをそのまま流し、残りをもう一度照合する。

use crate::Key;

#[derive(Clone, Debug)]
pub struct Mapping {
    pub lhs: Vec<Key>,
    pub rhs: Vec<Key>,
    /// true なら右辺を再マップしない（noremap）
    pub noremap: bool,
    /// どこで決めたか（"ファイル:行"・"nagi の既定"。設定の画面に出す）
    pub from: String,
}

/// 割り当てのモード（Vim の n・x（v）・i）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    Normal,
    Visual,
    Insert,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Normal, Mode::Visual, Mode::Insert];

    /// vimrc の頭の 1 文字（n・x・i）
    pub fn letter(self) -> char {
        match self {
            Mode::Normal => 'n',
            Mode::Visual => 'x',
            Mode::Insert => 'i',
        }
    }
}

#[derive(Default, Clone, Debug)]
pub struct Keymap {
    pub normal: Vec<Mapping>,
    pub visual: Vec<Mapping>,
    pub insert: Vec<Mapping>,
}

impl Keymap {
    pub fn list(&self, mode: Mode) -> &Vec<Mapping> {
        match mode {
            Mode::Normal => &self.normal,
            Mode::Visual => &self.visual,
            Mode::Insert => &self.insert,
        }
    }

    fn list_mut(&mut self, mode: Mode) -> &mut Vec<Mapping> {
        match mode {
            Mode::Normal => &mut self.normal,
            Mode::Visual => &mut self.visual,
            Mode::Insert => &mut self.insert,
        }
    }

    /// Normal の割り当てを足す
    pub fn add(&mut self, lhs: Vec<Key>, rhs: Vec<Key>, noremap: bool) {
        self.add_in(Mode::Normal, lhs, rhs, noremap, String::new());
    }

    /// 同じ左辺は後から定義したものが勝つ（Vim と同じ）
    pub fn add_in(&mut self, mode: Mode, lhs: Vec<Key>, rhs: Vec<Key>, noremap: bool, from: String) {
        if lhs.is_empty() {
            return;
        }
        let l = self.list_mut(mode);
        l.retain(|m| m.lhs != lhs);
        l.push(Mapping { lhs, rhs, noremap, from });
    }

    /// 割り当てを外す（unmap）。外したら true
    pub fn remove(&mut self, mode: Mode, lhs: &[Key]) -> bool {
        let l = self.list_mut(mode);
        let n = l.len();
        l.retain(|m| m.lhs != lhs);
        l.len() != n
    }

    pub fn len(&self) -> usize {
        self.normal.len() + self.visual.len() + self.insert.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 1 つのモードの割り当て（照合に使う）
struct Maps<'a>(&'a [Mapping]);

impl Maps<'_> {
    fn exact(&self, keys: &[Key]) -> Option<&Mapping> {
        self.0.iter().find(|m| m.lhs == keys)
    }

    fn has_longer(&self, keys: &[Key]) -> bool {
        self.0.iter().any(|m| m.lhs.len() > keys.len() && m.lhs.starts_with(keys))
    }
}

/// 照合の結果。`Emit` のキーはエディタへそのまま渡す（再マップ済み）
#[derive(Debug, PartialEq, Eq)]
pub enum Feed {
    /// 続きのキーを待っている（timeoutlen の間）
    Pending,
    Emit(Vec<Key>),
}

pub struct Matcher {
    buf: Vec<Key>,
    /// 待っているキーを打ったモード（timeoutlen が切れたときに使う）
    mode: Mode,
}

impl Default for Matcher {
    fn default() -> Self {
        Matcher { buf: Vec::new(), mode: Mode::Normal }
    }
}

const MAX_DEPTH: usize = 20;

impl Matcher {
    /// 待っているキー列（showcmd 用）
    pub fn pending(&self) -> &[Key] {
        &self.buf
    }

    pub fn feed(&mut self, km: &Keymap, key: Key) -> Feed {
        self.feed_in(km, Mode::Normal, key)
    }

    pub fn feed_in(&mut self, km: &Keymap, mode: Mode, key: Key) -> Feed {
        if self.buf.is_empty() {
            self.mode = mode;
        }
        let km = &Maps(km.list(self.mode));
        self.buf.push(key);
        let mut out = Vec::new();
        loop {
            if self.buf.is_empty() {
                break;
            }
            if km.has_longer(&self.buf) {
                return if out.is_empty() { Feed::Pending } else { Feed::Emit(out) };
            }
            if let Some(m) = km.exact(&self.buf).cloned() {
                self.buf.clear();
                expand(km, &m, &mut out, 0);
                break;
            }
            // どの左辺にも続かない: 先頭を素通しして残りを照合し直す
            let first = self.buf.remove(0);
            out.push(first);
        }
        Feed::Emit(out)
    }

    /// timeoutlen が切れた: 完全一致していれば展開し、そうでなければそのまま流す
    pub fn timeout(&mut self, km: &Keymap) -> Vec<Key> {
        let km = &Maps(km.list(self.mode));
        let mut out = Vec::new();
        while !self.buf.is_empty() {
            // 最も長い完全一致の左辺を探す
            let n = (1..=self.buf.len()).rev().find(|&n| km.exact(&self.buf[..n]).is_some());
            match n {
                Some(n) => {
                    let m = km.exact(&self.buf[..n]).cloned().unwrap();
                    self.buf.drain(..n);
                    expand(km, &m, &mut out, 0);
                }
                None => out.push(self.buf.remove(0)),
            }
        }
        out
    }
}

/// 右辺を展開する。nmap（noremap でない）なら右辺をもう一度照合する
fn expand(km: &Maps, m: &Mapping, out: &mut Vec<Key>, depth: usize) {
    if m.noremap || depth >= MAX_DEPTH {
        out.extend(m.rhs.iter().copied());
        return;
    }
    let mut i = 0;
    while i < m.rhs.len() {
        let rest = &m.rhs[i..];
        let n = (1..=rest.len()).rev().find(|&n| km.exact(&rest[..n]).is_some());
        match n {
            Some(n) => {
                let inner = km.exact(&rest[..n]).cloned().unwrap();
                expand(km, &inner, out, depth + 1);
                i += n;
            }
            None => {
                out.push(rest[0]);
                i += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_keys;

    fn km() -> Keymap {
        let mut k = Keymap::default();
        let p = |s| parse_keys(s, " ");
        k.add(p("ew"), p(":<C-u>w<CR>"), true);
        k.add(p("p"), p("]p"), true);
        k.add(p("]p"), p("p"), true);
        k.add(p("<leader>ff"), p(":<C-u>action Find<CR>"), true);
        k
    }

    #[test]
    fn prefix_waits_then_expands() {
        let k = km();
        let mut m = Matcher::default();
        assert_eq!(m.feed(&k, Key::Char('e')), Feed::Pending);
        assert_eq!(m.feed(&k, Key::Char('w')), Feed::Emit(parse_keys(":<C-u>w<CR>", " ")));
    }

    #[test]
    fn prefix_then_other_key_passes_through() {
        let k = km();
        let mut m = Matcher::default();
        assert_eq!(m.feed(&k, Key::Char('e')), Feed::Pending);
        assert_eq!(m.feed(&k, Key::Char('x')), Feed::Emit(vec![Key::Char('e'), Key::Char('x')]));
    }

    #[test]
    fn timeout_flushes_raw() {
        let k = km();
        let mut m = Matcher::default();
        assert_eq!(m.feed(&k, Key::Char('e')), Feed::Pending);
        assert_eq!(m.timeout(&k), vec![Key::Char('e')]);
    }

    #[test]
    fn noremap_swap_does_not_loop() {
        let k = km();
        let mut m = Matcher::default();
        assert_eq!(m.feed(&k, Key::Char('p')), Feed::Emit(vec![Key::Char(']'), Key::Char('p')]));
        assert_eq!(m.feed(&k, Key::Char(']')), Feed::Pending);
        assert_eq!(m.feed(&k, Key::Char('p')), Feed::Emit(vec![Key::Char('p')]));
    }

    #[test]
    fn leader() {
        let k = km();
        let mut m = Matcher::default();
        assert_eq!(m.feed(&k, Key::Char(' ')), Feed::Pending);
        assert_eq!(m.feed(&k, Key::Char('f')), Feed::Pending);
        assert_eq!(m.feed(&k, Key::Char('f')), Feed::Emit(parse_keys(":<C-u>action Find<CR>", " ")));
    }
}
