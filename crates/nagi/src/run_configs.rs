//! 実行の構成（IntelliJ の Run Configurations と同じ使い方）。
//!
//! 構成は IntelliJ が保存したものと `.nagi/run.conf` から読む（nagi_workers::runconfig）。動かすと、下の欄のターミナルに
//! 「▶ 名前」の窓を開いてその中で動かす（色付きの出力・入力・Ctrl+C で止める・同時にいくつでも）。同じ構成が動いていれば
//! 止めてから動かし直す。終わった窓は出力を残し、Enter でもう一度動かせる（ターミナルと同じ）。
//!
//! キーは IntelliJ と同じ: Shift+F10 選んでいる構成を実行、Alt+Shift+F10 選んで実行、Ctrl+F2 止める、Ctrl+F5 もう一度。
//! コマンドは `:Run [名前]`・`:RunConfigs`・`:RunStop`・`:RunEdit`（.nagi/run.conf を開く）。

use std::path::Path;

use nagi_workers::runconfig::{self, Kind, Launch, RunConfig};

use crate::editor::{Editor, Mode};
use crate::result_grid::BottomKind;

/// 構成を動かすターミナルの窓の名前の頭
pub const RUN_PREFIX: &str = "▶ ";

/// 一覧の最後の行（選ぶと .nagi/run.conf を開く）
const ADD_ITEM: &str = "＋ 構成を追加・編集…";

/// .nagi/run.conf を初めて作るときの中身
const RUN_CONF_TEMPLATE: &str = "\
# nagi の実行の構成。1 行に 1 つ「名前 = コマンド」。プロジェクトのフォルダで動かす（# から行末は注釈）。
# 保存すれば Shift+F10（選んでいる構成を実行）・Alt+Shift+F10（選んで実行）の一覧に出る。
# IntelliJ で作った構成（.run・.idea）はここに書かなくても読む。1 行で足すなら :RunAdd 名前 = コマンド
#
# 例:
# 開発サーバー = npm run dev
# テスト = mvn -B test
# Spring Boot = mvn -B -pl rest.demo spring-boot:run
# 掃除 = rmdir /s /q target
";

impl Editor {
    /// 今のプロジェクトの構成を読み直す（選んでいる構成が無くなっていれば、IntelliJ で選んでいたもの → 先頭）
    pub(crate) fn load_run_configs(&mut self) -> Vec<RunConfig> {
        let root = self.nav_root();
        let ws = nagi_workers::project::workspace_xmls(&root);
        let (v, sel) = runconfig::load(&root, &ws);
        if !self.run_selected.as_ref().is_some_and(|s| v.iter().any(|c| &c.name == s)) {
            self.run_selected = sel.filter(|s| v.iter().any(|c| &c.name == s)).or_else(|| v.first().map(|c| c.name.clone()));
        }
        v
    }

    /// `:Run [名前]`: 名前が無ければ選んでいる構成（無ければ選ぶ一覧）
    pub(crate) fn run_command(&mut self, arg: &str) {
        let v = self.load_run_configs();
        if v.is_empty() {
            // 構成が無ければ、追加の行だけの一覧を出す
            self.open_run_chooser();
            self.message = "実行の構成がありません。「＋ 構成を追加・編集」で足せます（IntelliJ で作った構成も読みます）".into();
            return;
        }
        let want = if arg.is_empty() { self.run_selected.clone().unwrap_or_default() } else { arg.to_string() };
        let hit = v.iter().find(|c| c.name == want).or_else(|| (!arg.is_empty()).then(|| v.iter().find(|c| c.name.to_lowercase().contains(&arg.to_lowercase()))).flatten());
        match hit {
            Some(c) => self.run_config(c.clone()),
            None => self.open_run_chooser(),
        }
    }

    /// 構成を選ぶ一覧（Alt+Shift+F10）。選ぶと実行する。最後の行は「構成を追加・編集」
    pub(crate) fn open_run_chooser(&mut self) {
        self.run_list = self.load_run_configs();
        self.open_finder(crate::finder::Source::RunConfigs);
    }

    /// 一覧の 1 行（名前・種類・どこの構成か。選んでいるものに ●）。最後に「構成を追加・編集」
    pub(crate) fn run_list_items(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .run_list
            .iter()
            .map(|c| format!("{}{}   {}{}", if Some(&c.name) == self.run_selected.as_ref() { "● " } else { "  " }, c.name, c.kind_label(), if c.from == "nagi" { "（.nagi/run.conf）" } else { "" }))
            .collect();
        v.push(ADD_ITEM.to_string());
        v
    }

    /// 一覧で `i` 番目を選んだ（最後の行なら構成のファイルを開く）
    pub(crate) fn run_list_pick(&mut self, i: usize) {
        match self.run_list.get(i).cloned() {
            Some(c) => self.run_config(c),
            None => self.open_run_dialog(None),
        }
    }

    /// `:RunAdd 名前 = コマンド`: nagi の構成に 1 行足して選ぶ
    pub(crate) fn run_add(&mut self, arg: &str) {
        let Some((name, cmd)) = arg.split_once('=').map(|(n, c)| (n.trim(), c.trim())).filter(|(n, c)| !n.is_empty() && !c.is_empty()) else {
            self.message = "書き方: :RunAdd 名前 = コマンド（例: :RunAdd 開発サーバー = npm run dev）".into();
            return;
        };
        let path = self.nav_root().join(".nagi").join("run.conf");
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let mut text = std::fs::read_to_string(&path).unwrap_or_else(|_| RUN_CONF_TEMPLATE.to_string());
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!("{name} = {cmd}\n"));
        match std::fs::write(&path, text) {
            Ok(()) => {
                self.run_selected = Some(name.to_string());
                self.message = format!("実行の構成を足しました: {name}（Shift+F10 で実行。{}）", path.display());
            }
            Err(e) => self.message = format!("{}: {e}", path.display()),
        }
    }

    /// 構成を動かす
    pub(crate) fn run_config(&mut self, cfg: RunConfig) {
        let root = self.nav_root();
        self.run_selected = Some(cfg.name.clone());
        self.run_last = Some(cfg.name.clone());
        match runconfig::launch(&cfg, &root) {
            Err(e) => self.message = e,
            Ok(Launch::Open(url)) => {
                open_url(&url);
                self.message = format!("ブラウザで開きました: {url}");
            }
            Ok(Launch::Command { cmd, cwd, mut env }) => {
                // Maven・Gradle で JAVA_HOME が使えなければ、見つけた JDK を使う（:Make と同じ）
                if matches!(cfg.kind, Kind::Maven { .. } | Kind::SpringBoot { .. } | Kind::Application { .. } | Kind::Gradle { .. }) && !env.iter().any(|(k, _)| k == "JAVA_HOME") {
                    if let Some(j) = nagi_workers::jobs::java_home_fix() {
                        env.push(("JAVA_HOME".into(), j.display().to_string()));
                    }
                }
                let shell = match shell_line(&cfg.name, &cmd, &cwd, &env) {
                    Ok(s) => s,
                    Err(e) => {
                        self.message = e;
                        return;
                    }
                };
                let name = format!("{RUN_PREFIX}{}", cfg.name);
                // 同じ構成が動いていれば止めて、同じ場所で動かし直す
                self.close_terminal_named(&name);
                self.bottom_hidden = false;
                self.bottom_kind = BottomKind::Terminal;
                self.rows = self.text_rows(self.active_rect());
                if self.new_terminal(&name, &shell) {
                    self.message = format!("実行: {}   {cmd}", cfg.name);
                }
            }
        }
    }

    /// Ctrl+F5: 最後に動かした構成をもう一度
    pub(crate) fn run_again(&mut self) {
        let name = self.run_last.clone().or_else(|| self.run_selected.clone()).unwrap_or_default();
        self.run_command(&name);
    }

    /// Ctrl+F2・`:RunStop`: 構成の窓を止める（ターミナルで構成の窓を出していればそれ、無ければ最後に動かしたもの）。出力は残す
    pub(crate) fn run_stop(&mut self) {
        let active = self.terminals.list.get(self.terminals.active).filter(|t| t.name.starts_with(RUN_PREFIX) && !t.exited).map(|t| t.name.clone());
        let last = self.run_last.as_ref().map(|n| format!("{RUN_PREFIX}{n}"));
        let target = active.or(last.filter(|n| self.terminals.list.iter().any(|t| &t.name == n && !t.exited)));
        match target.and_then(|n| self.terminals.list.iter_mut().find(|t| t.name == n)) {
            Some(t) => {
                t.term.kill();
                self.message = format!("止めました: {}", t.name.trim_start_matches(RUN_PREFIX));
            }
            None => self.message = "動いている構成はありません".into(),
        }
    }

    /// `:RunEdit`: nagi 独自の構成のファイル（.nagi/run.conf）を開く。無ければ書き方の例を入れて作る
    pub(crate) fn run_edit(&mut self) {
        let path = self.nav_root().join(".nagi").join("run.conf");
        if !path.exists() {
            if let Some(d) = path.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(&path, RUN_CONF_TEMPLATE);
        }
        self.edit_file(path);
        self.mode = Mode::Normal;
    }

    /// 名前の窓を閉じる（動いていれば止める）
    fn close_terminal_named(&mut self, name: &str) {
        if let Some(i) = self.terminals.list.iter().position(|t| t.name == name) {
            let mut t = self.terminals.list.remove(i);
            t.term.kill();
            if self.terminals.active > i || self.terminals.active >= self.terminals.list.len() {
                self.terminals.active = self.terminals.active.saturating_sub(1);
            }
        }
    }
}

/// ターミナルの窓で動かすコマンドの行。Windows は作業フォルダと環境変数も書いた .cmd を作って `cmd.exe /C` で、
/// WSL は `wsl.exe -d <名前> --cd <フォルダ> sh -lc "…"` で
fn shell_line(name: &str, cmd: &str, cwd: &Path, env: &[(String, String)]) -> Result<String, String> {
    if let Some((distro, linux)) = nagi_workers::wsl::split_unc(cwd) {
        let q = |v: &str| format!("'{}'", v.replace('\'', "'\\''"));
        let exports: String = env.iter().filter(|(k, _)| k != "JAVA_HOME").map(|(k, v)| format!("export {k}={}; ", q(v))).collect();
        let line = format!("{exports}{cmd}");
        return Ok(format!("wsl.exe -d {distro} --cd {} sh -lc \"{}\"", linux, line.replace('"', "\\\"")));
    }
    let dir = std::env::temp_dir().join("nagi-run");
    std::fs::create_dir_all(&dir).map_err(|e| format!("一時フォルダを作れません: {e}"))?;
    let file = dir.join(format!("run-{:016x}.cmd", hash(name)));
    let non_ascii = !cmd.is_ascii() || !cwd.to_string_lossy().is_ascii() || env.iter().any(|(k, v)| !k.is_ascii() || !v.is_ascii());
    let mut text = String::from("@echo off\r\n");
    // 日本語を含むときは UTF-8 で書いて、コードページも UTF-8 に
    if non_ascii {
        text.push_str("chcp 65001 >nul\r\n");
    }
    text.push_str(&format!("cd /d \"{}\"\r\n", cwd.display()));
    for (k, v) in env {
        text.push_str(&format!("set \"{k}={v}\"\r\n"));
    }
    text.push_str(cmd);
    text.push_str("\r\n");
    std::fs::write(&file, text).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(format!("cmd.exe /C \"{}\"", file.display()))
}

fn hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// URL を既定のブラウザで開く
fn open_url(url: &str) {
    #[cfg(not(test))]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("rundll32").arg("url.dll,FileProtocolHandler").arg(url).creation_flags(0x0800_0000).spawn();
    }
    let _ = url;
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    #[test]
    fn shell_lines() {
        let s = shell_line("サーバー", "npm run dev", Path::new(r"C:\work\shop"), &[("TZ".into(), "Asia/Tokyo".into())]).unwrap();
        assert!(s.starts_with("cmd.exe /C \"") && s.ends_with(".cmd\""), "{s}");
        let file = nagi_workers::term::split_args(&s)[2].clone();
        let text = std::fs::read_to_string(file).unwrap();
        assert_eq!(text, "@echo off\r\ncd /d \"C:\\work\\shop\"\r\nset \"TZ=Asia/Tokyo\"\r\nnpm run dev\r\n");
        let w = shell_line("x", "echo \"a\"", Path::new(r"\\wsl.localhost\kali-linux\home\me\os"), &[("A".into(), "it's".into())]).unwrap();
        assert_eq!(nagi_workers::term::split_args(&w), ["wsl.exe", "-d", "kali-linux", "--cd", "/home/me/os", "sh", "-lc", "export A='it'\\''s'; echo \"a\""]);
    }

    /// 構成を選んで動かすと、ターミナルの欄に「▶ 名前」の窓ができ、もう一度動かすと同じ窓を作り直す
    #[test]
    fn runs_in_terminal() {
        let dir = std::env::temp_dir().join(format!("nagi-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join(".nagi")).unwrap();
        std::fs::write(dir.join(".nagi/run.conf"), "あいさつ = echo nagi-run-ok\n").unwrap();
        std::fs::write(dir.join("a.txt"), "x\n").unwrap();
        let mut e = Editor::new(Buffer::open(&dir.join("a.txt")).unwrap(), None);
        e.buf.path = Some(dir.join("a.txt"));
        e.set_screen(120, 40);
        e.run_command("");
        assert_eq!(e.run_selected.as_deref(), Some("あいさつ"));
        assert_eq!(e.bottom_kind, BottomKind::Terminal);
        assert_eq!(e.terminals.list.len(), 1);
        assert_eq!(e.terminals.list[0].name, "▶ あいさつ");
        let mut ok = false;
        for _ in 0..300 {
            e.sync_terminal();
            if e.terminals.list[0].term.with_screen(|s| s.contents()).contains("nagi-run-ok") {
                ok = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(ok, "出力が出ない");
        // もう一度（Ctrl+F5）: 窓は増えない
        e.key(nagi_vim::Key::F(43));
        assert_eq!(e.terminals.list.len(), 1);
        // 一覧（Alt+Shift+F10）
        e.key(nagi_vim::Key::F(41));
        assert_eq!(e.mode, Mode::Finder);
        assert!(e.run_list_items()[0].contains("あいさつ"));
        // 最後の行は「追加・編集」
        assert!(e.run_list_items().last().unwrap().contains("追加"));
        // 選んで動かした後はエディタのまま
        e.key(nagi_vim::Key::Enter);
        assert_eq!(e.mode, Mode::Normal);
        // 実行の窓に移っても、Esc でエディタへ戻る
        e.mode = Mode::Terminal;
        e.key(nagi_vim::Key::Esc);
        assert_eq!(e.mode, Mode::Normal);
        // :RunAdd で足すと、一覧に出て選ばれる
        e.run_add("テスト = echo t");
        assert_eq!(e.run_selected.as_deref(), Some("テスト"));
        assert!(std::fs::read_to_string(dir.join(".nagi/run.conf")).unwrap().ends_with("あいさつ = echo nagi-run-ok\nテスト = echo t\n"));
        assert!(e.load_run_configs().iter().any(|c| c.name == "テスト"));
    }
}
