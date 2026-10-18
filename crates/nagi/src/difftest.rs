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
    ("Ctrl-n", (1, 1), "Ona<C-n><Esc>"),
    ("Ctrl-n 2 回", (11, 1), "ot<C-n><C-n><Esc>"),
    ("Ctrl-p", (11, 1), "ot<C-p><Esc>"),
    ("Ctrl-n 一周", (11, 1), "oit<C-n><C-n><C-n><Esc>"),
    ("Ctrl-n Ctrl-e", (9, 1), "Aba<C-n><C-e>!<Esc>"),
    ("Ctrl-n 続けて", (9, 1), "ob<C-n>.<Esc>"),
    ("Ctrl-n 大文字", (1, 1), "OPRI<C-n><Esc>"),
    ("g- 枝", (9, 1), "xxuxg-"),
    ("g- g-", (9, 1), "xxuxg-g-"),
    ("g- g+", (9, 1), "xxuxg-g-g+g+"),
    ("g- 回数", (9, 1), "xxxuu2g-"),
    ("u 枝の後", (9, 1), "xxuxuu<C-r>"),
    (":earlier", (9, 1), "xxuxx:earlier 3<CR>"),
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
    ("C-v d", (9, 1), "<C-v>jjld"),
    ("C-v y p", (9, 1), "<C-v>jly$p"),
    ("C-v d p", (9, 1), "<C-v>jld$p"),
    ("C-v x P", (10, 6), "<C-v>jlxggP"),
    ("C-v c", (9, 1), "<C-v>jjcXY<Esc>"),
    ("C-v I", (2, 5), "<C-v>jjI// <Esc>"),
    ("C-v A", (9, 1), "<C-v>jjeA!<Esc>"),
    ("C-v $A", (9, 1), "<C-v>jj$A;<Esc>"),
    ("C-v I 短い行", (5, 5), "<C-v>jjjI#<Esc>"),
    ("C-v A 短い行", (5, 5), "<C-v>jjjlA#<Esc>"),
    ("C-v c 短い行", (5, 9), "<C-v>jjjlcZ<Esc>"),
    ("C-v r", (9, 1), "<C-v>jjlrx"),
    ("C-v ~", (9, 1), "<C-v>jjl~"),
    ("C-v o d", (9, 3), "<C-v>jhod"),
    ("C-v O d", (9, 3), "<C-v>jlOd"),
    ("C-v >", (9, 1), "<C-v>j>"),
    ("C-v J", (9, 1), "<C-v>jJ"),
    ("C-v 1 行 y P", (9, 5), "<C-v>ey0P"),
    ("zf 飛ばす", (8, 1), "jVjzfkjjx"),
    ("zf dd", (9, 1), "Vjzfdd"),
    ("zf zo", (9, 1), "Vjzfzojx"),
    ("zf zE", (9, 1), "VjzfzEjx"),
    ("zf 中へ検索", (1, 1), "9GVjzfgg/two<CR>x"),
    ("zf yy p", (9, 1), "VjzfyyGp"),
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
    ("ma d'a", (2, 5), "majjd'a"),
    ("ma y`a P", (9, 5), "mawwy`aP"),
    ("'' 戻る", (2, 9), "G''x"),
    ("`` 戻る", (2, 9), "G``x"),
    ("% ``", (1, 11), "%``x"),
    ("Ctrl-o", (1, 5), "G<C-o>x"),
    ("Ctrl-o Ctrl-i", (1, 1), "5G10G<C-o><C-o><C-i>x"),
    ("/ Ctrl-o", (1, 1), "/items<CR><C-o>x"),
    ("* Ctrl-o", (2, 9), "*<C-o>x"),
    ("'.", (1, 1), "3jx gg'.x"),
    ("マークのずれ", (5, 1), "maggOnew<Esc>'ax"),
    ("マーク 行の削除", (9, 1), "magg2dd'ax"),
    ("'< '>", (9, 3), "vjj<Esc>gg'>x"),
    ("`< 位置", (9, 3), "vjl<Esc>gg`<x"),
    ("マクロ", (9, 1), "qaxjq2@a"),
    ("マクロ @@", (9, 1), "qadwjq@a@@"),
    ("マクロ Insert", (1, 1), "qqA;<Esc>jq3@q"),
    ("マクロ qA 追記", (9, 1), "qaxqqAjq@a"),
    ("マクロ 検索", (1, 1), "qa/let<CR>rLq@a"),
    ("マクロ \"ap", (9, 1), "qaxq\"ap"),
    ("? 検索", (11, 1), "?name<CR>x"),
    ("*", (2, 9), "*x"),
    ("dn", (1, 1), "/bar<CR>ggdn"),
    (":s", (9, 1), ":s/a/A/g<CR>"),
    (":%s", (1, 1), ":%s/let/var/<CR>"),
    (":3,4s", (1, 1), ":3,4s/\\d/N/g<CR>"),
    (":s 範囲 .,+2", (2, 1), ":.,+2s/let/LET/<CR>"),
    (":s 範囲 $", (1, 1), ":$-2,$s/a/A/g<CR>"),
    (":s マーク", (2, 1), "ma2j:'a,.s/ /_/g<CR>"),
    (":'<,'>s", (9, 1), "Vj:s/o/0/g<CR>"),
    (":s I", (1, 1), ":%s/Let/X/I<CR>"),
    (":s e 一致なし", (1, 1), ":s/zzz/y/e<CR>"),
    (":s 空のパターン", (1, 1), "/name<CR>:s//NAME/<CR>"),
    (":s \\r", (9, 1), ":s/ /\\r/g<CR>"),
    (":s &", (1, 1), ":s/n/N/<CR>j&"),
    (":s g&", (1, 1), ":s/i/I/<CR>g&"),
    (":&&", (2, 1), ":s/e/E/g<CR>j:&&<CR>"),
    (":g d", (1, 1), ":g/let/d<CR>"),
    (":g! d", (1, 1), ":g!/let/d<CR>"),
    (":v d", (1, 1), ":v/o/d<CR>"),
    (":g s", (1, 1), ":g/let/s/;/ \\/\\/ x/<CR>"),
    (":g normal", (1, 1), ":g/let/normal A!<CR>"),
    (":g normal dd", (1, 1), ":g/e/normal jdd<CR>"),
    (":g m0 逆順", (9, 1), ":9,11g/^/m8<CR>"),
    (":g u", (1, 1), ":g/let/d<CR>u"),
    (":d", (2, 1), ":2,3d<CR>"),
    (":y P", (9, 1), ":9,10y<CR>P"),
    (":j", (9, 1), ":9,11j<CR>"),
    (":>", (9, 1), ":9,10><CR>"),
    (":m", (9, 1), ":m+1<CR>"),
    (":m0", (10, 1), ":m0<CR>"),
    (":t.", (9, 1), ":t.<CR>"),
    (":normal 範囲", (9, 1), ":9,11normal Ax<CR>"),
];

fn nvim() -> Option<PathBuf> {
    let p = std::env::var_os("NAGI_NVIM").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Program Files\Neovim\bin\nvim.exe"));
    p.exists().then_some(p)
}

/// Vim の feedkeys に渡す文字列（`<Esc>` → `\<Esc>`）
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
        // 打ったキーとして渡す（:normal だと q の記録に残らない）。x ですぐ実行し、Insert は最後に抜ける
        .arg(format!("call feedkeys(\"{}\", 'xt')", vim_keys(keys)))
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
        if ed.state() != crate::editor::State::Normal {
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
