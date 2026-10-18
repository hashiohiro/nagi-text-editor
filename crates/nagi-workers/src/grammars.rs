//! 文法のダウンロード（nagi_syntax::grammar）。GitHub のリリースから zip を取り、中の DLL が exe に埋め込んだ
//! SHA-256 と合えば置き場所へ移す。合わなければ置かない。

use std::path::{Path, PathBuf};

use nagi_syntax::grammar;

use crate::Waker;
use crate::drivers::{Download, download_and_extract};

/// `id` の文法をダウンロードする（裏のスレッド）。Done で置いた DLL
pub fn download(id: &str, waker: Waker) -> Download {
    // 前に失敗して残った作業用のフォルダを消す
    for e in std::fs::read_dir(grammar::dir()).into_iter().flatten().flatten() {
        if e.file_name().to_string_lossy().starts_with(&format!(".download-{id}-")) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
    let work = grammar::dir().join(format!(".download-{id}-{}", std::process::id()));
    let name = format!("grammar-{id}.dll");
    let id2 = id.to_string();
    let find = move |d: &Path| -> Option<PathBuf> {
        let f = d.join(format!("grammar-{id2}.dll"));
        let ok = grammar::verified(&id2, &f);
        let target = grammar::dll_path(&id2);
        if ok {
            let _ = std::fs::remove_file(&target);
            if std::fs::rename(&f, &target).is_err() {
                std::fs::copy(&f, &target).ok()?;
            }
            // 文法の著作権表示とライセンス文も隣に置く（:Licenses で見せる）
            let _ = std::fs::copy(d.join("THIRD_PARTY_LICENSES.txt"), grammar::dir().join(format!("grammar-{id2}.LICENSES.txt")));
        }
        let _ = std::fs::remove_dir_all(d);
        ok.then_some(target)
    };
    let size = grammar::expected(id).map_or(0, |e| e.1);
    download_and_extract(&grammar::zip_url(id), size, &work, Box::new(find), &format!("{name}（SHA-256 が合うもの）"), waker)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// file:// の zip から入れる。中身が違えば入れない
    #[test]
    fn installs_verified_only() {
        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/grammars/dist");
        if !src.join("grammar-toml.zip").is_file() {
            eprintln!("grammars/build.sh で作った zip が無いので飛ばす");
            return;
        }
        let dir = std::env::temp_dir().join(format!("nagi-grammars-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let url = std::fs::canonicalize(&src).unwrap().display().to_string().replace('\\', "/");
        let url = format!("file:///{}", url.trim_start_matches("//?/").trim_start_matches('/'));
        // SAFETY: 試験の中だけ
        unsafe {
            std::env::set_var("NAGI_GRAMMARS_DIR", &dir);
            std::env::set_var("NAGI_GRAMMARS_URL", &url);
        }
        let wait = |d: Download| loop {
            match d.rx.recv_timeout(std::time::Duration::from_secs(30)).unwrap() {
                crate::drivers::DlMsg::Done(r) => break r,
                _ => {}
            }
        };
        let r = wait(download("toml", std::sync::Arc::new(|| {})));
        assert_eq!(r.as_deref().ok(), Some(dir.join("grammar-toml.dll").as_path()), "{r:?}");
        // 別の言語の zip を xml の名前で置いたもの → SHA-256 が合わないので入れない
        let fake = dir.join("fake");
        std::fs::create_dir_all(&fake).unwrap();
        std::fs::copy(src.join("grammar-toml.zip"), fake.join("grammar-xml.zip")).unwrap();
        let furl = format!("file:///{}", std::fs::canonicalize(&fake).unwrap().display().to_string().replace('\\', "/").trim_start_matches("//?/").trim_start_matches('/'));
        unsafe { std::env::set_var("NAGI_GRAMMARS_URL", &furl) };
        assert!(wait(download("xml", std::sync::Arc::new(|| {}))).is_err());
        assert!(!dir.join("grammar-xml.dll").exists());
        unsafe {
            std::env::remove_var("NAGI_GRAMMARS_DIR");
            std::env::remove_var("NAGI_GRAMMARS_URL");
        }
    }
}
