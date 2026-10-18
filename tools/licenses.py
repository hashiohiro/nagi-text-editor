#!/usr/bin/env python3
"""配布物に入るクレートのライセンス表記を集める（MIT・Apache-2.0 などは著作権表示とライセンス文を添えるのが条件）。

    tools/licenses.py nagi <出力>             nagi.exe に入るもの（crates/nagi から通常の依存をたどる）
    tools/licenses.py grammar <id> <出力>     文法の DLL に入るもの（grammars/ を --features <id> で）

cargo metadata（Windows の cargo.exe）で依存をたどり、各クレートの LICENSE・COPYING・NOTICE などのファイルを
そのまま書き写す。同じ文面（Apache-2.0 の全文など）は 1 度だけ書き、ほかからはそれを指す。
ビルドの台本・試験だけで使う依存（build・dev）は配布物に入らないので含めない。
"""
import hashlib
import json
import os
import re
import subprocess
import sys

def find_cargo():
    """Windows の cargo.exe（WSL から使う）。CARGO で指定、無ければ PATH、無ければ %USERPROFILE%\\.cargo\\bin"""
    import shutil
    if os.environ.get("CARGO"):
        return os.environ["CARGO"]
    if shutil.which("cargo.exe"):
        return shutil.which("cargo.exe")
    home = subprocess.run(["cmd.exe", "/c", "echo %USERPROFILE%"], capture_output=True, text=True).stdout.strip()
    return subprocess.run(["wslpath", home], capture_output=True, text=True).stdout.strip() + "/.cargo/bin/cargo.exe"


CARGO = find_cargo()
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LICENSE_FILE = re.compile(r"^(licen[cs]e|copying|notice|unlicense|copyright)([-_.].*)?$", re.I)

# Rust の標準ライブラリ（どの exe・DLL にも入る。配布物の中にライセンスのファイルは無いので文面をここに）
RUST_STD = """Rust standard library (std, core, alloc and their runtime dependencies)
License: MIT OR Apache-2.0

Copyright (c) The Rust Project Contributors

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated
documentation files (the "Software"), to deal in the Software without restriction, including without limitation
the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and
to permit persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of
the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO
THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF
CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
IN THE SOFTWARE.
"""


def wsl_path(p):
    """C:\\x\\y → /mnt/c/x/y（WSL の中で読むため）"""
    m = re.match(r"^([A-Za-z]):\\(.*)$", p)
    return f"/mnt/{m.group(1).lower()}/{m.group(2).replace(chr(92), '/')}" if m else p


def metadata(args):
    out = subprocess.run([CARGO, "metadata", "--format-version", "1", "--filter-platform", "x86_64-pc-windows-msvc", *args], cwd=ROOT, capture_output=True, check=True)
    return json.loads(out.stdout)


def shipped(meta, root_name):
    """root_name から通常の依存（build・dev でない）でたどれるパッケージ"""
    pk = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    root = next(p["id"] for p in meta["packages"] if p["name"] == root_name)
    seen, stack = set(), [root]
    while stack:
        i = stack.pop()
        if i in seen:
            continue
        # プロシージャマクロはコンパイルのときに動くだけで、配布物には入らない（その依存も）
        if all("proc-macro" in t["kind"] for t in pk[i]["targets"]):
            continue
        seen.add(i)
        for d in nodes[i]["deps"]:
            if any(k["kind"] is None for k in d["dep_kinds"]):
                stack.append(d["pkg"])
    # 自分のクレート（source が無い）は除く
    return sorted((pk[i] for i in seen if pk[i]["source"]), key=lambda p: (p["name"].lower(), p["version"]))


def license_files(pkg):
    """クレートのライセンスのファイル（直下と、1 段下に同梱した C のライブラリ。odpi/LICENSE.txt など）。
    無ければ tools/license-texts/<名前>.txt（リポジトリなどから補ったもの）"""
    d = os.path.dirname(wsl_path(pkg["manifest_path"]))
    files = []
    if pkg.get("license_file"):
        files.append(os.path.join(d, pkg["license_file"]))
    for f in sorted(os.listdir(d)):
        p = os.path.join(d, f)
        if os.path.isfile(p) and LICENSE_FILE.match(f) and p not in files:
            files.append(p)
        elif os.path.isdir(p) and not f.startswith(".") and f not in ("tests", "test", "examples", "benches", "docs", "doc", "target"):
            files.extend(os.path.join(p, g) for g in sorted(os.listdir(p)) if LICENSE_FILE.match(g) and os.path.isfile(os.path.join(p, g)))
    if not files:
        extra = os.path.join(ROOT, "tools", "license-texts", pkg["name"] + ".txt")
        if os.path.isfile(extra):
            files.append(extra)
    return files


def render(title, pkgs, extra=""):
    out = [title, "=" * 78, ""]
    texts = {}  # 文面の hash → 最初に書いたクレート
    body = []
    for p in pkgs:
        head = f"{p['name']} {p['version']}"
        lic = p.get("license") or "(license field not set)"
        src = p.get("repository") or ""
        body.append("-" * 78)
        body.append(head)
        body.append(f"License: {lic}")
        if src:
            body.append(f"Source: {src}")
        files = license_files(p)
        if not files:
            body.append("(The crate does not include a license file. See the source repository for the full text.)")
        for f in files:
            text = open(f, encoding="utf-8", errors="replace").read().strip()
            h = hashlib.sha256(text.encode()).hexdigest()
            name = os.path.relpath(f, os.path.dirname(wsl_path(p["manifest_path"]))).replace(os.sep, "/") if not f.startswith(os.path.join(ROOT, "tools")) else "license (supplemented)"
            if h in texts:
                body.append(f"[{name}: same text as {texts[h]}]")
            else:
                texts[h] = f"{head} / {name}"
                body.append("")
                body.append(f"[{name}]")
                body.append(text)
        body.append("")
    out.append(f"This file lists the third-party components included in this program and their license notices ({len(pkgs)} crates).")
    out.append("")
    out.extend(body)
    out.append("-" * 78)
    out.append(RUST_STD)
    if extra:
        out.append("-" * 78)
        out.append(extra)
    return "\n".join(out).rstrip() + "\n"


def main():
    if sys.argv[1] == "nagi":
        meta = metadata([])
        pkgs = shipped(meta, "nagi")
        text = render("nagi - third-party licenses", pkgs)
        dest = sys.argv[2]
    elif sys.argv[1] == "grammar":
        gid = sys.argv[2]
        meta = metadata(["--manifest-path", "grammars/Cargo.toml", "--features", gid])
        pkgs = shipped(meta, "nagi-grammar")
        text = render(f"nagi grammar '{gid}' - third-party licenses", pkgs)
        dest = sys.argv[3]
    else:
        sys.exit(__doc__)
    with open(dest, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)
    print(f"{dest}: {len(pkgs)} crates, {len(text)} bytes")


if __name__ == "__main__":
    main()
