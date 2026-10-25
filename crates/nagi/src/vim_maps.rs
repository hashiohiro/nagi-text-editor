//! Vim の割り当て（vimrc の nnoremap など）を設定の画面で見る・直すための部品。
//!
//! 画面で直したものは vimrc ではなく nagi の keys.vim（settings::keys_path）に、vimrc との違いだけを書く
//! （足した・変えたものは nnoremap、消したものは nunmap）。.ideavimrc は IntelliJ と共有しているので書き換えない。
//! 読む順は nagi の既定 → vimrc → keys.vim（nagi_vim::load_rc_with）。

use nagi_vim::{Key, Keymap, Mode};

/// 割り当て 1 つ（画面に出す形。キーは vimrc の書き方）
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapRow {
    pub mode: Mode,
    pub lhs: String,
    pub rhs: String,
    pub noremap: bool,
    /// どこで決めたか（vimrc の何行目・nagi の既定・設定の画面）
    pub from: String,
}

/// 画面で足した・変えた割り当ての出所
pub const SCREEN_FROM: &str = "設定の画面";

/// キーの並びを vimrc の書き方に（頭の leader は <leader>。`lhs` なら空白も <Space>）
pub fn keys_text(keys: &[Key], leader: &str, lhs: bool) -> String {
    let lead = nagi_vim::parse_keys(leader, leader).first().copied();
    let mut s = String::new();
    for (i, k) in keys.iter().enumerate() {
        match k {
            _ if i == 0 && lhs && Some(*k) == lead && keys.len() > 1 => s.push_str("<leader>"),
            Key::Char(' ') if lhs || i + 1 == keys.len() => s.push_str("<Space>"),
            Key::Char('|') => s.push_str("<Bar>"),
            k => s.push_str(&k.notation()),
        }
    }
    s
}

/// 割り当ての一覧（Normal・Visual・Insert の順。決めた順のまま）
pub fn rows(km: &Keymap, leader: &str) -> Vec<MapRow> {
    Mode::ALL
        .iter()
        .flat_map(|&mode| {
            km.list(mode).iter().map(move |m| MapRow {
                mode,
                lhs: keys_text(&m.lhs, leader, true),
                rhs: keys_text(&m.rhs, leader, false),
                noremap: m.noremap,
                from: short_from(&m.from),
            })
        })
        .collect()
}

/// 出所のファイルは名前だけに（C:\Users\x\.ideavimrc:12 → .ideavimrc:12）
fn short_from(from: &str) -> String {
    let short = from.rsplit(['\\', '/']).next().unwrap_or(from);
    if short.starts_with("keys.vim") { SCREEN_FROM.into() } else { short.to_string() }
}

/// 右辺が動くか（印, 説明）。:action と : のコマンドだけ見る（キーの並びは None）
pub fn status(rhs: &str) -> Option<(&'static str, String)> {
    let cmd = rhs.strip_prefix(':')?;
    let cmd = cmd.strip_prefix("<C-u>").or_else(|| cmd.strip_prefix("<C-U>")).unwrap_or(cmd);
    let cmd = cmd.split("<CR>").next().unwrap_or(cmd).trim();
    if let Some(name) = cmd.strip_prefix("action ") {
        let name = name.trim();
        return Some(match nagi_vim::action::ideavim(name) {
            Some((Some(id), desc)) if crate::commands::ACTIONS.iter().any(|(a, _)| *a == id) => ("", desc.to_string()),
            Some((Some(_), desc)) => ("未実装", desc.to_string()),
            Some((None, desc)) => ("nagi に無い", desc.to_string()),
            None => ("知らないアクション", name.to_string()),
        });
    }
    // 範囲・置換などは見ない（%s/… や 1,3d）
    let name: String = cmd.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    if name.is_empty() || cmd.starts_with(|c: char| !c.is_ascii_alphabetic()) {
        return None;
    }
    match crate::commands::find(&name) {
        Some((c, _)) => Some(("", c.help.to_string())),
        None => Some(("知らないコマンド", name)),
    }
}

/// 割り当てに選べる操作（出す名前, 右辺, 印）。IntelliJ のアクションと nagi のコマンド
pub fn choices() -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for (names, id, desc) in nagi_vim::action::IDEA_ACTIONS {
        let tag = match id {
            Some(id) if crate::commands::ACTIONS.iter().any(|(a, _)| a == id) => "アクション",
            Some(_) => "未実装",
            None => "nagi に無い",
        };
        out.push((format!("{}  {desc}", names[0]), format!(":<C-u>action {}<CR>", names[0]), tag.to_string()));
    }
    for c in crate::commands::COMMANDS.iter().filter(|c| !c.help.is_empty()) {
        out.push((format!(":{}  {}", c.names[0], c.help), format!(":<C-u>{}<CR>", c.names[0]), "コマンド".to_string()));
    }
    out
}

/// "n <leader>x :action Stop<CR>" を読む（頭はモード n・x・v・i。省けば n）
pub fn parse_line(s: &str) -> Result<MapRow, String> {
    let s = s.trim();
    let (mode, rest) = match s.split_once(' ') {
        Some(("n", r)) => (Mode::Normal, r),
        Some(("x" | "v", r)) => (Mode::Visual, r),
        Some(("i", r)) => (Mode::Insert, r),
        _ => (Mode::Normal, s),
    };
    let rest = rest.trim_start();
    let (lhs, rhs) = rest.split_once(char::is_whitespace).ok_or("「モード 左辺 右辺」で書いてください（例: n <leader>r :action Run<CR>）")?;
    let rhs = rhs.trim();
    if rhs.is_empty() {
        return Err("右辺がありません".into());
    }
    Ok(MapRow { mode, lhs: lhs.to_string(), rhs: rhs.to_string(), noremap: true, from: SCREEN_FROM.into() })
}

/// 画面に出す 1 行（"n <leader>ff :<C-u>action Find<CR>"）
pub fn line_of(r: &MapRow) -> String {
    format!("{} {} {}", r.mode.letter(), r.lhs, r.rhs)
}

/// keys.vim の中身: `base`（nagi の既定と vimrc）から `edited` への違い
pub fn render(base: &[MapRow], edited: &[MapRow]) -> String {
    let mut s = String::from("\" nagi の設定の画面で決めた Vim の割り当て（vimrc の後に読む。vimrc との違いだけ）\n");
    let same = |a: &MapRow, b: &MapRow| a.mode == b.mode && a.lhs == b.lhs;
    for r in edited {
        if !base.iter().any(|b| same(b, r) && b.rhs == r.rhs && b.noremap == r.noremap) {
            let m = r.mode.letter();
            s.push_str(&format!("{m}{}map {} {}\n", if r.noremap { "nore" } else { "" }, r.lhs, r.rhs));
        }
    }
    for b in base {
        if !edited.iter().any(|r| same(b, r)) {
            s.push_str(&format!("{}unmap {}\n", b.mode.letter(), b.lhs));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_and_file() {
        let rc = nagi_vim::parse_rc("let mapleader = \" \"\nnnoremap <leader>ff :<C-u>action Find<CR>\nnnoremap <leader>d :<C-u>action QuickJavaDoc<CR>\ninoremap jj <Esc>\nnnoremap ew :<C-u>w<CR>\n");
        let base = rows(&rc.keymap, " ");
        assert_eq!(line_of(&base[0]), "n <leader>ff :<C-u>action Find<CR>");
        assert_eq!(base[0].from, "<text>:2");
        assert_eq!(line_of(&base[3]), "i jj <Esc>");
        assert_eq!(status(&base[0].rhs).unwrap().0, "");
        assert_eq!(status(&base[1].rhs).unwrap().0, "nagi に無い");
        assert_eq!(status(&base[2].rhs).unwrap().0, "");
        assert_eq!(status(":<C-u>Nosuch<CR>").unwrap().0, "知らないコマンド");
        assert_eq!(status("]p"), None);
        // 1 つ変え、1 つ消し、1 つ足す → keys.vim にはその違いだけ
        let mut ed = base.clone();
        ed[0].rhs = ":<C-u>action FindInPath<CR>".into();
        ed.remove(1);
        ed.push(parse_line("x <leader>c y").unwrap());
        let text = render(&base, &ed);
        assert_eq!(text.lines().skip(1).collect::<Vec<_>>(), ["nnoremap <leader>ff :<C-u>action FindInPath<CR>", "xnoremap <leader>c y", "nunmap <leader>d"]);
        // 読み直すと画面のとおり
        let again = nagi_vim::parse_rc(&format!("let mapleader = \" \"\nnnoremap <leader>ff :<C-u>action Find<CR>\nnnoremap <leader>d :<C-u>action QuickJavaDoc<CR>\ninoremap jj <Esc>\nnnoremap ew :<C-u>w<CR>\n{text}"));
        let got: Vec<String> = rows(&again.keymap, " ").iter().map(line_of).collect();
        let want: Vec<String> = ed.iter().map(line_of).collect();
        let (mut got, mut want) = (got, want);
        got.sort();
        want.sort();
        assert_eq!(got, want);
        assert!(parse_line("n <leader>x").is_err());
    }
}
