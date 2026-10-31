#!/usr/bin/env python3
"""2 つの nagi.exe（A と B）の描く速さを、ぶれを抑えて比べる（WSL から動かす）。

  python3 tools/bench_ab.py --a target/bench/A.exe --b target/bench/B.exe [--runs 6] [--keys 150]

- 1 打ごとに UI のスレッドが使った CPU のサイクル数（NAGI_DEBUG_STATS の cyc_total・cyc_build）で比べる。
  経過時間と違い、ほかのプロセスに CPU を取られた時間や待ちが入らない。経過時間（µs）も参考に出す。
- A と B を交互に流し（ABAB…）、その時々の機械の状態を両方に均等にかける。最初の 1 回ずつは慣らしとして捨てる。
- 回ごとに、打鍵の値の下から 25%（p25。主に見る）と中央値（p50）を取り、B/A の比と、その 95% の信頼区間（回ごとの値をブートストラップ）を出す。
  区間が 1 をまたがなければ差があると判定する。同じ exe どうし（--a と --b に同じもの）で流すと、ぶれの大きさが分かる。
  2026-10-31 の A/A（5 回ずつ）: p25 のぶれは 1〜5%（5〜7% の差を見分けられる）、p50 は 8〜10%。
  修正の前と後（プロジェクトの根を覚える）では cyc_build が B/A 0.38 [0.25, 0.67] と出た。
- 窓は出さない（--headless）。LOCALAPPDATA は回ごとに新しく作る（普段の設定・記録に触れない。前の回の状態を持ち越さない）。
  文法（--grammars）と索引（--index）は回ごとに写す（文法が無いと色分けなしで測る。索引が無いと測っている間に裏で作り直し、
  メモリ・キャッシュを取り合ってサイクル数までぶれる）。1 回 40〜90 秒、5 回ずつで 15 分ほど。
"""

import argparse
import os
import random
import re
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
STAT = re.compile(r'stat "([jk])" total=(\d+) edit=\d+ wait=\d+ build=(\d+) present=\d+ .*cyc_total=(\d+) cyc_build=(\d+)')
METRICS = ["cyc_total", "cyc_build", "total_us", "build_us"]
# 回ごとの値: 中央値と、下から 25% の値（邪魔が入った打鍵に引きずられにくい。コードそのものの速さに近い）
# 主に見るのは p25（2026-10-31 の A/A で、回ごとのぶれが p25 は 1〜5%、p50 は 8〜10%。p25 なら 5〜7% の差を見分けられる）
SUMMARIES = {"p25": lambda v: sorted(v)[len(v) // 4], "p50": lambda v: statistics.median(v)}


def win(p: Path) -> str:
    return subprocess.run(["wslpath", "-w", str(p)], capture_output=True, text=True, check=True).stdout.strip()


def run_once(exe: Path, work: Path, i: int, args) -> dict:
    """1 回流して、1 打ごとの値を返す（{指標: [値…]}）"""
    lad = work / f"lad-{i}"
    shutil.rmtree(lad, ignore_errors=True)
    (lad / "nagi").mkdir(parents=True)
    # 打つキーは Vim のものなので Vim を有効に
    (lad / "nagi" / "settings.conf").write_text("vim.enabled = true\n", encoding="utf-8")
    # 文法（色分け。無ければダウンロードしに行き、色分けなしで測ることになる）
    if args.grammars and Path(args.grammars).is_dir():
        shutil.copytree(args.grammars, lad / "nagi" / "grammars")
    # 索引（無いと測っている間に同じプロセスの裏で作り直し、メモリ・キャッシュを取り合って UI のスレッドのサイクル数もぶれる）
    if args.index and Path(args.index).is_dir():
        shutil.copytree(args.index, lad / "nagi" / "index")
    keys = work / "keys.txt"
    # 下へ送って戻る（画面の送りも含める）
    keys.write_text(f"<Wait:{args.wait * 1000}>" + "j" * args.keys + "k" * args.keys + "<Wait:500><Quit>", encoding="utf-8")
    log = work / f"replay-{i}.log"
    env = dict(os.environ)
    env.update({"NAGI_DEBUG_STATS": "1", "LOCALAPPDATA": str(lad), "NAGI_REPLAY_LOG": str(log), "NAGI_REPLAY_OUT": str(work)})
    env["WSLENV"] = "NAGI_DEBUG_STATS:LOCALAPPDATA/p:NAGI_REPLAY_LOG/p:NAGI_REPLAY_OUT/p"
    target = Path(args.target).resolve()
    try:
        p = subprocess.run([str(exe), "--headless", "--replay-file", win(keys), win(target)], cwd=str(Path(args.dir).resolve()), env=env, capture_output=True, timeout=args.wait + 120)
        text = p.stderr.decode("utf-8", "replace")
    except subprocess.TimeoutExpired as e:
        text = (e.stderr or b"").decode("utf-8", "replace")
    # 標準エラーがつながっていないときはログのファイルへ出る
    nlog = lad / "nagi" / "nagi.log"
    if nlog.exists():
        text += nlog.read_text(encoding="utf-8", errors="replace")
    out = {m: [] for m in METRICS}
    for m in STAT.finditer(text):
        out["total_us"].append(int(m.group(2)))
        out["build_us"].append(int(m.group(3)))
        out["cyc_total"].append(int(m.group(4)))
        out["cyc_build"].append(int(m.group(5)))
    return out


def ci_ratio(a: list, b: list, n: int = 4000) -> tuple:
    """B/A の中央値の比と、その 95% の区間（回ごとの中央値をブートストラップ）"""
    rng = random.Random(1)
    rs = sorted(statistics.median(rng.choices(b, k=len(b))) / statistics.median(rng.choices(a, k=len(a))) for _ in range(n))
    return statistics.median(b) / statistics.median(a), rs[int(n * 0.025)], rs[int(n * 0.975)]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--a", required=True, help="比べる元の nagi.exe")
    ap.add_argument("--b", required=True, help="新しい nagi.exe")
    ap.add_argument("--runs", type=int, default=6, help="それぞれ何回流すか（慣らしの 1 回を除く）")
    ap.add_argument("--keys", type=int, default=150, help="1 回で打つ j の数（同じ数の k で戻る）")
    ap.add_argument("--wait", type=int, default=20, help="起動してから打ち始めるまでの秒")
    ap.add_argument("--target", default=str(ROOT / "crates/nagi/src/log_view.rs"), help="開くファイル（書き換えない）")
    ap.add_argument("--dir", default=str(ROOT), help="起動するフォルダ")
    ap.add_argument("--grammars", default=str(ROOT / "target/big/lad/nagi/grammars"), help="文法の DLL のフォルダ（回ごとに写す）")
    ap.add_argument("--index", default=str(ROOT / "target/big/lad/nagi/index"), help="索引のフォルダ（回ごとに写す。空なら写さない）")
    ap.add_argument("--work", default=str(ROOT / "target/bench/work"), help="作業のフォルダ（Windows から見える所）")
    args = ap.parse_args()
    work = Path(args.work)
    work.mkdir(parents=True, exist_ok=True)
    exes = {"A": Path(args.a).resolve(), "B": Path(args.b).resolve()}
    per_run = {n: {(m, s): [] for m in METRICS for s in SUMMARIES} for n in "AB"}
    order = ["A", "B"] * (args.runs + 1)
    t0 = time.time()
    for k, name in enumerate(order):
        r = run_once(exes[name], work, k, args)
        n = len(r["cyc_total"])
        warm = k < 2
        if n < args.keys:
            print(f"  {k + 1}/{len(order)} {name}: 打った記録が足りない（{n} 打）。この回は使わない", file=sys.stderr)
            continue
        print(f"  {k + 1}/{len(order)} {name}: {n} 打  cyc_build 中央 {statistics.median(r['cyc_build']) / 1e3:.0f}k{'（慣らし。使わない）' if warm else ''}", file=sys.stderr)
        if warm:
            continue
        for m in METRICS:
            for sname, f in SUMMARIES.items():
                per_run[name][(m, sname)].append(f(r[m]))
    print(f"\n{len(per_run['A'][('cyc_total', 'p25')])} 回ずつ（{time.time() - t0:.0f} 秒）。回ごとの値（p25 が主・p50 は参考）の中央値と B/A の比:")
    for m, sname in [(m, s) for s in SUMMARIES for m in METRICS]:
        a, b = per_run["A"][(m, sname)], per_run["B"][(m, sname)]
        if len(a) < 2 or len(b) < 2:
            print(f"  {m}: 回が足りない")
            continue
        ratio, lo, hi = ci_ratio(a, b)
        cv = lambda v: statistics.stdev(v) / statistics.mean(v) * 100
        verdict = "差は見えない" if lo <= 1.0 <= hi else ("B が速い" if hi < 1.0 else "B が遅い")
        unit = "k" if m.startswith("cyc") else "µs"
        scale = 1e3 if m.startswith("cyc") else 1
        print(f"  {m:10} {sname} A {statistics.median(a) / scale:8.1f}{unit}（ぶれ {cv(a):4.1f}%）  B {statistics.median(b) / scale:8.1f}{unit}（ぶれ {cv(b):4.1f}%）  B/A {ratio:.3f} [{lo:.3f}, {hi:.3f}]  {verdict}")


if __name__ == "__main__":
    main()
