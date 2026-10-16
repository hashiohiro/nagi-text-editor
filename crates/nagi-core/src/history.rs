//! undo / redo。Insert に入ってから出るまでのような一連の変更を 1 グループにまとめる（architecture.md §5.2）。
//! 当面は線形の履歴だけを公開する（木構造は M3）。

use crate::Buffer;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// 変更位置（絶対 byte）
    pub at: usize,
    pub removed: String,
    pub inserted: String,
}

#[derive(Default)]
pub struct History {
    undo: Vec<Vec<Change>>,
    redo: Vec<Vec<Change>>,
    open: Option<Vec<Change>>,
}

impl History {
    /// グループを開く（Insert に入るとき）。開いている間の変更は 1 回の u で戻る
    pub fn begin(&mut self) {
        self.close();
        self.open = Some(Vec::new());
    }

    /// グループを閉じる（Insert を出るとき）
    pub fn close(&mut self) {
        if let Some(g) = self.open.take() {
            if !g.is_empty() {
                self.undo.push(g);
                self.redo.clear();
            }
        }
    }

    pub fn record(&mut self, c: Change) {
        match &mut self.open {
            Some(g) => g.push(c),
            None => {
                self.undo.push(vec![c]);
                self.redo.clear();
            }
        }
    }

    /// 1 グループ戻す。戻した後にカーソルを置く絶対 byte 位置を返す
    pub fn undo(&mut self, buf: &mut Buffer) -> Option<usize> {
        self.close();
        let g = self.undo.pop()?;
        let mut pos = 0;
        for c in g.iter().rev() {
            buf.edit(c.at, c.at + c.inserted.len(), &c.removed);
            pos = c.at;
        }
        self.redo.push(g);
        Some(pos)
    }

    pub fn redo(&mut self, buf: &mut Buffer) -> Option<usize> {
        self.close();
        let g = self.redo.pop()?;
        let mut pos = 0;
        for c in &g {
            buf.edit(c.at, c.at + c.removed.len(), &c.inserted);
            pos = c.at;
        }
        self.undo.push(g);
        Some(pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_undo_redo() {
        let mut b = Buffer::from_str("abc\n");
        let mut h = History::default();
        h.begin();
        let c = b.edit(1, 1, "X");
        h.record(c);
        let c = b.edit(2, 2, "Y");
        h.record(c);
        h.close();
        assert_eq!(b.line(0), "aXYbc");
        assert_eq!(h.undo(&mut b), Some(1));
        assert_eq!(b.line(0), "abc");
        h.redo(&mut b);
        assert_eq!(b.line(0), "aXYbc");
    }

    #[test]
    fn positions_and_crlf() {
        let b = Buffer::from_str("ab\r\ncd\r\n");
        assert_eq!(b.line_start(1), 4);
        assert_eq!(b.pos_of(5), (1, 1));
        assert_eq!(b.newline(), "\r\n");
    }
}
