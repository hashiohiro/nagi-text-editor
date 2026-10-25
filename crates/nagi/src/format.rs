//! 整形（IntelliJ の ReformatCode・`:Format`）。外部の整形ツールに今の中身を標準入力で渡し、標準出力を入れる。
//!
//! ツールは拡張子ごとに設定の「整形のコマンド」で決める（{file} はファイルのパス）。裏のスレッドで動かし、
//! 終わったときに中身が変わっていなければ、違う所だけを 1 回の編集で入れ替える（u で戻せる）。

use std::io::Write;
use std::path::PathBuf;

use crate::editor::Editor;

/// 動いている整形（編集の版, ファイル, 結果）
pub(crate) struct FormatJob {
    version: u64,
    path: PathBuf,
    task: nagi_core::task::Task<Result<String, String>>,
}

/// 拡張子 → コマンド（設定の「整形のコマンド」。拡張子=コマンド をカンマで区切る）
fn command_for(list: &[String], ext: &str) -> Option<String> {
    list.iter().find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        k.split('|').any(|k| k.trim().eq_ignore_ascii_case(ext)).then(|| v.trim().to_string())
    })
}

/// 整形の前後で違う所（前の中の始め, 終わり, 入れる文字）。同じなら None
fn changed_span(old: &str, new: &str) -> Option<(usize, usize, String)> {
    if old == new {
        return None;
    }
    let mut pre = old.bytes().zip(new.bytes()).take_while(|(a, b)| a == b).count();
    while !old.is_char_boundary(pre) || !new.is_char_boundary(pre) {
        pre -= 1;
    }
    let max = old.len().min(new.len()) - pre;
    let mut suf = old.bytes().rev().zip(new.bytes().rev()).take(max).take_while(|(a, b)| a == b).count();
    while !old.is_char_boundary(old.len() - suf) || !new.is_char_boundary(new.len() - suf) {
        suf -= 1;
    }
    Some((pre, old.len() - suf, new[pre..new.len() - suf].to_string()))
}

impl Editor {
    /// 今のファイルを整形する（設定の「整形のコマンド」で）
    pub(crate) fn format_buffer(&mut self) {
        let Some(path) = self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok()) else {
            self.notify.info("ファイル名がありません");
            return;
        };
        let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let Some(cmd) = command_for(&self.settings.list("tools.formatters"), &ext) else {
            self.notify.info(format!(".{ext} の整形のコマンドがありません（設定の「整形のコマンド」で 拡張子=コマンド）"));
            return;
        };
        let root = self.nav_root();
        let ex = nagi_workers::exec::Exec::in_dir(&root);
        let line = cmd.replace("{file}", &ex.path(&path));
        let text = self.buf.text().replace("\r\n", "\n");
        let shown = line.clone();
        let task = nagi_core::task::spawn("format", self.waker(), move |ctx| {
            let run = || -> Result<String, String> {
                let mut child = ex.shell(&line).stdin(std::process::Stdio::piped()).spawn().map_err(|e| format!("{line}: {e}"))?;
                // 書くのは別のスレッドで（大きなファイルで出力が詰まらないように）
                let mut stdin = child.stdin.take().ok_or("標準入力を開けません")?;
                let writer = std::thread::spawn(move || stdin.write_all(text.as_bytes()));
                let out = child.wait_with_output().map_err(|e| e.to_string())?;
                let _ = writer.join();
                // cmd のメッセージは Shift_JIS のことがある
                let err = nagi_core::encoding::decode(&out.stderr, None).0;
                // cmd の「コマンドが見つからない」（9009・認識されていません）・WSL の sh の 127
                let missing = matches!(out.status.code(), Some(9009 | 127)) || err.contains("認識されていません") || err.contains("is not recognized") || err.contains("not found");
                if !out.status.success() && missing {
                    let prog = line.split_whitespace().next().unwrap_or("");
                    return Err(format!("{prog} が見つかりません。入れて PATH に置くか、設定の「整形のコマンド」で変えてください"));
                }
                if !out.status.success() {
                    let first = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("").to_string();
                    return Err(if first.is_empty() { format!("{line} が失敗しました（{}）", out.status) } else { first });
                }
                String::from_utf8(out.stdout).map_err(|_| "整形の結果が UTF-8 ではありません".to_string())
            };
            ctx.send(run());
        });
        self.format_job = Some(FormatJob { version: self.buf.version, path, task });
        self.notify.info(format!("整形しています: {shown}"));
    }

    /// 整形が終わっていれば入れる。変わったら true
    pub(crate) fn poll_format(&mut self) -> bool {
        let Some(job) = &self.format_job else { return false };
        let Ok(r) = job.task.try_recv() else {
            if job.task.finished() {
                self.format_job = None;
            }
            return false;
        };
        let job = self.format_job.take().unwrap();
        let here = self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok());
        match r {
            Err(e) => self.notify.error(format!("整形できません: {e}")),
            Ok(_) if here.as_ref() != Some(&job.path) || self.buf.version != job.version => self.notify.info("整形の間に中身が変わったので、入れませんでした"),
            Ok(out) => {
                let nl = self.buf.newline();
                let out = out.replace("\r\n", "\n").replace('\n', nl);
                let old = self.buf.text();
                match changed_span(&old, &out) {
                    None => self.notify.info("整形済みです（変わる所はありません）"),
                    Some((s, e, t)) => {
                        let (line, byte) = (self.cursor.line, self.cursor.byte);
                        self.hist.begin();
                        self.edit(s, e, &t);
                        self.hist.close();
                        self.cursor.line = line.min(self.buf.line_count().saturating_sub(1));
                        self.cursor.byte = byte.min(self.buf.line(self.cursor.line).len());
                        self.clamp_normal();
                        self.notify.info("整形しました（u で戻せる）");
                    }
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_and_commands() {
        assert_eq!(changed_span("a  = 1;\nb\n", "a = 1;\nb\n"), Some((2, 3, String::new())));
        assert_eq!(changed_span("x", "x"), None);
        assert_eq!(changed_span("あい", "あう"), Some((3, 6, "う".into())));
        let list = vec!["rs=rustfmt".to_string(), "ts|tsx=npx prettier --stdin-filepath {file}".to_string()];
        assert_eq!(command_for(&list, "tsx").as_deref(), Some("npx prettier --stdin-filepath {file}"));
        assert_eq!(command_for(&list, "java"), None);
    }

    /// 標準入力から読んで標準出力へ書くコマンドで整形する（Windows の findstr で b の行を除く）
    #[test]
    #[cfg(windows)]
    fn formats_through_command() {
        let dir = std::env::temp_dir().join(format!("nagi-fmt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.zz");
        std::fs::write(&f, "b\na\n").unwrap();
        let mut e = Editor::new(nagi_core::Buffer::open(&f).unwrap(), None);
        e.settings.set("tools.formatters", "zz=findstr /v b", false);
        e.format_buffer();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !e.poll_format() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(e.buf.text(), "a\n", "{}", e.notify.text);
        // 無いコマンド
        e.settings.set("tools.formatters", "zz=nagi-no-such-formatter -", false);
        e.format_buffer();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !e.poll_format() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(e.notify.text.contains("nagi-no-such-formatter が見つかりません"), "{}", e.notify.text);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
