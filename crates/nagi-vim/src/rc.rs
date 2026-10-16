//! vimrc（.ideavimrc）の読み込み。Vimscript は実行しない。マッピングと一部の set / let だけを解釈する。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::{Keymap, parse_keys};

#[derive(Default, Debug)]
pub struct Rc {
    pub leader: String,
    pub keymap: Keymap,
    /// `set` で有効にしたオプション（`clipboard+=unnamedplus` は "clipboard=unnamedplus" として持つ）
    pub options: BTreeSet<String>,
    /// 読み込んだファイル
    pub sources: Vec<PathBuf>,
    /// 解釈しなかった行（行番号付き。起動時のメッセージや :NagiKeys で見られるようにする）
    pub notes: Vec<String>,
}

impl Rc {
    pub fn has(&self, opt: &str) -> bool {
        self.options.contains(opt)
    }
}

fn home() -> PathBuf {
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap_or_default()
}

fn expand_path(p: &str) -> PathBuf {
    match p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        Some(rest) => home().join(rest),
        None => PathBuf::from(p),
    }
}

/// 設定を探して読む: `~/.config/nagi/nagi.vim` があればそれ、無ければ `~/.ideavimrc`
pub fn load_rc() -> Rc {
    let mut rc = Rc { leader: "\\".into(), ..Default::default() };
    let own = home().join(".config").join("nagi").join("nagi.vim");
    let path = if own.exists() { own } else { home().join(".ideavimrc") };
    if path.exists() {
        source(&mut rc, &path, 0);
    }
    rc
}

fn source(rc: &mut Rc, path: &Path, depth: usize) {
    if depth > 5 {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        rc.notes.push(format!("{}: 読めない", path.display()));
        return;
    };
    rc.sources.push(path.to_path_buf());
    parse_into(rc, &text, &path.display().to_string(), depth);
}

/// 文字列から読む（テスト用）
pub fn parse_rc(text: &str) -> Rc {
    let mut rc = Rc { leader: "\\".into(), ..Default::default() };
    parse_into(&mut rc, text, "<text>", 0);
    rc
}

fn parse_into(rc: &mut Rc, text: &str, name: &str, depth: usize) {
    for (no, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('"') {
            continue;
        }
        let note = |rc: &mut Rc, why: &str| rc.notes.push(format!("{name}:{}: {why}: {line}", no + 1));
        let (cmd, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim();
        match cmd {
            "let" => {
                // let mapleader = " "
                if let Some(v) = rest.strip_prefix("mapleader").map(str::trim).and_then(|r| r.strip_prefix('=')) {
                    let v = v.trim().trim_matches(['"', '\'']);
                    rc.leader = if v == "\\\\" { "\\".into() } else { v.to_string() };
                } else {
                    note(rc, "未対応の let");
                }
            }
            "set" => {
                for opt in rest.split_whitespace() {
                    if let Some((k, v)) = opt.split_once("+=") {
                        rc.options.insert(format!("{k}={v}"));
                    } else if let Some(k) = opt.strip_prefix("no") {
                        rc.options.remove(k);
                    } else {
                        rc.options.insert(opt.to_string());
                    }
                }
            }
            "source" | "so" => source(rc, &expand_path(rest), depth + 1),
            "nnoremap" | "nmap" | "noremap" | "map" | "nn" | "nno" | "nm" => {
                let noremap = !matches!(cmd, "nmap" | "map" | "nm");
                let mut args = rest;
                let mut is_expr = false;
                // <silent> <expr> などの修飾
                loop {
                    let lower = args.to_ascii_lowercase();
                    let Some(m) = ["<silent>", "<expr>", "<buffer>", "<nowait>", "<unique>"].iter().find(|m| lower.starts_with(**m)) else {
                        break;
                    };
                    is_expr |= *m == "<expr>";
                    args = args[m.len()..].trim_start();
                }
                let Some((lhs, rhs)) = args.split_once(char::is_whitespace) else {
                    note(rc, "右辺がない");
                    continue;
                };
                let (lhs, rhs) = (lhs.trim(), rhs.trim());
                if is_expr {
                    // 式マッピングは評価しない。折り返しを意識した j/k（gj/gk）は、
                    // Nagi が折り返しをしない間は j/k と同じなので、そのままにする
                    note(rc, "<expr> は評価しない（j/k の gj/gk 切り替えは、折り返しなしでは同じ動き）");
                    continue;
                }
                let lhs = parse_keys(lhs, &rc.leader);
                let rhs = parse_keys(rhs, &rc.leader);
                rc.keymap.add(lhs, rhs, noremap);
            }
            _ => note(rc, "未対応のコマンド"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Key;

    #[test]
    fn ideavimrc_subset() {
        let rc = parse_rc(
            r#"
" コメント
set clipboard+=unnamedplus
set surround
let mapleader = " "
nnoremap ew :<C-u>w<CR>
nnoremap <expr> j (v:count == 0 ? 'gj' : 'j')
nnoremap <leader>zj :<C-u>action EasymotionLineForward<CR>
nnoremap <leader>zj :action Back<CR>
nnoremap <leader><leader>R :<C-u>source ~/.ideavimrc \| echo "Reloaded"<CR>
"#,
        );
        assert_eq!(rc.leader, " ");
        assert!(rc.has("clipboard=unnamedplus"));
        assert!(rc.has("surround"));
        assert_eq!(rc.keymap.normal.len(), 3, "後から定義した <leader>zj が勝つ");
        let zj = rc.keymap.normal.iter().find(|m| m.lhs == vec![Key::Char(' '), Key::Char('z'), Key::Char('j')]).unwrap();
        assert_eq!(zj.rhs, parse_keys(":action Back<CR>", " "));
        assert_eq!(rc.notes.len(), 1, "{:?}", rc.notes);
    }
}
