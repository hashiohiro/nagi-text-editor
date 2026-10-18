//! マクロ（`q{reg}` で記録、`@{reg}` で再生）。
//!
//! 記録するのはマッピングを展開した後のキーで、再生ではそのままエディタに流す（マッピングは通さない）。
//! レジスタには `parse_keys` で読み戻せる表記の文字列で入れるので、`"ap` で貼り出して直し、`"ayy` で戻せる（Vim と同じ使い方）。

use nagi_vim::Key;

use crate::editor::{Editor, Effect};

/// 再生の入れ子の上限（自分を呼ぶマクロで止まらなくならないように）
const MAX_DEPTH: usize = 100;

impl Editor {
    /// 記録を止めてレジスタに入れる（大文字のレジスタなら追記）
    pub(crate) fn stop_recording(&mut self) {
        let Some((reg, keys)) = self.recording.take() else { return };
        let text: String = keys.iter().map(Key::notation).collect();
        let lower = reg.to_ascii_lowercase();
        let text = if reg.is_ascii_uppercase() {
            format!("{}{text}", self.registers.get(&lower).map_or("", |(t, _)| t.as_str()))
        } else {
            text
        };
        self.registers.insert(lower, (text, false));
    }

    /// `@{reg}` を `count` 回
    pub(crate) fn play_macro(&mut self, reg: char, count: usize) -> Effect {
        let reg = if reg == '@' {
            match self.last_macro {
                Some(r) => r,
                None => {
                    self.notify.error("E748: 直前に再生したマクロがありません");
                    return Effect::None;
                }
            }
        } else {
            reg
        };
        if reg == ':' {
            self.notify.info("未実装: @:（直前の Ex コマンドの繰り返し）");
            return Effect::None;
        }
        if self.macro_depth >= MAX_DEPTH {
            self.notify.error("E169: マクロの入れ子が深すぎます");
            return Effect::None;
        }
        self.last_macro = Some(reg);
        let (text, _) = self.get_register(Some(reg));
        let keys = nagi_vim::parse_keys(&text, "\\");
        self.macro_depth += 1;
        let mut effect = Effect::None;
        'outer: for _ in 0..count.max(1) {
            for &k in &keys {
                match self.key(k) {
                    Effect::None => {}
                    e => {
                        effect = e;
                        if matches!(effect, Effect::Quit) {
                            break 'outer;
                        }
                    }
                }
            }
        }
        self.macro_depth -= 1;
        effect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    fn keys(e: &mut Editor, k: &str) {
        for key in nagi_vim::parse_keys(k, " ") {
            e.key(key);
        }
    }

    #[test]
    fn record_and_play() {
        let mut e = Editor::new(Buffer::from_str("a1\na2\na3\na4\na5\n"), None);
        // 行頭に # を足して次の行へ、を記録し、3 回再生する
        keys(&mut e, "qaI# <Esc>jq");
        assert!(e.recording.is_none());
        assert_eq!(e.registers.get(&'a').map(|r| r.0.as_str()), Some("I# <Esc>j"));
        keys(&mut e, "2@a@@");
        let lines: Vec<String> = (0..5).map(|i| e.buf.line(i)).collect();
        assert_eq!(lines, vec!["# a1", "# a2", "# a3", "# a4", "a5"]);
        // 大文字は追記。<lt> と Ctrl も読み戻せる
        keys(&mut e, "qAx<C-a>q");
        assert_eq!(e.registers.get(&'a').map(|r| r.0.as_str()), Some("I# <Esc>jx<C-a>"));
        keys(&mut e, "qbi<lt><Esc>q");
        assert_eq!(e.registers.get(&'b').map(|r| r.0.as_str()), Some("i<lt><Esc>"));
        // 自分を呼ぶマクロも、深さの上限で止まる（行末に達して j が止まっても続けて呼ぶ）
        keys(&mut e, "qcx@cq");
        keys(&mut e, "gg@c");
        assert!(e.macro_depth == 0);
    }
}
