//! プロジェクトの木のファイル操作（IntelliJ のプロジェクトの木の右クリックと同じ）。
//!
//! 新規作成（ファイル・ディレクトリ）・名前の変更・削除（ごみ箱へ）・切り取り・コピー・貼り付け・パスと参照のコピー・
//! エクスプローラーで表示・ターミナルで開く。名前を打つのは入力のダイアログ（input_dialog.rs）。
//! 名前を変えたら、開いているバッファのパスも付いていく。

use std::path::{Path, PathBuf};

use crate::buffers::Slot;
use crate::editor::{Editor, State};
use crate::tree_panel::TreeAction;

impl Editor {
    /// 選んでいる行（パス, ディレクトリか）
    pub(crate) fn tree_target(&self) -> Option<(PathBuf, bool)> {
        let t = self.panels.tree.as_ref()?;
        t.rows.get(t.selected).map(|r| (r.path.clone(), r.dir))
    }

    /// 新しく作る・貼り付ける先のフォルダ（ディレクトリならそこ、ファイルならその親）
    fn tree_folder(&self) -> Option<PathBuf> {
        let (p, dir) = self.tree_target()?;
        if dir { Some(p) } else { p.parent().map(Path::to_path_buf) }
    }

    /// 新規作成: ファイル（`dir` ならディレクトリ）。名前に / \ があれば途中のフォルダも作る
    pub(crate) fn tree_new(&mut self, dir: bool) {
        let Some(folder) = self.tree_folder() else { return };
        let title = if dir { "新規ディレクトリ" } else { "新規ファイル" };
        let note = format!("作る場所: {}", folder.display());
        self.open_input(title, "名前", "", vec![note], move |e, name| {
            let name = name.trim();
            if name.is_empty() {
                return;
            }
            let path = folder.join(name);
            if path.exists() {
                e.svc.notify.error(format!("{} はもうあります", path.display()));
                return;
            }
            let made = if dir {
                std::fs::create_dir_all(&path)
            } else {
                path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(&path, ""))
            };
            match made {
                Ok(()) => {
                    e.tree_do(TreeAction::Reload);
                    if !dir {
                        e.edit_file(path);
                        e.set_state(State::Normal);
                    }
                }
                Err(err) => e.svc.notify.error(format!("{} を作れません: {err}", path.display())),
            }
        });
    }

    /// 名前の変更…（開いているバッファのパスも付いていく）
    pub(crate) fn tree_rename(&mut self) {
        let Some((old, _)) = self.tree_target() else { return };
        let name = old.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let initial = name.clone();
        self.open_input("名前の変更", "新しい名前", &initial, Vec::new(), move |e, new_name| {
            let new_name = new_name.trim();
            if new_name.is_empty() || new_name == name {
                return;
            }
            let new = old.with_file_name(new_name);
            if new.exists() {
                e.svc.notify.error(format!("{} はもうあります", new.display()));
                return;
            }
            match std::fs::rename(&old, &new) {
                Ok(()) => {
                    e.follow_rename(&old, &new);
                    e.tree_do(TreeAction::Reload);
                    e.svc.notify.info(format!("{name} を {new_name} にしました"));
                }
                Err(err) => e.svc.notify.error(format!("名前を変えられません: {err}")),
            }
        });
    }

    /// 開いているバッファのパスを `old` から `new` へ（フォルダなら中のファイルも）
    fn follow_rename(&mut self, old: &Path, new: &Path) {
        let moved = |p: &Path| p.strip_prefix(old).ok().map(|rest| if rest.as_os_str().is_empty() { new.to_path_buf() } else { new.join(rest) });
        let abs = |p: &Path| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
        if let Some(n) = self.core.buf.path.as_deref().map(abs).and_then(|p| moved(&p)) {
            self.core.buf.path = Some(n);
        }
        for s in &mut self.core.docs {
            if let Slot::Parked(d) = s {
                if let Some(n) = d.buf.path.as_deref().map(abs).and_then(|p| moved(&p)) {
                    d.buf.path = Some(n);
                }
            }
        }
    }

    /// 削除…（確かめてから、ごみ箱へ移す）
    pub(crate) fn tree_delete(&mut self) {
        let Some((path, dir)) = self.tree_target() else { return };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let q = format!("{}{name} をごみ箱へ移しますか", if dir { "ディレクトリ " } else { "" });
        self.ask_or(q, move |e| match to_recycle_bin(&path) {
            Ok(()) => {
                e.tree_do(TreeAction::Reload);
                e.svc.notify.info(format!("{name} をごみ箱へ移しました"));
            }
            Err(err) => e.svc.notify.error(format!("{name} を削除できません: {err}")),
        }, |e| e.svc.notify.info("やめました"));
    }

    /// 切り取り（true）・コピー（false）: 貼り付けるまで覚える
    pub(crate) fn tree_clip(&mut self, cut: bool) {
        let Some((path, _)) = self.tree_target() else { return };
        self.svc.notify.info(format!("{} を{}（貼り付けで移す先へ）", path.display(), if cut { "切り取りました" } else { "コピーしました" }));
        self.file_clip = Some((path, cut));
    }

    /// 貼り付け: 選んでいるフォルダへ。同じ名前があれば「名前 (2)」に
    pub(crate) fn tree_paste(&mut self) {
        let (Some((src, cut)), Some(folder)) = (self.file_clip.clone(), self.tree_folder()) else { return };
        let Some(name) = src.file_name() else { return };
        let dst = unique(&folder.join(name));
        if cut && folder.starts_with(&src) {
            self.svc.notify.error("フォルダをその中へは移せません");
            return;
        }
        let r = if cut { std::fs::rename(&src, &dst) } else { copy_all(&src, &dst) };
        match r {
            Ok(()) => {
                if cut {
                    self.follow_rename(&src, &dst);
                    self.file_clip = None;
                }
                self.tree_do(TreeAction::Reload);
                self.svc.notify.info(format!("{} に{}", dst.display(), if cut { "移しました" } else { "コピーしました" }));
            }
            Err(err) => self.svc.notify.error(format!("貼り付けられません: {err}")),
        }
    }

    /// パスをコピー（絶対パス）・参照をコピー（プロジェクトの根からの / 区切り）
    pub(crate) fn tree_copy_path(&mut self, reference: bool) {
        let Some((path, _)) = self.tree_target() else { return };
        let abs = std::path::absolute(&path).unwrap_or(path);
        let text = if reference {
            abs.strip_prefix(self.nav_root()).map_or_else(|_| abs.display().to_string(), |r| r.to_string_lossy().replace('\\', "/"))
        } else {
            abs.display().to_string()
        };
        self.copy_text(text, if reference { "参照" } else { "パス" });
    }

    /// エクスプローラーで表示（そのファイルを選んだ状態で開く）
    pub(crate) fn tree_reveal(&mut self) {
        let Some((path, _)) = self.tree_target() else { return };
        if cfg!(test) {
            return;
        }
        let abs = std::path::absolute(&path).unwrap_or(path);
        if let Err(e) = std::process::Command::new("explorer.exe").arg(format!("/select,{}", abs.display())).spawn() {
            self.svc.notify.error(format!("エクスプローラーを開けません: {e}"));
        }
    }

    /// ターミナルで開く（そのフォルダで新しいシェル）
    pub(crate) fn tree_open_terminal(&mut self) {
        let Some(folder) = self.tree_folder() else { return };
        self.open_terminal_in(&folder);
    }
}

/// 同じ名前があれば「名前 (2).拡張子」…にする
fn unique(p: &Path) -> PathBuf {
    if !p.exists() {
        return p.to_path_buf();
    }
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (2..).map(|n| p.with_file_name(format!("{stem} ({n}){ext}"))).find(|c| !c.exists()).unwrap_or_else(|| p.to_path_buf())
}

/// ファイル・フォルダを中身ごとコピーする
fn copy_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dst)?;
        for e in std::fs::read_dir(src)? {
            let e = e?;
            copy_all(&e.path(), &dst.join(e.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(src, dst).map(|_| ())
    }
}

/// ごみ箱へ移す（Windows のシェルの削除。元に戻せる）
fn to_recycle_bin(path: &Path) -> std::io::Result<()> {
    use windows::Win32::UI::Shell::{FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, SHFILEOPSTRUCTW, SHFileOperationW};
    let abs = std::path::absolute(path)?;
    // 試験ではごみ箱を汚さない
    if cfg!(test) {
        return if abs.is_dir() { std::fs::remove_dir_all(&abs) } else { std::fs::remove_file(&abs) };
    }
    // 2 つの NUL で終わる
    let mut from: Vec<u16> = abs.as_os_str().to_string_lossy().encode_utf16().collect();
    from.extend([0, 0]);
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: windows::core::PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT).0 as u16,
        ..Default::default()
    };
    // SAFETY: from は op より長く生き、2 つの NUL で終わる
    let r = unsafe { SHFileOperationW(&mut op) };
    if r != 0 || op.fAnyOperationsAborted.as_bool() {
        return Err(std::io::Error::other(format!("SHFileOperation が {r} を返しました")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;
    use nagi_vim::Key;

    fn wait_tree(e: &mut Editor, f: impl Fn(&Editor) -> bool) {
        for _ in 0..500 {
            e.poll_background();
            if f(e) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("木が読み終わりません");
    }

    fn select(e: &mut Editor, name: &str) {
        let t = e.panels.tree.as_mut().unwrap();
        t.selected = t.rows.iter().position(|r| r.name == name).unwrap_or_else(|| panic!("{name} が無い"));
    }

    fn type_input(e: &mut Editor, text: &str) {
        assert_eq!(e.state(), State::Input);
        e.panels.input.as_mut().unwrap().field.set(text);
        e.key(Key::Enter);
    }

    /// 新規作成・名前の変更（開いているバッファも付いていく）・コピーと貼り付け・切り取りと貼り付け・削除・パスのコピー
    #[test]
    fn file_operations() {
        let dir = std::env::temp_dir().join(format!("nagi-tree-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        let mut buf = Buffer::open(&dir.join("a.txt")).unwrap();
        buf.path = Some(dir.join("a.txt"));
        let mut e = Editor::new(buf, None);
        e.set_screen(120, 40);
        e.open_tree();
        wait_tree(&mut e, |e| e.panels.tree.as_ref().is_some_and(|t| t.rows.iter().any(|r| r.name == "a.txt")));
        // 新規作成: sub の中にファイル（開く）
        select(&mut e, "sub");
        e.tree_new(false);
        type_input(&mut e, "b.txt");
        assert!(dir.join("sub/b.txt").is_file());
        assert!(e.core.buf.path.as_ref().unwrap().ends_with("b.txt"), "作ったファイルを開く");
        // 名前の変更: 開いているバッファのパスも付いていく
        e.tree_do(TreeAction::Reload);
        wait_tree(&mut e, |e| e.panels.tree.as_ref().is_some_and(|t| t.rows.iter().any(|r| r.name == "a.txt")));
        select(&mut e, "a.txt");
        e.tree_rename();
        type_input(&mut e, "c.txt");
        assert!(dir.join("c.txt").is_file() && !dir.join("a.txt").exists());
        assert!(e.buffer_list().iter().any(|b| b.1.as_ref().is_some_and(|p| p.ends_with("c.txt"))), "{:?}", e.buffer_list());
        // コピー → sub へ貼り付け、同じ所へもう一度貼ると (2)
        wait_tree(&mut e, |e| e.panels.tree.as_ref().is_some_and(|t| t.rows.iter().any(|r| r.name == "c.txt")));
        select(&mut e, "c.txt");
        e.tree_clip(false);
        e.tree_paste();
        assert!(dir.join("c (2).txt").is_file(), "同じフォルダには (2) で");
        select(&mut e, "sub");
        e.tree_paste();
        assert!(dir.join("sub/c.txt").is_file() && dir.join("c.txt").is_file());
        // 切り取り → 貼り付けで移す
        wait_tree(&mut e, |e| e.panels.tree.as_ref().is_some_and(|t| t.rows.iter().any(|r| r.name == "c (2).txt")));
        select(&mut e, "c (2).txt");
        e.tree_clip(true);
        select(&mut e, "sub");
        e.tree_paste();
        assert!(dir.join("sub/c (2).txt").is_file() && !dir.join("c (2).txt").exists());
        // パス・参照のコピー
        thread_local! { static CLIP: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) }; }
        fn set(t: &str) { CLIP.with(|c| *c.borrow_mut() = t.to_string()); }
        fn get() -> Option<String> { CLIP.with(|c| Some(c.borrow().clone())) }
        e.svc.clipboard = Some(crate::editor::Clipboard { set, get });
        select(&mut e, "c.txt");
        e.tree_copy_path(true);
        assert_eq!(get().as_deref(), Some("c.txt"));
        e.tree_copy_path(false);
        assert!(get().unwrap().ends_with("c.txt") && get().unwrap().len() > "c.txt".len());
        // 削除: 確かめてから、ごみ箱へ
        e.tree_delete();
        assert_eq!(e.state(), State::Menu, "確かめる");
        e.key(Key::Char('y'));
        assert!(!dir.join("c.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
