//! exe にアイコンを埋め込む（Windows SDK の rc.exe で .res を作り、リンカーに渡す。ID 1 の ICON）

use std::path::PathBuf;
use std::process::Command;

fn rc() -> Option<PathBuf> {
    let base = PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(&base).ok()?.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.join(r"x64\rc.exe").exists()).collect();
    versions.sort();
    versions.pop().map(|v| v.join(r"x64\rc.exe"))
}

fn main() {
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let ico = dir.join(r"assets\nagi.ico");
    println!("cargo:rerun-if-changed={}", ico.display());
    println!("cargo:rerun-if-changed=build.rs");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let rcfile = out.join("nagi.rc");
    std::fs::write(&rcfile, format!("1 ICON \"{}\"\n", ico.display().to_string().replace('\\', "\\\\"))).unwrap();
    let res = out.join("nagi.res");
    let Some(rc) = rc() else {
        println!("cargo:warning=rc.exe が見つからないので、アイコンを埋め込まない");
        return;
    };
    let status = Command::new(rc).arg("/nologo").arg("/fo").arg(&res).arg(&rcfile).status().expect("rc.exe を起動できない");
    assert!(status.success(), "rc.exe が失敗");
    println!("cargo:rustc-link-arg-bins={}", res.display());
}
