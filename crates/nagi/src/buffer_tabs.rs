//! 本文の上のタブ（開いているバッファ）。Vim の有無を問わず出す（設定の「エディタ → タブ」で隠せる）。
//!
//! 押すとそのバッファへ、× で閉じる（保存していなければ、保存して閉じるか・保存せずに閉じるかを窓で確かめる）。
//! 変更のあるタブは × の代わりに ●（マウスを載せると ×）。右クリックで閉じる・ほかを閉じる・すべて閉じる・一覧。
//! ホイールで隣のバッファへ。入りきらなければ、作業中のタブが見えるように左を詰める。

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
        let len = match &self.docs[id] {
            Slot::Active => self.buf.len_bytes(),
            Slot::Parked(d) => d.buf.len_bytes(),
            Slot::Deleted => 0,
        };
        len > 0
    }

    /// 幅 `w` に入るタブ（同じ名前のファイルは親のフォルダを添える）
    pub(crate) fn buffer_tabs(&self, w: usize) -> Vec<BufTab> {
        let list: Vec<_> = self.buffer_list().into_iter().filter(|(i, p, d, _)| self.tab_worthy(*i, p, *d)).collect();
        let names: Vec<String> = list.iter().map(|(_, p, ..)| p.as_deref().map_or("無題".into(), file_name)).collect();
        let width = |s: &str| nagi_core::display_col(s, s.len(), false);
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
                BufTab { doc: *doc, w: width(&label) + 6, label, x: 0, active: *active, dirty: *dirty, path: path.clone() }
            })
            .collect();
        // 作業中のタブが入るまで左を詰める
        let fits = |t: &[BufTab]| t.iter().map(|t| t.w).sum::<usize>() <= w;
        if let Some(a) = tabs.iter().position(|t| t.active) {
            let mut start = 0;
            while start < a && !fits(&tabs[start..=a]) {
                start += 1;
            }
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
        if self.tabs_row() == 0 || row != self.area().y {
            return None;
        }
        let (x, w) = self.tabs_span();
        let c = col.checked_sub(x)?;
        let t = self.buffer_tabs(w).into_iter().find(|t| c >= t.x && c < t.x + t.w)?;
        Some((t.doc, c == t.close_col()))
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
        self.leave_panel();
        self.switch_doc(doc);
    }

    /// タブを閉じる。保存していなければ、保存して閉じるか・保存せずに閉じるかを窓で確かめる
    pub(crate) fn tab_close(&mut self, doc: usize) {
        let Some((_, path, dirty, _)) = self.buffer_list().into_iter().find(|t| t.0 == doc) else { return };
        let n = (doc + 1).to_string();
        if !dirty {
            self.delete_buffer(&n, false);
            return;
        }
        let name = path.as_deref().map_or("無題".into(), file_name);
        let m = crate::menu::Menu::new(0, 0)
            .item_key("保存して閉じる（S）", 's', move |e| {
                e.tab_select(doc);
                e.shortcut("w");
                // 保存できなかった（名前が無い・書き込めない）ら残す
                if e.cur_doc == doc && !e.buf.dirty {
                    e.delete_buffer("", false);
                }
            })
            .item_key("保存せずに閉じる（D）", 'd', move |e| e.delete_buffer(&n, true))
            .item_key("やめる（Esc）", 'c', |_| {});
        self.ask_menu(format!("{name} は保存していません"), m, 2);
    }

    /// `keep` 以外のタブ（None ならすべて）を閉じる。保存していないものは残して知らせる
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
            self.notify.info(format!("保存していない {kept} 個のタブは残しました（× で 1 つずつ閉じられます）"));
        }
    }
}

/// タブの行（layout.rs の View）
pub struct BufferTabsView;

impl crate::layout::View for BufferTabsView {
    fn rect(&self, ed: &Editor, s: crate::layout::Screen) -> Option<Rect> {
        let _ = s;
        if ed.tabs_row() == 0 {
            return None;
        }
        let (x, w) = ed.tabs_span();
        Some(Rect { x, y: ed.area().y, w, h: 1 })
    }

    fn draw(&self, ed: &Editor, p: &mut crate::view::Painter, r: Rect, s: crate::layout::Screen) {
        let _ = s;
        draw::draw_tabs(p, ed, r);
    }

    fn click(&self, ed: &mut Editor, r: Rect, row: usize, col: usize, clicks: u32) -> crate::layout::Click {
        let _ = (r, clicks);
        match ed.buftab_at(row, col) {
            Some((doc, true)) => ed.tab_close(doc),
            Some((doc, false)) => ed.tab_select(doc),
            None => {}
        }
        crate::layout::Click::Done
    }

    fn context(&self, ed: &mut Editor, r: Rect, row: usize, col: usize) -> bool {
        let _ = r;
        let tab = ed.buftab_at(row, col).map(|t| t.0);
        let mut m = crate::menu::Menu::new(row, col);
        if let Some(doc) = tab {
            m = m.item("閉じる", move |e| e.tab_close(doc)).item("ほかのタブを閉じる", move |e| e.tab_close_others(Some(doc)));
        }
        m = m.item("すべてのタブを閉じる", |e| e.tab_close_others(None)).sep().item("開いているファイルの一覧…", |e| e.open_finder(crate::finder::Source::Buffers));
        ed.open_menu(m);
        true
    }

    fn wheel(&self, ed: &mut Editor, r: Rect, row: usize, col: usize, dy: isize, dx: isize) -> bool {
        let _ = (r, row, col);
        let d = if dy != 0 { dy } else { dx };
        if d != 0 {
            ed.cycle_buffer(d > 0, 1);
        }
        true
    }
}

pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    pub(crate) fn draw_tabs(p: &mut Painter, ed: &Editor, r: Rect) {
        let cell = p.fonts.cell();
        let (x0, wpx, y0) = (r.x as f32 * cell.w, r.w as f32 * cell.w, r.y as f32 * cell.h);
        p.out.push(Instance::fill(x0, y0, wpx, cell.h, BG));
        p.out.push(Instance::fill(x0, y0 + cell.h - 1.0, wpx, 1.0, SEPARATOR));
        let saved = p.cols;
        p.cols = r.x + r.w;
        for t in ed.buffer_tabs(r.w) {
            let hover = ed.hover_buftab.filter(|h| h.0 == t.doc);
            let tx = (r.x + t.x) as f32 * cell.w;
            // 選んでいるタブは青い下線（IntelliJ の New UI と同じ）。載せたタブは地を明るく
            if hover.is_some() && !t.active {
                p.out.push(Instance::fill(tx, y0, t.w as f32 * cell.w, cell.h - 1.0, RAISED_BG));
            }
            if t.active {
                p.out.push(Instance::fill(tx, y0 + cell.h - 3.0, t.w as f32 * cell.w, 3.0, ACCENT_FG));
            }
            let fg = if t.active { FG_BRIGHT } else if hover.is_some() { FG } else { STATUS_FG };
            // ファイルの種類のアイコン（プロジェクトの木と同じ）
            if let Some(path) = &t.path {
                let row = crate::tree_panel::Row { path: path.clone(), name: file_name(path), depth: 0, dir: false };
                crate::tree_panel::draw::draw_tree_icon(p, &crate::tree_panel::draw::tree_icon(&row, ed.nav.index.as_ref(), &ed.nav_root()), r.x + t.x + 1, y0);
            }
            p.text(&t.label, r.x + t.x + 3, y0, fg);
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
        let dir = std::env::temp_dir().join(format!("nagi-tabs-{}-{}", std::process::id(), files.join("-")));
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
        for f in &files[1..] {
            e.edit_file(dir.join(f));
        }
        (e, dir)
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
        crate::buffer_tabs::BufferTabsView.click(&mut e, r, y, r.x + a.x + 1, 1);
        assert_eq!(e.buf.path.as_deref(), Some(dir.join("a.txt").as_path()));
        // b.txt の × で閉じる
        let b = e.buffer_tabs(100).remove(1);
        assert_eq!(e.buftab_at(y, r.x + b.close_col()), Some((b.doc, true)));
        crate::buffer_tabs::BufferTabsView.click(&mut e, r, y, r.x + b.close_col(), 1);
        assert_eq!(labels(&e), ["a.txt", "c.txt"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn closing_a_dirty_tab_asks() {
        let (mut e, dir) = editor(&["d.txt", "e.txt"]);
        // 作業中の e.txt を変える
        e.buf.edit(0, 0, "x");
        e.buf.dirty = true;
        let doc = e.cur_doc;
        e.tab_close(doc);
        assert!(e.menu.is_some(), "保存していなければ窓で確かめる");
        // 保存して閉じる
        e.menu_run(0);
        assert_eq!(labels(&e), ["d.txt"]);
        assert_eq!(std::fs::read_to_string(dir.join("e.txt")).unwrap(), "xe.txt\n");
        // 最後のタブを閉じると、タブは無くなる（空のバッファはタブにしない）
        e.tab_close(e.cur_doc);
        assert!(labels(&e).is_empty());
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
