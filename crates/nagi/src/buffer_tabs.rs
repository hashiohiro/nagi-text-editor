//! 本文の上のタブ（開いているバッファ）。Vim の有無を問わず出す（設定の「エディタ → タブ」で隠せる）。
//!
//! 押すとそのバッファへ、× で閉じる（保存していなければ、保存して閉じるか・保存せずに閉じるかを窓で確かめる）。
//! 変更のあるタブは × の代わりに ●（マウスを載せると ×）。右クリックで閉じる・ほかを閉じる・すべて閉じる・一覧。
//! ホイールで隣のバッファへ。入りきらなければ、作業中のタブが見えるように左を詰める。
//! 窓を分けている間は、窓ごとにその窓で開いたファイルのタブの列を窓の上に出す（IntelliJ の分割と同じ。新しい窓は
//! 今のファイルだけから始まる）。窓のタブを閉じるとその窓から外し、ほかの窓に無ければファイルも閉じる。最後のタブで窓を閉じる。

use std::path::PathBuf;

use crate::buffers::Slot;
use crate::editor::Editor;
use crate::panes::Rect;

/// 画面に出すタブ 1 つ（列は窓の領域の左端から）
pub(crate) struct BufTab {
    pub doc: usize,
    pub label: String,
    pub x: usize,
    pub w: usize,
    pub active: bool,
    pub dirty: bool,
    /// ファイル（種類のアイコンを出す）
    pub path: Option<std::path::PathBuf>,
}

impl BufTab {
    /// × の列（` 名前 × ` の終わりから 2 つ目）
    pub fn close_col(&self) -> usize {
        self.x + self.w - 2
    }
}

fn file_name(p: &std::path::Path) -> String {
    p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
}

impl Editor {
    /// 名前の無い空のバッファ（開いた直後・最後のタブを閉じた後）はタブにしない
    fn tab_worthy(&self, id: usize, path: &Option<PathBuf>, dirty: bool) -> bool {
        if path.is_some() || dirty {
            return true;
        }
        let len = match &self.core.docs[id] {
            Slot::Active => self.core.buf.len_bytes(),
            Slot::Parked(d) => d.buf.len_bytes(),
            Slot::Deleted => 0,
        };
        len > 0
    }

    /// 幅 `w` に入るタブ（同じ名前のファイルは親のフォルダを添える）
    pub(crate) fn buffer_tabs(&self, w: usize) -> Vec<BufTab> {
        self.buffer_tabs_in(None, w)
    }

    /// 窓を分けている間の、窓 `win` のバッファ（その窓で開いた順。今出しているものは必ず）
    pub(crate) fn win_docs(&self, win: usize) -> Vec<usize> {
        let shown = self.win_state(win).map(|s| s.doc);
        let mut v: Vec<usize> = self.win_tabs.get(&win).cloned().unwrap_or_default().into_iter().filter(|&d| matches!(self.core.docs.get(d), Some(Slot::Active | Slot::Parked(_)))).collect();
        if let Some(d) = shown.filter(|d| !v.contains(d)) {
            v.push(d);
        }
        v
    }

    /// 窓に出したバッファを、その窓のタブに覚える（窓を分けている間。消したバッファは外す）
    pub(crate) fn sync_win_tabs(&mut self) {
        if !self.is_split() {
            return;
        }
        let wins: Vec<usize> = self.window_rects().into_iter().map(|(w, _)| w).collect();
        for w in wins {
            let docs = self.win_docs(w);
            self.win_tabs.insert(w, docs);
        }
    }

    /// 窓ごとのタブの列（窓, 列の矩形）
    pub(crate) fn tab_strips(&self) -> Vec<(usize, Rect)> {
        if !self.win_tabs_on() {
            return Vec::new();
        }
        self.window_rects().into_iter().map(|(w, r)| (w, Rect { x: r.x, y: r.y.saturating_sub(1), w: r.w, h: 1 })).collect()
    }

    /// 幅 `w` に入るタブ（`win` が Some なら、窓を分けている間のその窓のタブ）
    pub(crate) fn buffer_tabs_in(&self, win: Option<usize>, w: usize) -> Vec<BufTab> {
        let mine = win.filter(|_| self.win_tabs_on()).map(|x| (self.win_docs(x), self.win_state(x).map(|s| s.doc)));
        let list: Vec<_> = self
            .buffer_list()
            .into_iter()
            .filter(|(i, p, d, _)| self.tab_worthy(*i, p, *d) || mine.as_ref().is_some_and(|m| m.1 == Some(*i)))
            .filter(|(i, ..)| mine.as_ref().is_none_or(|m| m.0.contains(i)))
            .map(|(i, p, d, a)| (i, p, d, mine.as_ref().map_or(a, |m| m.1 == Some(i))))
            .collect();
        let names: Vec<String> = list.iter().map(|(_, p, ..)| p.as_deref().map_or("無題".into(), file_name)).collect();
        let width = crate::chrome::ui_width;
        let mut tabs: Vec<BufTab> = list
            .iter()
            .zip(&names)
            .map(|((doc, path, dirty, active), name)| {
                let twice = names.iter().filter(|n| *n == name).count() > 1;
                let parent = path.as_deref().and_then(|p| p.parent()).filter(|_| twice).map(file_name);
                let label = match parent {
                    Some(d) => format!("{name} ‹{d}›"),
                    None => name.clone(),
                };
                // ` アイコン 名前 × ` の幅（アイコンは 2 列）
                BufTab { doc: *doc, w: width(&label) + 4 + crate::metrics::icon_cols(crate::metrics::Icon::Small), label, x: 0, active: *active, dirty: *dirty, path: path.clone() }
            })
            .collect();
        // 差分のタブ（diff_view.rs）。出している間は、それが作業中のタブ（窓ごとの列には出さない）
        if let Some(label) = self.diff_tab_label().filter(|_| mine.is_none()) {
            let shown = self.diff_shown();
            if shown {
                tabs.iter_mut().for_each(|t| t.active = false);
            }
            tabs.push(BufTab { doc: crate::diff_view::DIFF_TAB, w: width(&label) + 4 + crate::metrics::icon_cols(crate::metrics::Icon::Small), label, x: 0, active: shown, dirty: false, path: None });
        }
        // 作業中のタブが入るまで左を詰める
        let fits = |t: &[BufTab]| t.iter().map(|t| t.w).sum::<usize>() <= w;
        if let Some(a) = tabs.iter().position(|t| t.active) {
            let mut start = 0;
            while start < a && !fits(&tabs[start..=a]) {
                start += 1;
            }
            // ホイールで送った分（少なくとも 1 つは残す）
            let start = (start as isize + self.tab_shift).clamp(0, tabs.len() as isize - 1) as usize;
            tabs.drain(..start);
        }
        let mut x = 0;
        tabs.retain_mut(|t| {
            t.x = x;
            x += t.w;
            x <= w
        });
        tabs
    }

    /// 画面の (行, 列) にあるタブ（バッファの番号, × の上か）
    pub fn buftab_at(&self, row: usize, col: usize) -> Option<(usize, bool)> {
        self.buftab_win_at(row, col).map(|t| (t.1, t.2))
    }

    /// 画面の (行, 列) にあるタブ（窓ごとの列なら窓, バッファの番号, × の上か）
    pub fn buftab_win_at(&self, row: usize, col: usize) -> Option<(Option<usize>, usize, bool)> {
        if let Some((win, r)) = self.tab_strips().into_iter().find(|(_, r)| r.contains(row, col)) {
            let c = col - r.x;
            let t = self.buffer_tabs_in(Some(win), r.w).into_iter().find(|t| c >= t.x && c < t.x + t.w)?;
            return Some((Some(win), t.doc, c == t.close_col()));
        }
        if self.tabs_row() == 0 || row != self.area().y {
            return None;
        }
        let (x, w) = self.tabs_span();
        let c = col.checked_sub(x)?;
        let t = self.buffer_tabs(w).into_iter().find(|t| c >= t.x && c < t.x + t.w)?;
        Some((None, t.doc, c == t.close_col()))
    }

    /// 窓 `win` のタブを閉じる（窓を分けている間）。ほかの窓にもあれば、この窓から外すだけ。
    /// この窓に出していれば、窓の別のタブへ移す。窓のタブが無くなれば窓を閉じる。ほかの窓に無ければファイルも閉じる
    pub(crate) fn tab_close_in(&mut self, win: usize, doc: usize) {
        if !self.win_tabs_on() {
            self.tab_close(doc);
            return;
        }
        let elsewhere = self.win_tabs.iter().any(|(w, v)| *w != win && v.contains(&doc)) || self.window_rects().iter().any(|(w, _)| *w != win && self.win_state(*w).is_some_and(|s| s.doc == doc));
        let docs = self.win_docs(win);
        let rest: Vec<usize> = docs.iter().copied().filter(|&d| d != doc).collect();
        if self.win_state(win).is_some_and(|s| s.doc == doc) {
            match rest.last() {
                Some(&next) => {
                    self.focus_window(win);
                    self.switch_doc(next);
                }
                None => {
                    // 最後のタブ: 窓を閉じる
                    self.close_window(win);
                    if !elsewhere {
                        self.tab_close(doc);
                    }
                    return;
                }
            }
        }
        self.win_tabs.insert(win, rest);
        if !elsewhere {
            self.tab_close(doc);
        }
    }

    /// タブの行の (左端, 幅)。左上・右上の隅の出すボタン（» «）の所は空ける
    pub(crate) fn tabs_span(&self) -> (usize, usize) {
        let (mut x, mut w) = self.windows_x();
        for (_, r, c) in self.toggle_buttons() {
            if r != self.area().y || w == 0 {
                continue;
            }
            if c == x {
                x += 1;
                w -= 1;
            } else if c + 1 == x + w {
                w -= 1;
            }
        }
        (x, w)
    }

    /// タブを押した: 本文へ向き直ってそのバッファへ
    pub(crate) fn tab_select(&mut self, doc: usize) {
        if doc == crate::diff_view::DIFF_TAB {
            self.diff_tab_select();
            return;
        }
        // 差分を出していれば、本文へ向くときにタブに隠れる（Editor::set_state）
        self.leave_panel();
        self.switch_doc(doc);
    }

    /// タブを閉じる。保存していなければ、自動で保存する設定なら保存して閉じ（IntelliJ と同じく確かめない）、
    /// そうでなければ（名前の無いバッファも）、保存して閉じるか・保存せずに閉じるかを窓で確かめる
    pub(crate) fn tab_close(&mut self, doc: usize) {
        if doc == crate::diff_view::DIFF_TAB {
            self.close_diff();
            return;
        }
        let Some((_, path, dirty, _)) = self.buffer_list().into_iter().find(|t| t.0 == doc) else { return };
        let n = (doc + 1).to_string();
        if !dirty {
            self.delete_buffer(&n, false);
            return;
        }
        if path.is_some() && self.settings.get("editor.autosave") != "off" {
            self.tab_select(doc);
            self.shortcut("w");
            // 保存できなかった（書き込めない）ら残す
            if self.core.cur_doc == doc && !self.core.buf.dirty {
                self.delete_buffer("", false);
            }
            return;
        }
        let name = path.as_deref().map_or("無題".into(), file_name);
        let m = crate::menu::Menu::new(0, 0)
            .item_key("保存して閉じる（S）", 's', move |e| {
                e.tab_select(doc);
                e.shortcut("w");
                // 保存できなかった（名前が無い・書き込めない）ら残す
                if e.core.cur_doc == doc && !e.core.buf.dirty {
                    e.delete_buffer("", false);
                }
            })
            .item_key("保存せずに閉じる（D）", 'd', move |e| e.delete_buffer(&n, true))
            .item_key("やめる（Esc）", 'c', |_| {});
        self.ask_menu(format!("{name} は保存していません"), m, 2);
    }

    /// `keep` 以外のタブ（None ならすべて）を閉じる。保存していないものは残して知らせる
    /// 左側（`left`）・右側のタブを閉じる（保存していないものは残す）
    pub(crate) fn tab_close_side(&mut self, doc: usize, left: bool) {
        let order: Vec<usize> = self.buffer_list().into_iter().filter(|(i, p, d, _)| self.tab_worthy(*i, p, *d)).map(|t| t.0).collect();
        let Some(k) = order.iter().position(|&d| d == doc) else { return };
        let side: Vec<usize> = if left { order[..k].to_vec() } else { order[k + 1..].to_vec() };
        self.tab_close_set(doc, &side);
    }

    /// 未変更のタブを閉じる（今のタブは残す）
    pub(crate) fn tab_close_unmodified(&mut self) {
        let cur = self.core.cur_doc;
        let all: Vec<usize> = self.buffer_list().into_iter().filter(|t| !t.2).map(|t| t.0).collect();
        self.tab_close_set(cur, &all);
    }

    fn tab_close_set(&mut self, keep: usize, docs: &[usize]) {
        self.tab_select(keep);
        let dirty: Vec<usize> = self.buffer_list().into_iter().filter(|t| t.2).map(|t| t.0).collect();
        let mut kept = 0;
        for &d in docs.iter().filter(|&&d| d != keep) {
            if dirty.contains(&d) {
                kept += 1;
            } else {
                self.delete_buffer(&(d + 1).to_string(), false);
            }
        }
        if kept > 0 {
            self.svc.notify.info(format!("保存していない {kept} 個のタブは残しました（× で 1 つずつ閉じられます）"));
        }
    }

    pub(crate) fn tab_close_others(&mut self, keep: Option<usize>) {
        if let Some(k) = keep {
            self.tab_select(k);
        }
        let mut kept = 0;
        for (doc, _, dirty, _) in self.buffer_list() {
            if Some(doc) == keep {
                continue;
            }
            if dirty {
                kept += 1;
            } else {
                self.delete_buffer(&(doc + 1).to_string(), false);
            }
        }
        if kept > 0 {
            self.svc.notify.info(format!("保存していない {kept} 個のタブは残しました（× で 1 つずつ閉じられます）"));
        }
    }
}

/// タブの行（layout.rs の View）
pub struct BufferTabsView;

impl crate::layout::View for BufferTabsView {
    fn rect(&self, ed: &Editor, s: crate::layout::Screen) -> Option<Rect> {
        let _ = s;
        // 窓ごとの列: 窓の領域（列の行の外は下へ通す）
        if ed.win_tabs_on() {
            let (x, w) = ed.windows_x();
            let y = ed.windows_top();
            return Some(Rect { x, y, w, h: ed.windows_rows().saturating_sub(y) });
        }
        if ed.tabs_row() == 0 {
            return None;
        }
        let (x, w) = ed.tabs_span();
        Some(Rect { x, y: ed.area().y, w, h: 1 })
    }

    fn draw(&self, ed: &Editor, p: &mut crate::view::Painter, r: Rect, s: crate::layout::Screen) {
        let _ = s;
        if ed.win_tabs_on() {
            for (w, strip) in ed.tab_strips() {
                draw::draw_tabs_in(p, ed, strip, Some(w));
            }
            return;
        }
        draw::draw_tabs(p, ed, r);
    }

    fn click(&self, ed: &mut Editor, r: Rect, at: crate::layout::At, clicks: u32) -> crate::layout::Click {
        let (row, col) = at.screen;
        let _ = (r, clicks);
        let strip = ed.win_tabs_on();
        if strip && !ed.tab_strips().iter().any(|(_, s)| s.contains(row, col)) {
            return crate::layout::Click::Pass;
        }
        match ed.buftab_win_at(row, col) {
            Some((Some(w), doc, true)) => ed.tab_close_in(w, doc),
            Some((win, doc, close)) => {
                if let Some(w) = win {
                    ed.leave_panel();
                    ed.focus_window(w);
                }
                if close {
                    ed.tab_close(doc);
                } else {
                    ed.tab_select(doc);
                }
            }
            None => {
                // 列の空いた所: その窓へ
                if let Some((w, _)) = ed.tab_strips().into_iter().find(|(_, s)| s.contains(row, col)) {
                    ed.focus_window(w);
                }
            }
        }
        crate::layout::Click::Done
    }

    fn context(&self, ed: &mut Editor, r: Rect, at: crate::layout::At) -> bool {
        let (row, col) = at.screen;
        let _ = r;
        if ed.win_tabs_on() && !ed.tab_strips().iter().any(|(_, s)| s.contains(row, col)) {
            return false;
        }
        if let Some((Some(w), ..)) = ed.buftab_win_at(row, col) {
            ed.focus_window(w);
        }
        let tab = ed.buftab_at(row, col).map(|t| t.0);
        let mut m = crate::menu::Menu::new(row, col);
        // IntelliJ のエディタのタブと同じ並び: 閉じる → 他・すべて・未変更・左側・右側 → 分割 → パス → 閉じたタブを再度開く
        if let Some(doc) = tab {
            m = m.item("タブを閉じる", move |e| e.tab_close(doc)).item("他のタブを閉じる", move |e| e.tab_close_others(Some(doc)));
        }
        m = m.item("すべてのタブを閉じる", |e| e.tab_close_others(None)).item("未変更のタブを閉じる", |e| e.tab_close_unmodified());
        if let Some(doc) = tab {
            m = m.item("左側のタブを閉じる", move |e| e.tab_close_side(doc, true)).item("右側のタブを閉じる", move |e| e.tab_close_side(doc, false)).sep();
            m = m
                .item("右に分割", move |e| {
                    e.tab_select(doc);
                    e.shortcut("vs");
                })
                .item("下に分割", move |e| {
                    e.tab_select(doc);
                    e.shortcut("sp");
                });
            if let Some(path) = ed.buffer_list().into_iter().find(|t| t.0 == doc).and_then(|t| t.1) {
                let p = std::path::absolute(&path).unwrap_or(path).display().to_string();
                m = m.sep().item("パスをコピー", move |e| e.copy_text(p, "パス"));
            }
        }
        m = m.sep().item("閉じたタブを再度開く", |e| {
            e.shortcut("ReopenClosed");
        });
        m = m.sep().item("タブの一覧…", |e| e.open_finder(crate::finder::Source::Buffers));
        ed.open_menu(m);
        true
    }

    fn wheel(&self, ed: &mut Editor, r: Rect, at: crate::layout::At, dy: isize, dx: isize) -> bool {
        let _ = (at, r);
        // タブの列を横に送る（IntelliJ と同じ。バッファは切り替えない）
        let d = if dy != 0 { dy } else { dx };
        let n = ed.buffer_list().len() as isize;
        ed.tab_shift = (ed.tab_shift + d.signum()).clamp(-n, n);
        true
    }
}

pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    pub(crate) fn draw_tabs(p: &mut Painter, ed: &Editor, r: Rect) {
        draw_tabs_in(p, ed, r, None);
    }

    /// タブの列（`win` が Some なら窓ごとの列。作業中の窓でなければ下線を控えめに）
    pub(crate) fn draw_tabs_in(p: &mut Painter, ed: &Editor, r: Rect, win: Option<usize>) {
        let cell = p.fonts.cell();
        let current = win.is_none_or(|w| w == ed.core.cur_win);
        let (x0, wpx, y0) = (r.x as f32 * cell.w, r.w as f32 * cell.w, r.y as f32 * cell.h);
        p.out.push(Instance::fill(x0, y0, wpx, cell.h, BG));
        p.out.push(Instance::fill(x0, y0 + cell.h - 1.0, wpx, 1.0, SEPARATOR));
        let saved = p.cols;
        p.cols = r.x + r.w;
        for t in ed.buffer_tabs_in(win, r.w) {
            let hover = ed.hover_buftab.filter(|h| h.0 == t.doc);
            let tx = (r.x + t.x) as f32 * cell.w;
            // 選んでいるタブは青い下線（IntelliJ の New UI と同じ）。載せたタブは地を明るく
            if hover.is_some() && !t.active {
                p.out.push(Instance::fill(tx, y0, t.w as f32 * cell.w, cell.h - 1.0, RAISED_BG));
            }
            if t.active {
                p.out.push(Instance::fill(tx, y0 + cell.h - crate::metrics::accent_line(), t.w as f32 * cell.w, crate::metrics::accent_line(), if current { ACCENT_FG } else { STATUS_FG }));
            }
            let fg = if t.active && current { FG_BRIGHT } else if t.active { FG } else if hover.is_some() { FG } else { STATUS_FG };
            // ファイルの種類のアイコン（プロジェクトの木と同じ）。差分のタブは差分のアイコン
            if t.doc == crate::diff_view::DIFF_TAB {
                let px = crate::metrics::icon(crate::metrics::Icon::Small);
                crate::icons::draw(p, "g_diff", (r.x + t.x + 1) as f32 * cell.w, crate::metrics::centered(y0, px), px, None);
            } else if let Some(path) = &t.path {
                let row = crate::tree_panel::Row { path: path.clone(), name: file_name(path), depth: 0, dir: false };
                crate::tree_panel::draw::draw_tree_icon(p, &crate::tree_panel::draw::tree_icon(&row, ed.nav.index.as_ref(), &ed.nav_root()), r.x + t.x + 1, y0);
            }
            p.text(&t.label, r.x + t.x + 1 + crate::metrics::icon_cols(crate::metrics::Icon::Small), y0, fg);
            // × は控えめに。載せたら明るく、変更があれば載せるまで ●
            let cx = r.x + t.close_col();
            let (mark, color) = match hover {
                Some((_, true)) => {
                    p.out.push(Instance::fill(cx as f32 * cell.w - 2.0, y0 + 1.0, cell.w + 4.0, cell.h - 3.0, MATCH_BG));
                    ("×", FG)
                }
                Some(_) => ("×", STATUS_FG),
                None if t.dirty => ("●", STATUS_FG),
                None => ("×", LINE_NO),
            };
            p.text(mark, cx, y0, color);
        }
        p.cols = saved;
    }
}

#[cfg(test)]
mod tests {
    use crate::editor::Editor;
    use crate::layout::View;
    use nagi_core::Buffer;

    fn editor(files: &[&str]) -> (Editor, std::path::PathBuf) {
        // 同じファイルの並びを使う試験が並んで走ってもぶつからないように、呼ぶたびに別のフォルダ
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("nagi-tabs-{}-{n}-{}", std::process::id(), files.join("-")));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for f in files {
            std::fs::write(dir.join(f), format!("{f}\n")).unwrap();
        }
        let mut buf = Buffer::open(&dir.join(files[0])).unwrap();
        buf.path = Some(dir.join(files[0]));
        let mut e = Editor::new(buf, None);
        e.settings.set("editor.tabs", "true", false);
        e.set_screen(100, 30);
        // 自動で保存しないとき（閉じるときに確かめる）。自動で保存するときは closing_a_dirty_tab_asks の後半
        e.settings.set("editor.autosave", "off", false);
        for f in &files[1..] {
            e.edit_file(dir.join(f));
        }
        (e, dir)
    }

    /// 窓を分けると、窓ごとにタブの列を出す: 新しい窓は今のファイルだけ、前の窓は開いていたものすべて。
    /// 窓の本文は列の下から。窓のタブを押すとその窓へ移って開く。ほかの窓にあるタブを閉じても、その窓から外すだけ。
    /// 最後のタブを閉じると窓を閉じ、分けていなければ上の列に戻る
    #[test]
    fn tabs_per_split() {
        let (mut e, dir) = editor(&["a.txt", "b.txt", "c.txt"]);
        // 今は c.txt
        let top = e.area().y;
        assert_eq!(e.tabs_row(), 1);
        e.split(true);
        assert!(e.win_tabs_on());
        assert_eq!(e.tabs_row(), 0, "上の共通の列は出さない");
        let rects = e.window_rects();
        assert!(rects.iter().all(|(_, r)| r.y == top + 1), "窓の本文は列の下から: {rects:?}");
        let new = e.core.cur_win;
        let old = rects.iter().map(|x| x.0).find(|&w| w != new).unwrap();
        let names = |e: &Editor, w: usize| e.buffer_tabs_in(Some(w), 100).into_iter().map(|t| t.label).collect::<Vec<_>>();
        assert_eq!(names(&e, new), ["c.txt"], "新しい窓は今のファイルだけ");
        assert_eq!(names(&e, old), ["a.txt", "b.txt", "c.txt"]);
        // 前の窓の a.txt を押す: その窓へ移って開く
        let strip = e.tab_strips().into_iter().find(|s| s.0 == old).unwrap().1;
        let t = e.buffer_tabs_in(Some(old), strip.w).into_iter().find(|t| t.label == "a.txt").unwrap();
        let s = crate::layout::Screen { rows: 30, cols: 100 };
        crate::layout::press(&mut e, s, strip.y, strip.x + t.x + 1, |_| 1);
        assert_eq!(e.core.cur_win, old);
        assert!(e.core.buf.path.as_ref().unwrap().ends_with("a.txt"));
        // 新しい窓で b.txt を開くと、その窓のタブに増える
        e.focus_window(new);
        let b = e.buffer_list().into_iter().find(|t| t.1.as_ref().is_some_and(|p| p.ends_with("b.txt"))).unwrap().0;
        e.switch_doc(b);
        assert_eq!(names(&e, new), ["b.txt", "c.txt"]);
        // 前の窓にもある c.txt を新しい窓で閉じる: 新しい窓から外すだけ（ファイルは開いたまま）
        let c = e.buffer_list().into_iter().find(|t| t.1.as_ref().is_some_and(|p| p.ends_with("c.txt"))).unwrap().0;
        e.tab_close_in(new, c);
        assert_eq!(names(&e, new), ["b.txt"]);
        assert!(e.buffer_list().iter().any(|t| t.0 == c));
        // 最後のタブを閉じると窓を閉じ、上の列に戻る（b.txt は前の窓にもあるので開いたまま）
        e.tab_close_in(new, b);
        assert!(!e.is_split());
        assert_eq!(e.tabs_row(), 1);
        assert!(e.buffer_list().iter().any(|t| t.0 == b));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn labels(e: &Editor) -> Vec<String> {
        e.buffer_tabs(100).into_iter().map(|t| t.label).collect()
    }

    #[test]
    fn tabs_switch_and_close() {
        let (mut e, dir) = editor(&["a.txt", "b.txt", "c.txt"]);
        assert_eq!(labels(&e), ["a.txt", "b.txt", "c.txt"]);
        // 本文はタブの下から
        let y = e.area().y;
        assert_eq!(e.windows_top(), y + 1);
        let s = crate::layout::Screen { rows: 30, cols: 100 };
        let r = crate::buffer_tabs::BufferTabsView.rect(&e, s).unwrap();
        // a.txt を押すとそのバッファへ
        let a = e.buffer_tabs(100).remove(0);
        crate::buffer_tabs::BufferTabsView.click(&mut e, r, crate::layout::At::new(r, y, r.x + a.x + 1), 1);
        assert_eq!(e.core.buf.path.as_deref(), Some(dir.join("a.txt").as_path()));
        // b.txt の × で閉じる
        let b = e.buffer_tabs(100).remove(1);
        assert_eq!(e.buftab_at(y, r.x + b.close_col()), Some((b.doc, true)));
        crate::buffer_tabs::BufferTabsView.click(&mut e, r, crate::layout::At::new(r, y, r.x + b.close_col()), 1);
        assert_eq!(labels(&e), ["a.txt", "c.txt"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 左側・右側・未変更のタブを閉じる（保存していないものは残す）
    #[test]
    fn close_left_right_unmodified() {
        let (mut e, _dir) = editor(&["a.txt", "b.txt", "c.txt", "d.txt"]);
        let doc = |e: &Editor, n: &str| e.buffer_list().into_iter().find(|t| t.1.as_ref().is_some_and(|p| p.ends_with(n))).unwrap().0;
        let b = doc(&e, "b.txt");
        e.tab_close_side(b, true);
        assert_eq!(labels(&e), ["b.txt", "c.txt", "d.txt"]);
        e.tab_close_side(b, false);
        assert_eq!(labels(&e), ["b.txt"]);
        let (mut e, _dir) = editor(&["a.txt", "b.txt", "c.txt"]);
        let a = doc(&e, "a.txt");
        e.tab_select(a);
        e.core.buf.edit(0, 0, "x");
        e.core.buf.dirty = true;
        let c = doc(&e, "c.txt");
        e.tab_select(c);
        e.tab_close_unmodified();
        assert_eq!(labels(&e), ["a.txt", "c.txt"], "変えた a と今の c は残す");
    }

    #[test]
    fn closing_a_dirty_tab_asks() {
        let (mut e, dir) = editor(&["d.txt", "e.txt"]);
        // 作業中の e.txt を変える
        e.core.buf.edit(0, 0, "x");
        e.core.buf.dirty = true;
        let doc = e.core.cur_doc;
        e.tab_close(doc);
        assert!(e.menu.is_some(), "保存していなければ窓で確かめる");
        // 保存して閉じる
        e.menu_run(0);
        assert_eq!(labels(&e), ["d.txt"]);
        assert_eq!(std::fs::read_to_string(dir.join("e.txt")).unwrap(), "xe.txt\n");
        // 最後のタブを閉じると、タブは無くなる（空のバッファはタブにしない）
        e.tab_close(e.core.cur_doc);
        assert!(labels(&e).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
        // 自動で保存する（既定）ときは、確かめずに保存して閉じる（IntelliJ と同じ）
        let (mut e, dir) = editor(&["f.txt", "g.txt"]);
        e.settings.set("editor.autosave", "idle", false);
        e.core.buf.edit(0, 0, "y");
        e.core.buf.dirty = true;
        e.tab_close(e.core.cur_doc);
        assert!(e.menu.is_none(), "確かめない");
        assert_eq!(labels(&e), ["f.txt"]);
        assert_eq!(std::fs::read_to_string(dir.join("g.txt")).unwrap(), "yg.txt\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_names_show_the_folder_and_active_tab_stays_visible() {
        let dir = std::env::temp_dir().join(format!("nagi-tabs-same-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["x", "y"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
            std::fs::write(dir.join(d).join("mod.rs"), "\n").unwrap();
        }
        let mut buf = Buffer::open(&dir.join("x/mod.rs")).unwrap();
        buf.path = Some(dir.join("x/mod.rs"));
        let mut e = Editor::new(buf, None);
        e.settings.set("editor.tabs", "true", false);
        e.set_screen(100, 30);
        e.edit_file(dir.join("y/mod.rs"));
        assert_eq!(labels(&e), ["mod.rs ‹x›", "mod.rs ‹y›"]);
        // 狭ければ左を詰めて、作業中（y）を出す
        let t = e.buffer_tabs(20);
        assert_eq!(t.len(), 1);
        assert!(t[0].active);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
