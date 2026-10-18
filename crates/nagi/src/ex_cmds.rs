//! 範囲を取る Ex コマンド: `:s` `:g` `:v` `:d` `:y` `:j` `:>` `:<` `:m` `:t` `:normal` `:&`。
//!
//! 範囲は `%` `.` `$` 数字、マーク（`'a` `'<` `'>`）と、それぞれに付ける `+N` `-N`。`,` でつなぐ。
//! 正規表現は Vim の方言ではなく regex クレートの書き方（architecture.md §8）。大文字小文字は区別しない
//! （`:s` の `I` フラグで区別する）。

use nagi_vim::Key;
use nagi_vim::cmd::Op;
use regex::{Regex, RegexBuilder};

use crate::editor::{Editor, Effect, Mode};

/// 直前の `:s`（`&` `g&` `:&&` で繰り返す）
#[derive(Clone, Default)]
pub struct LastSub {
    pub pat: String,
    pub rep: String,
    pub flags: String,
}

/// 区切り文字 `delim` で最大 `n` 個に分ける。`\{delim}` は区切りではなく文字そのもの（Vim と同じ）
fn split_delim(s: &str, delim: char, n: usize) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if out.len() < n && c == '\\' && chars.peek() == Some(&delim) {
            out.last_mut().unwrap().push(delim);
            chars.next();
        } else if out.len() < n && c == '\\' {
            out.last_mut().unwrap().push(c);
            if let Some(d) = chars.next() {
                out.last_mut().unwrap().push(d);
            }
        } else if c == delim && out.len() < n {
            out.push(String::new());
        } else {
            out.last_mut().unwrap().push(c);
        }
    }
    out
}

fn compile(pat: &str, case: Option<bool>) -> Option<Regex> {
    // case: Some(true) は区別する、Some(false) と None は区別しない
    RegexBuilder::new(pat).case_insensitive(case != Some(true)).multi_line(true).build().ok()
}

impl Editor {
    /// 1 つの位置（`.` `$` 数字 `'x` と `+N` `-N`）。何も無ければ None
    fn parse_addr<'a>(&self, s: &'a str) -> nagi_core::Result<(Option<usize>, &'a str)> {
        let last = self.buf.line_count().saturating_sub(1);
        let mut rest = s;
        let mut base: Option<usize> = None;
        if let Some(r) = rest.strip_prefix('.') {
            base = Some(self.cursor.line);
            rest = r;
        } else if let Some(r) = rest.strip_prefix('$') {
            base = Some(last);
            rest = r;
        } else if rest.starts_with(|c: char| c.is_ascii_digit()) {
            let n = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
            base = Some(rest[..n].parse::<usize>().unwrap_or(1).saturating_sub(1));
            rest = &rest[n..];
        } else if let Some(r) = rest.strip_prefix('\'') {
            let c = r.chars().next().ok_or("E20: マークがありません")?;
            let pos = match c {
                '\'' | '`' => self.marks.pc,
                _ => self.marks.named.get(&c).copied(),
            }
            .ok_or_else(|| format!("E20: マークは設定されていません: {c}"))?;
            base = Some(pos.line);
            rest = &r[c.len_utf8()..];
        }
        // +N / -N（数字が無ければ 1）。位置が無くて足し引きだけなら今の行から
        while let Some(sign) = rest.chars().next().filter(|c| *c == '+' || *c == '-') {
            let r = &rest[1..];
            let n = r.find(|c: char| !c.is_ascii_digit()).unwrap_or(r.len());
            let v = if n == 0 { 1 } else { r[..n].parse::<usize>().unwrap_or(1) };
            let b = base.unwrap_or(self.cursor.line);
            base = Some(if sign == '+' { b + v } else { b.saturating_sub(v) });
            rest = &r[n..];
        }
        Ok((base.map(|b| b.min(last)), rest))
    }

    /// 範囲（行は 0 始まり）と残り。範囲が無ければ None
    pub(crate) fn parse_range<'a>(&self, s: &'a str) -> nagi_core::Result<(Option<(usize, usize)>, &'a str)> {
        let s = s.trim_start();
        if let Some(r) = s.strip_prefix('%') {
            return Ok((Some((0, self.buf.line_count().saturating_sub(1))), r));
        }
        let (a, rest) = self.parse_addr(s)?;
        let Some(a) = a else { return Ok((None, rest)) };
        if let Some(r) = rest.strip_prefix(',').or_else(|| rest.strip_prefix(';')) {
            let (b, rest) = self.parse_addr(r)?;
            let b = b.unwrap_or(a);
            if b < a {
                return Ok((Some((b, a)), rest));
            }
            return Ok((Some((a, b)), rest));
        }
        Ok((Some((a, a)), rest))
    }

    /// `|` で次のコマンドへ区切らないコマンドか（:s :g :v :normal はパターンやキーに | を含められる。Vim と同じ）
    pub(crate) fn keeps_bar(&self, line: &str) -> bool {
        let rest = self.parse_range(line.trim_start_matches(':')).map_or(line, |(_, r)| r).trim_start();
        let n = rest.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(rest.len());
        let (name, arg) = rest.split_at(n);
        let delim = arg.chars().next().is_some_and(|c| !c.is_alphanumeric() && !c.is_whitespace());
        matches!(name, "norm" | "normal") || (delim && matches!(name, "s" | "substitute" | "g" | "global" | "v" | "vglobal"))
    }

    /// 範囲を取るコマンドなら実行して Some を返す。そうでなければ None（ほかの Ex コマンドとして扱う）
    pub(crate) fn ex_ranged(&mut self, cmd: &str) -> Option<Effect> {
        let (range, rest) = match self.parse_range(cmd) {
            Ok(v) => v,
            Err(e) => {
                self.notify.info(e);
                return Some(Effect::None);
            }
        };
        let rest = rest.trim_start();
        // `:{行}` だけなら移動
        if rest.is_empty() {
            if let Some((_, l)) = range {
                self.push_jump();
                self.with_motion(|m, c| m.goto_line(c, l));
                return Some(Effect::None);
            }
            return None;
        }
        let cur = (self.cursor.line, self.cursor.line);
        let name_len = rest.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(rest.len());
        let (name, arg) = rest.split_at(name_len);
        // `s/…/…/` `g/…/…` のように、名前の直後が区切り文字のもの
        let delim_after = |n: &str| name == n && arg.chars().next().is_some_and(|c| !c.is_alphanumeric() && !c.is_whitespace() && c != '"' && c != '|');
        if delim_after("s") || delim_after("substitute") {
            self.substitute(range.unwrap_or(cur), arg, None);
            return Some(Effect::None);
        }
        if (name == "s" || name == "substitute") && arg.trim().is_empty() || rest == "&" || rest == "&&" {
            // :s / :& は直前の置換をフラグなしで、:&& はフラグごと繰り返す
            let keep = rest == "&&";
            self.repeat_sub(range.unwrap_or(cur), keep);
            return Some(Effect::None);
        }
        if delim_after("g") || delim_after("global") || delim_after("v") || delim_after("vglobal") || name == "g" && arg.starts_with('!') {
            let invert = name.starts_with('v') || arg.starts_with('!');
            let arg = arg.strip_prefix('!').unwrap_or(arg);
            let all = (0, self.buf.line_count().saturating_sub(1));
            return Some(self.global(range.unwrap_or(all), arg, invert));
        }
        let arg = arg.trim();
        let (l1, l2) = range.unwrap_or(cur);
        match name {
            "d" | "de" | "del" | "delete" => {
                let reg = arg.chars().next();
                let (s, e) = self.lines_range(l1, l2);
                self.apply(Op::Delete, s, e, true, reg);
                // Ex の :d は、消した次の行の最初の空白でない文字へ（Normal の dd と違い列を保たない）
                self.with_motion(|m, c| m.first_non_blank(c));
            }
            "y" | "ya" | "yank" => {
                let reg = arg.chars().next();
                let (s, e) = self.lines_range(l1, l2);
                let cur = self.cursor;
                self.apply(Op::Yank, s, e, true, reg);
                self.cursor = cur;
            }
            "j" | "join" => {
                // 範囲が 1 行なら次の行と（Vim と同じ）
                let times = if l2 > l1 { l2 - l1 } else { 1 };
                self.join(l1, times);
                self.cursor.line = l1;
                self.with_motion(|m, c| m.first_non_blank(c));
            }
            "m" | "move" | "t" | "co" | "copy" => {
                let (to, _) = match self.parse_addr(arg) {
                    Ok(v) => v,
                    Err(e) => {
                        self.notify.info(e);
                        return Some(Effect::None);
                    }
                };
                // 0 は先頭の前。`:m 0` で 1 行目の上へ
                let to = if arg.trim() == "0" { None } else { Some(to.unwrap_or(self.cursor.line)) };
                self.move_copy(l1, l2, to, name.starts_with('m'));
            }
            "norm" | "normal" | "norm!" | "normal!" => return Some(self.normal_cmd(range.unwrap_or(cur), arg)),
            "" if rest.starts_with('>') || rest.starts_with('<') => {
                let op = if rest.starts_with('>') { Op::Indent } else { Op::Dedent };
                // `:>>` は 2 段
                let n = rest.chars().take_while(|&c| c == '>' || c == '<').count();
                for _ in 0..n {
                    let (s, e) = self.lines_range(l1, l2);
                    self.apply(op, s, e, true, None);
                }
                // 範囲の最後の行の最初の空白でない文字へ（Vim と同じ）
                self.cursor.line = l2.min(self.buf.line_count().saturating_sub(1));
                self.with_motion(|m, c| m.first_non_blank(c));
            }
            _ if range.is_some() => {
                self.notify.error(format!("E492: 範囲を取るコマンドではありません: {rest}"));
            }
            _ => return None,
        }
        Some(Effect::None)
    }

    /// `:m` / `:t`。`to` の行の下へ（None は先頭へ）
    fn move_copy(&mut self, l1: usize, l2: usize, to: Option<usize>, mv: bool) {
        if mv && to.is_some_and(|t| t >= l1 && t < l2) {
            self.notify.error("E134: 範囲の中へは移せません");
            return;
        }
        let nl = self.buf.newline().to_string();
        let mut text: String = (l1..=l2).map(|l| self.buf.line(l) + &nl).collect();
        let undo = self.hist.len();
        self.hist.begin();
        // 先に入れてから消す（入れる位置が範囲より前でも後でもずれないように）
        let total = self.buf.line_count();
        let at = match to {
            None => 0,
            Some(t) if t + 1 >= total => {
                // 最終行の後ろ: 改行が無ければ足してから
                let end = self.buf.len_bytes();
                if !self.buf.slice_string(end.saturating_sub(1), end).ends_with('\n') {
                    text = format!("{nl}{}", text.trim_end_matches(['\r', '\n']));
                }
                end
            }
            Some(t) => self.buf.line_start(t + 1),
        };
        self.edit(at, at, &text);
        let n = l2 - l1 + 1;
        let dest_first = match to {
            None => 0,
            Some(t) => t + 1,
        };
        if mv {
            let (sl, sl2) = if dest_first <= l1 { (l1 + n, l2 + n) } else { (l1, l2) };
            let (s, e) = self.lines_range(sl, sl2);
            self.edit(s, e, "");
        }
        self.hist.close();
        self.hist.squash_from(undo);
        let last_line = if mv && dest_first > l1 { dest_first - 1 } else { dest_first + n - 1 };
        self.cursor.line = last_line.min(self.buf.line_count().saturating_sub(1));
        self.with_motion(|m, c| m.first_non_blank(c));
    }

    /// `:normal {keys}`: 範囲の行ごとに、行頭からキーを流す。Insert などは最後に抜ける
    fn normal_cmd(&mut self, (l1, l2): (usize, usize), keys: &str) -> Effect {
        let keys: Vec<Key> = keys
            .chars()
            .map(|c| match c {
                '\u{1b}' => Key::Esc,
                '\r' | '\n' => Key::Enter,
                c => Key::Char(c),
            })
            .collect();
        let lines: Vec<Option<usize>> = (l1..=l2).map(Some).collect();
        self.for_each_line(lines, |ed| {
            for &k in &keys {
                if let Effect::Quit = ed.key(k) {
                    return Effect::Quit;
                }
            }
            if ed.mode != Mode::Normal {
                ed.key(Key::Esc);
            }
            Effect::None
        })
    }

    /// 行ごとに `f` を実行する。行は編集でずらし、消えた行は飛ばす。全体を 1 回の u で戻せるようにする
    fn for_each_line(&mut self, lines: Vec<Option<usize>>, mut f: impl FnMut(&mut Editor) -> Effect) -> Effect {
        // 入れ子（:g の中の :normal）でも、外側の目印は編集でずれ続けるように段ごとに積む
        let outer = !self.line_anchors.is_empty();
        let level = self.line_anchors.len();
        self.line_anchors.push(lines);
        let undo = self.hist.len();
        let mut i = 0;
        let mut effect = Effect::None;
        loop {
            let Some(entry) = self.line_anchors.get(level).and_then(|v| v.get(i).copied()) else { break };
            i += 1;
            let Some(l) = entry else { continue };
            self.cursor.line = l.min(self.buf.line_count().saturating_sub(1));
            self.cursor.byte = 0;
            if let Effect::Quit = f(self) {
                effect = Effect::Quit;
                break;
            }
        }
        self.line_anchors.truncate(level);
        if !outer {
            self.hist.squash_from(undo);
        }
        self.clamp_normal();
        effect
    }

    /// `:g/pat/cmd`（`invert` なら当たらない行。`:g!` と `:v`）
    fn global(&mut self, (l1, l2): (usize, usize), arg: &str, invert: bool) -> Effect {
        if self.in_global {
            self.notify.error("E147: :g の中で :g は使えません");
            return Effect::None;
        }
        let Some(delim) = arg.chars().next() else { return Effect::None };
        let parts = split_delim(&arg[delim.len_utf8()..], delim, 2);
        let (pat, cmd) = (parts.first().cloned().unwrap_or_default(), parts.get(1).cloned().unwrap_or_default());
        let pat = if pat.is_empty() { self.search.last.as_ref().map(|l| l.0.clone()).unwrap_or_default() } else { pat.to_string() };
        let Some(re) = compile(&pat, None) else {
            self.notify.error(format!("E486: パターンが不正です: {pat}"));
            return Effect::None;
        };
        let lines: Vec<Option<usize>> = (l1..=l2).filter(|&l| re.is_match(&self.buf.line(l)) != invert).map(Some).collect();
        if lines.is_empty() {
            self.notify.error(format!("E486: パターンは見つかりませんでした: {pat}"));
            return Effect::None;
        }
        let cmd = cmd.trim();
        if cmd.is_empty() || cmd == "p" || cmd == "print" {
            // 行を並べて出す代わりに、件数と最初の行を知らせる
            let n = lines.len();
            self.cursor.line = lines.last().copied().flatten().unwrap_or(l1);
            self.with_motion(|m, c| m.first_non_blank(c));
            self.notify.info(format!("{n} 行が一致しました"));
            return Effect::None;
        }
        let cmd = cmd.to_string();
        let mut last = None;
        self.in_global = true;
        let e = self.for_each_line(lines, |ed| {
            let e = ed.run_ex(&cmd);
            last = Some(ed.cursor);
            e
        });
        self.in_global = false;
        if let Some(c) = last {
            self.cursor = c;
            self.clamp_normal();
        }
        e
    }

    /// 1 行ずつ処理する `:g` `:normal` の行の目印をずらす（編集の前に呼ぶ）
    /// `whole` は行頭から行頭までの置き換え（sl..el の行を丸ごと消した）
    pub(crate) fn shift_anchors(&mut self, sl: usize, el: usize, nl: usize, whole: bool) {
        for e in self.line_anchors.iter_mut().flatten() {
            if let Some(l) = *e {
                *e = if l > el {
                    Some(l + nl - (el - sl))
                } else if l == el && el > sl {
                    Some(sl + nl)
                } else if (l > sl && l < el) || (whole && l == sl && el > sl) {
                    None // 消えた行（印の付いた行が消えたら飛ばす。Vim と同じ）
                } else {
                    Some(l)
                };
            }
        }
    }

    /// `:[range]s/pat/rep/flags`。`last` は `&` などで繰り返すときの直前の置換
    pub(crate) fn substitute(&mut self, (l1, l2): (usize, usize), arg: &str, last: Option<LastSub>) {
        let (pat, rep, mut flags) = match last {
            Some(l) => (l.pat, l.rep, l.flags),
            None => {
                let Some(delim) = arg.chars().next() else { return };
                let parts = split_delim(&arg[delim.len_utf8()..], delim, 3);
                (parts.first().cloned().unwrap_or_default(), parts.get(1).cloned().unwrap_or_default(), parts.get(2).cloned().unwrap_or_default().trim().to_string())
            }
        };
        if let Some(f) = flags.strip_prefix('&') {
            flags = format!("{}{f}", self.last_sub.as_ref().map_or("", |l| l.flags.as_str()));
        }
        // 確認付き（c）は置換の入力欄で 1 件ずつ（ideavimrc の <C-s> = :%s///cg）
        if flags.contains('c') {
            let p = if pat.is_empty() { self.search.last.as_ref().map(|l| l.0.clone()).unwrap_or_default() } else { pat.clone() };
            self.start_search(true, true, &p);
            if !rep.is_empty() {
                self.repl.set(&rep);
            }
            if !p.is_empty() {
                self.search.regex = true;
                self.update_incsearch();
            }
            return;
        }
        // 空のパターンは直前の検索語（Vim と同じ）
        let pat = if pat.is_empty() {
            match self.search.last.as_ref() {
                Some((p, true, _)) => p.clone(),
                Some((p, false, _)) => regex::escape(p),
                None => {
                    self.notify.error("E35: 直前の検索語がありません");
                    return;
                }
            }
        } else {
            pat
        };
        let case = if flags.contains('I') { Some(true) } else if flags.contains('i') { Some(false) } else { None };
        let Some(re) = compile(&pat, case) else {
            self.notify.error(format!("E486: パターンが不正です: {pat}"));
            return;
        };
        self.last_sub = Some(LastSub { pat: pat.clone(), rep: rep.clone(), flags: flags.clone() });
        // 置換の後の n / N は、置換したパターンで探す（Vim と同じ）
        self.search.last = Some((pat.clone(), true, true));
        let global = flags.contains('g');
        if flags.contains('n') {
            let (mut n, mut lines) = (0, 0);
            for l in l1..=l2 {
                let c = if global { re.find_iter(&self.buf.line(l)).count() } else { usize::from(re.is_match(&self.buf.line(l))) };
                n += c;
                lines += usize::from(c > 0);
            }
            self.notify.info(format!("{n} 箇所に一致（{lines} 行）"));
            return;
        }
        // Vim の置換文字列（\1 と &、\r の改行、\t）を regex の書き方に直す
        let nl = self.buf.newline().to_string();
        let rep = rep.replace("\\\\", "\u{1}").replace("\\&", "\u{0}").replace('&', "${0}").replace('\u{0}', "&");
        let rep = (1..=9).fold(rep, |r, i| r.replace(&format!("\\{i}"), &format!("${{{i}}}")));
        let rep = rep.replace("\\r", &nl).replace("\\n", &nl).replace("\\t", "\t").replace('\u{1}', "\\");
        let mut count = 0;
        let mut last_hit = None;
        let undo = self.hist.len();
        self.hist.begin();
        for l in (l1..=l2).rev() {
            let line = self.buf.line(l);
            let new = if global { re.replace_all(&line, rep.as_str()) } else { re.replace(&line, rep.as_str()) };
            if new != line {
                // 改行を足した置換では、最後の行の頭へ（Vim と同じ）
                last_hit = last_hit.or(Some(l + new.matches('\n').count()));
                count += if global { re.find_iter(&line).count() } else { 1 };
                let start = self.buf.line_start(l);
                let new = new.into_owned();
                self.edit(start, start + line.len(), &new);
            }
        }
        self.hist.close();
        self.hist.squash_from(undo);
        if count == 0 {
            if !flags.contains('e') {
                self.notify.error(format!("E486: パターンは見つかりませんでした: {pat}"));
            }
        } else if self.line_anchors.is_empty() {
            self.notify.info(format!("{count} 箇所を置換しました"));
        }
        if let Some(l) = last_hit {
            self.cursor.line = l.min(self.buf.line_count().saturating_sub(1));
            self.with_motion(|m, c| m.first_non_blank(c));
        }
        self.clamp_normal();
    }

    /// `&` / `:s` / `:&`（フラグなし）と `g&` / `:&&`（フラグごと）
    pub(crate) fn repeat_sub(&mut self, range: (usize, usize), keep_flags: bool) {
        let Some(mut last) = self.last_sub.clone() else {
            self.notify.error("E35: 直前の置換がありません");
            return;
        };
        if !keep_flags {
            last.flags.clear();
        }
        self.substitute(range, "", Some(last));
    }
}
