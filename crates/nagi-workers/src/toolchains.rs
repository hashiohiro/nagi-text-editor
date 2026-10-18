//! 言語のツール（IntelliJ の SDK と同じ考え方。JDK は jdk.rs）。入っているものを探し、無ければダウンロードする。
//!
//! 探す場所: nagi が置いたもの（`%LOCALAPPDATA%\nagi\tools\<種類>-<版>`）、よくあるインストール先、PATH。
//! ダウンロードは公式の配布の最新版（Python は python-build-standalone、Rust は rustup）。設定には bin のフォルダを入れ、
//! nagi から起動するもの（ターミナル・実行の構成・:Make）の PATH の頭に足す。
//! WSL のプロジェクトでは使わない（WSL の中に入っているものを使う。`wsl_report` で何が入っているかを見せる）。

use nagi_core::Context;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::Waker;
use crate::drivers::{DlMsg, Download, download_and_extract};

/// ツールの種類
pub struct ToolDef {
    pub id: &'static str,
    /// 画面に出す名前
    pub label: &'static str,
    /// Windows の実行ファイル（これがあるフォルダを bin とみなす）
    pub exe: &'static str,
    /// 版を出す引数
    pub version_args: &'static [&'static str],
    /// WSL の中のコマンドの名前
    pub wsl: &'static str,
    /// よくあるインストール先（%名前% は環境変数）
    pub known: &'static [&'static str],
    /// ダウンロードできるもの（画面に出す説明）。None ならできない
    pub download: Option<&'static str>,
}

pub const TOOLS: [ToolDef; 10] = [
    ToolDef { id: "python", label: "Python", exe: "python.exe", version_args: &["--version"], wsl: "python3", known: &[r"%LOCALAPPDATA%\Programs\Python\*", r"%ProgramFiles%\Python*"], download: Some("Python（python-build-standalone、約 30MB）") },
    ToolDef { id: "node", label: "Node.js", exe: "node.exe", version_args: &["--version"], wsl: "node", known: &[r"%ProgramFiles%\nodejs"], download: Some("Node.js LTS（約 35MB）") },
    ToolDef { id: "go", label: "Go", exe: "go.exe", version_args: &["version"], wsl: "go", known: &[r"%ProgramFiles%\Go\bin", r"%USERPROFILE%\go\bin"], download: Some("Go（約 80MB）") },
    ToolDef { id: "rust", label: "Rust（cargo）", exe: "cargo.exe", version_args: &["--version"], wsl: "cargo", known: &[r"%USERPROFILE%\.cargo\bin"], download: Some("rustup で stable を入れる（~\\.cargo。MSVC の Build Tools が要る）") },
    ToolDef { id: "dotnet", label: ".NET SDK", exe: "dotnet.exe", version_args: &["--version"], wsl: "dotnet", known: &[r"%ProgramFiles%\dotnet"], download: Some(".NET SDK 10（約 280MB）") },
    ToolDef { id: "clang", label: "C/C++（clang）", exe: "clang.exe", version_args: &["--version"], wsl: "gcc", known: &[r"%ProgramFiles%\LLVM\bin", r"C:\msys64\ucrt64\bin", r"C:\msys64\mingw64\bin"], download: Some("LLVM-MinGW（clang・lld・make 互換の mingw32-make。約 150MB）") },
    ToolDef { id: "cmake", label: "CMake", exe: "cmake.exe", version_args: &["--version"], wsl: "cmake", known: &[r"%ProgramFiles%\CMake\bin"], download: Some("CMake（約 45MB）") },
    ToolDef { id: "zig", label: "Zig", exe: "zig.exe", version_args: &["version"], wsl: "zig", known: &[], download: Some("Zig（約 80MB）") },
    ToolDef { id: "dart", label: "Dart", exe: "dart.exe", version_args: &["--version"], wsl: "dart", known: &[r"C:\tools\dart-sdk\bin", r"%LOCALAPPDATA%\Pub\Cache\bin"], download: Some("Dart SDK（約 200MB）") },
    ToolDef { id: "php", label: "PHP", exe: "php.exe", version_args: &["--version"], wsl: "php", known: &[r"C:\php", r"C:\tools\php*"], download: Some("PHP（NTS、約 35MB）") },
];

pub fn def(id: &str) -> Option<&'static ToolDef> {
    TOOLS.iter().find(|t| t.id == id)
}

/// nagi がダウンロードしたものを置く場所
pub fn tools_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("NAGI_TOOLS_DIR") {
        return PathBuf::from(d);
    }
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("nagi").join("tools")
}

/// 見つけたツール
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// 実行ファイルのあるフォルダ（設定に入れる値）
    pub bin: PathBuf,
    pub version: String,
    /// どこで見つけたか（nagi・PATH・インストール先）
    pub source: &'static str,
}

fn hidden(c: &mut Command) -> &mut Command {
    crate::sys::no_window(c).stdin(Stdio::null())
}

/// 文字の中の最初の版（1.2.3 か 1.2）
fn first_version(s: &str) -> Option<String> {
    let re = regex::Regex::new(r"\d+\.\d+(?:\.\d+)?").ok()?;
    re.find(s).map(|m| m.as_str().to_string())
}

/// 実行ファイルの版（`--version` などの出力の最初の版。3 秒で諦める）。分かった版は exe の場所と更新時刻で覚えておく
/// （設定の画面は一覧を作るたびに呼ぶため）
pub fn version_of(exe: &Path, args: &[&str]) -> String {
    version_within(exe, args, std::time::Duration::from_secs(3))
}

/// 版を覚えておく表（exe の場所 → (更新時刻, 版)）
fn versions() -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, (std::time::SystemTime, String)>> {
    static V: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<PathBuf, (std::time::SystemTime, String)>>> = std::sync::OnceLock::new();
    V.get_or_init(Default::default)
}

/// `version_of` の上限を指定する形（ダウンロードの直後は、展開したばかりの exe の初回の起動が検査などで遅いので長めに。裏の糸から）
fn version_within(exe: &Path, args: &[&str], limit: std::time::Duration) -> String {
    let mtime = std::fs::metadata(exe).and_then(|m| m.modified()).ok();
    if let (Some(t), Ok(v)) = (mtime, versions().lock()) {
        if let Some((_, ver)) = v.get(exe).filter(|(at, _)| *at == t) {
            return ver.clone();
        }
    }
    let ver = run_version(exe, args, limit);
    if let (Some(t), false, Ok(mut v)) = (mtime, ver.is_empty(), versions().lock()) {
        v.insert(exe.to_path_buf(), (t, ver.clone()));
    }
    ver
}

fn run_version(exe: &Path, args: &[&str], limit: std::time::Duration) -> String {
    let Ok(mut child) = hidden(Command::new(exe).args(args)).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() else { return String::new() };
    let deadline = std::time::Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                return String::new();
            }
        }
    }
    // 版の出力は短いので、終わってから読んでも詰まらない
    let Ok(o) = child.wait_with_output() else { return String::new() };
    first_version(&format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))).unwrap_or_default()
}

/// `%名前%` を環境変数に、最後の `*` をそのフォルダの中身に広げる
fn expand_known(pat: &str) -> Vec<PathBuf> {
    let mut s = pat.to_string();
    while let Some(i) = s.find('%') {
        let Some(j) = s[i + 1..].find('%') else { break };
        let name = &s[i + 1..i + 1 + j];
        let v = std::env::var(name).unwrap_or_default();
        if v.is_empty() {
            return Vec::new();
        }
        s = format!("{}{v}{}", &s[..i], &s[i + 2 + j..]);
    }
    match s.rsplit_once('\\') {
        Some((dir, last)) if last.contains('*') => {
            let prefix = last.trim_end_matches('*').to_ascii_lowercase();
            let mut v: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().to_ascii_lowercase().starts_with(&prefix))).collect();
            v.sort();
            v.reverse();
            v
        }
        _ => vec![PathBuf::from(s)],
    }
}

/// `dir` の下（深さ 3 まで）で `exe` のあるフォルダ
fn find_exe(dir: &Path, exe: &str, depth: usize) -> Option<PathBuf> {
    if dir.join(exe).is_file() {
        return Some(dir.to_path_buf());
    }
    if depth == 0 {
        return None;
    }
    let mut subs: Vec<PathBuf> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    subs.sort();
    // bin を先に
    subs.sort_by_key(|p| p.file_name().is_none_or(|n| n != "bin"));
    subs.iter().find_map(|s| find_exe(s, exe, depth - 1))
}

/// 入っているもの（nagi が置いたもの → よくある場所 → PATH。同じフォルダは 1 つ）
pub fn installed(id: &str) -> Vec<Found> {
    let Some(t) = def(id) else { return Vec::new() };
    let mut cands: Vec<(PathBuf, &'static str)> = Vec::new();
    let mut mine: Vec<PathBuf> = std::fs::read_dir(tools_dir()).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(&format!("{id}-")))).collect();
    mine.sort();
    mine.reverse();
    cands.extend(mine.iter().filter_map(|d| find_exe(d, t.exe, 3)).map(|b| (b, "nagi")));
    for k in t.known {
        cands.extend(expand_known(k).iter().filter_map(|d| find_exe(d, t.exe, 1)).map(|b| (b, "インストール先")));
    }
    if let Some(path) = std::env::var_os("PATH") {
        cands.extend(std::env::split_paths(&path).filter(|d| d.join(t.exe).is_file()).map(|b| (b, "PATH")));
    }
    let mut out: Vec<Found> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for (bin, source) in cands {
        // Microsoft Store へ案内するだけの python.exe は使えない
        if bin.to_string_lossy().contains("WindowsApps") {
            continue;
        }
        let canon = std::fs::canonicalize(&bin).unwrap_or(bin.clone());
        if seen.contains(&canon) {
            continue;
        }
        seen.push(canon);
        let version = version_of(&bin.join(t.exe), t.version_args);
        out.push(Found { bin, version, source });
    }
    out
}

/// URL の中身（Windows の curl.exe。GitHub の API のために User-Agent を付ける）
fn fetch_text(url: &str) -> nagi_core::Result<String> {
    let out = crate::exec::system("curl.exe").args(["-sSfL", "--retry", "2", "-A", "nagi", url]).output().map_err(|e| format!("curl.exe を起動できません: {e}"))?;
    if !out.status.success() {
        return Err(format!("{url} を読めません: {}", String::from_utf8_lossy(&out.stderr).trim()).into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn json(url: &str) -> nagi_core::Result<serde_json::Value> {
    serde_json::from_str(&fetch_text(url)?).context(format!("{url} の JSON を読めません"))
}

use crate::sys::version_key;

/// GitHub のいちばん新しいリリースの中で `pick` が選ぶ添付ファイル（名前から版を返す）の URL と版
fn github_asset(repo: &str, pick: impl Fn(&str) -> Option<String>) -> nagi_core::Result<(String, String)> {
    let j = json(&format!("https://api.github.com/repos/{repo}/releases/latest"))?;
    let mut best: Option<(String, String)> = None;
    for a in j["assets"].as_array().into_iter().flatten() {
        let (Some(name), Some(url)) = (a["name"].as_str(), a["browser_download_url"].as_str()) else { continue };
        if let Some(v) = pick(name) {
            if best.as_ref().is_none_or(|(bv, _)| version_key(&v) > version_key(bv)) {
                best = Some((v, url.to_string()));
            }
        }
    }
    best.map(|(v, u)| (u, v)).with_context(|| format!("{repo} のリリースに Windows 用のものがありません"))
}

/// ダウンロードできる版 1 つ
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// 版（3.14.8 など。Rust は stable・beta・nightly）
    pub version: String,
    /// zip（tar.gz）の URL。Rust は空
    pub url: String,
    /// 一覧に添える言葉（LTS・最新 など）
    pub note: String,
}

fn rel(version: impl Into<String>, url: impl Into<String>, note: &str) -> Release {
    Release { version: version.into(), url: url.into(), note: note.to_string() }
}

/// (版, 中身) を系列（`series` が返すもの。3.14 など）ごとにいちばん新しい 1 つにし、新しい系列から `max` まで
fn newest_per_series<T>(mut v: Vec<(String, T)>, series: impl Fn(&str) -> String, max: usize) -> Vec<(String, T)> {
    v.sort_by(|a, b| version_key(&b.0).cmp(&version_key(&a.0)));
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for (ver, x) in v {
        let s = series(&ver);
        if !seen.contains(&s) {
            seen.push(s);
            out.push((ver, x));
        }
        if out.len() >= max {
            break;
        }
    }
    out
}

/// 版の頭の 2 つ（3.14.8 → 3.14）
fn minor(v: &str) -> String {
    v.split('.').take(2).collect::<Vec<_>>().join(".")
}

/// ダウンロードできる版（新しいものから。系列ごとにいちばん新しいもの）
pub fn releases(id: &str) -> nagi_core::Result<Vec<Release>> {
    let out = match id {
        "python" => {
            let j = json("https://api.github.com/repos/astral-sh/python-build-standalone/releases/latest")?;
            let re = regex::Regex::new(r"^cpython-(3\.\d+\.\d+)\+\d+-x86_64-pc-windows-msvc-install_only\.tar\.gz$").unwrap();
            let v: Vec<(String, String)> = j["assets"].as_array().into_iter().flatten().filter_map(|a| Some((re.captures(a["name"].as_str()?)?[1].to_string(), a["browser_download_url"].as_str()?.to_string()))).collect();
            newest_per_series(v, minor, 6).into_iter().map(|(v, u)| rel(v, u, "")).collect()
        }
        "node" => {
            let j = json("https://nodejs.org/dist/index.json")?;
            let mut v: Vec<(String, String)> = Vec::new();
            let mut newest_major = None;
            for r in j.as_array().into_iter().flatten() {
                let (Some(ver), true) = (r["version"].as_str(), r["files"].as_array().is_some_and(|f| f.iter().any(|x| x == "win-x64-zip"))) else { continue };
                let major = ver.trim_start_matches('v').split('.').next().unwrap_or("").to_string();
                let newest = *newest_major.get_or_insert(major.clone()) == major;
                // LTS の系列と、いちばん新しい系列（Current）
                if r["lts"].as_str().is_some() || newest {
                    v.push((ver.trim_start_matches('v').to_string(), if r["lts"].as_str().is_some() { "LTS".into() } else { "Current".into() }));
                }
            }
            newest_per_series(v, |x| x.split('.').next().unwrap_or("").to_string(), 5).into_iter().map(|(v, note)| rel(&v, format!("https://nodejs.org/dist/v{v}/node-v{v}-win-x64.zip"), &note)).collect()
        }
        "go" => {
            let j = json("https://go.dev/dl/?mode=json&include=all")?;
            let v: Vec<(String, ())> = j.as_array().into_iter().flatten().filter(|r| r["stable"].as_bool() == Some(true)).filter_map(|r| Some((r["version"].as_str()?.trim_start_matches("go").to_string(), ()))).collect();
            newest_per_series(v, minor, 4).into_iter().map(|(v, _)| rel(&v, format!("https://go.dev/dl/go{v}.windows-amd64.zip"), "")).collect()
        }
        "dotnet" => {
            let j = json("https://dotnetcli.blob.core.windows.net/dotnet/release-metadata/releases-index.json")?;
            j["releases-index"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|r| matches!(r["support-phase"].as_str(), Some("active" | "maintenance")))
                .filter_map(|r| {
                    let ch = r["channel-version"].as_str()?;
                    let lts = r["release-type"].as_str() == Some("lts");
                    Some(rel(r["latest-sdk"].as_str()?, format!("https://aka.ms/dotnet/{ch}/dotnet-sdk-win-x64.zip"), if lts { "LTS" } else { "STS" }))
                })
                .collect()
        }
        "clang" => {
            let re = regex::Regex::new(r"^llvm-mingw-(\d+)-ucrt-x86_64\.zip$").unwrap();
            let (u, v) = github_asset("mstorsjo/llvm-mingw", |n| re.captures(n).map(|c| c[1].to_string()))?;
            vec![rel(v, u, "最新")]
        }
        "cmake" => {
            let j = json("https://api.github.com/repos/Kitware/CMake/releases?per_page=40")?;
            let re = regex::Regex::new(r"^cmake-(\d+\.\d+\.\d+)-windows-x86_64\.zip$").unwrap();
            let v: Vec<(String, String)> = j
                .as_array()
                .into_iter()
                .flatten()
                .filter(|r| r["prerelease"].as_bool() != Some(true))
                .flat_map(|r| r["assets"].as_array().cloned().unwrap_or_default())
                .filter_map(|a| Some((re.captures(a["name"].as_str()?)?[1].to_string(), a["browser_download_url"].as_str()?.to_string())))
                .collect();
            newest_per_series(v, minor, 4).into_iter().map(|(v, u)| rel(v, u, "")).collect()
        }
        "zig" => {
            let j = json("https://ziglang.org/download/index.json")?;
            let v: Vec<(String, String)> = j.as_object().into_iter().flatten().filter(|(k, _)| *k != "master").filter_map(|(k, r)| Some((k.clone(), r["x86_64-windows"]["tarball"].as_str()?.to_string()))).collect();
            newest_per_series(v, minor, 5).into_iter().map(|(v, u)| rel(v, u, "")).collect()
        }
        "dart" => {
            let j = json("https://storage.googleapis.com/storage/v1/b/dart-archive/o?prefix=channels/stable/release/&delimiter=/")?;
            let re = regex::Regex::new(r"/release/(\d+\.\d+\.\d+)/$").unwrap();
            let v: Vec<(String, ())> = j["prefixes"].as_array().into_iter().flatten().filter_map(|p| Some((re.captures(p.as_str()?)?[1].to_string(), ()))).collect();
            newest_per_series(v, minor, 4)
                .into_iter()
                .map(|(v, _)| rel(&v, format!("https://storage.googleapis.com/dart-archive/channels/stable/release/{v}/sdk/dartsdk-windows-x64-release.zip"), ""))
                .collect()
        }
        "php" => {
            let j = json("https://windows.php.net/downloads/releases/releases.json")?;
            let v: Vec<(String, String)> = j
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(_, r)| {
                    let path = r["nts-vs17-x64"]["zip"]["path"].as_str().or_else(|| r["nts-vs16-x64"]["zip"]["path"].as_str())?;
                    Some((r["version"].as_str()?.to_string(), format!("https://windows.php.net/downloads/releases/{path}")))
                })
                .collect();
            newest_per_series(v, minor, 4).into_iter().map(|(v, u)| rel(v, u, "")).collect()
        }
        // rustup のツールチェーン（プロジェクトごとの版は rust-toolchain.toml で）
        "rust" => vec![rel("stable", "", "rustup"), rel("beta", "", "rustup"), rel("nightly", "", "rustup")],
        _ => return Err(format!("{id} はダウンロードできません").into()),
    };
    if out.is_empty() {
        return Err(format!("{id} の Windows 用の版が見つかりません").into());
    }
    Ok(out)
}

/// いちばん新しい版の (zip の URL, 版)
pub fn resolve(id: &str) -> nagi_core::Result<(String, String)> {
    releases(id)?.into_iter().next().map(|r| (r.url, r.version)).with_context(|| format!("{id} の版がありません"))
}

/// 版 `release`（None ならいちばん新しいもの）をダウンロードして `tools\<種類>-<版>` に置く（裏のスレッド）。Done で bin のフォルダ
pub fn download(id: &str, release: Option<Release>, waker: Waker) -> Download {
    let id = id.to_string();
    let rx = nagi_core::task::spawn("tool-download", waker, move |ctx| {
        let send = |m: DlMsg| {
            ctx.send(m);
        };
        let Some(t) = def(&id) else { return };
        // 入れた直後の初回の起動は遅い（ウイルス対策の検査など。LLVM-MinGW の clang で 3 秒を超えた）ので、ここで一度版を調べて覚えておく
        let warm = |r: &nagi_core::Result<PathBuf>| {
            if let Ok(bin) = r {
                version_within(&bin.join(t.exe), t.version_args, std::time::Duration::from_secs(30));
            }
        };
        if id == "rust" {
            let ch = release.map_or("stable".to_string(), |r| r.version);
            let r = install_rustup(&ch, &send);
            warm(&r);
            send(DlMsg::Done(r));
            return;
        }
        let (url, version) = match release {
            Some(r) => (r.url, r.version),
            None => match resolve(&id) {
                Ok(x) => x,
                Err(e) => {
                    send(DlMsg::Done(Err(e)));
                    return;
                }
            },
        };
        let dest = tools_dir().join(format!("{id}-{version}"));
        if let Some(bin) = find_exe(&dest, t.exe, 3) {
            send(DlMsg::Done(Ok(bin)));
            return;
        }
        let exe = t.exe;
        let inner = download_and_extract(&url, 0, &dest, Box::new(move |d: &Path| find_exe(d, exe, 3)), exe, nagi_core::task::no_waker());
        // 進み具合をそのまま渡す
        while let Ok(m) = inner.rx.recv() {
            if let DlMsg::Done(r) = &m {
                warm(r);
                send(m);
                break;
            }
            send(m);
        }
    });
    Download { rx }
}

/// rustup-init.exe を取ってきて、stable を ~\.cargo に入れる（PATH は書き換えない。nagi の設定で渡す）
fn install_rustup(channel: &str, send: &dyn Fn(DlMsg)) -> nagi_core::Result<PathBuf> {
    let home = std::env::var_os("CARGO_HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()).join(".cargo"));
    // rustup が入っていれば、そのツールチェーンを足すだけ（既定は変えない。プロジェクトの版は rust-toolchain.toml で）
    let rustup = home.join("bin").join("rustup.exe");
    if rustup.is_file() {
        send(DlMsg::Extracting);
        let out = hidden(Command::new(&rustup).args(["toolchain", "install", channel, "--profile", "default"])).output().map_err(|e| format!("rustup を起動できません: {e}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("rustup が失敗しました").to_string().into());
        }
        return Ok(home.join("bin"));
    }
    let dir = tools_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let exe = dir.join("rustup-init.exe");
    let st = crate::exec::system("curl.exe").args(["-sSfL", "-o"]).arg(&exe).arg("https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe").status().map_err(|e| e.to_string())?;
    if !st.success() {
        return Err("rustup-init.exe をダウンロードできません".into());
    }
    send(DlMsg::Extracting);
    let out = hidden(Command::new(&exe).args(["-y", "--no-modify-path", "--default-toolchain", channel, "--profile", "default"])).output().map_err(|e| format!("rustup-init.exe を起動できません: {e}"))?;
    let _ = std::fs::remove_file(&exe);
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("rustup が失敗しました").to_string().into());
    }
    Ok(home.join("bin"))
}

/// WSL のディストリビューションに入っているもの（種類, コマンドの場所, 版）。入っていなければ場所と版は空
pub fn wsl_report(distro: &str) -> Vec<(&'static str, String, String)> {
    let mut script = String::new();
    for t in &TOOLS {
        let ver = if t.version_args == ["version"] { "version" } else { "--version" };
        script.push_str(&format!("p=$(command -v {c} 2>/dev/null) && echo \"{id}|$p|$({c} {ver} 2>&1 | head -n 1)\"; ", c = t.wsl, id = t.id));
    }
    // コマンドを動かすときと同じ環境（ログインのシェルの PATH。nvm・pyenv なども）で調べる
    let out = crate::exec::Exec::in_distro(distro).shell(&script).output();
    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    TOOLS
        .iter()
        .map(|t| {
            let hit = text.lines().find_map(|l| l.strip_prefix(&format!("{}|", t.id)));
            match hit.and_then(|h| h.split_once('|')) {
                Some((p, v)) => (t.id, p.to_string(), first_version(v).unwrap_or_default()),
                None => (t.id, String::new(), String::new()),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_tools() {
        let dir = std::env::temp_dir().join(format!("nagi-tools-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("go-1.27.2/go/bin")).unwrap();
        std::fs::write(dir.join("go-1.27.2/go/bin/go.exe"), "").unwrap();
        assert_eq!(find_exe(&dir.join("go-1.27.2"), "go.exe", 3), Some(dir.join("go-1.27.2/go/bin")));
        assert_eq!(first_version("Python 3.14.7"), Some("3.14.7".into()));
        assert_eq!(first_version("go version go1.27.2 windows/amd64"), Some("1.27.2".into()));
        assert!(version_key("3.15.0") > version_key("3.9.20"));
        // SAFETY: 試験の中だけ
        unsafe { std::env::set_var("NAGI_TOOLS_DIR", &dir) };
        let v = installed("go");
        assert_eq!(v.first().map(|f| (f.bin.clone(), f.source)), Some((dir.join("go-1.27.2/go/bin"), "nagi")));
        unsafe { std::env::remove_var("NAGI_TOOLS_DIR") };
    }

    /// ネットワークで最新版を調べる（手で流す: cargo test -- --ignored resolve_all）
    #[test]
    #[ignore]
    fn resolve_all() {
        for t in &TOOLS {
            if t.id == "rust" {
                continue;
            }
            let r = releases(t.id).map(|v| v.iter().map(|r| format!("{} {}", r.version, r.note)).collect::<Vec<_>>());
            eprintln!("{}: {r:?}", t.id);
            assert!(r.is_ok(), "{}: {r:?}", t.id);
        }
    }

    /// 実際にダウンロードして展開し、版を出せるか（手で流す。置き場所は NAGI_TOOLS_DIR、種類は NAGI_TEST_TOOLS（カンマ区切り）。
    /// rust は NAGI_TEST_CARGO_HOME を一時の場所へ向けて流す（CARGO_HOME・RUSTUP_HOME をそこへ。普段使いの rustup を更新しないため））
    #[test]
    #[ignore]
    fn download_real() {
        assert!(std::env::var_os("NAGI_TOOLS_DIR").is_some(), "NAGI_TOOLS_DIR を一時の場所へ");
        let ids = std::env::var("NAGI_TEST_TOOLS").unwrap_or_default();
        if let Some(h) = std::env::var_os("NAGI_TEST_CARGO_HOME") {
            let h = PathBuf::from(h);
            // SAFETY: 手で 1 つだけ流す試験
            unsafe {
                std::env::set_var("CARGO_HOME", h.join("cargo"));
                std::env::set_var("RUSTUP_HOME", h.join("rustup"));
            }
        }
        let mut failed = Vec::new();
        for id in ids.split(',').filter(|s| !s.is_empty()) {
            let t = def(id).unwrap();
            let started = std::time::Instant::now();
            let d = download(id, None, nagi_core::task::no_waker());
            let mut result = None;
            while let Ok(m) = d.rx.recv() {
                if let DlMsg::Done(r) = m {
                    result = Some(r);
                    break;
                }
            }
            match result {
                Some(Ok(bin)) => {
                    let v = version_of(&bin.join(t.exe), t.version_args);
                    eprintln!("{id}: {} 版 {v:?}（{:.0} 秒）", bin.display(), started.elapsed().as_secs_f32());
                    if v.is_empty() {
                        failed.push(format!("{id}: 版が出ない"));
                    }
                }
                other => {
                    eprintln!("{id}: 失敗 {other:?}");
                    failed.push(format!("{id}: {other:?}"));
                }
            }
        }
        assert!(failed.is_empty(), "{failed:?}");
    }
}
