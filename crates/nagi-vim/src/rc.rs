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
    /// `set` の 1 つずつ（出所 "ファイル:行", 書いたまま "ts=4" "noet" など）。値はエディタが設定の表で読む
    pub sets: Vec<(String, String)>,
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

/// 設定を探して読む: `~/.config/nagi/nagi.vim` があればそれ、無ければ `~/.ideavimrc`・`~/_vimrc`・`~/.vimrc` の順
pub fn load_rc() -> Rc {
    load_rc_from(None)
}

/// 設定を読む（`path` が空でなければそのファイル。nagi の設定の「Vim の設定ファイル」）
pub fn load_rc_from(path: Option<&str>) -> Rc {
    let mut rc = Rc { leader: "\\".into(), ..Default::default() };
    let path = match path.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => expand_path(p),
        None => {
            let own = home().join(".config").join("nagi").join("nagi.vim");
            [own, home().join(".ideavimrc"), home().join("_vimrc"), home().join(".vimrc")].into_iter().find(|p| p.exists()).unwrap_or_else(|| home().join(".ideavimrc"))
        }
    };
    if path.exists() {
        source(&mut rc, &path, 0);
    }
    add_default_maps(&mut rc);
    rc
}

/// git の既定の割り当て。設定で同じ左辺（またはその途中・その先）を決めていれば足さない
pub fn add_default_maps(rc: &mut Rc) {
    const DEFAULTS: [(&str, &str); 8] = [
        ("<leader>gg", ":<C-u>Gcommit<CR>"),
        ("<leader>gd", ":<C-u>Gdiff<CR>"),
        ("<leader>gb", ":<C-u>Gblame<CR>"),
        ("<leader>gl", ":<C-u>Glog<CR>"),
        ("<leader>gB", ":<C-u>Gbranches<CR>"),
        ("<leader>hp", ":<C-u>GitHunk<CR>"),
        ("<leader>hs", ":<C-u>GitStage<CR>"),
        ("<leader>hu", ":<C-u>GitRevert<CR>"),
    ];
    for (lhs, rhs) in DEFAULTS {
        let lhs = parse_keys(lhs, &rc.leader);
        let taken = rc.keymap.normal.iter().any(|m| m.lhs.starts_with(&lhs) || lhs.starts_with(&m.lhs));
        if !taken {
            let rhs = parse_keys(rhs, &rc.leader);
            rc.keymap.add(lhs, rhs, true);
        }
    }
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
            "set" | "se" | "setlocal" | "setl" | "setglobal" | "setg" => {
                // 後ろの `" 注釈` は外す
                let rest = rest.split_once(" \"").map_or(rest, |(a, _)| a);
                for opt in rest.split_whitespace() {
                    rc.sets.push((format!("{name}:{}", no + 1), opt.to_string()));
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
                let mut rhs = parse_keys(rhs, &rc.leader);
                // `:%s///cg<Left>...` のように置換コマンドを途中まで入力して止まる割り当ては、
                // Nagi の置換（最下行の 2 行で入力し、本文に予告表示する）を開く形に読み替える
                let typed: String = rhs.iter().filter_map(|k| if let crate::Key::Char(c) = k { Some(*c) } else { None }).collect();
                if typed.starts_with(':') && typed.contains("%s/") && rhs.last() != Some(&crate::Key::Enter) {
                    rhs = parse_keys(":<C-u>%s///cg<CR>", &rc.leader);
                }
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
        // 既定の git の割り当ては、設定と重ならない分だけ足す
        let mut rc2 = parse_rc("let mapleader = \" \"\nnnoremap <leader>g :echo<CR>\n");
        add_default_maps(&mut rc2);
        assert_eq!(rc2.keymap.normal.len(), 1 + 3, "<leader>g と重なる g〜 は足さない");
        let zj = rc.keymap.normal.iter().find(|m| m.lhs == vec![Key::Char(' '), Key::Char('z'), Key::Char('j')]).unwrap();
        assert_eq!(zj.rhs, parse_keys(":action Back<CR>", " "));
        assert_eq!(rc.notes.len(), 1, "{:?}", rc.notes);
        let rc = parse_rc("nnoremap <C-s> :<C-u>%s///cg<left><left><left><left>");
        assert_eq!(rc.keymap.normal[0].rhs, parse_keys(":<C-u>%s///cg<CR>", " "));
    }
}
