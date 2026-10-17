//! Normal / Visual モードのキー列を、実行する「コマンド」に解釈する（`2d3w` → 削除・単語・6 回）。
//!
//! 解釈だけを行い、実行はエディタが受け持つ。キーを 1 つ受け取るたびに先頭から解釈し直し、
//! 足りなければ `Incomplete`、意味をなさなければ `Invalid` を返す。

use crate::Key;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordFwd { big: bool },
    WordBack { big: bool },
    WordEnd { big: bool },
    LineStart,
    FirstNonBlank,
    LineEnd,
    /// `G`（回数があればその行、無ければ最終行）
    GotoLine,
    /// `gg`（回数があればその行、無ければ先頭行）
    FirstLine,
    /// f / F / t / T
    Find { ch: char, forward: bool, till: bool },
    /// `;`（reverse=false）と `,`（reverse=true）
    RepeatFind { reverse: bool },
    /// `%`
    MatchPair,
    /// `}` / `{`
    Paragraph { forward: bool },
    /// `n` / `N`
    SearchNext { reverse: bool },
    /// `'a`（行単位、最初の空白でない文字へ）と `` `a ``（その位置へ）
    Mark { ch: char, exact: bool },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextObj {
    Word { big: bool },
    /// " ' `
    Quote(char),
    /// ( [ { <（開き括弧で持つ）
    Block(char),
    Paragraph,
    /// tree-sitter の関数（if / af）・クラス（ic / ac）・引数（ia / aa）
    Function,
    Class,
    Argument,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    Delete,
    Change,
    Yank,
    Indent,
    Dedent,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Motion(Motion),
    /// dd / cc / yy / >> / <<
    Lines,
    Obj { inner: bool, obj: TextObj },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// i a I A o O
    Insert(char),
    /// x X s S C D Y
    Short(char),
    /// p P（indent=true は ]p ]P）
    Put { after: bool, indent: bool },
    Join,
    Replace(char),
    ToggleCase,
    Undo,
    Redo,
    Dot,
    /// `:` `/` `?`
    Cmdline(char),
    /// `*` / `#`
    SearchWord { forward: bool },
    /// v / V
    Visual { line: bool },
    /// Ctrl-v（Ctrl-q）
    VisualBlock,
    Scroll { half: bool, down: bool },
    LastChange,
    /// `Ctrl-^`（直前のバッファ）
    AltBuffer,
    /// `Ctrl-w {c}`（窓の操作。`Ctrl-w Ctrl-h` などは `h` と同じ）
    Window(char),
    /// `m{a-zA-Z}`
    SetMark(char),
    /// `Ctrl-o` / `Ctrl-i`（ジャンプリストを戻る・進む）
    Jump { older: bool },
    /// `q{reg}`（マクロの記録を始める。止める `q` はエディタが受ける）
    Record(char),
    /// `@{reg}`（`@@` は直前のマクロ）
    Play(char),
    /// `z{c}`（折りたたみ: c o a M R E j k、Visual の f）
    Fold(char),
    /// `zz` `zt` `zb`（カーソルの行を画面の中央・上・下へ）
    ScrollCursor(char),
    /// `]c` / `[c`（次・前の変更へ）
    Hunk { next: bool },
    /// `gd` / `gD` / `Ctrl-]`（定義へ）
    GotoDef,
    /// `gI`（実装へ。IntelliJ の Go to Implementation）
    GotoImpl,
    /// `gr`（参照を探す）
    References,
    /// `g-` / `g+`（undo の木を時間の順に）
    UndoTime { later: bool },
    /// `&`（直前の :s を今の行に）と `g&`（フラグごと全体に）
    RepeatSub { all: bool },
    Esc,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Move(Motion),
    Operate(Op, Target),
    Act(Action),
    /// Visual モードで選択範囲に対して行う操作
    VisualOp(Op),
    /// Visual モードで選択範囲を text object に広げる
    VisualObj { inner: bool, obj: TextObj },
    /// Visual モードの `o`（選択の両端を入れ替える）
    VisualSwap,
    /// Visual block の `O`（同じ行の反対の角へ）
    VisualCorner,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cmd {
    /// 回数（`2d3w` は 6）。省略は None
    pub count: Option<usize>,
    /// `"a` のレジスタ
    pub reg: Option<char>,
    pub kind: Kind,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Parse {
    Incomplete,
    Done(Cmd),
    Invalid,
}

struct P<'a> {
    keys: &'a [Key],
    i: usize,
}

impl P<'_> {
    fn next(&mut self) -> Option<Key> {
        let k = self.keys.get(self.i).copied();
        self.i += 1;
        k
    }

    fn peek(&self) -> Option<Key> {
        self.keys.get(self.i).copied()
    }

    /// 回数（先頭の 0 は回数ではなく `0` 移動）
    fn count(&mut self) -> Option<usize> {
        let mut n: Option<usize> = None;
        while let Some(Key::Char(c @ '0'..='9')) = self.peek() {
            if c == '0' && n.is_none() {
                break;
            }
            n = Some(n.unwrap_or(0).saturating_mul(10).saturating_add(c as usize - '0' as usize));
            self.i += 1;
        }
        n
    }
}

/// 移動の解釈。None なら移動ではない。Some(None) は続きのキーが要る
fn motion(p: &mut P) -> Option<Option<Motion>> {
    let k = p.peek()?;
    let m = match k {
        Key::Char('h') | Key::Left | Key::Backspace => Motion::Left,
        Key::Char('l') | Key::Right | Key::Char(' ') => Motion::Right,
        Key::Char('j') | Key::Down | Key::Ctrl('n') => Motion::Down,
        Key::Char('k') | Key::Up | Key::Ctrl('p') => Motion::Up,
        Key::Char('w') => Motion::WordFwd { big: false },
        Key::Char('W') => Motion::WordFwd { big: true },
        Key::Char('b') => Motion::WordBack { big: false },
        Key::Char('B') => Motion::WordBack { big: true },
        Key::Char('e') => Motion::WordEnd { big: false },
        Key::Char('E') => Motion::WordEnd { big: true },
        Key::Char('0') | Key::Home => Motion::LineStart,
        Key::Char('^') => Motion::FirstNonBlank,
        Key::Char('$') | Key::End => Motion::LineEnd,
        Key::Char('G') => Motion::GotoLine,
        Key::Char(';') => Motion::RepeatFind { reverse: false },
        Key::Char(',') => Motion::RepeatFind { reverse: true },
        Key::Char('%') => Motion::MatchPair,
        Key::Char('}') => Motion::Paragraph { forward: true },
        Key::Char('{') => Motion::Paragraph { forward: false },
        Key::Char('n') => Motion::SearchNext { reverse: false },
        Key::Char('N') => Motion::SearchNext { reverse: true },
        Key::Char(q @ ('\'' | '`')) => {
            p.i += 1;
            return match p.next() {
                None => Some(None),
                Some(Key::Char(ch)) => Some(Some(Motion::Mark { ch, exact: q == '`' })),
                Some(_) => None,
            };
        }
        Key::Char(c @ ('f' | 'F' | 't' | 'T')) => {
            p.i += 1;
            return match p.next() {
                None => Some(None),
                Some(Key::Char(ch)) => Some(Some(Motion::Find { ch, forward: c.is_lowercase(), till: c == 't' || c == 'T' })),
                Some(_) => None,
            };
        }
        Key::Char('g') => {
            return match p.keys.get(p.i + 1) {
                None => Some(None),
                Some(Key::Char('g')) => {
                    p.i += 2;
                    Some(Some(Motion::FirstLine))
                }
                // 折り返しをしない間は gj / gk は j / k と同じ
                Some(Key::Char('j')) => {
                    p.i += 2;
                    Some(Some(Motion::Down))
                }
                Some(Key::Char('k')) => {
                    p.i += 2;
                    Some(Some(Motion::Up))
                }
                _ => None,
            };
        }
        _ => return None,
    };
    p.i += 1;
    Some(Some(m))
}

fn text_obj(p: &mut P) -> Option<Option<(bool, TextObj)>> {
    let inner = match p.peek()? {
        Key::Char('i') => true,
        Key::Char('a') => false,
        _ => return None,
    };
    p.i += 1;
    let obj = match p.next() {
        None => return Some(None),
        Some(Key::Char(c)) => match c {
            'w' => TextObj::Word { big: false },
            'W' => TextObj::Word { big: true },
            '"' | '\'' | '`' => TextObj::Quote(c),
            '(' | ')' | 'b' => TextObj::Block('('),
            '[' | ']' => TextObj::Block('['),
            '{' | '}' | 'B' => TextObj::Block('{'),
            '<' | '>' => TextObj::Block('<'),
            'p' => TextObj::Paragraph,
            'f' => TextObj::Function,
            'c' => TextObj::Class,
            'a' => TextObj::Argument,
            _ => return None,
        },
        Some(_) => return None,
    };
    Some(Some((inner, obj)))
}

fn op_of(k: Key) -> Option<Op> {
    Some(match k {
        Key::Char('d') => Op::Delete,
        Key::Char('c') => Op::Change,
        Key::Char('y') => Op::Yank,
        Key::Char('>') => Op::Indent,
        Key::Char('<') => Op::Dedent,
        _ => return None,
    })
}

fn mul(a: Option<usize>, b: Option<usize>) -> Option<usize> {
    match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(1).saturating_mul(b.unwrap_or(1))),
    }
}

/// レジスタと回数の前置き（`"a3`）
fn prefix(p: &mut P) -> Result<(Option<char>, Option<usize>), Parse> {
    let mut reg = None;
    let mut count = p.count();
    if p.peek() == Some(Key::Char('"')) {
        p.i += 1;
        match p.next() {
            None => return Err(Parse::Incomplete),
            Some(Key::Char(r)) => reg = Some(r),
            Some(_) => return Err(Parse::Invalid),
        }
        count = mul(count, p.count());
    }
    Ok((reg, count))
}

pub fn parse_normal(keys: &[Key]) -> Parse {
    let mut p = P { keys, i: 0 };
    let (reg, count) = match prefix(&mut p) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let done = |kind| Parse::Done(Cmd { count, reg, kind });
    let Some(k) = p.peek() else { return Parse::Incomplete };

    // オペレーター
    if let Some(op) = op_of(k) {
        p.i += 1;
        let c2 = p.count();
        let count = mul(count, c2);
        let done = |t| Parse::Done(Cmd { count, reg, kind: Kind::Operate(op, t) });
        let Some(k2) = p.peek() else { return Parse::Incomplete };
        if k2 == k {
            return done(Target::Lines);
        }
        if let Some(r) = text_obj(&mut p) {
            return match r {
                None => Parse::Incomplete,
                Some((inner, obj)) => done(Target::Obj { inner, obj }),
            };
        }
        return match motion(&mut p) {
            Some(None) => Parse::Incomplete,
            Some(Some(m)) => done(Target::Motion(m)),
            None => Parse::Invalid,
        };
    }

    match motion(&mut p) {
        Some(None) => return Parse::Incomplete,
        Some(Some(m)) => return done(Kind::Move(m)),
        None => {}
    }

    p.i += 1;
    let act = match k {
        Key::Char(c @ ('i' | 'a' | 'I' | 'A' | 'o' | 'O')) => Action::Insert(c),
        Key::Char(c @ ('x' | 'X' | 's' | 'S' | 'C' | 'D' | 'Y')) => Action::Short(c),
        Key::Char('p') => Action::Put { after: true, indent: false },
        Key::Char('P') => Action::Put { after: false, indent: false },
        Key::Char(']') | Key::Char('[') => match p.next() {
            None => return Parse::Incomplete,
            Some(Key::Char('p')) => Action::Put { after: k == Key::Char(']'), indent: true },
            Some(Key::Char('P')) => Action::Put { after: false, indent: true },
            Some(Key::Char('c')) => Action::Hunk { next: k == Key::Char(']') },
            Some(_) => return Parse::Invalid,
        },
        Key::Char('J') => Action::Join,
        Key::Char('r') => match p.next() {
            None => return Parse::Incomplete,
            Some(Key::Char(c)) => Action::Replace(c),
            Some(Key::Enter) => Action::Replace('\n'),
            Some(_) => return Parse::Invalid,
        },
        Key::Char('~') => Action::ToggleCase,
        Key::Char('u') => Action::Undo,
        Key::Ctrl('r') => Action::Redo,
        Key::Ctrl('^' | '6') => Action::AltBuffer,
        Key::Ctrl('o') => Action::Jump { older: true },
        Key::Ctrl(']') | Key::F(12) => Action::GotoDef,
        Key::Ctrl('i') | Key::Tab => Action::Jump { older: false },
        Key::Char('q') => match p.next() {
            None => return Parse::Incomplete,
            Some(Key::Char(c)) if c.is_ascii_alphanumeric() || c == '"' => Action::Record(c),
            Some(_) => return Parse::Invalid,
        },
        Key::Char('@') => match p.next() {
            None => return Parse::Incomplete,
            Some(Key::Char(c)) if c.is_ascii_alphanumeric() || matches!(c, '@' | '"' | ':') => Action::Play(c),
            Some(_) => return Parse::Invalid,
        },
        Key::Char('&') => Action::RepeatSub { all: false },
        Key::Char('z') => match p.next() {
            None => return Parse::Incomplete,
            Some(Key::Char(c @ ('c' | 'o' | 'a' | 'M' | 'R' | 'E' | 'j' | 'k'))) => Action::Fold(c),
            Some(Key::Char(c @ ('z' | 't' | 'b'))) => Action::ScrollCursor(c),
            Some(Key::Char('.')) => Action::ScrollCursor('z'),
            Some(Key::Enter) => Action::ScrollCursor('t'),
            Some(Key::Char('-')) => Action::ScrollCursor('b'),
            Some(_) => return Parse::Invalid,
        },
        Key::Char('m') => match p.next() {
            None => return Parse::Incomplete,
            Some(Key::Char(c)) if c.is_ascii_alphabetic() => Action::SetMark(c),
            Some(_) => return Parse::Invalid,
        },
        Key::Ctrl('w') => match p.next() {
            None => return Parse::Incomplete,
            Some(Key::Char(c)) => Action::Window(c),
            Some(Key::Ctrl(c)) => Action::Window(c),
            Some(_) => return Parse::Invalid,
        },
        Key::Char('.') => Action::Dot,
        Key::Char(c @ (':' | '/' | '?')) => Action::Cmdline(c),
        Key::Char('*') => Action::SearchWord { forward: true },
        Key::Char('#') => Action::SearchWord { forward: false },
        Key::Char('v') => Action::Visual { line: false },
        Key::Char('V') => Action::Visual { line: true },
        Key::Ctrl('v' | 'q') => Action::VisualBlock,
        Key::Ctrl('d') => Action::Scroll { half: true, down: true },
        Key::Ctrl('u') => Action::Scroll { half: true, down: false },
        Key::Ctrl('f') | Key::PageDown => Action::Scroll { half: false, down: true },
        Key::Ctrl('b') | Key::PageUp => Action::Scroll { half: false, down: false },
        Key::Esc => Action::Esc,
        Key::Char('g') => match p.next() {
            None => return Parse::Incomplete,
            Some(Key::Char(';')) => Action::LastChange,
            Some(Key::Char('&')) => Action::RepeatSub { all: true },
            Some(Key::Char('-')) => Action::UndoTime { later: false },
            Some(Key::Char('d' | 'D')) => Action::GotoDef,
            Some(Key::Char('I')) => Action::GotoImpl,
            Some(Key::Char('r')) => Action::References,
            Some(Key::Char('+')) => Action::UndoTime { later: true },
            Some(_) => return Parse::Invalid,
        },
        _ => return Parse::Invalid,
    };
    done(Kind::Act(act))
}

pub fn parse_visual(keys: &[Key]) -> Parse {
    let mut p = P { keys, i: 0 };
    let (reg, count) = match prefix(&mut p) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let done = |kind| Parse::Done(Cmd { count, reg, kind });
    let Some(k) = p.peek() else { return Parse::Incomplete };
    if let Some(op) = op_of(k) {
        return done(Kind::VisualOp(op));
    }
    match k {
        Key::Char('x') | Key::Delete => return done(Kind::VisualOp(Op::Delete)),
        Key::Char('s') => return done(Kind::VisualOp(Op::Change)),
        Key::Char('o') => return done(Kind::VisualSwap),
        Key::Char('O') => return done(Kind::VisualCorner),
        _ => {}
    }
    if let Some(r) = text_obj(&mut p) {
        return match r {
            None => Parse::Incomplete,
            Some((inner, obj)) => done(Kind::VisualObj { inner, obj }),
        };
    }
    match motion(&mut p) {
        Some(None) => return Parse::Incomplete,
        Some(Some(m)) => return done(Kind::Move(m)),
        None => {}
    }
    p.i += 1;
    let act = match k {
        Key::Esc | Key::Ctrl('c') => Action::Esc,
        Key::Char('v') => Action::Visual { line: false },
        Key::Char('V') => Action::Visual { line: true },
        Key::Ctrl('v' | 'q') => Action::VisualBlock,
        Key::Char(c @ ('I' | 'A')) => Action::Insert(c),
        Key::Char('z') => match p.next() {
            None => return Parse::Incomplete,
            Some(Key::Char('f')) => Action::Fold('f'),
            Some(_) => return Parse::Invalid,
        },
        Key::Char('J') => Action::Join,
        Key::Char('~') => Action::ToggleCase,
        Key::Char('p') | Key::Char('P') => Action::Put { after: true, indent: false },
        Key::Char('r') => match p.next() {
            None => return Parse::Incomplete,
            Some(Key::Char(c)) => Action::Replace(c),
            Some(_) => return Parse::Invalid,
        },
        Key::Char(':') => Action::Cmdline(':'),
        Key::Ctrl('d') => Action::Scroll { half: true, down: true },
        Key::Ctrl('u') => Action::Scroll { half: true, down: false },
        _ => return Parse::Invalid,
    };
    done(Kind::Act(act))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_keys;

    fn n(s: &str) -> Parse {
        parse_normal(&parse_keys(s, " "))
    }

    #[test]
    fn operators_and_counts() {
        assert_eq!(n("2d3w"), Parse::Done(Cmd { count: Some(6), reg: None, kind: Kind::Operate(Op::Delete, Target::Motion(Motion::WordFwd { big: false })) }));
        assert_eq!(n("dd"), Parse::Done(Cmd { count: None, reg: None, kind: Kind::Operate(Op::Delete, Target::Lines) }));
        assert_eq!(n("ci\""), Parse::Done(Cmd { count: None, reg: None, kind: Kind::Operate(Op::Change, Target::Obj { inner: true, obj: TextObj::Quote('"') }) }));
        assert_eq!(n("\"ayy"), Parse::Done(Cmd { count: None, reg: Some('a'), kind: Kind::Operate(Op::Yank, Target::Lines) }));
        assert_eq!(n("dt,"), Parse::Done(Cmd { count: None, reg: None, kind: Kind::Operate(Op::Delete, Target::Motion(Motion::Find { ch: ',', forward: true, till: true })) }));
        assert_eq!(n("d"), Parse::Incomplete);
        assert_eq!(n("df"), Parse::Incomplete);
        assert_eq!(n("di"), Parse::Incomplete);
        assert_eq!(n("dgg"), Parse::Done(Cmd { count: None, reg: None, kind: Kind::Operate(Op::Delete, Target::Motion(Motion::FirstLine)) }));
        assert_eq!(n("dz"), Parse::Invalid);
        assert_eq!(n("0"), Parse::Done(Cmd { count: None, reg: None, kind: Kind::Move(Motion::LineStart) }));
        assert_eq!(n("10j"), Parse::Done(Cmd { count: Some(10), reg: None, kind: Kind::Move(Motion::Down) }));
        assert_eq!(n("ra"), Parse::Done(Cmd { count: None, reg: None, kind: Kind::Act(Action::Replace('a')) }));
        assert_eq!(n("]p"), Parse::Done(Cmd { count: None, reg: None, kind: Kind::Act(Action::Put { after: true, indent: true }) }));
        assert_eq!(n("ma"), Parse::Done(Cmd { count: None, reg: None, kind: Kind::Act(Action::SetMark('a')) }));
        assert_eq!(n("d'a"), Parse::Done(Cmd { count: None, reg: None, kind: Kind::Operate(Op::Delete, Target::Motion(Motion::Mark { ch: 'a', exact: false })) }));
        assert_eq!(n("`"), Parse::Incomplete);
    }
}
