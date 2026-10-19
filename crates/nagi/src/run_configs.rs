//! 実行の構成（IntelliJ の Run Configurations と同じ使い方）。
//!
//! 構成は IntelliJ が保存したものと `.nagi/run.conf` から読む（nagi_workers::runconfig）。動かすと、下の欄のターミナルに
//! 「▶ 名前」の窓を開いてその中で動かす（色付きの出力・入力・Ctrl+C で止める・同時にいくつでも）。同じ構成が動いていれば
//! 止めてから動かし直す。終わった窓は出力を残し、Enter でもう一度動かせる（ターミナルと同じ）。
//!
//! キーは IntelliJ と同じ: Shift+F10 選んでいる構成を実行、Alt+Shift+F10 選んで実行、Ctrl+F2 止める、Ctrl+F5 もう一度。
//! コマンドは `:Run [名前]`・`:RunConfigs`・`:RunStop`・`:RunEdit`（.nagi/run.conf を開く）。


use nagi_workers::runconfig::{self, Kind, Launch, RunConfig};

use crate::editor::{Editor, State};
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
        if v.is_empty() && arg.is_empty() {
            // 構成が無くても、開いているファイルが動かせればそれを動かす（IntelliJ の Ctrl+Shift+F10）
            if let Some(c) = self.current_file_config() {
                self.run_config(c);
                return;
            }
        }
        if v.is_empty() {
            // 構成が無ければ、追加の行だけの一覧を出す
            self.open_run_chooser();
            self.notify.info("実行の構成がありません。「＋ 構成を追加・編集」で足せます（IntelliJ で作った構成も読みます）");
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
        if let Some(c) = self.current_file_config() {
            self.run_list.push(c);
        }
        self.open_finder(crate::finder::Source::RunConfigs);
    }

    /// 開いているファイルをそのまま動かす構成（Python・Node.js・PHP・Ruby・Go）
    fn current_file_config(&self) -> Option<RunConfig> {
        let f = std::path::absolute(self.buf.path.as_ref()?).ok()?;
        runconfig::for_file(&f, &self.nav_root())
    }

    /// 一覧の 1 行（名前・種類・どこの構成か。選んでいるものに ●）。最後に「構成を追加・編集」
    pub(crate) fn run_list_items(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .run_list
            .iter()
            .map(|c| format!("{}{}   {}{}", if Some(&c.name) == self.run_selected.as_ref() { "● " } else { "  " }, c.name, c.kind_label(), match c.from {
                "nagi" => "（.nagi/run.conf）",
                "file" => "（保存しない）",
                _ => "",
            }))
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
            self.notify.info("書き方: :RunAdd 名前 = コマンド（例: :RunAdd 開発サーバー = npm run dev）");
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
                self.notify.info(format!("実行の構成を足しました: {name}（Shift+F10 で実行。{}）", path.display()));
            }
            Err(e) => self.notify.error(format!("{}: {e}", path.display())),
        }
    }

    /// 構成を動かす
    pub(crate) fn run_config(&mut self, cfg: RunConfig) {
        let root = self.nav_root();
        self.run_selected = Some(cfg.name.clone());
        self.run_last = Some(cfg.name.clone());
        match runconfig::launch(&cfg, &root) {
            Err(e) => self.notify.error(e),
            Ok(Launch::Open(url)) => {
                open_url(&url);
                self.notify.info(format!("ブラウザで開きました: {url}"));
            }
            Ok(Launch::Command { cmd, cwd, env }) => {
                // Maven・Gradle で JAVA_HOME が使えなければ、見つけた JDK を使う（:Make と同じ。WSL では渡さない）
                let mut ex = nagi_workers::exec::Exec::in_dir(&cwd).envs(&env);
                if matches!(cfg.kind, Kind::Maven { .. } | Kind::SpringBoot { .. } | Kind::Application { .. } | Kind::Gradle { .. }) {
                    ex.fix_jdk();
                }
                // Maven・JDK が無ければ nagi 用に入れてから動かす
                if !self.fetch_missing_tools(ex.missing_tools(&cmd), crate::settings_dialog::AfterTools::Config(cfg.clone())) {
                    return;
                }
                let shell = match ex.terminal_line(&cfg.name, &cmd) {
                    Ok(s) => s,
                    Err(e) => {
                        self.notify.error(e);
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
                    self.notify.info(format!("実行: {}   {cmd}", cfg.name));
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
        let stopped = target.and_then(|n| self.terminals.list.iter_mut().find(|t| t.name == n)).map(|t| {
            t.term.kill();
            t.name.trim_start_matches(RUN_PREFIX).to_string()
        });
        match stopped {
            Some(name) => self.notify.info(format!("止めました: {name}")),
            None => self.notify.info("動いている構成はありません"),
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
        self.set_state(State::Normal);
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


/// URL を既定のブラウザで開く
pub(crate) fn open_url(url: &str) {
    #[cfg(not(test))]
    {
        let _ = nagi_workers::config::open_url(url);
    }
    let _ = url;
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

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
        e.shortcut("RunAgain");
        assert_eq!(e.terminals.list.len(), 1);
        // 一覧（Alt+Shift+F10）
        e.shortcut("RunConfigs");
        assert_eq!(e.state(), State::Finder);
        assert!(e.run_list_items()[0].contains("あいさつ"));
        // 最後の行は「追加・編集」
        assert!(e.run_list_items().last().unwrap().contains("追加"));
        // 選んで動かした後はエディタのまま
        e.key(nagi_vim::Key::Enter);
        assert_eq!(e.state(), State::Normal);
        // 実行の窓に移っても、Esc でエディタへ戻る
        e.set_state(State::Terminal);
        e.key(nagi_vim::Key::Esc);
        assert_eq!(e.state(), State::Normal);
        // :RunAdd で足すと、一覧に出て選ばれる
        e.run_add("テスト = echo t");
        assert_eq!(e.run_selected.as_deref(), Some("テスト"));
        assert!(std::fs::read_to_string(dir.join(".nagi/run.conf")).unwrap().ends_with("あいさつ = echo nagi-run-ok\nテスト = echo t\n"));
        assert!(e.load_run_configs().iter().any(|c| c.name == "テスト"));
    }

    /// 構成が無くても、開いている Python などのファイルは一覧に出て動かせる
    #[test]
    fn current_file() {
        let dir = std::env::temp_dir().join(format!("nagi-runfile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("app")).unwrap();
        std::fs::write(dir.join("app/main.py"), "print('hi')\n").unwrap();
        let mut e = Editor::new(Buffer::open(&dir.join("app/main.py")).unwrap(), None);
        e.buf.path = Some(dir.join("app/main.py"));
        e.set_screen(120, 40);
        e.open_run_chooser();
        let items = e.run_list_items();
        assert!(items[0].contains("今のファイル: main.py") && items[0].contains("Python"), "{items:?}");
        let c = e.run_list[0].clone();
        let Ok(Launch::Command { cmd, .. }) = runconfig::launch(&c, &dir) else { panic!() };
        assert_eq!(cmd, "python -u app/main.py");
    }
}
