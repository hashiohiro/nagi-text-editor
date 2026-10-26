//! undo / redo（architecture.md §5.2）。Insert に入ってから出るまでのような一連の変更を 1 グループにまとめる。
//!
//! 履歴は Vim と同じく木で持つ。グループ 1 つが節 1 つで、作った順の番号（seq）を持つ。
//! - `u` は親へ、`Ctrl-r` は最後に作った（または最後に通った）子へ移る
//! - `g-` / `g+` は時間の順で 1 つ前・後の状態へ移る（別の枝へも、共通の祖先まで戻ってから下る）

use crate::Buffer;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// 変更位置（絶対 byte）
    pub at: usize,
    pub removed: String,
    pub inserted: String,
}

struct Node {
    parent: usize,
    changes: Vec<Change>,
    seq: usize,
    /// redo で進む子
    next: Option<usize>,
    /// まとめて畳んだ節（どこからも辿らない）
    dead: bool,
    /// いくつものファイルを一度に変えた操作（名前の変更）の札。同じ札の節は、どれかを戻せばほかも戻す
    tag: Option<u64>,
}

pub struct History {
    nodes: Vec<Node>,
    /// 今の状態の節（0 は何も変えていない状態）
    cur: usize,
    open: Option<Vec<Change>>,
}

impl Default for History {
    fn default() -> Self {
        Self { nodes: vec![Node { parent: 0, changes: Vec::new(), seq: 0, next: None, dead: false, tag: None }], cur: 0, open: None }
    }
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
                self.push(g);
            }
        }
    }

    fn push(&mut self, changes: Vec<Change>) {
        let id = self.nodes.len();
        let seq = self.nodes.iter().map(|n| n.seq).max().unwrap_or(0) + 1;
        self.nodes.push(Node { parent: self.cur, changes, seq, next: None, dead: false, tag: None });
        self.nodes[self.cur].next = Some(id);
        self.cur = id;
    }

    pub fn record(&mut self, c: Change) {
        match &mut self.open {
            Some(g) => g.push(c),
            None => self.push(vec![c]),
        }
    }

    /// 今の状態の目印（`squash_from` に渡す）
    pub fn len(&mut self) -> usize {
        self.close();
        self.cur
    }

    /// 目印 `from` から今の状態までのグループを 1 つにまとめる（`:g` のように、いくつもの変更を 1 回の u で戻すため）
    pub fn squash_from(&mut self, from: usize) {
        self.close();
        let mut path = Vec::new();
        let mut n = self.cur;
        while n != from {
            if n == 0 {
                return; // 目印が今の状態の祖先でない（途中で undo した）ときはまとめない
            }
            path.push(n);
            n = self.nodes[n].parent;
        }
        if path.len() < 2 {
            return;
        }
        path.reverse();
        let first = path[0];
        let rest: Vec<Change> = path[1..].iter().flat_map(|&id| std::mem::take(&mut self.nodes[id].changes)).collect();
        for &id in &path[1..] {
            self.nodes[id].dead = true;
        }
        self.nodes[first].changes.extend(rest);
        self.nodes[first].next = None;
        self.cur = first;
    }

    fn undo_node(&self, id: usize, buf: &mut Buffer) -> usize {
        let mut pos = 0;
        for c in self.nodes[id].changes.iter().rev() {
            buf.edit(c.at, c.at + c.inserted.len(), &c.removed);
            pos = c.at;
        }
        pos
    }

    fn redo_node(&self, id: usize, buf: &mut Buffer) -> usize {
        let mut pos = 0;
        for c in &self.nodes[id].changes {
            buf.edit(c.at, c.at + c.removed.len(), &c.inserted);
            pos = c.at;
        }
        pos
    }

    /// 1 グループ戻す。戻した後にカーソルを置く絶対 byte 位置を返す
    pub fn undo(&mut self, buf: &mut Buffer) -> Option<usize> {
        self.close();
        if self.cur == 0 {
            return None;
        }
        let id = self.cur;
        let pos = self.undo_node(id, buf);
        self.cur = self.nodes[id].parent;
        self.nodes[self.cur].next = Some(id);
        Some(pos)
    }

    pub fn redo(&mut self, buf: &mut Buffer) -> Option<usize> {
        self.close();
        let id = self.nodes[self.cur].next?;
        let pos = self.redo_node(id, buf);
        self.cur = id;
        Some(pos)
    }

    /// 今の状態の節（直前のグループ）に札を付ける（グループを閉じてから）
    pub fn tag_current(&mut self, tag: u64) {
        self.close();
        if self.cur != 0 {
            self.nodes[self.cur].tag = Some(tag);
        }
    }

    /// u で戻す節の札
    pub fn undo_tag(&self) -> Option<u64> {
        if self.open.as_ref().is_some_and(|g| !g.is_empty()) {
            return None;
        }
        self.nodes[self.cur].tag
    }

    /// Ctrl-r で進む節の札
    pub fn redo_tag(&self) -> Option<u64> {
        if self.open.as_ref().is_some_and(|g| !g.is_empty()) {
            return None;
        }
        self.nodes[self.nodes[self.cur].next?].tag
    }

    /// 今の状態の番号（`g-` `g+` の時間の順）
    pub fn seq(&self) -> usize {
        self.nodes[self.cur].seq
    }

    /// `g-`（`later` = false）/ `g+`: 時間の順で `count` 個前・後の状態へ移る
    pub fn travel(&mut self, buf: &mut Buffer, later: bool, count: usize) -> Option<usize> {
        self.close();
        let mut seqs: Vec<(usize, usize)> = self.nodes.iter().enumerate().filter(|(_, n)| !n.dead).map(|(i, n)| (n.seq, i)).collect();
        seqs.sort_unstable();
        let at = seqs.iter().position(|&(_, i)| i == self.cur)?;
        let to = if later { (at + count).min(seqs.len() - 1) } else { at.saturating_sub(count) };
        if to == at {
            return None;
        }
        Some(self.goto(buf, seqs[to].1))
    }

    fn ancestors(&self, mut n: usize) -> Vec<usize> {
        let mut v = vec![n];
        while n != 0 {
            n = self.nodes[n].parent;
            v.push(n);
        }
        v
    }

    /// 節 `target` の状態へ移る（共通の祖先まで戻ってから下る）
    fn goto(&mut self, buf: &mut Buffer, target: usize) -> usize {
        let up = self.ancestors(self.cur);
        let down = self.ancestors(target);
        let common = *up.iter().find(|n| down.contains(n)).unwrap_or(&0);
        let mut pos = 0;
        for &n in up.iter().take_while(|&&n| n != common) {
            pos = self.undo_node(n, buf);
        }
        let path: Vec<usize> = down.iter().take_while(|&&n| n != common).copied().collect();
        for &n in path.iter().rev() {
            pos = self.redo_node(n, buf);
            let p = self.nodes[n].parent;
            self.nodes[p].next = Some(n);
        }
        self.cur = target;
        pos
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

    /// 戻してから別の変更をしても、g- / g+ で前の枝に行ける
    #[test]
    fn tree_and_time_travel() {
        let mut b = Buffer::from_str("0\n");
        let mut h = History::default();
        let set = |b: &mut Buffer, h: &mut History, s: &str| {
            let n = b.line(0).len();
            let c = b.edit(0, n, s);
            h.record(c);
        };
        set(&mut b, &mut h, "1");
        set(&mut b, &mut h, "2");
        h.undo(&mut b);
        assert_eq!(b.line(0), "1");
        set(&mut b, &mut h, "3"); // 1 から枝分かれ
        // 時間の順: 0 → 1 → 2 → 3
        h.travel(&mut b, false, 1);
        assert_eq!(b.line(0), "2", "g- で前の枝の 2 へ");
        h.travel(&mut b, false, 1);
        assert_eq!(b.line(0), "1");
        h.travel(&mut b, true, 2);
        assert_eq!(b.line(0), "3");
        h.travel(&mut b, false, 10);
        assert_eq!(b.line(0), "0");
        // u / Ctrl-r は通った枝をたどる
        h.travel(&mut b, true, 2);
        assert_eq!(b.line(0), "2");
        h.undo(&mut b);
        h.redo(&mut b);
        assert_eq!(b.line(0), "2");
    }

    #[test]
    fn squash() {
        let mut b = Buffer::from_str("abc\n");
        let mut h = History::default();
        let c = b.edit(0, 1, "A");
        h.record(c);
        let mark = h.len();
        for i in 1..3 {
            let c = b.edit(i, i + 1, "Z");
            h.record(c);
        }
        h.squash_from(mark);
        assert_eq!(b.line(0), "AZZ");
        h.undo(&mut b);
        assert_eq!(b.line(0), "Abc", "まとめた分は 1 回で戻る");
        h.travel(&mut b, true, 1);
        assert_eq!(b.line(0), "AZZ");
    }

    #[test]
    fn positions_and_crlf() {
        let b = Buffer::from_str("ab\r\ncd\r\n");
        assert_eq!(b.line_start(1), 4);
        assert_eq!(b.pos_of(5), (1, 1));
        assert_eq!(b.newline(), "\r\n");
    }
}
