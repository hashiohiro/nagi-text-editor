//! ダウンロードする文法（exe を小さく保つため、使われることの少ない言語の文法は exe に入れない）。
//!
//! 文法は 1 言語ずつの DLL（`grammars/` のクレート。`nagi_grammar()` が tree-sitter の言語を返す）にして、
//! GitHub のリリース（`RELEASE`）に `grammar-<id>.zip` として置く。nagi は初めてその言語のファイルを開いたときに
//! ダウンロードして `%LOCALAPPDATA%\nagi\grammars\<版>\grammar-<id>.dll` に置き、読み込む。
//! 読み込む前に、exe に埋め込んだ SHA-256（`grammars.lock`）と照らし合わせ、合わないものは読まない
//! （ネットワークの途中や置き場所で差し替えられた DLL を動かさない）。色分けの規則（クエリ）は exe に入っている。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use tree_sitter::Language;

/// 文法を置くリリース（文法を作り直したら版を上げる。古い nagi は古い版を取りにいく）
pub const RELEASE: &str = "grammars-v1";
pub const REPO: &str = "hashiohiro/nagi-text-editor";

/// 文法の DLL の SHA-256 と、zip の大きさ（grammars/build.sh が書く）
const LOCK: &str = include_str!("../grammars.lock");

/// (DLL の SHA-256, zip の大きさ)
pub fn expected(id: &str) -> Option<(&'static str, u64)> {
    LOCK.lines().filter(|l| !l.starts_with('#')).find_map(|l| {
        let mut it = l.split_whitespace();
        (it.next()? == id).then_some(())?;
        Some((it.next()?, it.next()?.parse().ok()?))
    })
}

/// 置き場所（NAGI_GRAMMARS_DIR があればそこ）
pub fn dir() -> PathBuf {
    if let Some(d) = std::env::var_os("NAGI_GRAMMARS_DIR") {
        return PathBuf::from(d);
    }
    // 試験では、grammars/build.sh が作ったもの
    if cfg!(test) {
        return PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/grammars/dist");
    }
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("nagi").join("grammars").join(RELEASE)
}

pub fn dll_path(id: &str) -> PathBuf {
    dir().join(format!("grammar-{id}.dll"))
}

/// リリースの zip の URL（NAGI_GRAMMARS_URL があればそこ。試験で file:// を使う）
pub fn zip_url(id: &str) -> String {
    let base = std::env::var("NAGI_GRAMMARS_URL").unwrap_or_else(|_| format!("https://github.com/{REPO}/releases/download/{RELEASE}"));
    format!("{}/grammar-{id}.zip", base.trim_end_matches('/'))
}

/// 16 進の SHA-256
pub fn sha256_hex(data: &[u8]) -> Option<String> {
    let h = sha256(data)?;
    Some(h.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(windows)]
fn sha256(data: &[u8]) -> Option<[u8; 32]> {
    use windows_sys::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};
    let mut out = [0u8; 32];
    // SAFETY: 入力と出力の長さをそのまま渡す
    let st = unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, std::ptr::null(), 0, data.as_ptr(), data.len() as u32, out.as_mut_ptr(), 32) };
    (st == 0).then_some(out)
}

#[cfg(not(windows))]
fn sha256(_: &[u8]) -> Option<[u8; 32]> {
    None
}

/// ファイルが表の SHA-256 と合うか
pub fn verified(id: &str, path: &std::path::Path) -> bool {
    let Some((want, _)) = expected(id) else { return false };
    std::fs::read(path).ok().and_then(|b| sha256_hex(&b)).is_some_and(|h| h == want)
}

/// 読み込んだ文法（1 度読んだら exe が終わるまで使う。DLL は解放しない）
static LOADED: Mutex<Option<HashMap<String, Language>>> = Mutex::new(None);

/// 入っていて正しければ読み込む。無い・合わないなら None（呼ぶ側がダウンロードを始める）
pub fn load(id: &str) -> Option<Language> {
    let mut m = LOADED.lock().ok()?;
    let map = m.get_or_insert_with(HashMap::new);
    if let Some(l) = map.get(id) {
        return Some(l.clone());
    }
    let path = dll_path(id);
    if !path.is_file() || !verified(id, &path) {
        return None;
    }
    let lang = open(&path)?;
    map.insert(id.to_string(), lang.clone());
    Some(lang)
}

/// 入っているか（読み込みはしない）
pub fn installed(id: &str) -> bool {
    LOADED.lock().ok().is_some_and(|m| m.as_ref().is_some_and(|m| m.contains_key(id))) || dll_path(id).is_file()
}

#[cfg(windows)]
fn open(path: &std::path::Path) -> Option<Language> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    // SAFETY: SHA-256 を確かめた nagi の文法の DLL だけを読み込む。解放はしない（言語の表を指し続けるため）
    let module = unsafe { LoadLibraryW(wide.as_ptr()) };
    if module.is_null() {
        return None;
    }
    // SAFETY: nagi_grammar は引数なしで TSLanguage を指すポインタを返す（grammars/lib.rs）
    let f = unsafe { GetProcAddress(module, c"nagi_grammar".as_ptr().cast()) }?;
    // SAFETY: 呼び出し規約は x64 では同じ。型だけを LanguageFn の形に直す
    let f: unsafe extern "C" fn() -> *const () = unsafe { std::mem::transmute(f) };
    // SAFETY: 同上
    let lf = unsafe { tree_sitter_language::LanguageFn::from_raw(f) };
    Some(Language::new(lf))
}

#[cfg(not(windows))]
fn open(_: &std::path::Path) -> Option<Language> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_and_paths() {
        let (sha, size) = expected("c").unwrap();
        assert_eq!(sha.len(), 64);
        assert!(size > 10_000);
        assert!(expected("rust").is_none(), "Rust は exe に入っている");
        assert_eq!(zip_url("go"), format!("https://github.com/{REPO}/releases/download/{RELEASE}/grammar-go.zip"));
        assert_eq!(sha256_hex(b"abc").unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
