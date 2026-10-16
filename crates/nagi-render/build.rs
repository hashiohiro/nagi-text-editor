//! HLSL をビルド時に fxc でコンパイルし、バイトコードを OUT_DIR に置く（実行時に d3dcompiler を読まない。ADR 0013）
use std::path::PathBuf;
use std::process::Command;

fn fxc() -> PathBuf {
    let base = PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(&base)
        .expect("Windows SDK が見つからない")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join(r"x64\fxc.exe").exists())
        .collect();
    versions.sort();
    versions.pop().expect("fxc.exe が見つからない").join(r"x64\fxc.exe")
}

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let src = "shaders/grid.hlsl";
    println!("cargo:rerun-if-changed={src}");
    let fxc = fxc();
    for (entry, profile, file) in [("vs", "vs_5_0", "grid_vs.cso"), ("ps", "ps_5_0", "grid_ps.cso")] {
        let status = Command::new(&fxc)
            .args(["/nologo", "/O3", "/T", profile, "/E", entry, "/Fo"])
            .arg(out.join(file))
            .arg(src)
            .status()
            .expect("fxc を起動できない");
        assert!(status.success(), "fxc が失敗: {entry}");
    }
}
