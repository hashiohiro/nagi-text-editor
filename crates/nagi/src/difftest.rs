//! Neovim との差分テスト（architecture.md §8）。同じキー列を Nagi と `nvim --headless -u NONE` に流し、
//! 操作後のテキストとカーソル位置を比べる。Neovim が見つからなければ飛ばす。
//!
//! 意図して Neovim と違えている点:
//! - 検索は大文字小文字を区別しない（ユーザーの要望）。比べるときは Neovim 側を `ignorecase` にする。
//! - インデントは空白 4 つ（`>>` `<<`）。比べるときは Neovim 側を `shiftwidth=4 expandtab` にする。

use std::path::PathBuf;
use std::process::Command;

use nagi_core::Buffer;
use nagi_vim::parse_keys;

use crate::editor::Editor;

const TEXT: &str = "\
fn main() {
    let name = \"nagi\";
    let items = vec![1, 2, 3];
    if items.len() > (1 + 2) {
        println!(\"{}, {}\", name, items[0]);
    }
}

foo bar baz
one, two, three
alpha beta gamma
";

/// (説明, 開始位置 [行, 列]（1 始まり, byte）, キー列)
const CASES: &[(&str, (usize, usize), &str)] = &[
    ("dw", (9, 1), "dw"),
    ("d2w", (9, 1), "d2w"),
    ("2dw", (9, 1), "2dw"),
    ("dw 行末", (9, 9), "dw"),
    ("3dd", (2, 1), "3dd"),
    ("dd 最終行", (11, 1), "dd"),
    ("cw", (9, 1), "cwxyz<Esc>"),
    ("ce", (9, 5), "cexyz<Esc>"),
    ("cc", (2, 9), "ccnew<Esc>"),
    ("C", (2, 9), "Cend<Esc>"),
    ("D", (9, 5), "D"),
    ("x", (9, 1), "x"),
    ("3x", (9, 1), "3x"),
    ("X", (9, 5), "X"),
    ("s", (9, 1), "sZ<Esc>"),
    ("S", (2, 5), "Sstmt;<Esc>"),
    ("r", (9, 1), "rX"),
    ("3r", (9, 1), "3rX"),
    ("J", (9, 1), "J"),
    ("3J", (9, 1), "3J"),
    ("~", (9, 1), "~"),
    ("3~", (9, 1), "3~"),
    ("yyp", (9, 1), "yyp"),
    ("yyP", (9, 1), "yyP"),
    ("yiwP", (9, 5), "yiwP"),
    ("ywp", (9, 1), "ywp"),
    ("diw", (9, 5), "diw"),
    ("daw", (9, 5), "daw"),
    ("ci\"", (2, 17), "ci\"X<Esc>"),
    ("da\"", (2, 17), "da\""),
    ("ci(", (4, 23), "ci(Y<Esc>"),
    ("di{ 複数行", (5, 9), "di{"),
    ("ca[", (3, 22), "ca[Z<Esc>"),
    ("dip", (9, 1), "dip"),
    ("dap", (9, 1), "dap"),
    (">>", (9, 1), ">>"),
    ("<<", (2, 1), "<<"),
    ("3>>", (9, 1), "3>>"),
    ("dt,", (10, 1), "dt,"),
    ("df,;", (10, 1), "df,;x"),
    ("f t ,", (10, 1), "f,;,x"),
    ("d$", (9, 5), "d$"),
    ("d0", (9, 5), "d0"),
    ("d^", (2, 9), "d^"),
    ("dG", (9, 1), "dG"),
    ("dgg", (3, 1), "dgg"),
    ("d}", (9, 1), "d}"),
    ("d%", (4, 22), "d%"),
    ("%", (1, 11), "%x"),
    ("o", (9, 1), "onew line<Esc>"),
    ("O", (2, 1), "Oabove<Esc>"),
    ("A", (9, 1), "A!<Esc>"),
    ("I", (2, 1), "I//<Esc>"),
    ("a", (9, 1), "aX<Esc>"),
    ("u", (9, 1), "ddu"),
    ("u Ctrl-r", (9, 1), "ddu<C-r>"),
    (".", (9, 1), "dw."),
    ("3.", (10, 1), "x3."),
    ("cw .", (9, 1), "cwX<Esc>w."),
    ("viwd", (9, 5), "viwd"),
    ("vjd", (9, 2), "vjd"),
    ("Vd", (9, 1), "Vd"),
    ("Vjd", (9, 1), "Vjd"),
    ("veyP", (9, 1), "veyP"),
    ("v$y p", (9, 5), "vey$p"),
    ("V> ", (9, 1), "Vj>"),
    ("vjJ", (9, 1), "VjJ"),
    ("v~", (9, 1), "ve~"),
    ("\"ayy \"ap", (9, 1), "\"ayyj\"ap"),
    ("w b e", (9, 1), "wwbex"),
    ("W B E", (10, 1), "WEx"),
    ("G gg", (5, 1), "Gggx"),
    ("10G", (1, 1), "3Gx"),
    ("3G 列を保つ", (2, 9), "3Gx"),
    ("6G 短い行", (4, 25), "6Gx"),
    ("2gg 列", (5, 12), "2ggx"),
    ("G 列", (2, 9), "Gx"),
    ("d3G", (5, 9), "d3G"),
    ("y2Gp", (4, 5), "y2GGp"),
    ("di( 括弧の上", (4, 22), "di("),
    ("2di{", (5, 9), "2di{"),
    ("ci{ 開き括弧の上", (1, 11), "ci{X<Esc>"),
    ("da( 入れ子", (4, 24), "da("),
    ("yi{P 複数行", (5, 9), "yi{P"),
    ("ci{ 複数行の中", (5, 9), "ci{X<Esc>"),
    ("di{p 複数行", (5, 9), "di{p"),
    ("vi{d 複数行", (5, 9), "vi{d"),
    ("% 閉じ括弧から", (7, 1), "%x"),
    ("d% 逆向き", (6, 5), "d%"),
    ("/検索 n N", (1, 1), "/items<CR>nx"),
    ("? 検索", (11, 1), "?name<CR>x"),
    ("*", (2, 9), "*x"),
    ("dn", (1, 1), "/bar<CR>ggdn"),
    (":s", (9, 1), ":s/a/A/g<CR>"),
    (":%s", (1, 1), ":%s/let/var/<CR>"),
    (":3,4s", (1, 1), ":3,4s/\\d/N/g<CR>"),
];

fn nvim() -> Option<PathBuf> {
    let p = std::env::var_os("NAGI_NVIM").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Program Files\Neovim\bin\nvim.exe"));
    p.exists().then_some(p)
}

/// Vim の :normal に渡す文字列（`<Esc>` → `\<Esc>`）
fn vim_keys(keys: &str) -> String {
    let mut out = String::new();
    let mut chars = keys.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '<' => {
                let name: String = chars.clone().take_while(|&c| c != '>').collect();
                if !name.is_empty() && name.len() < 8 && chars.clone().nth(name.chars().count()) == Some('>') {
                    out.push_str(&format!("\\<{name}>"));
                    for _ in 0..=name.chars().count() {
                        chars.next();
                    }
                } else {
                    out.push('<');
                }
            }
            c => out.push(c),
        }
    }
    out
}

fn run_nvim(nvim: &PathBuf, dir: &std::path::Path, start: (usize, usize), keys: &str) -> (String, (usize, usize)) {
    let input = dir.join("in.txt");
    std::fs::write(&input, TEXT).unwrap();
    let out = dir.join("out.txt");
    let cur = dir.join("cur.txt");
    let _ = std::fs::remove_file(&out);
    let status = Command::new(nvim)
        .args(["--headless", "-u", "NONE", "-i", "NONE", "-n", "--cmd", "set noswapfile ignorecase shiftwidth=4 expandtab"])
        .arg(&input)
        .arg("-c")
        .arg(format!("call cursor({}, {})", start.0, start.1))
        .arg("-c")
        .arg(format!("exe \"normal {}\"", vim_keys(keys)))
        .arg("-c")
        .arg(format!("call writefile([line('.') . ',' . col('.')], '{}')", cur.display().to_string().replace('\\', "/")))
        .arg("-c")
        .arg(format!("w! {}", out.display().to_string().replace('\\', "/")))
        .arg("-c")
        .arg("qa!")
        .status()
        .unwrap();
    assert!(status.success());
    let text = std::fs::read_to_string(&out).unwrap();
    let c = std::fs::read_to_string(&cur).unwrap();
    let (l, col) = c.trim().split_once(',').unwrap();
    (text, (l.parse().unwrap(), col.parse().unwrap()))
}

fn run_nagi(start: (usize, usize), keys: &str) -> (String, (usize, usize)) {
    let mut ed = Editor::new(Buffer::from_str(TEXT), None);
    ed.rows = 30;
    ed.cursor.line = start.0 - 1;
    ed.cursor.byte = start.1 - 1;
    ed.cursor.want_col = nagi_core::display_col(&ed.buf.line(start.0 - 1), start.1 - 1, false);
    for k in parse_keys(keys, " ") {
        ed.key(k);
    }
    // :normal と同じく、最後に Insert / Visual / 入力中のコマンドを終える
    for _ in 0..2 {
        if ed.mode != crate::editor::Mode::Normal {
            ed.key(nagi_vim::Key::Esc);
        }
    }
    (ed.buf.text(), (ed.cursor.line + 1, ed.cursor.byte + 1))
}

#[test]
fn neovim_differential() {
    let Some(nvim) = nvim() else {
        eprintln!("Neovim が見つからないので差分テストを飛ばす（NAGI_NVIM で指定できる）");
        return;
    };
    let dir = std::env::temp_dir().join("nagi-difftest");
    std::fs::create_dir_all(&dir).unwrap();
    let mut failures = Vec::new();
    for (name, start, keys) in CASES {
        let want = run_nvim(&nvim, &dir, *start, keys);
        let got = run_nagi(*start, keys);
        if got != want {
            failures.push(format!(
                "== {name}（{keys}）\n  nvim: {:?} {:?}\n  nagi: {:?} {:?}",
                want.1,
                diff_hint(&want.0, &got.0).0,
                got.1,
                diff_hint(&want.0, &got.0).1
            ));
        }
    }
    eprintln!("差分テスト: {} 件中 {} 件一致", CASES.len(), CASES.len() - failures.len());
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// 最初に食い違う行を見せる
fn diff_hint(a: &str, b: &str) -> (String, String) {
    let (al, bl): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
    for i in 0..al.len().max(bl.len()) {
        if al.get(i) != bl.get(i) {
            return (format!("{}行目 {:?}", i + 1, al.get(i)), format!("{}行目 {:?}", i + 1, bl.get(i)));
        }
    }
    (format!("{} 行", al.len()), format!("{} 行", bl.len()))
}
