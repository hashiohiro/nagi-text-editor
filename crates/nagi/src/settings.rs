//! 設定（IntelliJ の Settings と同じ考え方）。
//!
//! 項目は `SCHEMA` に 1 か所で決める（キー・分類・名前・種類・既定値・説明）。値は文字で持ち、読むときに型に直す。
//! 保存先は `%LOCALAPPDATA%\nagi\settings.conf`（全体）と、プロジェクトの `.nagi\settings.conf`（上書き。プロジェクトごとに
//! 変えたい項目だけ）。どちらも `キー = 値` の 1 行に 1 つ（`#` で始まる行は注釈）。既定値のままの項目は書かない。
//! 試験では全体のファイルを読み書きしない（使っている人の設定を変えないため）。

use nagi_core::Context;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 項目の種類
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SKind {
    Bool,
    /// 整数（下限, 上限）
    Int(i64, i64),
    Text,
    /// 選択肢（値, 画面に出す名前）
    Choice(&'static [(&'static str, &'static str)]),
    /// キーの組み合わせ（Ctrl+Alt+S など）
    Key,
    /// フォントの名前（Windows に入っているフォントから選ぶ）
    Font,
    /// JDK のフォルダ（入っているものから選ぶか、ダウンロードする）
    Jdk,
    /// 言語のツールの bin のフォルダ（nagi_workers::toolchains の種類。入っているものから選ぶか、ダウンロードする）
    Tool(&'static str),
}

const TOOL_HELP: &str = "Enter で入っているものから選ぶか、版を選んでダウンロード。空なら PATH のもの。WSL のプロジェクトでは WSL の中のものを使う";

/// 項目 1 つ
pub struct Item {
    pub key: &'static str,
    /// 分類（画面の左の一覧）
    pub cat: &'static str,
    pub label: &'static str,
    pub kind: SKind,
    pub default: &'static str,
    pub help: &'static str,
    /// プロジェクトの .nagi\settings.conf で上書きできるか
    pub project: bool,
}

const fn item(key: &'static str, cat: &'static str, label: &'static str, kind: SKind, default: &'static str, help: &'static str) -> Item {
    Item { key, cat, label, kind, default, help, project: false }
}

const fn pitem(key: &'static str, cat: &'static str, label: &'static str, kind: SKind, default: &'static str, help: &'static str) -> Item {
    Item { key, cat, label, kind, default, help, project: true }
}

const B: SKind = SKind::Bool;
const T: SKind = SKind::Text;
const K: SKind = SKind::Key;

/// 分類（画面の左の一覧の並び）
pub const CATEGORIES: [&str; 10] = ["外観", "エディタ", "Vim", "入力補完", "キーマップ", "ビルド・実行", "ツール", "プロジェクト", "その他", "（すべて）"];

pub static SCHEMA: &[Item] = &[
    // 外観
    item("appearance.font", "外観", "フォント（英数字）", SKind::Font, "Cascadia Mono", "Enter で一覧から選ぶ（等幅のものを先に）"),
    item("appearance.font_ja", "外観", "フォント（日本語）", SKind::Font, "BIZ UDGothic", "英数字のフォントに無い字に使う。Enter で一覧から選ぶ"),
    item("appearance.font_size", "外観", "文字の大きさ（pt）", SKind::Int(0, 48), "0", "0 なら既定"),
    item("appearance.line_height", "外観", "行の高さ（%）", SKind::Int(80, 200), "100", "文字の高さに対する行の高さ"),
    item("appearance.theme", "外観", "配色", SKind::Choice(&[("dark", "ダーク（既定）"), ("black", "ブラック"), ("dim", "ダークグレー"), ("light", "ライト")]), "dark", ""),
    item("appearance.accent", "外観", "強調の色", T, "#87afd7", "選んでいるタブの下線・ボタンなど（#rrggbb）"),
    item("appearance.remember_panels", "外観", "欄の大きさを覚える", B, "true", "ドラッグで変えた左・右・下の欄の大きさを次の起動でも使う"),
    item("appearance.tab_names", "外観", "欄のタブの名前", SKind::Choice(&[("hover", "マウスを載せたとき"), ("always", "いつも（アイコンの横）")]), "hover", ""),
    item("appearance.status_run", "外観", "状態の行: 実行の構成", B, "true", ""),
    item("appearance.status_branch", "外観", "状態の行: ブランチ", B, "true", ""),
    item("appearance.status_path", "外観", "状態の行: ファイルのパス", B, "true", ""),
    item("appearance.status_lang", "外観", "状態の行: 言語", B, "true", ""),
    // エディタ
    item("editor.line_numbers", "エディタ", "行番号", SKind::Choice(&[("absolute", "絶対"), ("relative", "相対（今の行は絶対）"), ("off", "出さない")]), "absolute", "Vim の number / relativenumber"),
    item("editor.wrap", "エディタ", "折り返す", B, "false", "画面の幅で行を折り返して表示する"),
    pitem("editor.indent_style", "エディタ", "インデント", SKind::Choice(&[("spaces", "空白"), ("tabs", "タブ")]), "spaces", "Tab キー・>>・自動インデントで入れるもの"),
    pitem("editor.indent_width", "エディタ", "インデントの幅", SKind::Int(1, 16), "4", ""),
    pitem("editor.indent_overrides", "エディタ", "言語ごとのインデントの幅", T, "js=2, ts=2, json=2, yaml=2, html=2, css=2, scss=2", "拡張子=幅 をカンマで区切る"),
    pitem("editor.trim_trailing", "エディタ", "保存時に行末の空白を消す", B, "false", ""),
    pitem("editor.final_newline", "エディタ", "保存時に最後に改行を入れる", B, "false", ""),
    pitem("editor.new_newline", "エディタ", "新しいファイルの改行", SKind::Choice(&[("lf", "LF"), ("crlf", "CRLF")]), "lf", ""),
    pitem("editor.new_bom", "エディタ", "新しいファイルに BOM を付ける", B, "false", ""),
    item("editor.grammar_download", "エディタ", "文法を自動でダウンロード", B, "true", "C・Go・C# など 21 言語の文法（色分け・定義へ移動）は、初めて開いたときに GitHub から取ってくる（1 言語 60〜510KB）。オフなら :GrammarInstall で"),
    item("editor.autoread", "エディタ", "外部での変更", SKind::Choice(&[("auto", "自動で読み直す（変えていなければ）"), ("ask", "知らせるだけ"), ("off", "何もしない")]), "auto", ""),
    item("editor.scroll_ms", "エディタ", "スムーズスクロールの時間（ms）", SKind::Int(0, 1000), "110", "0 ならスクロールを滑らかにしない"),
    item("editor.repeat_delay", "エディタ", "押しっぱなし: 繰り返すまで（ms）", SKind::Int(50, 1000), "250", "hjkl などの移動のキー"),
    item("editor.repeat_interval", "エディタ", "押しっぱなし: 間隔（ms）", SKind::Int(5, 200), "25", ""),
    item("editor.repeat_fast", "エディタ", "押しっぱなし: 速めた間隔（ms）", SKind::Int(5, 200), "16", "1.2 秒押し続けた後"),
    item("editor.squiggles", "エディタ", "問題を波線で出す", B, "true", "エラーは赤、警告は黄色"),
    // Vim
    item("vim.rc_path", "Vim", "設定ファイル", T, "", "空なら ~\\.ideavimrc（無ければ ~\\_vimrc・~\\.vimrc）"),
    item("vim.ignorecase", "Vim", "検索で大文字小文字を区別しない", B, "true", "ignorecase"),
    item("vim.smartcase", "Vim", "大文字を含めば区別する", B, "true", "smartcase"),
    item("vim.hlsearch", "Vim", "検索の一致を強調する", B, "true", "hlsearch"),
    item("vim.incsearch", "Vim", "打ちながら検索する", B, "true", "incsearch"),
    item("vim.clipboard", "Vim", "クリップボード", SKind::Choice(&[("unnamed", "ヤンク・削除をいつもクリップボードへ"), ("explicit", "\"+ と \"* のときだけ")]), "unnamed", "clipboard=unnamed"),
    // 入力補完
    item("completion.auto", "入力補完", "打っている間に自動で出す", B, "false", "オフなら Ctrl+Space で出す"),
    item("completion.delay_ms", "入力補完", "自動で出すまで（ms）", SKind::Int(0, 2000), "300", ""),
    item("completion.min_chars", "入力補完", "自動で出す文字数", SKind::Int(1, 10), "2", ". の後はすぐ出す"),
    item("completion.case", "入力補完", "大文字小文字", SKind::Choice(&[("first", "最初の文字だけ区別"), ("none", "区別しない"), ("all", "区別する")]), "none", "IntelliJ の Match case"),
    item("completion.keywords", "入力補完", "予約語を出す", B, "true", ""),
    item("completion.words", "入力補完", "ファイルの中の語を出す", B, "true", ""),
    // キーマップ
    item("keymap.settings", "キーマップ", "設定", K, "Ctrl+Alt+S", ""),
    item("keymap.complete", "キーマップ", "入力補完", K, "Ctrl+Space", ""),
    item("keymap.run", "キーマップ", "実行", K, "Shift+F10", ""),
    item("keymap.run_choose", "キーマップ", "選んで実行", K, "Alt+Shift+F10", ""),
    item("keymap.run_stop", "キーマップ", "止める", K, "Ctrl+F2", ""),
    item("keymap.run_again", "キーマップ", "もう一度実行", K, "Ctrl+F5", ""),
    item("keymap.goto_def", "キーマップ", "定義へ", K, "F12", ""),
    item("keymap.terminal", "キーマップ", "ターミナル", K, "Alt+F12", ""),
    item("keymap.project", "キーマップ", "プロジェクト", K, "Alt+1", ""),
    item("keymap.commit", "キーマップ", "コミット", K, "Alt+0", ""),
    item("keymap.git", "キーマップ", "Git", K, "Alt+9", ""),
    item("keymap.find", "キーマップ", "検索の欄", K, "Alt+3", ""),
    item("keymap.problems", "キーマップ", "問題", K, "Alt+6", ""),
    item("keymap.log", "キーマップ", "ログ", K, "Alt+4", ""),
    // ツール
    item("terminal.shell", "ツール", "ターミナル: 既定のシェル", T, "", "空なら最後に選んだもの（cmd・powershell・pwsh・bash・wsl、またはコマンド）"),
    item("terminal.start_dir", "ツール", "ターミナル: 始めるフォルダ", SKind::Choice(&[("project", "プロジェクト"), ("file", "開いているファイルのフォルダ"), ("home", "ホーム")]), "project", ""),
    item("terminal.utf8", "ツール", "ターミナル: UTF-8 で始める", B, "true", "オフなら Windows の既定（CP932）"),
    pitem("maven.jdk", "ビルド・実行", "JDK（コンパイル・実行）", SKind::Jdk, "", "Enter で入っているものから選ぶか、Eclipse Temurin をダウンロード（~\\.jdks に置く）。空なら JAVA_HOME か ~\\.jdks から探す"),
    item("build.jdk_env", "ビルド・実行", "JDK を nagi から起動するものに渡す", B, "true", "ターミナル・実行の構成・Maven に JAVA_HOME と PATH（JDK の bin）として"),
    pitem("maven.path", "ビルド・実行", "Maven: mvn の場所", T, "", "空なら PATH の mvn"),
    pitem("maven.use_wrapper", "ビルド・実行", "Maven: Wrapper（mvnw）を使う", B, "true", "設定ファイルもそろっているときだけ"),
    pitem("maven.options", "ビルド・実行", "Maven: 追加のオプション", T, "", "いつも付ける（-o・-T 4 など）"),
    pitem("toolchain.python", "ビルド・実行", "Python", SKind::Tool("python"), "", TOOL_HELP),
    pitem("toolchain.node", "ビルド・実行", "Node.js", SKind::Tool("node"), "", TOOL_HELP),
    pitem("toolchain.go", "ビルド・実行", "Go", SKind::Tool("go"), "", TOOL_HELP),
    pitem("toolchain.rust", "ビルド・実行", "Rust（cargo）", SKind::Tool("rust"), "", TOOL_HELP),
    pitem("toolchain.dotnet", "ビルド・実行", ".NET SDK", SKind::Tool("dotnet"), "", TOOL_HELP),
    pitem("toolchain.clang", "ビルド・実行", "C/C++（clang）", SKind::Tool("clang"), "", TOOL_HELP),
    pitem("toolchain.cmake", "ビルド・実行", "CMake", SKind::Tool("cmake"), "", TOOL_HELP),
    pitem("toolchain.zig", "ビルド・実行", "Zig", SKind::Tool("zig"), "", TOOL_HELP),
    pitem("toolchain.dart", "ビルド・実行", "Dart", SKind::Tool("dart"), "", TOOL_HELP),
    pitem("toolchain.php", "ビルド・実行", "PHP", SKind::Tool("php"), "", TOOL_HELP),

    item("tools.extra_path", "ツール", "追加の PATH", T, "", "gradle・npm・cargo などの場所（; で区切る）"),
    item("git.path", "ツール", "Git: git の場所", T, "", "空なら PATH の git"),
    item("git.wsl", "ツール", "Git: WSL のプロジェクトは WSL の git", B, "true", ""),
    item("db.drivers_dir", "ツール", "データベース: ドライバーの置き場所", T, "", "空なら %LOCALAPPDATA%\\nagi\\drivers"),
    item("tools.browser", "ツール", "ブラウザ", T, "", "空なら既定のブラウザ（実行の構成の URL・Markdown のリンク）"),
    // プロジェクト
    pitem("project.exclude", "プロジェクト", "除外するフォルダ", T, "", "名前をカンマで区切る（検索・ツリー・索引から外す）"),
    pitem("project.index", "プロジェクト", "定義の索引を作る", B, "true", "定義へ移動・入力補完に使う"),
    item("project.startup", "プロジェクト", "起動時（引数なし）", SKind::Choice(&[("launcher", "ランチャー"), ("last", "前回のプロジェクト"), ("empty", "空のバッファ")]), "launcher", ""),
    item("project.recent_ide", "プロジェクト", "最近のプロジェクトに IntelliJ などの履歴を混ぜる", B, "true", ""),
    // その他
    item("other.log_max_kb", "その他", "ログ（nagi.log）の上限（KB）", SKind::Int(16, 102400), "1024", "超えたら古いものを消す"),
    item("other.jumplist_count", "その他", "ジャンプリストの件数", SKind::Int(0, 30), "10", "タスクバーの右クリックの最近のプロジェクト"),
];

pub fn schema(key: &str) -> Option<&'static Item> {
    SCHEMA.iter().find(|i| i.key == key)
}

/// 設定の値（キー → 文字。既定値と違うものだけ）
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    /// 全体の設定
    pub global: HashMap<String, String>,
    /// プロジェクトの上書き
    pub project: HashMap<String, String>,
    /// プロジェクトの根（上書きの読み書きの場所）
    pub root: Option<PathBuf>,
}

/// 全体の設定のファイル
pub fn global_path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("nagi").join("settings.conf"))
}

pub fn project_path(root: &Path) -> PathBuf {
    root.join(".nagi").join("settings.conf")
}

/// `キー = 値` の行を読む（知らないキーも残す）
pub fn parse(s: &str) -> HashMap<String, String> {
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .filter(|(k, _)| !k.is_empty())
        .collect()
}

/// 書き出す（SCHEMA の順。既定値と同じものは書かない）
pub fn render(values: &HashMap<String, String>, header: &str) -> String {
    let mut s = format!("# {header}（nagi の設定の画面で直せる。キー = 値）\n");
    let mut cat = "";
    for it in SCHEMA {
        if let Some(v) = values.get(it.key).filter(|v| v.as_str() != it.default) {
            if it.cat != cat {
                s.push_str(&format!("\n# {}\n", it.cat));
                cat = it.cat;
            }
            s.push_str(&format!("{} = {v}\n", it.key));
        }
    }
    // 知らないキー（新しい版で消えた項目など）も消さずに残す
    let mut rest: Vec<(&String, &String)> = values.iter().filter(|(k, _)| schema(k).is_none()).collect();
    rest.sort();
    for (k, v) in rest {
        s.push_str(&format!("{k} = {v}\n"));
    }
    s
}

impl Settings {
    /// 全体の設定と、`root` のプロジェクトの上書きを読む（試験では全体を読まない）
    pub fn load(root: Option<&Path>) -> Settings {
        let global = if cfg!(test) { HashMap::new() } else { global_path().and_then(|p| std::fs::read_to_string(p).ok()).map(|s| parse(&s)).unwrap_or_default() };
        let mut s = Settings { global, project: HashMap::new(), root: None };
        s.set_root(root);
        s
    }

    /// プロジェクトが変わった（上書きを読み直す）
    pub fn set_root(&mut self, root: Option<&Path>) {
        self.root = root.map(Path::to_path_buf);
        self.project = root.and_then(|r| std::fs::read_to_string(project_path(r)).ok()).map(|s| parse(&s)).unwrap_or_default();
    }

    /// 値（プロジェクトの上書き → 全体 → 既定値）
    pub fn get(&self, key: &str) -> String {
        let it = schema(key);
        if it.is_some_and(|i| i.project) {
            if let Some(v) = self.project.get(key) {
                return v.clone();
            }
        }
        self.global.get(key).cloned().unwrap_or_else(|| it.map_or(String::new(), |i| i.default.to_string()))
    }

    pub fn bool(&self, key: &str) -> bool {
        matches!(self.get(key).to_ascii_lowercase().as_str(), "true" | "on" | "yes" | "1")
    }

    /// 整数（範囲の外は端に。読めなければ既定値）
    pub fn int(&self, key: &str) -> i64 {
        let it = schema(key);
        let v = self.get(key).trim().parse::<i64>().ok().or_else(|| it.and_then(|i| i.default.parse().ok())).unwrap_or(0);
        match it.map(|i| i.kind) {
            Some(SKind::Int(lo, hi)) => v.clamp(lo, hi),
            _ => v,
        }
    }

    /// 空なら None
    pub fn text(&self, key: &str) -> Option<String> {
        let v = self.get(key);
        (!v.trim().is_empty()).then(|| v.trim().to_string())
    }

    /// カンマで区切った並び
    pub fn list(&self, key: &str) -> Vec<String> {
        self.get(key).split([',', ';']).map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()
    }

    /// 値を変える（`project` ならプロジェクトの上書きに）。既定値に戻すときは消す
    pub fn set(&mut self, key: &str, value: &str, project: bool) {
        let map = if project { &mut self.project } else { &mut self.global };
        if schema(key).is_some_and(|i| i.default == value) && !project {
            map.remove(key);
        } else {
            map.insert(key.to_string(), value.to_string());
        }
    }

    /// プロジェクトの上書きを消す（全体の値に戻す）
    pub fn unset_project(&mut self, key: &str) {
        self.project.remove(key);
    }

    /// ファイルに書く（試験では全体を書かない）
    pub fn save(&self) -> nagi_core::Result<()> {
        if !cfg!(test) {
            if let Some(p) = global_path() {
                write(&p, &render(&self.global, "nagi の設定"))?;
            }
        }
        if let Some(r) = &self.root {
            let p = project_path(r);
            if self.project.is_empty() {
                if p.exists() {
                    let _ = std::fs::remove_file(&p);
                }
            } else {
                write(&p, &render(&self.project, "このプロジェクトの nagi の設定（全体の設定を上書き）"))?;
            }
        }
        Ok(())
    }
}

impl crate::editor::Editor {
    /// 設定を動いている nagi に効かせる（`old` は前の設定。変わった項目だけ効かせる）
    pub(crate) fn apply_settings(&mut self, old: Option<&Settings>) {
        // 外観: 配色・強調の色・タブの名前は描き方へ、フォント・大きさ・行の高さが変わったら App が作り直す
        crate::view::set_appearance(&self.settings.get("appearance.theme"), &self.settings.get("appearance.accent"), self.settings.get("appearance.tab_names") == "always");
        if let Some(o) = old {
            if ["appearance.font", "appearance.font_ja", "appearance.font_size", "appearance.line_height"].iter().any(|k| o.get(k) != self.settings.get(k)) {
                self.appearance_changed = true;
            }
        }
        // 欄の大きさを覚えていれば、まだ決めていない欄に入れる
        if self.settings.bool("appearance.remember_panels") {
            let num = |k: &str| self.settings.global.get(k).and_then(|v| v.parse::<usize>().ok());
            self.left_w = self.left_w.or(num("state.left_width"));
            self.right_w = self.right_w.or(num("state.right_width"));
            self.bottom_h = self.bottom_h.or(num("state.bottom_height"));
        }
        self.sync_clipboard();
        if !self.settings.bool("vim.hlsearch") {
            self.search.highlight = false;
        }
        // 裏の処理（Maven・git・JDK・ターミナル・ブラウザ）が読む設定
        let s = &self.settings;
        nagi_workers::config::set(nagi_workers::config::Config {
            mvn: s.text("maven.path"),
            use_wrapper: s.bool("maven.use_wrapper"),
            mvn_options: s.get("maven.options"),
            jdk: s.text("maven.jdk").map(PathBuf::from),
            git: s.text("git.path"),
            git_wsl: s.bool("git.wsl"),
            browser: s.text("tools.browser"),
            terminal_utf8: s.bool("terminal.utf8"),
            exclude: s.list("project.exclude"),
        });
        // 追加の PATH とドライバーの置き場所は、nagi の環境変数に（nagi から起動するものに引き継がれる）。試験では変えない
        if !cfg!(test) {
            static BASE_PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
            let base = BASE_PATH.get_or_init(|| std::env::var("PATH").unwrap_or_default());
            let mut extra = self.settings.list("tools.extra_path");
            // 設定の JDK を渡す（JAVA_HOME と、java・javac が先に見つかるように PATH の頭に bin）
            static BASE_JAVA_HOME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
            let base_jh = BASE_JAVA_HOME.get_or_init(|| std::env::var("JAVA_HOME").ok());
            let jdk = self.settings.text("maven.jdk").filter(|j| self.settings.bool("build.jdk_env") && Path::new(j).join("bin").is_dir());
            if let Some(j) = &jdk {
                extra.insert(0, Path::new(j).join("bin").display().to_string());
            }
            // 言語のツール（設定で選んだ bin のフォルダを PATH の頭に。WSL の中のコマンドには届かない）
            for t in &nagi_workers::toolchains::TOOLS {
                if let Some(b) = self.settings.text(&format!("toolchain.{}", t.id)).filter(|b| Path::new(b).is_dir()) {
                    extra.insert(0, b);
                }
            }
            let extra = extra.join(";");
            let path = if extra.is_empty() { base.clone() } else { format!("{extra};{base}") };
            // SAFETY: 下と同じ（UI のスレッドだけが書く）
            unsafe {
                match (&jdk, base_jh) {
                    (Some(j), _) => std::env::set_var("JAVA_HOME", j),
                    (None, Some(b)) => std::env::set_var("JAVA_HOME", b),
                    (None, None) => std::env::remove_var("JAVA_HOME"),
                }
            }
            // SAFETY: 設定を適用するのは UI のスレッドだけで、ほかのスレッドが同時に環境変数を書くことはない
            unsafe {
                std::env::set_var("PATH", path);
                match self.settings.text("db.drivers_dir") {
                    Some(d) => std::env::set_var("NAGI_DRIVERS_DIR", d),
                    None => std::env::remove_var("NAGI_DRIVERS_DIR"),
                }
            }
        }
    }

    /// ドラッグで変えた欄の大きさを覚える（設定の「欄の大きさを覚える」）
    pub(crate) fn remember_panel_sizes(&mut self) {
        if !self.settings.bool("appearance.remember_panels") {
            return;
        }
        for (k, v) in [("state.left_width", self.left_w), ("state.right_width", self.right_w), ("state.bottom_height", self.bottom_h)] {
            if let Some(v) = v {
                self.settings.global.insert(k.to_string(), v.to_string());
            }
        }
        if let Err(e) = self.settings.save() {
            self.notify.info(format!("欄の大きさを覚えられません: {e}"));
        }
    }

    /// ヤンク・削除をいつもクリップボードへ送るか（.ideavimrc の clipboard=unnamed か、設定の「クリップボード」）
    pub(crate) fn sync_clipboard(&mut self) {
        // 試験では OS のクリップボードを触らない（試験が入れた偽物のまま）
        if cfg!(test) {
            return;
        }
        let on = self.rc_clipboard || self.settings.get("vim.clipboard") == "unnamed";
        self.clipboard = on.then_some(crate::editor::Clipboard { get: crate::clipboard::get, set: crate::clipboard::set });
    }

    /// "+ と "* のクリップボード（いつも OS。試験では入れた偽物だけ）
    pub(crate) fn plus_clipboard(&self) -> Option<crate::editor::Clipboard> {
        if cfg!(test) {
            return self.clipboard;
        }
        Some(self.clipboard.unwrap_or(crate::editor::Clipboard { get: crate::clipboard::get, set: crate::clipboard::set }))
    }
}

fn write(p: &Path, text: &str) -> nagi_core::Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    std::fs::write(p, text).context(format!("{}", p.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_sound() {
        let mut keys = std::collections::HashSet::new();
        for it in SCHEMA {
            assert!(keys.insert(it.key), "キーが重なっている: {}", it.key);
            assert!(CATEGORIES.contains(&it.cat), "分類が無い: {}", it.cat);
            match it.kind {
                SKind::Bool => assert!(matches!(it.default, "true" | "false"), "{}", it.key),
                SKind::Int(lo, hi) => {
                    let v: i64 = it.default.parse().unwrap();
                    assert!(v >= lo && v <= hi, "{}", it.key);
                }
                SKind::Choice(c) => assert!(c.iter().any(|x| x.0 == it.default), "{}", it.key),
                _ => {}
            }
        }
    }

    #[test]
    fn layering_and_round_trip() {
        let dir = std::env::temp_dir().join(format!("nagi-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = Settings::load(Some(&dir));
        assert_eq!(s.int("editor.indent_width"), 4);
        s.set("editor.indent_width", "2", false);
        s.set("editor.indent_width", "8", true);
        // プロジェクトの上書きが勝つ。上書きできない項目は全体の値
        assert_eq!(s.int("editor.indent_width"), 8);
        s.set("editor.scroll_ms", "0", true);
        assert_eq!(s.int("editor.scroll_ms"), 110, "上書きできない項目は全体の値");
        // 範囲の外は端に
        s.set("appearance.line_height", "500", false);
        assert_eq!(s.int("appearance.line_height"), 200);
        s.save().unwrap();
        let text = std::fs::read_to_string(project_path(&dir)).unwrap();
        assert!(text.contains("editor.indent_width = 8"), "{text}");
        let again = Settings::load(Some(&dir));
        assert_eq!(again.project.get("editor.indent_width").map(String::as_str), Some("8"));
        // 既定値に戻すと全体からは消える
        s.set("editor.indent_width", "4", false);
        assert!(!s.global.contains_key("editor.indent_width"));
        assert_eq!(render(&HashMap::from([("x.unknown".to_string(), "1".to_string())]), "h").lines().last(), Some("x.unknown = 1"), "知らないキーも残す");
    }
}
