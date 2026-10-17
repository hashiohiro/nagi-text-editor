//! Normal / Visual モードの実行（移動・オペレーター・text object・レジスタ・`.` リピート）。
//!
//! キー列の解釈は nagi-vim の `cmd` が受け持ち、ここは解釈済みのコマンドを実行する。

use nagi_core::textops::{self, Class, class};
use nagi_core::{Cursor, display_col};
use nagi_vim::Key;
use nagi_vim::cmd::{Action, Cmd, Kind, Motion as M, Op, Parse, Target, TextObj, parse_normal, parse_visual};
use unicode_segmentation::UnicodeSegmentation;

use crate::editor::{Editor, Effect, Mode};

/// 移動の種類（オペレーターがどこまでを対象にするか）
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kd {
    /// 移動先の文字を含まない
    Exclusive,
    /// 移動先の文字を含む
    Inclusive,
    /// 行全体
    Linewise,
}

const SHIFT: &str = "    ";

fn leading_ws(line: &str) -> &str {
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
}

impl Editor {
    pub(crate) fn normal_key(&mut self, key: Key) -> Effect {
        self.pending_keys.push(key);
        let visual = matches!(self.mode, Mode::Visual { .. });
        let parsed = if visual { parse_visual(&self.pending_keys) } else { parse_normal(&self.pending_keys) };
        match parsed {
            Parse::Incomplete => Effect::None,
            Parse::Invalid => {
                self.pending_keys.clear();
                Effect::None
            }
            Parse::Done(cmd) => {
                let keys = std::mem::take(&mut self.pending_keys);
                self.exec(cmd, keys)
            }
        }
    }

    /// 変更を伴うコマンドか（`.` で繰り返す対象）
    fn is_change(kind: &Kind) -> bool {
        match kind {
            Kind::Operate(op, _) => *op != Op::Yank,
            Kind::Act(a) => matches!(
                a,
                Action::Insert(_) | Action::Put { .. } | Action::Join | Action::Replace(_) | Action::ToggleCase
            ) || matches!(a, Action::Short(c) if *c != 'Y'),
            _ => false,
        }
    }

    /// `.` の記録を終える（Insert を抜けたとき、または変更が 1 打で終わったとき）
    pub(crate) fn finish_dot(&mut self) {
        if let Some(rec) = self.dot_rec.take() {
            if !self.replaying {
                self.dot = rec;
            }
        }
    }

    fn exec(&mut self, cmd: Cmd, keys: Vec<Key>) -> Effect {
        if Self::is_change(&cmd.kind) && !self.replaying {
            self.dot_rec = Some(keys);
        }
        let was_visual = matches!(self.mode, Mode::Visual { .. }).then(|| self.visual_range());
        let effect = self.exec_inner(cmd);
        // Visual を抜けたら、選んでいた範囲を '< '> に残す
        if let (Some((s, e, _)), false) = (was_visual, matches!(self.mode, Mode::Visual { .. })) {
            let at = |a: usize| {
                let (l, b) = self.buf.pos_of(a.min(self.buf.len_bytes()));
                Cursor { line: l, byte: b, want_col: 0 }
            };
            let (a, b) = (at(s), at(e.saturating_sub(1).max(s)));
            self.marks.named.insert('<', a);
            self.marks.named.insert('>', b);
        }
        // Insert に入らなかった変更はここで記録を終える
        if self.mode != Mode::Insert {
            self.finish_dot();
        }
        if matches!(self.mode, Mode::Normal) {
            self.clamp_normal();
        }
        effect
    }

    fn exec_inner(&mut self, cmd: Cmd) -> Effect {
        let n = cmd.count.unwrap_or(1);
        let visual = match self.mode {
            Mode::Visual { line } => Some(line),
            _ => None,
        };
        match cmd.kind {
            Kind::Move(m) => {
                let from = self.cursor;
                let doc = self.cur_doc;
                if let Some((c, _)) = self.dest(m, cmd.count, false) {
                    // 大きく跳ぶ移動はジャンプリストに残す（別のファイルへのマークは開くときに残している）
                    let jump = matches!(m, M::GotoLine | M::FirstLine | M::MatchPair | M::Paragraph { .. } | M::SearchNext { .. } | M::Mark { .. });
                    if jump && doc == self.cur_doc && (c.line, c.byte) != (from.line, from.byte) {
                        self.push_jump_from(from);
                    }
                    self.cursor = c;
                }
            }
            Kind::Operate(op, t) => self.operate(op, t, cmd.count, cmd.reg),
            Kind::VisualOp(op) => {
                let (s, e, lw) = self.visual_range();
                self.mode = Mode::Normal;
                self.apply(op, s, e, lw, cmd.reg);
            }
            Kind::VisualObj { inner, obj } => {
                if let Some((s, e, lw)) = self.object(inner, obj, n) {
                    self.anchor = s;
                    self.set_abs(self.prev_grapheme(e).max(s));
                    if lw {
                        self.mode = Mode::Visual { line: true };
                    }
                }
            }
            Kind::VisualSwap => {
                let a = self.abs();
                self.set_abs(self.anchor);
                self.anchor = a;
            }
            Kind::Act(a) => return self.act(a, cmd, visual),
        }
        Effect::None
    }

    // ---------------------------------------------------------------- 移動

    fn line_len(&self, l: usize) -> usize {
        self.buf.line_len(l)
    }

    /// 絶対位置 `abs` の書記素の長さ
    fn grapheme_len_at(&self, abs: usize) -> usize {
        let (l, b) = self.buf.pos_of(abs);
        let line = self.buf.line(l);
        line.get(b..).and_then(|r| r.graphemes(true).next()).map_or(0, str::len)
    }

    fn prev_grapheme(&self, abs: usize) -> usize {
        let (l, b) = self.buf.pos_of(abs);
        // 行頭なら、前の行の改行
        if b == 0 && l > 0 {
            return self.buf.line_start(l - 1) + self.line_len(l - 1);
        }
        let line = self.buf.line(l);
        let start = self.buf.line_start(l);
        line[..b.min(line.len())].grapheme_indices(true).next_back().map_or(start, |(i, _)| start + i)
    }

    /// 移動先のカーソルと、移動の種類。`op` はオペレーターの対象として計算する（l で行末を越えられる等）
    fn dest(&mut self, m: M, count: Option<usize>, op: bool) -> Option<(Cursor, Kd)> {
        let n = count.unwrap_or(1);
        let mut c = self.cursor;
        let mo = self.motion();
        let kd = match m {
            M::Left => {
                mo.left(&mut c, n);
                Kd::Exclusive
            }
            M::Right => {
                let line = self.buf.line(c.line);
                // カーソルから先の n+1 個だけ見る（長い行で行全体を書記素に分けない）
                let from = if line.is_char_boundary(c.byte.min(line.len())) { c.byte.min(line.len()) } else { 0 };
                let gs: Vec<usize> = line[from..].grapheme_indices(true).map(|(i, _)| from + i).chain([line.len()]).take(n + 2).collect();
                let reached_end = gs.last() == Some(&line.len());
                // オペレーターの後は行末（改行の手前）まで、そうでなければ最後の書記素まで
                let max = if op || !reached_end { gs.len() - 1 } else { gs.len().saturating_sub(2) };
                c.byte = gs[n.min(max)];
                c.want_col = display_col(&line, c.byte, false);
                Kd::Exclusive
            }
            M::Up => {
                if c.line < n && !op {
                    return None;
                }
                mo.vertical(&mut c, -(n as isize));
                Kd::Linewise
            }
            M::Down => {
                mo.vertical(&mut c, n as isize);
                Kd::Linewise
            }
            M::WordFwd { big } => {
                (0..n).for_each(|_| mo.word_forward(&mut c, big));
                Kd::Exclusive
            }
            M::WordBack { big } => {
                (0..n).for_each(|_| mo.word_backward(&mut c, big));
                Kd::Exclusive
            }
            M::WordEnd { big } => {
                (0..n).for_each(|_| mo.word_end(&mut c, big));
                Kd::Inclusive
            }
            M::LineStart => {
                mo.line_start(&mut c);
                Kd::Exclusive
            }
            M::FirstNonBlank => {
                mo.first_non_blank(&mut c);
                Kd::Exclusive
            }
            M::LineEnd => {
                if n > 1 {
                    mo.vertical(&mut c, (n - 1) as isize);
                }
                mo.line_end(&mut c);
                Kd::Inclusive
            }
            // Neovim の既定（nostartofline）と同じく、列を保ったまま行を移る
            M::GotoLine | M::FirstLine => {
                let last = self.buf.line_count().saturating_sub(1);
                let target = match (m, count) {
                    (_, Some(n)) => (n - 1).min(last),
                    (M::GotoLine, None) => last,
                    _ => 0,
                };
                let d = target as isize - c.line as isize;
                mo.vertical(&mut c, d);
                Kd::Linewise
            }
            M::Find { ch, forward, till } => {
                self.last_find = Some((ch, forward, till));
                return self.find(ch, forward, till, n);
            }
            M::RepeatFind { reverse } => {
                let (ch, forward, till) = self.last_find?;
                return self.find(ch, forward != reverse, till, n);
            }
            M::MatchPair => {
                let abs = self.abs();
                let end = self.buf.line_start(c.line) + self.line_len(c.line);
                let to = textops::match_pair(&self.buf, abs, end)?;
                let (l, b) = self.buf.pos_of(to);
                c.line = l;
                c.byte = b;
                Kd::Inclusive
            }
            M::Paragraph { forward } => {
                let total = self.buf.line_count();
                let blank = |l: usize| self.buf.line(l).trim().is_empty();
                let mut l = c.line;
                for _ in 0..n {
                    if forward {
                        while l + 1 < total && blank(l) {
                            l += 1;
                        }
                        while l + 1 < total && !blank(l) {
                            l += 1;
                        }
                    } else {
                        while l > 0 && blank(l) {
                            l -= 1;
                        }
                        while l > 0 && !blank(l) {
                            l -= 1;
                        }
                    }
                }
                c.line = l;
                c.byte = if forward && !blank(l) { self.line_len(l) } else { 0 };
                // 末尾まで空行が無いとき、行頭からの d} は行単位になる（Vim の例外）
                if op && forward && !blank(l) && self.cursor.byte == 0 {
                    Kd::Linewise
                } else {
                    Kd::Exclusive
                }
            }
            M::Mark { ch, exact } => {
                let to = self.mark_pos(ch)?;
                c.line = to.line;
                c.byte = to.byte;
                if exact {
                    Kd::Exclusive
                } else {
                    let line = self.buf.line(c.line);
                    c.byte = line.len() - line.trim_start_matches([' ', '\t']).len();
                    Kd::Linewise
                }
            }
            M::SearchNext { reverse } => {
                let at = self.search_next(reverse, n)?;
                let (l, b) = self.buf.pos_of(at);
                c.line = l;
                c.byte = b;
                Kd::Exclusive
            }
        };
        if !matches!(m, M::Up | M::Down) {
            let line = self.buf.line(c.line);
            if m != M::LineEnd {
                c.want_col = display_col(&line, c.byte.min(line.len()), false);
            }
        }
        Some((c, kd))
    }

    fn find(&self, ch: char, forward: bool, till: bool, n: usize) -> Option<(Cursor, Kd)> {
        let mut c = self.cursor;
        let line = self.buf.line(c.line);
        let b = textops::find_in_line(&line, c.byte, ch, forward, till, n)?;
        c.byte = b;
        c.want_col = display_col(&line, b, false);
        Some((c, if forward { Kd::Inclusive } else { Kd::Exclusive }))
    }

    // ---------------------------------------------------------------- 範囲

    /// 行 l1..=l2 の範囲（改行込み）。最終行で改行が無ければ、前の行の改行を含める
    fn lines_range(&self, l1: usize, l2: usize) -> (usize, usize) {
        let l2 = l2.min(self.buf.line_count().saturating_sub(1));
        let start = self.buf.line_start(l1);
        let end = self.buf.line_start(l2) + self.buf.line_len_with_break(l2);
        if end == self.buf.len_bytes() && !self.buf.slice_string(start, end).ends_with('\n') && l1 > 0 {
            let brk = self.buf.line_len_with_break(l1 - 1) - self.buf.line(l1 - 1).len();
            (start - brk, end)
        } else {
            (start, end)
        }
    }

    /// 行単位の範囲 [s, e)（`text` はその中身）が含む最初と最後の行
    fn lines_of(&self, s: usize, e: usize, text: &str) -> (usize, usize) {
        // 最終行を消す範囲は、前の行の改行から始まる
        let first = self.buf.pos_of(if text.starts_with(['\r', '\n']) && s > 0 { s + text.len() - text.trim_start_matches(['\r', '\n']).len() } else { s }).0;
        let last = self.buf.pos_of(if text.ends_with('\n') { e - 1 } else { e }).0;
        (first, last.max(first))
    }

    /// Visual の選択範囲 (start, end, 行単位か)
    pub fn visual_range(&self) -> (usize, usize, bool) {
        let a = self.anchor.min(self.buf.len_bytes());
        let b = self.abs();
        let (s, e) = (a.min(b), a.max(b));
        match self.mode {
            Mode::Visual { line: true } => {
                let (l1, l2) = (self.buf.pos_of(s).0, self.buf.pos_of(e).0);
                let (s, e) = self.lines_range(l1, l2);
                (s, e, true)
            }
            _ => (s, e + self.grapheme_len_at(e).max(if e < self.buf.len_bytes() { 1 } else { 0 }), false),
        }
    }

    /// text object の範囲 (start, end, 行単位か)
    fn object(&self, inner: bool, obj: TextObj, n: usize) -> Option<(usize, usize, bool)> {
        let ls = self.buf.line_start(self.cursor.line);
        // 行の文字列は単語と引用符のときだけ作る（括弧と段落は rope の上で探す）
        let line = || self.buf.line(self.cursor.line);
        match obj {
            TextObj::Word { big } => {
                let line = line();
                let (mut s, mut e) = textops::word_object(&line, self.cursor.byte.min(line.len()), big, !inner)?;
                for _ in 1..n {
                    if let Some((_, e2)) = textops::word_object(&line, e.min(line.len().saturating_sub(1)), big, !inner) {
                        e = e2;
                    }
                }
                if line.is_empty() {
                    s = 0;
                    e = 0;
                }
                Some((ls + s, ls + e, false))
            }
            TextObj::Quote(q) => {
                let line = line();
                let (a, b) = textops::enclosing_quote(&line, self.cursor.byte, q)?;
                if inner {
                    return Some((ls + a + 1, ls + b, false));
                }
                // a" は後ろの空白を含める。後ろに無ければ前の空白を含める（Vim と同じ）
                let after = line[b + 1..].len() - line[b + 1..].trim_start_matches([' ', '\t']).len();
                let before = line[..a].len() - line[..a].trim_end_matches([' ', '\t']).len();
                Some(if after > 0 { (ls + a, ls + b + 1 + after, false) } else { (ls + a - before, ls + b + 1, false) })
            }
            TextObj::Block(open) => {
                let mut at = self.abs();
                let mut range = textops::enclosing_block(&self.buf, at, open)?;
                for _ in 1..n {
                    at = range.0.saturating_sub(1);
                    match textops::enclosing_block(&self.buf, at, open) {
                        Some(r) => range = r,
                        None => break,
                    }
                }
                let (a, b) = range;
                if !inner {
                    return Some((a, b + 1, false));
                }
                let mut s = a + 1;
                let mut e = b;
                // 複数行のブロックの内側は、括弧の行を除いた行全体（Vim と同じ）
                let (al, ab) = self.buf.pos_of(a);
                if ab + 1 == self.line_len(al) {
                    s = self.buf.line_start(al + 1);
                    let line_start = self.buf.line_start(self.buf.pos_of(e).0);
                    if self.buf.slice_string(line_start, e).trim().is_empty() && line_start >= s {
                        e = line_start;
                    }
                }
                Some((s, e, false))
            }
            TextObj::Paragraph => {
                let total = self.buf.line_count();
                let blank = |l: usize| self.buf.line(l).trim().is_empty();
                let kind = blank(self.cursor.line);
                let (mut l1, mut l2) = (self.cursor.line, self.cursor.line);
                while l1 > 0 && blank(l1 - 1) == kind {
                    l1 -= 1;
                }
                while l2 + 1 < total && blank(l2 + 1) == kind {
                    l2 += 1;
                }
                if !inner {
                    let before = l2;
                    while l2 + 1 < total && blank(l2 + 1) != kind {
                        l2 += 1;
                    }
                    // 後ろに空行が無ければ、前の空行を含める（Vim と同じ）
                    if l2 == before {
                        while l1 > 0 && blank(l1 - 1) != kind {
                            l1 -= 1;
                        }
                    }
                }
                let (s, e) = self.lines_range(l1, l2);
                Some((s, e, true))
            }
        }
    }

    // ---------------------------------------------------------------- オペレーター

    fn operate(&mut self, op: Op, t: Target, count: Option<usize>, reg: Option<char>) {
        let n = count.unwrap_or(1);
        let from = self.cursor;
        let from_abs = self.abs();
        let (s, e, lw) = match t {
            Target::Lines => {
                let l2 = from.line + n - 1;
                if l2 >= self.buf.line_count() && n > 1 {
                    return;
                }
                let (s, e) = self.lines_range(from.line, l2);
                (s, e, true)
            }
            Target::Obj { inner, obj } => match self.object(inner, obj, n) {
                Some((s, e, lw)) => {
                    // 複数行のブロックの内側が丸ごとの行になったら、行単位で扱う（Vim の exclusive-linewise）。
                    // 消したときのカーソルは閉じ括弧の行の最初の文字へ
                    if matches!(obj, TextObj::Block(_)) && inner && s < e && self.buf.pos_of(s).1 == 0 && self.buf.pos_of(e).1 == 0 {
                        if op == Op::Delete {
                            self.apply(op, s, e, true, reg);
                            self.with_motion(|m, c| m.first_non_blank(c));
                            return;
                        }
                        (s, e, true)
                    } else {
                        (s, e, lw)
                    }
                }
                None => return,
            },
            Target::Motion(m) => {
                // cw はカーソルが空白でなければ ce と同じ（Vim の例外）
                let on_word = {
                    let line = self.buf.line(from.line);
                    line.get(from.byte..).and_then(|r| r.graphemes(true).next()).is_some_and(|g| class(g, false) != Class::Space)
                };
                let m = match m {
                    M::WordFwd { big } if op == Op::Change && on_word => M::WordEnd { big },
                    m => m,
                };
                let Some((to, kd)) = self.dest(m, count, true) else { return };
                let to_abs = self.buf.line_start(to.line) + to.byte;
                match kd {
                    Kd::Linewise => {
                        let (s, e) = self.lines_range(from.line.min(to.line), from.line.max(to.line));
                        (s, e, true)
                    }
                    Kd::Inclusive => {
                        let (a, b) = (from_abs.min(to_abs), from_abs.max(to_abs));
                        (a, b + self.grapheme_len_at(b), false)
                    }
                    Kd::Exclusive => {
                        let (a, mut b) = (from_abs.min(to_abs), from_abs.max(to_abs));
                        // dw が次の行へ進んだら、今の行の行末で止める（Vim の例外）
                        if matches!(m, M::WordFwd { .. }) && to.line > from.line {
                            b = self.buf.line_start(from.line) + self.line_len(from.line);
                        }
                        // 終わりが次の行の行頭なら、前の行の行末までにする。始まりが行の最初の空白でない文字以前なら
                        // 行単位にする（Vim の :help exclusive の 2 つの規則）
                        let ((al, ab), (bl, bb)) = (self.buf.pos_of(a), self.buf.pos_of(b));
                        if bb == 0 && bl > al {
                            let line = self.buf.line(al);
                            let first = line.len() - line.trim_start_matches([' ', '\t']).len();
                            if ab <= first {
                                let (s, e) = self.lines_range(al, bl - 1);
                                self.apply(op, s, e, true, reg);
                                return;
                            }
                            b = self.buf.line_start(bl - 1) + self.line_len(bl - 1);
                        }
                        (a, b, false)
                    }
                }
            }
        };
        self.apply(op, s, e, lw, reg);
    }

    /// 範囲 [s, e) にオペレーターを適用する
    fn apply(&mut self, op: Op, s: usize, e: usize, lw: bool, reg: Option<char>) {
        let text = self.buf.slice_string(s, e);
        match op {
            Op::Yank => {
                self.set_register(reg, normalize(&text, lw), lw, true);
                if !lw {
                    self.set_abs(s);
                } else {
                    let (l, _) = self.buf.pos_of(if text.starts_with(['\r', '\n']) { s + 1 } else { s });
                    self.cursor.line = self.cursor.line.min(l.max(self.buf.pos_of(s).0));
                }
            }
            Op::Delete => {
                if s == e {
                    return;
                }
                self.set_register(reg, normalize(&text, lw), lw, false);
                self.hist.begin();
                self.edit(s, e, "");
                self.hist.close();
                if lw {
                    // Neovim の既定（nostartofline）と同じく、列を保つ
                    let mut line = self.buf.pos_of(s).0;
                    if text.starts_with(['\r', '\n']) {
                        line += 1;
                    }
                    let want = self.cursor.want_col;
                    self.cursor.line = line.min(self.buf.line_count().saturating_sub(1));
                    let l = self.buf.line(self.cursor.line);
                    self.cursor.byte = nagi_core::byte_at_col(&l, want, false);
                } else {
                    self.set_abs(s);
                }
            }
            Op::Change => {
                self.set_register(reg, normalize(&text, lw), lw, false);
                self.hist.begin();
                if lw {
                    // 行の中身を消して、最初の行のインデントは残す（autoindent）
                    let (first, last) = self.lines_of(s, e, &text);
                    let indent = leading_ws(&self.buf.line(first)).to_string();
                    let cs = self.buf.line_start(first);
                    let ce = self.buf.line_start(last) + self.line_len(last);
                    self.edit(cs, ce, &indent);
                    self.set_abs(cs + indent.len());
                } else {
                    self.edit(s, e, "");
                    self.set_abs(s);
                }
                // グループは Insert を抜けるまで開いたまま
                self.mode = Mode::Insert;
                self.message.clear();
            }
            Op::Indent | Op::Dedent => {
                let (a, b) = if lw { self.lines_of(s, e, &text) } else { (self.buf.pos_of(s).0, self.buf.pos_of(e.saturating_sub(1).max(s)).0) };
                self.hist.begin();
                for l in a..=b.max(a) {
                    let line = self.buf.line(l);
                    let ls = self.buf.line_start(l);
                    if op == Op::Indent {
                        if !line.is_empty() {
                            self.edit(ls, ls, SHIFT);
                        }
                    } else {
                        let ws = leading_ws(&line);
                        let cut = if ws.starts_with('\t') { 1 } else { ws.len().min(SHIFT.len()) };
                        if cut > 0 {
                            self.edit(ls, ls + cut, "");
                        }
                    }
                }
                self.hist.close();
                // 列は保つ（nostartofline）
                self.cursor.line = a;
            }
        }
    }

    // ---------------------------------------------------------------- レジスタ

    fn set_register(&mut self, reg: Option<char>, text: String, linewise: bool, yank: bool) {
        match reg {
            Some('_') => return,
            Some(c @ 'a'..='z') => {
                self.registers.insert(c, (text.clone(), linewise));
            }
            Some(c @ 'A'..='Z') => {
                let e = self.registers.entry(c.to_ascii_lowercase()).or_default();
                e.0.push_str(&text);
                e.1 |= linewise;
            }
            _ => {}
        }
        if yank {
            self.registers.insert('0', (text.clone(), linewise));
        } else if linewise || text.contains('\n') {
            for i in (1..9).rev() {
                if let Some(v) = self.registers.get(&char::from(b'0' + i)).cloned() {
                    self.registers.insert(char::from(b'0' + i + 1), v);
                }
            }
            self.registers.insert('1', (text.clone(), linewise));
        } else {
            self.registers.insert('-', (text.clone(), linewise));
        }
        // 無名レジスタ。clipboard=unnamedplus なら OS のクリップボードも
        self.registers.insert('"', (text.clone(), linewise));
        if let Some(cb) = &self.clipboard {
            if matches!(reg, None | Some('+') | Some('*')) {
                (cb.set)(&text.replace("\r\n", "\n").replace('\n', "\r\n"));
            }
        }
    }

    fn get_register(&self, reg: Option<char>) -> (String, bool) {
        let from_clip = |t: String| {
            let t = t.replace("\r\n", "\n");
            // 自分が入れた内容なら、行単位かどうかも引き継ぐ
            let lw = match self.registers.get(&'"') {
                Some((u, lw)) if u.replace("\r\n", "\n") == t => *lw,
                _ => t.ends_with('\n'),
            };
            (t, lw)
        };
        match reg {
            Some('+') | Some('*') => self.clipboard.as_ref().and_then(|cb| (cb.get)()).map(from_clip).unwrap_or_default(),
            Some(c) => self.registers.get(&c.to_ascii_lowercase()).cloned().unwrap_or_default(),
            None => match self.clipboard.as_ref().and_then(|cb| (cb.get)()) {
                Some(t) => from_clip(t),
                None => self.registers.get(&'"').cloned().unwrap_or_default(),
            },
        }
    }

    // ---------------------------------------------------------------- 単発の操作

    fn act(&mut self, a: Action, cmd: Cmd, visual: Option<bool>) -> Effect {
        let n = cmd.count.unwrap_or(1);
        match a {
            Action::Esc => {
                if visual.is_some() {
                    self.mode = Mode::Normal;
                }
                self.message.clear();
            }
            Action::Insert(c) => self.insert_cmd(c),
            Action::Short(c) => {
                let (op, t) = match c {
                    'x' => (Op::Delete, Target::Motion(M::Right)),
                    'X' => (Op::Delete, Target::Motion(M::Left)),
                    's' => (Op::Change, Target::Motion(M::Right)),
                    'S' => (Op::Change, Target::Lines),
                    'C' => (Op::Change, Target::Motion(M::LineEnd)),
                    'D' => (Op::Delete, Target::Motion(M::LineEnd)),
                    _ => (Op::Yank, Target::Lines),
                };
                self.operate(op, t, cmd.count, cmd.reg);
            }
            Action::Put { after, indent } => {
                if visual.is_some() {
                    self.visual_put(cmd.reg);
                } else {
                    for _ in 0..n {
                        self.put(after, indent, cmd.reg);
                    }
                }
            }
            Action::Join => {
                let (l, times) = match visual {
                    Some(_) => {
                        let (s, e, _) = self.visual_range();
                        let (a, b) = (self.buf.pos_of(s).0, self.buf.pos_of(e.saturating_sub(1)).0);
                        self.mode = Mode::Normal;
                        (a, (b - a).max(1))
                    }
                    None => (self.cursor.line, n.max(2) - 1),
                };
                self.join(l, times);
            }
            Action::Replace(ch) => self.replace_chars(ch, n, visual),
            Action::ToggleCase => self.toggle_case(n, visual),
            Action::Undo => match self.hist.undo(&mut self.buf) {
                Some(at) => self.set_abs(at),
                None => self.message = "これ以上は元に戻せません".into(),
            },
            Action::Redo => {
                if let Some(at) = self.hist.redo(&mut self.buf) {
                    self.set_abs(at);
                }
            }
            Action::Dot => self.repeat_dot(cmd.count),
            Action::Window(c) => self.window_command(c, cmd.count),
            Action::SetMark(c) => self.set_mark(c),
            Action::Jump { older } => self.jump(older, n),
            Action::AltBuffer => match cmd.count {
                Some(n) => self.goto_buffer(&n.to_string()),
                None => self.goto_buffer("#"),
            },
            Action::Cmdline(':') => {
                self.mode = Mode::Command;
                self.cmdline = Default::default();
                if visual.is_some() {
                    self.cmdline.set("'<,'>");
                }
                self.message.clear();
            }
            Action::Cmdline(c) => self.start_search(c == '/', false, ""),
            Action::SearchWord { forward } => {
                let line = self.buf.line(self.cursor.line);
                if let Some((s, e)) = textops::word_object(&line, self.cursor.byte.min(line.len()), false, false) {
                    let word = &line[s..e];
                    if class(word, false) == Class::Word {
                        let pat = format!(r"\b{}\b", regex_escape(word));
                        self.search.last = Some((pat, true, forward));
                        if let Some(at) = self.search_next(false, n) {
                            self.push_jump();
                            self.set_abs(at);
                        }
                    }
                }
            }
            Action::Visual { line } => match visual {
                Some(cur) if cur == line => self.mode = Mode::Normal,
                Some(_) => self.mode = Mode::Visual { line },
                None => {
                    self.anchor = self.abs();
                    self.mode = Mode::Visual { line };
                }
            },
            Action::Scroll { half, down } => {
                let amount = if half { (self.rows / 2).max(1) } else { self.rows.saturating_sub(2).max(1) } * n;
                let d = if down { amount as isize } else { -(amount as isize) };
                self.with_motion(|m, c| m.vertical(c, d));
                let max_top = self.buf.line_count().saturating_sub(1) as isize;
                self.top = (self.top as isize + d).clamp(0, max_top) as usize;
            }
            Action::LastChange => self.jump_last_change(),
        }
        Effect::None
    }

    fn insert_cmd(&mut self, c: char) {
        let mut cur = self.cursor;
        match c {
            'a' => {
                let line = self.buf.line(cur.line);
                if let Some(g) = line[cur.byte.min(line.len())..].graphemes(true).next() {
                    cur.byte += g.len();
                }
            }
            'I' => self.motion().first_non_blank(&mut cur),
            'A' => cur.byte = self.line_len(cur.line),
            'o' | 'O' => {
                // 新しい行のインデントは今の行に合わせる（autoindent。Neovim の既定と同じ）
                let indent = leading_ws(&self.buf.line(cur.line)).to_string();
                let nl = self.buf.newline();
                self.enter_insert();
                if c == 'o' {
                    let at = self.buf.line_start(cur.line) + self.line_len(cur.line);
                    self.edit(at, at, &format!("{nl}{indent}"));
                    self.set_abs(at + nl.len() + indent.len());
                } else {
                    let at = self.buf.line_start(cur.line);
                    self.edit(at, at, &format!("{indent}{nl}"));
                    self.set_abs(at + indent.len());
                }
                return;
            }
            _ => {}
        }
        self.cursor = cur;
        self.enter_insert();
    }

    /// p / P。`indent` は ]p（インデントを現在行に合わせる）
    fn put(&mut self, after: bool, indent: bool, reg: Option<char>) {
        let (text, linewise) = self.get_register(reg);
        if text.is_empty() {
            return;
        }
        let nl = self.buf.newline();
        self.hist.begin();
        if linewise {
            let mut text = if text.ends_with('\n') { text } else { text + "\n" };
            if indent {
                text = reindent(&text, leading_ws(&self.buf.line(self.cursor.line)));
            }
            let text = text.replace('\n', nl);
            let line = if after { self.cursor.line + 1 } else { self.cursor.line };
            let at = if line >= self.buf.line_count() {
                // 最終行の後ろ: 改行が無ければ先に足す
                let end = self.buf.len_bytes();
                if !self.buf.slice_string(end.saturating_sub(1), end).ends_with('\n') {
                    self.edit(end, end, nl);
                }
                self.buf.len_bytes()
            } else {
                self.buf.line_start(line)
            };
            self.edit(at, at, &text);
            self.set_abs(at);
            self.with_motion(|m, c| m.first_non_blank(c));
        } else {
            let text = text.replace('\n', nl);
            let mut at = self.abs();
            if after && !self.buf.line(self.cursor.line).is_empty() {
                at += self.grapheme_len_at(at);
            }
            self.edit(at, at, &text);
            self.set_abs(at + text.len());
            self.with_motion(|m, c| m.left(c, 1));
        }
        self.hist.close();
    }

    /// Visual で p: 選択範囲をレジスタの内容で置き換える
    fn visual_put(&mut self, reg: Option<char>) {
        let (text, linewise) = self.get_register(reg);
        let (s, e, lw) = self.visual_range();
        self.mode = Mode::Normal;
        let nl = self.buf.newline();
        let mut t = text.replace('\n', nl);
        if lw && !linewise {
            t.push_str(nl);
        } else if !lw && linewise {
            t = format!("{nl}{t}");
        }
        self.hist.begin();
        self.edit(s, e, &t);
        self.hist.close();
        self.set_abs(s);
    }

    fn join(&mut self, l: usize, times: usize) {
        self.hist.begin();
        let mut pos = None;
        for _ in 0..times {
            if l + 1 >= self.buf.line_count() {
                break;
            }
            let cur = self.buf.line(l);
            let next = self.buf.line(l + 1);
            let start = self.buf.line_start(l) + cur.len();
            let end = self.buf.line_start(l + 1) + leading_ws(&next).len();
            let rest = next.trim_start_matches([' ', '\t']);
            let sep = if cur.ends_with([' ', '\t']) || rest.is_empty() || rest.starts_with(')') || cur.is_empty() { "" } else { " " };
            let cur_trimmed = cur.trim_end_matches([' ', '\t']).len();
            let start2 = if sep.is_empty() { start } else { self.buf.line_start(l) + cur_trimmed };
            self.edit(start2, end, sep);
            pos = Some(start2);
        }
        self.hist.close();
        if let Some(p) = pos {
            self.set_abs(p);
        }
    }

    fn replace_chars(&mut self, ch: char, n: usize, visual: Option<bool>) {
        let (s, e) = match visual {
            Some(_) => {
                let (s, e, _) = self.visual_range();
                self.mode = Mode::Normal;
                (s, e)
            }
            None => {
                let line = self.buf.line(self.cursor.line);
                let gs: Vec<&str> = line[self.cursor.byte.min(line.len())..].graphemes(true).take(n).collect();
                if gs.len() < n {
                    return;
                }
                let s = self.abs();
                (s, s + gs.iter().map(|g| g.len()).sum::<usize>())
            }
        };
        let old = self.buf.slice_string(s, e);
        let new: String = if ch == '\n' {
            self.buf.newline().to_string()
        } else {
            old.graphemes(true).map(|g| if g == "\n" || g == "\r\n" { g.to_string() } else { ch.to_string() }).collect()
        };
        self.hist.begin();
        self.edit(s, e, &new);
        self.hist.close();
        if ch == '\n' {
            self.set_abs(s + new.len());
        } else if visual.is_some() {
            self.set_abs(s);
        } else {
            self.set_abs(s + new.len() - ch.len_utf8());
        }
    }

    fn toggle_case(&mut self, n: usize, visual: Option<bool>) {
        let (s, e) = match visual {
            Some(_) => {
                let (s, e, _) = self.visual_range();
                self.mode = Mode::Normal;
                (s, e)
            }
            None => {
                let line = self.buf.line(self.cursor.line);
                let len: usize = line[self.cursor.byte.min(line.len())..].graphemes(true).take(n).map(str::len).sum();
                let s = self.abs();
                (s, s + len)
            }
        };
        let old = self.buf.slice_string(s, e);
        let new: String = old
            .chars()
            .flat_map(|c| -> Vec<char> { if c.is_lowercase() { c.to_uppercase().collect() } else { c.to_lowercase().collect() } })
            .collect();
        if new != old {
            self.hist.begin();
            self.edit(s, e, &new);
            self.hist.close();
        }
        if visual.is_some() {
            self.set_abs(s);
        } else {
            self.set_abs(s + new.len());
        }
    }

    fn repeat_dot(&mut self, count: Option<usize>) {
        if self.dot.is_empty() {
            return;
        }
        let mut keys = self.dot.clone();
        if let Some(n) = count {
            // 記録の先頭の回数を、新しい回数で置き換える
            let skip = keys.iter().take_while(|k| matches!(k, Key::Char('0'..='9'))).count();
            keys.drain(..skip);
            let digits: Vec<Key> = n.to_string().chars().map(Key::Char).collect();
            keys.splice(0..0, digits);
        }
        self.replaying = true;
        for k in keys {
            self.key(k);
        }
        self.replaying = false;
    }
}

/// 行単位のテキストは改行で終わる形にそろえる（CRLF は LF にする）
fn normalize(text: &str, linewise: bool) -> String {
    let t = text.replace("\r\n", "\n");
    if linewise {
        // 最終行の削除で前の行の改行から始まる場合は、その改行を後ろへ回す
        let t = match t.strip_prefix('\n') {
            Some(rest) if !rest.ends_with('\n') => format!("{rest}\n"),
            _ => t,
        };
        if t.ends_with('\n') { t } else { t + "\n" }
    } else {
        t
    }
}

/// ]p: 行単位のテキストのインデントを、先頭行が `indent` になるようにずらす
fn reindent(text: &str, indent: &str) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let base = lines.first().map(|l| leading_ws(l).to_string()).unwrap_or_default();
    lines
        .iter()
        .map(|l| {
            if l.trim().is_empty() {
                l.to_string()
            } else if let Some(rest) = l.strip_prefix(base.as_str()) {
                format!("{indent}{rest}")
            } else {
                format!("{indent}{}", l.trim_start_matches([' ', '\t']))
            }
        })
        .collect()
}

fn regex_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}
