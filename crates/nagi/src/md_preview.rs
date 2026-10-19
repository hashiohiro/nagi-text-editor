//! Markdown のプレビュー（右の欄の「プレビュー」タブ）。
//!
//! `:MarkdownPreview`（IntelliJ の Markdown.Layout.EditorAndPreview と同じ割り当て）で出し、もう一度で閉じる。打つたびに
//! 作り直し、エディタのカーソルの行に合わせてスクロールする。欄に移れば j / k で送り、Enter でリンクを開く
//! （.md などのファイルは Nagi で、URL はブラウザで）。字はすべて等幅のまま。

use std::path::PathBuf;

use nagi_syntax::markdown::{Line, render};
use nagi_vim::Key;

use crate::editor::{Editor, Effect, State};
use crate::right_panel::RightTab;

pub struct MdPreview {
    /// 作り直した元（パス, 版, 幅）
    pub key: (PathBuf, u64, usize),
    pub lines: Vec<Line>,
    pub scroll: usize,
    /// 欄にいるときに選んでいる行（Enter でリンクを開く）
    pub selected: usize,
    /// エディタのカーソルに合わせてスクロールする（欄で j / k を押したら、エディタに戻るまで止める。
    /// ホイールで送ったら、エディタのカーソルが動くまで止める）
    pub follow: bool,
    /// 最後に見たエディタのカーソルの行（動いたら、また付いていく）
    seen_line: usize,
}

pub fn is_markdown(p: &std::path::Path) -> bool {
    p.extension().is_some_and(|e| matches!(e.to_string_lossy().to_ascii_lowercase().as_str(), "md" | "markdown" | "mdx"))
}

impl Editor {
    /// `:MarkdownPreview`: 出す・閉じる
    pub(crate) fn toggle_md_preview(&mut self) {
        if self.md_preview.is_some() && self.right_visible() == Some(RightTab::Preview) {
            self.md_preview = None;
            if self.right_focused() {
                self.set_state(State::Normal);
            }
            self.rows = self.text_rows(self.active_rect());
            return;
        }
        if !self.buf.path.as_deref().is_some_and(is_markdown) {
            self.notify.info("Markdown のファイルではありません");
            return;
        }
        self.md_preview = Some(MdPreview { key: (PathBuf::new(), 0, 0), lines: Vec::new(), scroll: 0, selected: 0, follow: true, seen_line: usize::MAX });
        self.right_tab = RightTab::Preview;
        self.right_hidden = false;
        self.rows = self.text_rows(self.active_rect());
        self.refresh_md_preview();
    }

    /// 今の Markdown のバッファに合わせて作り直し、カーソルの行へスクロールする（描く前に呼ぶ）
    pub(crate) fn refresh_md_preview(&mut self) {
        if self.md_preview.is_none() || self.right_visible() != Some(RightTab::Preview) {
            return;
        }
        let w = self.right_rect().map_or(40, |r| r.w.saturating_sub(2));
        let h = self.right_rect().map_or(20, |r| r.h.saturating_sub(1)).max(1);
        let focused = self.right_focused();
        // エディタが Markdown のバッファなら、それを映す（ほかのファイルに移っても最後の Markdown を出したまま）
        let md = self.buf.path.as_deref().is_some_and(is_markdown);
        let cur_line = self.cursor.line;
        let key = (self.buf.path.clone().unwrap_or_default(), self.buf.version, w);
        let Some(pv) = &mut self.md_preview else { return };
        if md && pv.key != key {
            pv.lines = render(&self.buf.text(), w);
            pv.key = key;
            pv.selected = pv.selected.min(pv.lines.len().saturating_sub(1));
        }
        if !focused && pv.seen_line != cur_line {
            pv.follow = true;
        }
        pv.seen_line = cur_line;
        if md && pv.follow && !focused {
            // カーソルの行を作った行のうち、元の行がそれ以下で最後のもの。上から 1/3 に置く
            let at = pv.lines.iter().rposition(|l| l.src <= cur_line).unwrap_or(0);
            pv.selected = at;
            pv.scroll = at.saturating_sub(h / 3);
        }
        let max = pv.lines.len().saturating_sub(h);
        pv.scroll = pv.scroll.min(max);
        crate::list::fit(pv.selected, &mut pv.scroll, h);
    }

    pub(crate) fn md_preview_key(&mut self, key: Key) -> Effect {
        let h = self.right_rect().map_or(20, |r| r.h.saturating_sub(1)).max(1);
        let Some(pv) = &mut self.md_preview else {
            self.set_state(State::Normal);
            return Effect::None;
        };
        pv.follow = false;
        let moved = crate::list::step(key, pv.selected, pv.lines.len(), h);
        match key {
            _ if moved.is_some() => pv.selected = moved.unwrap_or(pv.selected),
            Key::Char('g') => pv.selected = 0,
            Key::Enter => {
                let link = pv.lines.get(pv.selected).and_then(|l| l.links.first()).map(|x| x.1.clone());
                match link {
                    Some(u) => self.open_md_link(&u),
                    None => self.notify.info("この行にリンクはありません"),
                }
            }
            Key::Char('q') => {
                self.md_preview = None;
                self.set_state(State::Normal);
                self.rows = self.text_rows(self.active_rect());
            }
            Key::Esc => self.set_state(State::Normal),
            Key::Char(':') => self.enter_command(""),
            _ => {}
        }
        Effect::None
    }

    /// リンクを開く: URL はブラウザ、ほかは Markdown のファイルからの相対パスで Nagi に（#見出し は落とす）
    fn open_md_link(&mut self, url: &str) {
        if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:") {
            // 試験ではブラウザを開かない
            if !cfg!(test) {
                let _ = nagi_workers::config::open_url(url);
            }
            self.notify.info(format!("ブラウザで開きました: {url}"));
            return;
        }
        let path = url.split('#').next().unwrap_or("");
        if path.is_empty() {
            return;
        }
        let base = self.md_preview.as_ref().and_then(|p| p.key.0.parent().map(|d| d.to_path_buf())).unwrap_or_default();
        let target = base.join(path.replace('/', std::path::MAIN_SEPARATOR_STR));
        if target.is_file() {
            self.set_state(State::Normal);
            self.edit_file(target);
        } else {
            self.notify.info(format!("ファイルがありません: {}", target.display()));
        }
    }

    /// プレビューの行のクリック（2 回でリンクを開く）
    pub(crate) fn md_preview_click(&mut self, row: usize, clicks: u32) {
        let Some(pv) = &mut self.md_preview else { return };
        let Some(i) = crate::list::row_at(pv.scroll, row, 1, pv.lines.len()) else { return };
        pv.selected = i;
        pv.follow = false;
        self.focus_right(RightTab::Preview);
        if clicks >= 2 {
            self.md_preview_key(Key::Enter);
        }
    }
}


/// 描き方（view.rs の Painter で描く）
pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    /// Markdown のプレビュー（右の欄）。等幅のまま、見出しの罫・コードの地・表の罫線・引用の ▎
    pub(crate) fn draw_md_preview(p: &mut Painter, pv: &crate::md_preview::MdPreview, r: Rect, focused: bool) {
        use nagi_syntax::markdown::Style;
        const CODE_BG: [f32; 4] = rgb(0x1a1a1a);
        let cell = p.fonts.cell();
        let (x0, y0, wpx) = (r.x as f32 * cell.w, r.y as f32 * cell.h, r.w as f32 * cell.w);
        let body = r.h.saturating_sub(1);
        if pv.lines.is_empty() {
            p.text(" （空の文書）", r.x, y0 + cell.h, STATUS_FG);
            return;
        }
        for (k, l) in pv.lines.iter().enumerate().skip(pv.scroll).take(body) {
            let y = y0 + (1 + k - pv.scroll) as f32 * cell.h;
            if l.code {
                p.out.push(Instance::fill(x0 + cell.w, y, wpx - 2.0 * cell.w, cell.h, CODE_BG));
            }
            if focused && k == pv.selected {
                p.out.push(Instance::fill(x0, y, 2.0, cell.h, FG));
            }
            // 範囲ごとの色（無いところは地の色）
            let color = |st: Style| match st {
                Style::Plain => FG,
                Style::Heading => rgb(0x87afd7),
                Style::Strong => rgb(0xeeeeee),
                Style::Emph => rgb(0xd7af87),
                Style::Code => rgb(0xafaf87),
                Style::Link => rgb(0x87afd7),
                Style::Strike => rgb(0x6c6c6c),
                Style::Dim => rgb(0x6c6c6c),
                Style::Marker => rgb(0xd7875f),
                Style::Hl(h) => hl_color(h),
            };
            let mut items: Vec<Item> = Vec::new();
            let mut bi = 0;
            for g in l.text.graphemes(true) {
                let st = l.spans.iter().rev().find(|(rg, _)| rg.start <= bi && bi < rg.end).map(|x| x.1);
                let width = p.cell_width(g);
                let fg = st.map_or(FG, color);
                let bg = if st == Some(Style::Code) && !l.code { Some(CODE_BG) } else { None };
                let underline = st == Some(Style::Link);
                items.push(Item { g: Cow::Borrowed(g), width, fg, bg, underline });
                bi += g.len();
            }
            p.draw(&items, r.x + 1, y);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    #[test]
    fn preview_follows_cursor_and_opens_links() {
        let dir = std::env::temp_dir().join(format!("nagi-mdp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        let mut doc = String::from("# T\n\n[手順](docs/setup.md)\n\n");
        for i in 0..80 {
            doc.push_str(&format!("段落 {i}\n\n"));
        }
        std::fs::write(dir.join("README.md"), &doc).unwrap();
        std::fs::write(dir.join("docs/setup.md"), "# setup\n").unwrap();
        let mut buf = Buffer::open(&dir.join("README.md")).unwrap();
        buf.path = Some(dir.join("README.md"));
        let mut e = Editor::new(buf, None);
        e.set_screen(160, 40);
        e.toggle_md_preview();
        assert_eq!(e.right_visible(), Some(RightTab::Preview));
        assert!(e.right_width() >= 60, "プレビューは広め: {}", e.right_width());
        let pv = e.md_preview.as_ref().unwrap();
        assert_eq!(pv.lines[0].text, "T");
        // カーソルを下へ: プレビューも送られる
        for k in nagi_vim::parse_keys("121G", " ") {
            e.key(k);
        }
        e.refresh_md_preview();
        let pv = e.md_preview.as_ref().unwrap();
        assert!(pv.scroll > 30, "{}", pv.scroll);
        assert_eq!(pv.lines[pv.selected].text.trim(), "段落 58");
        // 打つと作り直す
        for k in nagi_vim::parse_keys("ggA追記<Esc>", " ") {
            e.key(k);
        }
        e.refresh_md_preview();
        assert_eq!(e.md_preview.as_ref().unwrap().lines[0].text, "T追記");
        // 欄に移ってリンクの行で Enter → そのファイルを開く
        e.focus_right(RightTab::Preview);
        let at = e.md_preview.as_ref().unwrap().lines.iter().position(|l| !l.links.is_empty()).unwrap();
        e.md_preview.as_mut().unwrap().selected = at;
        e.key(Key::Enter);
        assert!(e.buf.path.as_ref().unwrap().ends_with("setup.md"), "{:?} {}", e.buf.path, e.notify.text);
        // もう一度で閉じる
        e.set_state(State::Normal);
        e.toggle_md_preview();
        assert!(e.md_preview.is_none());
    }
}
