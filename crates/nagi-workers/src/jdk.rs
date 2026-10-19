//! JDK（IntelliJ の Project SDK と同じ考え方）。入っているものを探し、無ければ Eclipse Temurin をダウンロードする。
//!
//! 探す場所: nagi が入れたもの（`%LOCALAPPDATA%\nagi\jdks`）、Program Files の Java・Eclipse Adoptium・Microsoft・Zulu・
//! Amazon Corretto、JAVA_HOME。IDE（IntelliJ の `~\.jdks` など）のものは使わない（IDE が無くても同じに動くように）。
//! ダウンロードは Adoptium の API から zip を取り、`%LOCALAPPDATA%\nagi\jdks\temurin-<版>` に置く。

use std::path::{Path, PathBuf};

use crate::Waker;
use crate::drivers::{Download, download_and_extract};

/// 見つけた JDK
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Jdk {
    pub path: PathBuf,
    /// 版（release の JAVA_VERSION。無ければフォルダの名前）
    pub version: String,
    /// 作ったところ（release の IMPLEMENTOR）
    pub vendor: String,
}

impl Jdk {
    pub fn java(&self) -> PathBuf {
        self.path.join("bin").join("java.exe")
    }
}

/// ダウンロードできる版（Eclipse Temurin。LTS）
pub const DOWNLOADS: [u32; 5] = [25, 21, 17, 11, 8];

/// 自動で入れるときの版（Eclipse Temurin。LTS）
pub const AUTO: u32 = 21;

/// nagi がダウンロードした JDK を置く場所（`%LOCALAPPDATA%\nagi\jdks`。試験では NAGI_JDKS_DIR）
pub fn jdks_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("NAGI_JDKS_DIR") {
        return PathBuf::from(d);
    }
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("nagi").join("jdks")
}

/// nagi が入れた JDK のうち、いちばん新しい版
pub fn newest_mine() -> Option<Jdk> {
    let mut v: Vec<Jdk> = std::fs::read_dir(jdks_dir()).into_iter().flatten().flatten().filter_map(|e| read(&e.path())).collect();
    v.sort_by(|a, b| version_key(&a.version).cmp(&version_key(&b.version)));
    v.pop()
}

/// JDK のフォルダか（javac がある）。あれば版と作ったところを読む
pub fn read(path: &Path) -> Option<Jdk> {
    if !path.join("bin").join("javac.exe").is_file() && !path.join("bin").join("javac").is_file() {
        return None;
    }
    let release = std::fs::read_to_string(path.join("release")).unwrap_or_default();
    let field = |k: &str| {
        release.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).map(|v| v.trim().trim_matches('"').to_string())
    };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Some(Jdk { path: path.to_path_buf(), version: field("JAVA_VERSION").unwrap_or(name), vendor: field("IMPLEMENTOR").unwrap_or_default() })
}

/// 版の数の並び（並べ替え用。"21.0.4" → [21, 0, 4]、"1.8.0_402" → [8, 0, 402]）
fn version_key(v: &str) -> Vec<u32> {
    let mut parts = crate::sys::version_key(v);
    if parts.first() == Some(&1) && parts.len() > 1 {
        parts.remove(0);
    }
    parts
}

/// 入っている JDK（新しい版から）
pub fn installed() -> Vec<Jdk> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut scan = |base: PathBuf| {
        dirs.extend(std::fs::read_dir(base).into_iter().flatten().flatten().map(|e| e.path()));
    };
    scan(jdks_dir());
    for pf in ["ProgramFiles", "ProgramW6432"] {
        if let Some(p) = std::env::var_os(pf).map(PathBuf::from) {
            for v in ["Java", "Eclipse Adoptium", "Microsoft", "Zulu", "Amazon Corretto", "BellSoft", "Semeru"] {
                scan(p.join(v));
            }
        }
    }
    if let Some(h) = std::env::var_os("JAVA_HOME") {
        dirs.push(PathBuf::from(h));
    }
    let mut out: Vec<Jdk> = Vec::new();
    for d in dirs {
        if let Some(j) = read(&d) {
            let canon = std::fs::canonicalize(&j.path).unwrap_or(j.path.clone());
            if !out.iter().any(|o| std::fs::canonicalize(&o.path).unwrap_or(o.path.clone()) == canon) {
                out.push(j);
            }
        }
    }
    out.sort_by(|a, b| version_key(&b.version).cmp(&version_key(&a.version)));
    out
}

/// Eclipse Temurin の `version`（25・21・17 など）の zip の URL（Adoptium の API。いちばん新しい GA）
pub fn temurin_url(version: u32) -> String {
    format!("https://api.adoptium.net/v3/binary/latest/{version}/ga/windows/x64/jdk/hotspot/normal/eclipse?project=jdk")
}

/// Eclipse Temurin をダウンロードして `~\.jdks\temurin-<版>` に置く（裏のスレッド）。Done で置いたフォルダ
pub fn download_temurin(version: u32, waker: Waker) -> Download {
    download_from(&temurin_url(version), &jdks_dir(), waker)
}

/// `url` の zip（中に JDK が 1 つ）を `dir` に置く（試験では file:// の zip を使う）
pub fn download_from(url: &str, dir: &Path, waker: Waker) -> Download {
    let work = dir.join(format!(".download-{}", std::process::id()));
    let final_dir = dir.to_path_buf();
    let find = move |dest: &Path| -> Option<PathBuf> {
        // 展開したもの（jdk-21.0.4+7 など）を ~\.jdks\temurin-21.0.4 へ移す
        let found = std::fs::read_dir(dest).ok()?.flatten().map(|e| e.path()).find(|p| read(p).is_some())?;
        let j = read(&found)?;
        let ver = j.version.clone();
        let mut target = final_dir.join(format!("temurin-{ver}"));
        let mut n = 2;
        while target.exists() {
            target = final_dir.join(format!("temurin-{ver}-{n}"));
            n += 1;
        }
        std::fs::rename(&found, &target).ok()?;
        let _ = std::fs::remove_dir_all(dest);
        Some(target)
    };
    download_and_extract(url, 200 * 1024 * 1024, &work, Box::new(find), "JDK（bin\\javac.exe）", waker)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_sort() {
        let mut v = ["1.8.0_402", "21.0.4", "17.0.15", "11.0.2"].map(String::from).to_vec();
        v.sort_by(|a, b| version_key(b).cmp(&version_key(a)));
        assert_eq!(v, ["21.0.4", "17.0.15", "11.0.2", "1.8.0_402"]);
    }

    /// ダウンロード（file:// の小さな zip で）: 展開して ~\.jdks の代わりの場所に temurin-<版> として置く
    #[test]
    fn downloads_and_places() {
        let base = std::env::temp_dir().join(format!("nagi-jdk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let src = base.join("src").join("jdk-21.0.9+10");
        std::fs::create_dir_all(src.join("bin")).unwrap();
        std::fs::write(src.join("bin").join("javac.exe"), "").unwrap();
        std::fs::write(src.join("release"), "IMPLEMENTOR=\"Eclipse Adoptium\"\nJAVA_VERSION=\"21.0.9\"\n").unwrap();
        let zip = base.join("jdk.zip");
        let st = std::process::Command::new("tar.exe").args(["-a", "-cf"]).arg(&zip).arg("-C").arg(base.join("src")).arg("jdk-21.0.9+10").status().unwrap();
        assert!(st.success());
        let url = format!("file:///{}", zip.to_string_lossy().replace('\\', "/"));
        let dir = base.join("jdks");
        let d = download_from(&url, &dir, std::sync::Arc::new(|| {}));
        let mut result = None;
        while let Ok(m) = d.rx.recv() {
            if let crate::drivers::DlMsg::Done(r) = m {
                result = Some(r);
                break;
            }
        }
        let path = result.unwrap().unwrap();
        assert_eq!(path, dir.join("temurin-21.0.9"));
        let j = read(&path).unwrap();
        assert_eq!((j.version.as_str(), j.vendor.as_str()), ("21.0.9", "Eclipse Adoptium"));
        assert!(!dir.join(format!(".download-{}", std::process::id())).exists(), "作業のフォルダは消す");
    }
}
