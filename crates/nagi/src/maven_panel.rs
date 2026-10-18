//! 右の欄の「Maven」タブ（IntelliJ の Maven の画面と同じ形）。
//!
//! プロジェクトの木: プロジェクト ▸ ライフサイクル（clean … deploy）・依存・モジュール（入れ子のプロジェクト）。
//! ゴールの上で Enter（ダブルクリック）を押すと、そのモジュールの pom.xml を指定して動かし（根に mvnw があればそれ）、
//! 出力とコンパイルエラーを下のパネルに出す（`:Make` と同じ。エラーの行から飛べる）。

use std::collections::HashSet;
use std::path::PathBuf;

use nagi_vim::Key;
use nagi_workers::maven::{LIFECYCLE, Pom};

use crate::editor::{Editor, Effect, Mode};
use crate::right_panel::RightTab;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// 開け閉めできる行（キー）
    Folder(String),
    /// ゴール（pom.xml のあるフォルダ, ゴール）
    Goal(PathBuf, &'static str),
    Leaf,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub depth: usize,
    pub label: String,
    /// 右に薄く出す補足（packaging、scope）
    pub note: String,
    pub kind: RowKind,
}

pub struct MavenPanel {
    pub root: PathBuf,
    pub pom: Option<Pom>,
    pub expanded: HashSet<String>,
    pub rows: Vec<Row>,
    pub selected: usize,
    pub scroll: usize,
}

impl MavenPanel {
    fn rebuild(&mut self) {
        let mut rows = Vec::new();
        if let Some(p) = &self.pom {
            push_project(p, 0, &self.expanded, &mut rows);
        }
        self.rows = rows;
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }

    pub fn is_open(&self, key: &str) -> bool {
        self.expanded.contains(key)
    }
}

fn push_project(p: &Pom, depth: usize, open: &HashSet<String>, rows: &mut Vec<Row>) {
    let key = format!("p:{}", p.dir.display());
    rows.push(Row { depth, label: p.label().to_string(), note: p.packaging.clone(), kind: RowKind::Folder(key.clone()) });
    if !open.contains(&key) {
        return;
    }
    let lk = format!("l:{}", p.dir.display());
    rows.push(Row { depth: depth + 1, label: "ライフサイクル".into(), note: String::new(), kind: RowKind::Folder(lk.clone()) });
    if open.contains(&lk) {
        for g in LIFECYCLE {
            rows.push(Row { depth: depth + 2, label: g.into(), note: String::new(), kind: RowKind::Goal(p.dir.clone(), g) });
        }
    }
    if !p.deps.is_empty() {
        let dk = format!("d:{}", p.dir.display());
        rows.push(Row { depth: depth + 1, label: "依存".into(), note: format!("{}", p.deps.len()), kind: RowKind::Folder(dk.clone()) });
        if open.contains(&dk) {
            for d in &p.deps {
                let v = if d.version.is_empty() { String::new() } else { format!(":{}", d.version) };
                rows.push(Row { depth: depth + 2, label: format!("{}:{}{v}", d.group, d.artifact), note: d.scope.clone(), kind: RowKind::Leaf });
            }
        }
    }
    for c in &p.children {
        push_project(c, depth + 1, open, rows);
    }
}

impl Editor {
    /// `:Maven`（ActivateMavenToolWindow）: Maven タブを出して移る
    pub(crate) fn open_maven(&mut self) {
        if self.maven.is_none() {
            let from = self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok()).unwrap_or_else(|| self.nav_root());
            let Some(root) = nagi_workers::maven::find_root(&from).or_else(|| nagi_workers::maven::find_root(&self.nav_root())) else {
                self.message = "pom.xml が見つかりません（Maven のプロジェクトではありません）".into();
                return;
            };
            let mut m = MavenPanel { root, pom: None, expanded: HashSet::new(), rows: Vec::new(), selected: 0, scroll: 0 };
            m.pom = nagi_workers::maven::load(&m.root);
            if let Some(p) = &m.pom {
                // 根のプロジェクトとそのライフサイクルは開いておく
                m.expanded.insert(format!("p:{}", p.dir.display()));
                m.expanded.insert(format!("l:{}", p.dir.display()));
            }
            m.rebuild();
            self.maven = Some(m);
        }
        self.focus_right(RightTab::Maven);
    }

    /// 読み直す（r）
    fn reload_maven(&mut self) {
        if let Some(m) = &mut self.maven {
            m.pom = nagi_workers::maven::load(&m.root);
            m.rebuild();
            self.message = "pom.xml を読み直しました".into();
        }
    }

    /// `:Maven ゴール…`: 根のプロジェクトで動かす
    pub(crate) fn maven_run(&mut self, goals: &str) {
        let root = match &self.maven {
            Some(m) => m.root.clone(),
            None => {
                let from = self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok()).unwrap_or_else(|| self.nav_root());
                match nagi_workers::maven::find_root(&from) {
                    Some(r) => r,
                    None => {
                        self.message = "pom.xml が見つかりません".into();
                        return;
                    }
                }
            }
        };
        let cmd = nagi_workers::maven::command(&root, &root, goals);
        self.run_job(&cmd, &root, &format!("Maven: {goals}"), true);
    }

    fn maven_activate(&mut self) {
        let Some(m) = &mut self.maven else { return };
        let Some(row) = m.rows.get(m.selected).cloned() else { return };
        match row.kind {
            RowKind::Folder(k) => {
                if !m.expanded.remove(&k) {
                    m.expanded.insert(k);
                }
                m.rebuild();
            }
            RowKind::Goal(dir, goal) => {
                let root = m.root.clone();
                let cmd = nagi_workers::maven::command(&root, &dir, goal);
                let name = dir.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
                self.run_job(&cmd, &root, &format!("Maven: {goal}（{name}）"), true);
                // 右の欄にいたまま続けて操作できるように
                self.mode = Mode::List;
                self.list_side = crate::editor::Side::Right;
            }
            RowKind::Leaf => {}
        }
    }

    pub(crate) fn maven_key(&mut self, key: Key) -> Effect {
        let half = (self.windows_rows() / 2).max(1);
        let Some(m) = &mut self.maven else {
            self.mode = Mode::Normal;
            return Effect::None;
        };
        let moved = crate::list::step(key, m.selected, m.rows.len(), half * 2);
        match key {
            _ if moved.is_some() => m.selected = moved.unwrap_or(m.selected),
            Key::Char('g') => m.selected = 0,
            Key::Enter | Key::Char('o') => self.maven_activate(),
            // l で開く・h で閉じる（閉じていれば親へ）
            Key::Char('l') | Key::Right => {
                if let Some(RowKind::Folder(k)) = m.rows.get(m.selected).map(|r| r.kind.clone()) {
                    m.expanded.insert(k);
                    m.rebuild();
                }
            }
            Key::Char('h') | Key::Left => {
                let cur = m.rows.get(m.selected).cloned();
                match cur {
                    Some(Row { kind: RowKind::Folder(k), .. }) if m.expanded.contains(&k) => {
                        m.expanded.remove(&k);
                        m.rebuild();
                    }
                    Some(r) => {
                        if let Some(p) = m.rows[..m.selected].iter().rposition(|x| x.depth < r.depth) {
                            m.selected = p;
                        }
                    }
                    None => {}
                }
            }
            Key::Char('r') => self.reload_maven(),
            Key::Char('q') => {
                self.maven = None;
                self.mode = Mode::Normal;
                self.rows = self.text_rows(self.active_rect());
            }
            Key::Esc => self.mode = Mode::Normal,
            Key::Char(':') => {
                self.enter_command("");
            }
            _ => {}
        }
        Effect::None
    }

    pub(crate) fn maven_click(&mut self, row: usize, clicks: u32) {
        let Some(m) = &mut self.maven else { return };
        let i = m.scroll + row - 1;
        if i >= m.rows.len() {
            return;
        }
        m.selected = i;
        self.focus_right(RightTab::Maven);
        let folder = matches!(self.maven.as_ref().and_then(|m| m.rows.get(i)).map(|r| &r.kind), Some(RowKind::Folder(_)));
        // フォルダは 1 回、ゴールはダブルクリックで
        if folder || clicks >= 2 {
            self.maven_activate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    #[test]
    fn tree_and_goal() {
        let dir = std::env::temp_dir().join(format!("nagi-maven-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("core/src")).unwrap();
        std::fs::write(dir.join("pom.xml"), "<project><artifactId>top</artifactId><packaging>pom</packaging><modules><module>core</module></modules></project>").unwrap();
        std::fs::write(
            dir.join("core/pom.xml"),
            "<project><artifactId>core</artifactId><dependencies><dependency><groupId>g</groupId><artifactId>a</artifactId><scope>test</scope></dependency></dependencies></project>",
        )
        .unwrap();
        std::fs::write(dir.join("core/src/A.java"), "class A {}\n").unwrap();
        let mut buf = Buffer::open(&dir.join("core/src/A.java")).unwrap();
        buf.path = Some(dir.join("core/src/A.java"));
        let mut e = Editor::new(buf, None);
        e.set_screen(120, 40);
        e.open_maven();
        let labels = |e: &Editor| e.maven.as_ref().unwrap().rows.iter().map(|r| r.label.clone()).collect::<Vec<_>>();
        // 根は開き、ライフサイクルも開いている。モジュールは閉じている
        let l = labels(&e);
        assert_eq!(&l[..3], &["top", "ライフサイクル", "clean"]);
        assert_eq!(l.last().unwrap(), "core");
        assert_eq!(e.right_visible(), Some(RightTab::Maven));
        assert!(e.windows_x().1 < 120 - 40, "右の欄の分だけ窓が狭まる");
        // core を開くと、ライフサイクルと依存
        for k in nagi_vim::parse_keys("G<CR>", " ") {
            e.key(k);
        }
        let l = labels(&e);
        assert_eq!(&l[l.len() - 3..], &["core", "ライフサイクル", "依存"]);
        // 依存を開く
        for k in nagi_vim::parse_keys("Gl", " ") {
            e.key(k);
        }
        assert_eq!(labels(&e).last().unwrap(), "g:a");
        assert_eq!(e.maven.as_ref().unwrap().rows.last().unwrap().note, "test");
        // コマンドの形（mvnw が無ければ mvn）
        assert_eq!(nagi_workers::maven::command(&dir, &dir.join("core"), "compile"), format!("mvn -B -f \"{}\" compile", dir.join("core").join("pom.xml").display()));
    }
}
