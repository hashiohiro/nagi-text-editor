//! 文法のダウンロード（nagi_syntax::grammar）。GitHub のリリースから zip を取り、中の DLL が exe に埋め込んだ
//! SHA-256 と合えば置き場所へ移す。合わなければ置かない。

use std::path::{Path, PathBuf};

use nagi_syntax::grammar;

use crate::Waker;
use crate::drivers::{Download, download_and_extract};

/// `id` の文法をダウンロードする（裏のスレッド）。Done で置いた DLL
pub fn download(id: &str, waker: Waker) -> Download {
    download_into(id, &grammar::zip_url(id), &grammar::dir(), waker)
}

/// `url` の zip から `dir` に置く（試験は手元の file:// と一時のフォルダで）
fn download_into(id: &str, url: &str, dir: &Path, waker: Waker) -> Download {
    // 前に失敗して残った作業用のフォルダを消す
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if e.file_name().to_string_lossy().starts_with(&format!(".download-{id}-")) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
    let work = dir.join(format!(".download-{id}-{}", std::process::id()));
    let name = format!("grammar-{id}.dll");
    let (id2, dir2) = (id.to_string(), dir.to_path_buf());
    let find = move |d: &Path| -> Option<PathBuf> {
        let f = d.join(format!("grammar-{id2}.dll"));
        let ok = grammar::verified(&id2, &f);
        let target = dir2.join(format!("grammar-{id2}.dll"));
        if ok {
            let _ = std::fs::remove_file(&target);
            if std::fs::rename(&f, &target).is_err() {
                std::fs::copy(&f, &target).ok()?;
            }
            // 文法の著作権表示とライセンス文も隣に置く（:Licenses で見せる）
            let _ = std::fs::copy(d.join("THIRD_PARTY_LICENSES.txt"), dir2.join(format!("grammar-{id2}.LICENSES.txt")));
        }
        let _ = std::fs::remove_dir_all(d);
        ok.then_some(target)
    };
    let size = grammar::expected(id).map_or(0, |e| e.1);
    download_and_extract(url, size, &work, Box::new(find), &format!("{name}（SHA-256 が合うもの）"), waker)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_url(dir: &Path) -> String {
        let p = std::fs::canonicalize(dir).unwrap().display().to_string().replace('\\', "/");
        format!("file:///{}", p.trim_start_matches("//?/").trim_start_matches('/'))
    }

    fn wait(d: Download) -> Result<PathBuf, String> {
        loop {
            if let crate::drivers::DlMsg::Done(r) = d.rx.recv_timeout(std::time::Duration::from_secs(30)).unwrap() {
                break r;
            }
        }
    }

    /// file:// の zip から入れる。中身が違えば入れない（環境変数は使わない。ほかの試験と並んで動くので）
    #[test]
    fn installs_verified_only() {
        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/grammars/dist");
        if !src.join("grammar-toml.zip").is_file() {
            eprintln!("grammars/build.sh で作った zip が無いので飛ばす");
            return;
        }
        let dir = std::env::temp_dir().join(format!("nagi-grammars-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let r = wait(download_into("toml", &format!("{}/grammar-toml.zip", file_url(&src)), &dir, std::sync::Arc::new(|| {})));
        assert_eq!(r.as_deref().ok(), Some(dir.join("grammar-toml.dll").as_path()), "{r:?}");
        assert!(dir.join("grammar-toml.LICENSES.txt").is_file());
        // 別の言語の zip を go の名前で取ってきたもの → SHA-256 が合わないので入れない
        let r = wait(download_into("go", &format!("{}/grammar-toml.zip", file_url(&src)), &dir, std::sync::Arc::new(|| {})));
        assert!(r.is_err());
        assert!(!dir.join("grammar-go.dll").exists());
    }
}
