//! 前回開いていたファイル（設定の「前回開いていたファイルを開き直す」）と、自動で保存する（設定の「自動で保存」）。
//!
//! 開いていたファイルは、プロジェクトの根ごとに %LOCALAPPDATA%\nagi\sessions\<根の SHA-256>.txt に
//! 「パス<TAB>行<TAB>byte<TAB>前面なら 1」で書く（窓を閉じたとき・別のプロジェクトへ移るとき）。無ければ IntelliJ の
//! workspace.xml から（project_open.rs）。プロジェクトのフォルダには書かない。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nagi_workers::project::OpenFile;

use crate::editor::Editor;

/// 開いていたファイルを書く場所（試験では None）
fn session_path(root: &Path) -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    let key = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf()).to_string_lossy().to_lowercase();
    let hex = nagi_core::hash::sha256_hex(key.as_bytes())?;
    Some(crate::settings::global_path()?.with_file_name("sessions").join(format!("{}.txt", &hex[..16])))
}

pub fn render(files: &[OpenFile]) -> String {
    files.iter().map(|f| format!("{}\t{}\t{}\t{}\n", f.path.display(), f.line, f.col, u8::from(f.current))).collect()
}

/// 窓の分け方（`#layout 形` と `#win パス 行 byte 上 作業中なら 1`）
#[derive(Debug, PartialEq)]
pub struct Windows {
    pub shape: String,
    pub wins: Vec<(PathBuf, usize, usize, usize, bool)>,
}

pub fn render_windows(w: &Windows) -> String {
    let mut s = format!("#layout\t{}\n", w.shape);
    for (p, l, b, top, active) in &w.wins {
        s.push_str(&format!("#win\t{}\t{l}\t{b}\t{top}\t{}\n", p.display(), u8::from(*active)));
    }
    s
}

pub fn parse_windows(text: &str) -> Option<Windows> {
    let shape = text.lines().find_map(|l| l.strip_prefix("#layout\t"))?.to_string();
    let wins = text
        .lines()
        .filter_map(|l| {
            let mut it = l.strip_prefix("#win\t")?.split('\t');
            Some((PathBuf::from(it.next()?), it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next() == Some("1")))
        })
        .collect();
    Some(Windows { shape, wins })
}

pub fn parse(text: &str) -> Vec<OpenFile> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let mut it = l.split('\t');
            let path = PathBuf::from(it.next()?);
            let line = it.next()?.parse().ok()?;
            let col = it.next()?.parse().ok()?;
            Some(OpenFile { path, line, col, current: it.next() == Some("1") })
        })
        .collect()
}

/// 前回 nagi で開いていたファイル（無ければ None。消えたファイルは除く）と、窓の分け方
pub fn load(root: &Path) -> Option<(Vec<OpenFile>, Option<Windows>)> {
    let text = std::fs::read_to_string(session_path(root)?).ok()?;
    let files: Vec<OpenFile> = parse(&text).into_iter().filter(|f| f.path.is_file()).collect();
    (!files.is_empty()).then(|| (files, parse_windows(&text)))
}

impl Editor {
    /// 開いているファイルをプロジェクトの根ごとに書く（窓を閉じたとき・別のプロジェクトへ移るとき）
    pub(crate) fn save_session(&self) {
        if !self.settings.bool("editor.restore_session") {
            return;
        }
        let Some(root) = self.settings.root.clone() else { return };
        let Some(p) = session_path(&root) else { return };
        let files = self.open_file_positions(&root);
        if files.is_empty() {
            return;
        }
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let mut text = render(&files);
        if let Some(w) = self.windows_to_save() {
            text.push_str(&render_windows(&w));
        }
        let _ = std::fs::write(&p, text);
    }

    /// 書き出す窓の分け方（分けていて、どの窓も名前のあるファイルを出しているとき）
    fn windows_to_save(&self) -> Option<Windows> {
        let (shape, wins) = self.window_snapshot()?;
        let wins = wins.into_iter().map(|(doc, c, top, active)| Some((self.doc_path(doc)?, c.line, c.byte, top, active))).collect::<Option<Vec<_>>>()?;
        Some(Windows { shape, wins })
    }

    /// 前回の窓の分け方に戻す（ファイルは開いてあるもの）
    pub(crate) fn restore_saved_windows(&mut self, w: Windows) {
        let Some(layout) = crate::panes::Layout::parse(&w.shape, w.wins.len()) else { return };
        let mut docs = Vec::new();
        for (p, line, byte, top, _) in &w.wins {
            let Some(id) = self.find_doc(p) else { return };
            docs.push((id, nagi_core::Cursor { line: *line, byte: *byte, want_col: 0 }, *top));
        }
        let active = w.wins.iter().position(|x| x.4).unwrap_or(0);
        self.restore_windows(layout, docs, active);
    }

    /// 自動で保存する（設定の「自動で保存」）。`why` が設定に合えば、名前のある変更したファイルをすべて保存する
    pub(crate) fn autosave(&mut self, why: AutoSave) {
        let mode = self.settings.get("editor.autosave");
        let on = match why {
            AutoSave::FocusLost | AutoSave::Switch => mode != "off",
            AutoSave::Idle => mode == "idle",
        };
        if !on || self.dirty_docs().is_empty() || self.buffer_list().iter().all(|(_, p, d, _)| !*d || p.is_none()) {
            return;
        }
        let msg = self.notify.text.clone();
        // 自動の保存では、前の知らせを消さない（保存できなかったときはその知らせ）
        if self.write_all() {
            self.notify.info(msg);
        }
    }

    /// 止まって何秒たったら保存するか（設定が idle のとき）
    pub fn autosave_deadline(&self) -> Option<Instant> {
        (self.settings.get("editor.autosave") == "idle" && self.buf.dirty && self.buf.path.is_some())
            .then(|| self.last_edit_at.map(|t| t + Duration::from_secs(self.settings.int("editor.autosave_secs") as u64)))
            .flatten()
    }
}

/// 自動で保存するきっかけ
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoSave {
    /// 窓を離れた
    FocusLost,
    /// 別のファイルへ移った
    Switch,
    /// 止まって決めた秒数たった
    Idle,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 自動で保存: しない設定なら保存せず、「窓を離れたとき」なら保存する。打つのを止めた時刻から数える
    #[test]
    fn autosave_modes() {
        let dir = std::env::temp_dir().join(format!("nagi-autosave-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.txt");
        std::fs::write(&f, "x\n").unwrap();
        let mut e = Editor::new(nagi_core::Buffer::open(&f).unwrap(), None);
        e.insert_text("y");
        e.autosave(AutoSave::FocusLost);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "x\n", "既定はしない");
        e.settings.set("editor.autosave", "focus", false);
        assert!(e.autosave_deadline().is_none());
        e.autosave(AutoSave::Idle);
        assert!(e.buf.dirty, "focus では止まっても保存しない");
        e.autosave(AutoSave::FocusLost);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "yx\n");
        e.settings.set("editor.autosave", "idle", false);
        e.insert_text("z");
        assert!(e.autosave_deadline().is_some());
        let rows = e.open_file_positions(&dir);
        assert_eq!((rows.len(), rows[0].col, rows[0].current), (1, 2, true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 分けた窓を書き出して、読み戻すと同じ形・同じファイル・同じ位置
    #[test]
    fn windows_round_trip() {
        let dir = std::env::temp_dir().join(format!("nagi-wins-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (n, t) in [("a.txt", "a1\na2\na3\n"), ("b.txt", "b1\nb2\n")] {
            std::fs::write(dir.join(n), t).unwrap();
        }
        let mut e = Editor::new(nagi_core::Buffer::open(&dir.join("a.txt")).unwrap(), None);
        e.buf.path = Some(dir.join("a.txt"));
        e.set_screen(120, 40);
        e.split(true);
        e.edit_file(dir.join("b.txt"));
        e.cursor.line = 1;
        e.split(false);
        let w = e.windows_to_save().unwrap();
        let text = render_windows(&w);
        let back = parse_windows(&text).unwrap();
        assert_eq!(back, w);
        assert_eq!(back.shape, "(v 1 1 : (h 1 1 : 0 1) 2)");
        // 分けていない窓に戻す
        let mut e2 = Editor::new(nagi_core::Buffer::open(&dir.join("a.txt")).unwrap(), None);
        e2.buf.path = Some(dir.join("a.txt"));
        e2.set_screen(120, 40);
        e2.edit_file(dir.join("b.txt"));
        e2.restore_saved_windows(back);
        let again = e2.windows_to_save().unwrap();
        assert_eq!(again.shape, w.shape);
        assert_eq!(again.wins.iter().map(|x| (x.0.clone(), x.1, x.4)).collect::<Vec<_>>(), w.wins.iter().map(|x| (x.0.clone(), x.1, x.4)).collect::<Vec<_>>());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trip() {
        let files = vec![OpenFile { path: "C:\\p\\a.rs".into(), line: 3, col: 5, current: false }, OpenFile { path: "C:\\p\\b c.rs".into(), line: 0, col: 0, current: true }];
        let back = parse(&render(&files));
        assert_eq!(back.len(), 2);
        assert_eq!((back[1].path.to_string_lossy().as_ref(), back[1].current, back[0].line, back[0].col), ("C:\\p\\b c.rs", true, 3, 5));
    }
}
