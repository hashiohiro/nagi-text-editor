//! vim のオプション（vimrc の `set`・`:set`）と設定の項目の対応（1 か所）。
//!
//! vimrc の `set` は設定の vimrc の層（Settings::rc。全体の設定より弱い）に、`:set` はこの窓だけの層（Settings::session）に入れる。
//! オプションを足すときは `OPTS` に 1 行足す（名前と短い名前・種類・設定の項目・値の読み書き）。

use std::collections::HashMap;

use crate::settings::{Settings, schema};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VKind {
    Bool,
    Num,
    Str,
}

pub struct VimOpt {
    pub names: &'static [&'static str],
    pub kind: VKind,
    /// 設定の項目（リセット・出所に使う）
    pub keys: &'static [&'static str],
    /// 設定の値 → vim の値（Bool は "1" か "0"）
    get: fn(&Settings) -> String,
    /// vim の値 → 設定に入れる（キー, 値）
    put: fn(&Settings, &str) -> Vec<(&'static str, String)>,
}

fn b(v: bool) -> String {
    if v { "1".into() } else { "0".into() }
}

fn on(v: &str) -> bool {
    v == "1"
}

/// 設定の真偽をそのまま vim の真偽に
macro_rules! bool_opt {
    ($names:expr, $key:literal) => {
        VimOpt { names: &$names, kind: VKind::Bool, keys: &[$key], get: |s| b(s.bool($key)), put: |_, v| vec![($key, on(v).to_string())] }
    };
}

/// 設定の整数をそのまま vim の数に
macro_rules! num_opt {
    ($names:expr, $key:literal) => {
        VimOpt { names: &$names, kind: VKind::Num, keys: &[$key], get: |s| s.int($key).to_string(), put: |_, v| vec![($key, v.to_string())] }
    };
}

pub static OPTS: &[VimOpt] = &[
    // 検索
    bool_opt!(["ignorecase", "ic"], "vim.ignorecase"),
    bool_opt!(["smartcase", "scs"], "vim.smartcase"),
    bool_opt!(["hlsearch", "hls"], "vim.hlsearch"),
    bool_opt!(["incsearch", "is"], "vim.incsearch"),
    // 表示
    bool_opt!(["wrap"], "editor.wrap"),
    VimOpt {
        names: &["number", "nu"],
        kind: VKind::Bool,
        keys: &["editor.line_numbers"],
        get: |s| b(s.get("editor.line_numbers") != "off"),
        put: |s, v| {
            let cur = s.get("editor.line_numbers");
            let to = match (on(v), cur.as_str()) {
                (true, "off") => "absolute",
                (false, "absolute") => "off",
                _ => cur.as_str(),
            };
            vec![("editor.line_numbers", to.to_string())]
        },
    },
    VimOpt {
        names: &["relativenumber", "rnu"],
        kind: VKind::Bool,
        keys: &["editor.line_numbers"],
        get: |s| b(s.get("editor.line_numbers") == "relative"),
        put: |s, v| {
            let cur = s.get("editor.line_numbers");
            let to = match (on(v), cur.as_str()) {
                (true, _) => "relative",
                (false, "relative") => "absolute",
                _ => cur.as_str(),
            };
            vec![("editor.line_numbers", to.to_string())]
        },
    },
    // インデント
    VimOpt {
        names: &["expandtab", "et"],
        kind: VKind::Bool,
        keys: &["editor.indent_style"],
        get: |s| b(s.get("editor.indent_style") != "tabs"),
        put: |_, v| vec![("editor.indent_style", if on(v) { "spaces" } else { "tabs" }.to_string())],
    },
    // sw=0 は tabstop と同じ（vim と同じ）
    VimOpt {
        names: &["shiftwidth", "sw"],
        kind: VKind::Num,
        keys: &["editor.indent_width"],
        get: |s| s.int("editor.indent_width").to_string(),
        put: |s, v| vec![("editor.indent_width", if v == "0" { s.int("editor.tab_width").to_string() } else { v.to_string() })],
    },
    num_opt!(["tabstop", "ts"], "editor.tab_width"),
    // 0 でなければソフトタブ（幅はいつもインデントの幅。vim の sts=-1 と同じ）
    VimOpt {
        names: &["softtabstop", "sts"],
        kind: VKind::Num,
        keys: &["editor.soft_tabs"],
        get: |s| if s.bool("editor.soft_tabs") { "-1".into() } else { "0".into() },
        put: |_, v| vec![("editor.soft_tabs", (v != "0").to_string())],
    },
    bool_opt!(["autoindent", "ai"], "editor.auto_indent"),
    bool_opt!(["smartindent", "si"], "editor.smart_indent"),
    bool_opt!(["cindent", "cin"], "editor.smart_indent"),
    num_opt!(["scrolloff", "so"], "editor.scrolloff"),
    VimOpt {
        names: &["list"],
        kind: VKind::Bool,
        keys: &["editor.show_whitespace"],
        get: |s| b(s.get("editor.show_whitespace") != "off"),
        put: |_, v| vec![("editor.show_whitespace", if on(v) { "trailing" } else { "off" }.to_string())],
    },
    bool_opt!(["cursorline", "cul"], "editor.cursorline"),
    VimOpt {
        names: &["colorcolumn", "cc"],
        kind: VKind::Str,
        keys: &["editor.colorcolumn"],
        get: |s| s.get("editor.colorcolumn"),
        put: |_, v| vec![("editor.colorcolumn", v.to_string())],
    },
    bool_opt!(["splitbelow", "sb"], "editor.split_below"),
    bool_opt!(["splitright", "spr"], "editor.split_right"),
    bool_opt!(["wrapscan", "ws"], "vim.wrapscan"),
    num_opt!(["timeoutlen", "tm"], "vim.timeoutlen"),
    bool_opt!(["highlightedyank"], "vim.highlight_yank"),
    bool_opt!(["ideajoin"], "vim.ideajoin"),
    // 保存
    bool_opt!(["fixendofline", "fixeol"], "editor.final_newline"),
    // 新しいファイルの改行・文字コード（vimrc の ff・fenc。:set では今のファイルに。editor_settings.rs の buffer_option）
    VimOpt {
        names: &["fileformat", "ff"],
        kind: VKind::Str,
        keys: &["editor.new_newline"],
        get: |s| if s.get("editor.new_newline") == "crlf" { "dos".into() } else { "unix".into() },
        put: |_, v| vec![("editor.new_newline", if v == "dos" { "crlf" } else { "lf" }.to_string())],
    },
    VimOpt {
        names: &["fileencoding", "fenc"],
        kind: VKind::Str,
        keys: &["editor.new_encoding"],
        get: |s| s.get("editor.new_encoding"),
        put: |_, v| vec![("editor.new_encoding", enc_choice(v).unwrap_or("utf-8").to_string())],
    },
    // 読むときに試す文字コード（ucs-bom,utf-8,cp932,euc-jp なら、UTF-8 でないときは cp932 → Shift_JIS）
    VimOpt {
        names: &["fileencodings", "fencs"],
        kind: VKind::Str,
        keys: &["editor.encoding_fallback"],
        get: |s| s.get("editor.encoding_fallback"),
        put: |_, v| {
            let first = v.split(',').filter_map(enc_choice).find(|c| *c != "utf-8");
            vec![("editor.encoding_fallback", first.unwrap_or("utf-8").to_string())]
        },
    },
    // クリップボード（unnamed・unnamedplus のどちらかがあれば、ヤンク・削除をいつもクリップボードへ）
    VimOpt {
        names: &["clipboard", "cb"],
        kind: VKind::Str,
        keys: &["vim.clipboard"],
        get: |s| if s.get("vim.clipboard") == "unnamed" { "unnamed".into() } else { String::new() },
        put: |_, v| vec![("vim.clipboard", if v.split(',').any(|x| x.starts_with("unnamed")) { "unnamed" } else { "explicit" }.to_string())],
    },
];

/// vim の文字コードの名前 → 設定の選択肢（shift_jis・euc-jp・utf-8）
fn enc_choice(v: &str) -> Option<&'static str> {
    let e = nagi_core::encoding::by_name(v)?;
    Some(if e == nagi_core::encoding::SHIFT_JIS {
        "shift_jis"
    } else if e == nagi_core::encoding::EUC_JP {
        "euc-jp"
    } else if e == nagi_core::encoding::UTF_8 {
        "utf-8"
    } else {
        return None;
    })
}

pub fn find(name: &str) -> Option<&'static VimOpt> {
    OPTS.iter().find(|o| o.names.contains(&name))
}

/// `set` の 1 語の結果
#[derive(Debug, PartialEq)]
pub enum Applied {
    /// 設定に入れる（キー, 値）
    Set(Vec<(&'static str, String)>),
    /// 値を見る（`ts?`・値なしの数や文字）。"tabstop=4" のように
    Query(String),
    /// 既定に戻す（`ts&`）。このキーを層から消す
    Reset(&'static [&'static str]),
}

/// `set` の 1 語（`ts=4` `noet` `invwrap` `wrap!` `sw+=2` `cb^=unnamed` `ts?` `ts&`）を読む。`cur` は今の値を読む設定
pub fn apply(cur: &Settings, token: &str) -> Result<Applied, String> {
    let unknown = |n: &str| format!("E518: 知らないオプション: {n}");
    if let Some(n) = token.strip_suffix('?') {
        let o = find(n).ok_or_else(|| unknown(n))?;
        return Ok(Applied::Query(show(cur, o)));
    }
    if let Some(n) = token.strip_suffix("&vim").or_else(|| token.strip_suffix('&')) {
        return Ok(Applied::Reset(find(n).ok_or_else(|| unknown(n))?.keys));
    }
    if let Some(n) = token.strip_suffix('!') {
        let o = find(n).filter(|o| o.kind == VKind::Bool).ok_or_else(|| unknown(n))?;
        return Ok(Applied::Set((o.put)(cur, &b(!on(&(o.get)(cur))))));
    }
    // 値つき（= か :。+= -= ^= は数なら足す・引く・掛ける、文字ならカンマの並びに足す・除く・頭に足す）
    if let Some(i) = token.find(['=', ':']) {
        let (name, op) = match &token[..i] {
            n if n.ends_with(['+', '-', '^']) => (&n[..n.len() - 1], n.chars().last()),
            n => (n, None),
        };
        let v = &token[i + 1..];
        let o = find(name).ok_or_else(|| unknown(name))?;
        let new = match (o.kind, op) {
            (VKind::Bool, _) => return Err(format!("E474: {name} は値を取らない")),
            (VKind::Num, op) => {
                let n: i64 = v.parse().map_err(|_| format!("E521: 数が要る: {token}"))?;
                let c: i64 = (o.get)(cur).parse().unwrap_or(0);
                match op {
                    Some('+') => c + n,
                    Some('-') => c - n,
                    Some('^') => c * n,
                    _ => n,
                }
                .to_string()
            }
            (VKind::Str, op) => {
                let c = (o.get)(cur);
                let mut items: Vec<&str> = c.split(',').filter(|x| !x.is_empty()).collect();
                match op {
                    Some('+') => items.push(v),
                    Some('^') => items.insert(0, v),
                    Some('-') => items.retain(|x| *x != v),
                    _ => items = vec![v],
                }
                items.join(",")
            }
        };
        return Ok(Applied::Set((o.put)(cur, &new)));
    }
    if let Some(o) = find(token) {
        return Ok(match o.kind {
            VKind::Bool => Applied::Set((o.put)(cur, "1")),
            _ => Applied::Query(show(cur, o)),
        });
    }
    for (pre, f) in [("no", false), ("inv", true)] {
        if let Some(o) = token.strip_prefix(pre).and_then(find).filter(|o| o.kind == VKind::Bool) {
            let v = if f { !on(&(o.get)(cur)) } else { false };
            return Ok(Applied::Set((o.put)(cur, &b(v))));
        }
    }
    Err(unknown(token))
}

/// vim の見せ方（"tabstop=4"、真偽は "wrap" か "nowrap"）
pub fn show(s: &Settings, o: &VimOpt) -> String {
    let v = (o.get)(s);
    match o.kind {
        VKind::Bool if on(&v) => o.names[0].to_string(),
        VKind::Bool => format!("no{}", o.names[0]),
        _ => format!("{}={v}", o.names[0]),
    }
}

/// vimrc の set（出所, 語）を順に読んで vimrc の層を作る（層, 出所, 読めなかった語）。
/// `base` の全体の設定・プロジェクトは見ない（vimrc だけで決まる値にする）
pub fn rc_layer(sets: &[(String, String)]) -> (HashMap<String, String>, HashMap<String, String>, Vec<String>) {
    let mut s = Settings::default();
    // 層の値を読むために、Vim をオンにした設定で積み上げる
    s.global.insert("vim.enabled".into(), "true".into());
    let (mut from, mut notes) = (HashMap::new(), Vec::new());
    for (at, tok) in sets {
        // nagi が持たないもの（IdeaVim のプラグインの surround など）は飛ばす。知らないものだけ書き留める
        match apply(&s, tok) {
            Ok(Applied::Set(kv)) => {
                for (k, v) in kv {
                    s.rc.insert(k.to_string(), v);
                    let file = at.rsplit(['\\', '/']).next().unwrap_or(at);
                    from.insert(k.to_string(), format!("vimrc（{file} set {tok}）"));
                }
            }
            Ok(Applied::Reset(keys)) => {
                for k in keys {
                    s.rc.remove(*k);
                    from.remove(*k);
                }
            }
            Ok(Applied::Query(_)) => {}
            Err(e) if !IGNORED.contains(&tok.split(['=', '+', '-', '^', ':']).next().unwrap_or("")) => notes.push(format!("{at}: {e}")),
            Err(_) => {}
        }
    }
    (s.rc, from, notes)
}

/// vimrc にあってもよい、nagi が設定として持たないもの（IdeaVim のプラグイン・ほかの動きで足りているもの）
const IGNORED: &[&str] = &["surround", "commentary", "easymotion", "multiple-cursors", "NERDTree", "argtextobj", "exchange", "textobj-entire", "ReplaceWithRegister", "idearefactormode", "ideaput", "ideamarks", "ideastatusicon", "visualbell", "vb", "noerrorbells", "showmode", "showcmd", "history", "hi", "nocompatible", "compatible", "encoding", "enc"];

/// `:set` の引数を、この窓だけの層に当てる。見るものがあれば、その文字を返す
pub fn ex_set(s: &mut Settings, arg: &str) -> Result<Option<String>, String> {
    let arg = arg.trim();
    if arg.is_empty() || arg == "all" {
        // 既定から変わっているもの（all ならすべて）
        let shown: Vec<String> = OPTS
            .iter()
            .filter(|o| arg == "all" || o.keys.iter().any(|k| schema(k).is_some_and(|i| s.get(k) != i.default)))
            .map(|o| show(s, o))
            .collect();
        return Ok(Some(if shown.is_empty() { "既定から変えたオプションはありません".into() } else { shown.join("  ") }));
    }
    let mut out = Vec::new();
    for tok in arg.split_whitespace() {
        match apply(s, tok)? {
            Applied::Set(kv) => {
                for (k, v) in kv {
                    s.session.insert(k.to_string(), v);
                }
            }
            Applied::Reset(keys) => {
                // この窓の値をやめ、既定に（vimrc・設定の画面の値も使わない。vim の & と同じ）
                for k in keys {
                    let def = schema(k).map_or("", |i| i.default);
                    s.session.insert(k.to_string(), def.to_string());
                }
            }
            Applied::Query(q) => out.push(q),
        }
    }
    Ok((!out.is_empty()).then(|| out.join("  ")))
}

impl crate::editor::Editor {
    /// `:set`（この窓だけ。設定のファイルには書かない）。fileencoding・fileformat・bomb は今のファイルに
    pub(crate) fn ex_set(&mut self, arg: &str) {
        let mut shown = Vec::new();
        let mut rest = Vec::new();
        for tok in arg.split_whitespace() {
            match self.buffer_option(tok) {
                Some(Ok(Some(s))) => shown.push(s),
                Some(Ok(None)) => {}
                Some(Err(e)) => {
                    self.notify.error(e);
                    return;
                }
                None => rest.push(tok),
            }
        }
        if rest.is_empty() && !arg.trim().is_empty() {
            if !shown.is_empty() {
                self.notify.info(shown.join("  "));
            }
            return;
        }
        let old = self.settings.clone();
        let r = ex_set(&mut self.settings, &rest.join(" ")).map(|m| match (m, shown.is_empty()) {
            (m, true) => m,
            (m, false) => Some(shown.into_iter().chain(m).collect::<Vec<_>>().join("  ")),
        });
        if self.settings != old {
            self.apply_settings(Some(&old));
        }
        match r {
            Ok(Some(m)) => self.notify.info(m),
            Ok(None) => {}
            Err(e) => self.notify.error(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        let s = Settings::default();
        assert_eq!(apply(&s, "sw=2"), Ok(Applied::Set(vec![("editor.indent_width", "2".into())])));
        assert_eq!(apply(&s, "noet"), Ok(Applied::Set(vec![("editor.indent_style", "tabs".into())])));
        assert_eq!(apply(&s, "invwrap"), Ok(Applied::Set(vec![("editor.wrap", "true".into())])));
        assert_eq!(apply(&s, "wrap!"), Ok(Applied::Set(vec![("editor.wrap", "true".into())])));
        assert_eq!(apply(&s, "sw+=2"), Ok(Applied::Set(vec![("editor.indent_width", "6".into())])));
        assert_eq!(apply(&s, "sw?"), Ok(Applied::Query("shiftwidth=4".into())));
        assert_eq!(apply(&s, "sw"), Ok(Applied::Query("shiftwidth=4".into())));
        assert_eq!(apply(&s, "rnu"), Ok(Applied::Set(vec![("editor.line_numbers", "relative".into())])));
        assert_eq!(apply(&s, "cb+=unnamedplus"), Ok(Applied::Set(vec![("vim.clipboard", "unnamed".into())])));
        assert!(apply(&s, "nosuch").is_err());
        assert!(apply(&s, "et=1").is_err());
    }

    /// vimrc の層は全体の設定より弱く、Vim がオフなら効かない。:set はいちばん強い
    #[test]
    fn layers() {
        let sets: Vec<(String, String)> = ["sw=2", "noet", "surround", "nosuch"].iter().map(|t| ("C:\\u\\.ideavimrc:3".to_string(), t.to_string())).collect();
        let (rc, from, notes) = rc_layer(&sets);
        assert_eq!(notes.len(), 1, "{notes:?}");
        let mut s = Settings::default();
        s.global.insert("vim.enabled".into(), "true".into());
        (s.rc, s.rc_from) = (rc, from);
        assert_eq!(s.int("editor.indent_width"), 2);
        assert_eq!(s.origin("editor.indent_width").as_deref(), Some("vimrc（.ideavimrc:3 set sw=2）"));
        // 画面で既定値（4）に戻しても、vimrc の 2 より画面の値が勝つ
        s.set("editor.indent_width", "4", false);
        assert_eq!(s.int("editor.indent_width"), 4);
        assert_eq!(s.origin("editor.indent_width"), None);
        assert_eq!(s.get("editor.indent_style"), "tabs");
        s.global.insert("vim.enabled".into(), "false".into());
        assert_eq!(s.get("editor.indent_style"), "spaces", "Vim を使わないなら vimrc は効かない");
        assert_eq!(ex_set(&mut s, "sw=3 sw?").unwrap().as_deref(), Some("shiftwidth=3"));
        assert_eq!(s.int("editor.indent_width"), 3);
        ex_set(&mut s, "sw&").unwrap();
        assert_eq!(s.int("editor.indent_width"), 4);
    }
}
