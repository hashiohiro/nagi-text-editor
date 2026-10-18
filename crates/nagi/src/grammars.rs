//! 文法のダウンロード（nagi_syntax::grammar）。C・Go・C# などのファイルを初めて開いたときに、その言語の文法を
//! GitHub のリリースから取ってきて、届いたら色分けと索引をやり直す。`:GrammarInstall [名前|all]` で先に入れておける。

use nagi_syntax::Lang;

use crate::editor::Editor;

/// ダウンロードする文法の言語（名前の順）
fn downloadable() -> Vec<(Lang, &'static str)> {
    Lang::all().iter().filter_map(|l| Some((*l, l.grammar_id()?))).collect()
}

/// nagi.exe に入っているもの（クレート・Rust の標準ライブラリ）の著作権表示とライセンス文（tools/licenses.py が作る）
const LICENSES: &str = include_str!("../../../THIRD_PARTY_LICENSES.txt");

impl Editor {
    /// `:Licenses`: nagi.exe と、ダウンロードした文法のライセンス表記を開く
    pub(crate) fn open_licenses(&mut self) {
        let mut text = LICENSES.to_string();
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(nagi_syntax::grammar::dir())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.to_string_lossy().ends_with(".LICENSES.txt"))
            .collect();
        files.sort();
        for f in files {
            if let Ok(t) = std::fs::read_to_string(&f) {
                text.push_str(&format!("\n\n{}\n(downloaded grammar: {})\n\n{t}", "#".repeat(78), f.display()));
            }
        }
        let dir = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir).join("nagi");
        let path = dir.join("THIRD_PARTY_LICENSES.txt");
        let _ = std::fs::create_dir_all(&dir);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
            if let Err(e) = std::fs::write(&path, &text) {
                self.notify.error(format!("{}: {e}", path.display()));
                return;
            }
        }
        self.edit_file(path);
    }

    /// `id` の文法をダウンロードし始める（もうダウンロードしているなら何もしない）
    pub(crate) fn install_grammar(&mut self, id: &str) {
        if self.grammar_downloads.iter().any(|(x, _)| x == id) {
            return;
        }
        let name = downloadable().iter().find(|(_, g)| *g == id).map_or(id.to_string(), |(l, _)| l.name().to_string());
        if cfg!(test) {
            self.notify.info(format!("（試験）{name} の文法のダウンロード"));
            return;
        }
        let waker = self.waker();
        self.grammar_downloads.push((id.to_string(), nagi_workers::grammars::download(id, waker)));
        let kb = nagi_syntax::grammar::expected(id).map_or(0, |e| e.1 / 1024);
        self.notify.info(format!("{name} の文法をダウンロードしています（色分け・定義へ移動に使う。{kb}KB）…"));
    }

    /// `:GrammarInstall [名前|all]`: 名前が無ければ今のファイルの言語
    pub(crate) fn grammar_install_command(&mut self, arg: &str) {
        let arg = arg.trim().to_ascii_lowercase();
        let all = downloadable();
        let targets: Vec<&str> = match arg.as_str() {
            "all" => all.iter().filter(|(l, _)| !l.available()).map(|(_, g)| *g).collect(),
            "" => match self.buf.path.as_deref().and_then(Lang::detect).and_then(|l| l.grammar_id()) {
                Some(g) => vec![g],
                None => {
                    let names: Vec<&str> = all.iter().map(|(_, g)| *g).collect();
                    self.notify.info(format!("この言語は nagi に入っています。ダウンロードする文法: {}（:GrammarInstall all）", names.join(" ")));
                    return;
                }
            },
            a => match all.iter().find(|(l, g)| *g == a || l.name().eq_ignore_ascii_case(a)) {
                Some((_, g)) => vec![*g],
                None => {
                    self.notify.info(format!("その名前の文法はありません: {a}"));
                    return;
                }
            },
        };
        if targets.is_empty() {
            self.notify.info("文法はすべて入っています");
            return;
        }
        for g in &targets {
            self.grammar_failed.remove(*g);
            self.install_grammar(g);
        }
        if targets.len() > 1 {
            self.notify.info(format!("文法を {} 個ダウンロードしています…", targets.len()));
        }
    }

    /// ダウンロードの知らせを受け取る。届いたら色分けと索引をやり直す。変わったら true
    pub fn poll_grammars(&mut self) -> bool {
        let mut changed = false;
        let mut finished = Vec::new();
        for (i, (id, d)) in self.grammar_downloads.iter().enumerate() {
            if let Some(r) = d.poll().done {
                finished.push((i, id.clone(), r));
            }
        }
        for (i, id, r) in finished.into_iter().rev() {
            self.grammar_downloads.remove(i);
            changed = true;
            let name = downloadable().iter().find(|(_, g)| *g == id).map_or(id.clone(), |(l, _)| l.name().to_string());
            match r {
                Ok(_) => {
                    // 今のバッファがその言語なら、色分けをやり直す（ほかのバッファは戻ったときに作る）
                    if self.buf.path.as_deref().and_then(Lang::detect).and_then(|l| l.grammar_id()) == Some(id.as_str()) {
                        self.syntax = None;
                        self.pending_syntax = None;
                        self.start_syntax();
                    }
                    // 索引にもその言語のファイルを入れる
                    if self.nav.index.is_some() || self.nav.progress.is_some() {
                        self.start_index();
                    }
                    if self.grammar_downloads.is_empty() {
                        self.notify.info(format!("{name} の文法を入れました（色分け・定義へ移動）"));
                    }
                }
                Err(e) => {
                    self.grammar_failed.insert(id);
                    self.notify.error(format!("{name} の文法をダウンロードできません: {e}（:GrammarInstall でやり直す）"));
                }
            }
        }
        changed
    }
}
