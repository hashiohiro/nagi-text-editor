#!/usr/bin/env python3
"""コミットの欄と Git の履歴の右クリックのメニューを、本物の nagi の窓で押して確かめる（E2E）。

WSL から動かす。1 件ごとに、試験用のリポジトリ（素のリポジトリを origin に持つ）を作り直し、
nagi を `--headless --replay-file` で（窓を表に出さずに）起動して手順（replay.rs の <RClick:…> など）を流し、閉じた後に git の状態を確かめる。
nagi の設定・記録は件ごとのフォルダ（LOCALAPPDATA を差し替える）に置くので、普段の nagi には触れない。

  python3 tools/e2e/git_menus.py [--exe target/debug/nagi.exe] [件の名前の一部 ...]

結果は target/e2e/<日時>/ に、件ごとのフォルダ（repo・記録 log.txt・写し *.png）と、まとめ summary.md を置く。
"""

import argparse
import datetime
import os
import re
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
GIT = "git.exe"


def winpath(p: Path) -> str:
    return subprocess.run(["wslpath", "-w", str(p)], capture_output=True, text=True, check=True).stdout.strip()


def git(repo: Path, *args: str, check: bool = True) -> str:
    r = subprocess.run([GIT, *args], cwd=repo, capture_output=True, text=True, encoding="utf-8")
    if check and r.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)}: {r.stderr.strip()}")
    # 行頭の空白は残す（status の " M a.txt" は、空白で作業ツリーの変更と分かる）
    return r.stdout.rstrip()


def write(repo: Path, name: str, text: str) -> None:
    (repo / name).write_text(text, encoding="utf-8", newline="\n")


def make_repo(d: Path, dirty: bool) -> dict:
    """共通のリポジトリ。main は c1〜c4（c4 は origin に送っていない）、ブランチ・タグ・リモートだけのブランチを持つ。
    `dirty` なら作業ツリーに M・A・D・R・未追跡を 1 つずつ作る。戻り値はコミットの名前 → ハッシュ"""
    remote, repo = d / "remote.git", d / "repo"
    remote.mkdir(parents=True)
    repo.mkdir()
    git(remote, "init", "-q", "--bare", "-b", "main")
    git(repo, "init", "-q", "-b", "main")
    for k, v in [("user.email", "e2e@example.com"), ("user.name", "e2e"), ("core.autocrlf", "false"), ("commit.gpgsign", "false")]:
        git(repo, "config", k, v)
    git(repo, "remote", "add", "origin", "../remote.git")
    h = {}

    def commit(name: str, files: dict) -> None:
        for f, t in files.items():
            write(repo, f, t)
        git(repo, "add", "-A")
        git(repo, "commit", "-q", "-m", name)
        h[name] = git(repo, "rev-parse", "HEAD")

    commit("c1-first", {"a.txt": "a1\n", "b.txt": "b1\n", "z.md": "# z\n"})
    commit("c2-second", {"a.txt": "a1\na2\n"})
    git(repo, "tag", "v1-tag")
    git(repo, "branch", "merged-br")
    commit("c3-third", {"c.txt": "c1\n"})
    # topic-br: c3 から 1 つ（main にはマージしていない）。conflict-br: a.txt の同じ行を変える
    git(repo, "switch", "-q", "-c", "topic-br")
    commit("t1-topic", {"t.txt": "t1\n"})
    git(repo, "switch", "-q", "-c", "conflict-br", "main")
    commit("x1-conflict", {"a.txt": "a1\nX\n"})
    git(repo, "switch", "-q", "-c", "remote-br", "main")
    commit("r1-remote", {"r.txt": "r1\n"})
    git(repo, "switch", "-q", "main")
    git(repo, "push", "-q", "-u", "origin", "main", "remote-br")
    git(repo, "branch", "-q", "-D", "remote-br")
    commit("c4-fourth", {"a.txt": "a1\na4\n"})
    if dirty:
        write(repo, "a.txt", "a1\na4\nlocal-mod\n")
        write(repo, "n.txt", "added\n")
        git(repo, "add", "n.txt")
        (repo / "b.txt").unlink()
        git(repo, "mv", "c.txt", "c2.txt")
        write(repo, "u.txt", "untracked\n")
    return h


@dataclass
class Case:
    name: str
    # 確かめる操作（洗い出しの表の項目）
    what: str
    steps: str
    # 閉じた後に確かめる (説明, 関数（リポジトリ, ハッシュの辞書） → 合否)
    checks: list = field(default_factory=list)
    dirty: bool = False
    # 手順の前に git で整える（リポジトリ, ハッシュの辞書）
    prepare: object = None
    # nagi が動いている途中に、外で行う操作（記録にこの行が出たら, 関数（リポジトリ））
    during: tuple = None


# --- 閉じた後の git の状態を読む小道具 ---
def sub(r: Path, rev: str = "HEAD") -> str:
    return git(r, "log", "-1", "--format=%s", rev, check=False)


def rev(r: Path, x: str) -> str:
    return git(r, "rev-parse", "-q", "--verify", x + "^{commit}", check=False)


def branch(r: Path) -> str:
    return git(r, "rev-parse", "--abbrev-ref", "HEAD", check=False)


def status(r: Path) -> list:
    return git(r, "status", "--porcelain", check=False).splitlines()


def read(r: Path, f: str):
    p = r / f
    return p.read_text(encoding="utf-8") if p.exists() else None


def ancestor(r: Path, a: str, b: str) -> bool:
    return subprocess.run([GIT, "merge-base", "--is-ancestor", a, b], cwd=r).returncode == 0


def subjects(r: Path, n: int = 10) -> list:
    return git(r, "log", f"-{n}", "--format=%s", check=False).splitlines()


# 開いておくファイル（変えないファイル。題名や状態行に、確かめる名前が出ないように）
OPEN = "z.md"
COMMIT = ":Git<CR><WaitFor:変更 (4)>"
LOG = ":Glog<CR><WaitFor:c4-fourth>"
# 履歴で c2-second を選び、その中身（親は c1）が出るまで待つ
C2 = LOG + "<Click:c2-second><WaitFor:親 {c1-first}>"


def menu(target: str, item: str, shot: str = "menu") -> str:
    """`target` を右クリックし、メニューを写してから `item` を押す"""
    return f"<RClick:{target}><WaitFor:{item}><Shot:{shot}><Click:{item}>"


def yes(done: str) -> str:
    """確認の窓を写して「はい」を押し、`done` の知らせを待つ"""
    return f"<WaitFor:はい（Y）><Shot:confirm><Click:はい（Y）><WaitGone:はい（Y）><WaitFor:{done}>"


NO = "<WaitFor:いいえ（N）><Shot:confirm><Click:いいえ（N）><WaitGone:いいえ（N）>"


def dirty_wt(r: Path, h: dict) -> None:
    write(r, "a.txt", "a1\na4\nlocal\n")


CASES: list[Case] = [
    # ---------------- コミットの欄 ----------------
    Case("commit-diff", "差分を見る", COMMIT + menu("a.txt", "差分を見る") + "<WaitFor:local-mod>", dirty=True),
    Case("commit-open", "開く（ノーマルへ）", COMMIT + menu("a.txt", "開く") + "<WaitFor:local-mod><NotSee:-- コミット -->", dirty=True),
    Case("commit-open-stay", "開いて欄に残る", COMMIT + menu("a.txt", "開いて欄に残る") + "<WaitFor:local-mod><See:-- コミット -->", dirty=True),
    Case("commit-open-deleted", "開く（消したファイル）", COMMIT + menu("b.txt", "開く") + "<WaitFor:消したファイルは開けません>", dirty=True),
    Case("commit-toggle", "選ぶ・外す", COMMIT + menu("a.txt", "選ぶ・外す") + "<WaitFor:コミットメッセージ（3 ファイル）>", dirty=True),
    Case(
        "commit-toggle-all",
        "すべて選ぶ・外す（2 回）",
        COMMIT + menu("a.txt", "すべて選ぶ・外す") + "<WaitFor:コミットメッセージ（5 ファイル）>" + menu("a.txt", "すべて選ぶ・外す", "menu2") + "<WaitFor:コミットメッセージ（0 ファイル）>",
        dirty=True,
    ),
    Case(
        "commit-revert-m",
        "変更を戻す…（M）",
        COMMIT + menu("a.txt", "変更を戻す…") + yes("変更を戻しました"),
        [("a.txt が HEAD の中身", lambda r, h: read(r, "a.txt") == "a1\na4\n"), ("a.txt が変更に無い", lambda r, h: not any(l.endswith(" a.txt") for l in status(r)))],
        dirty=True,
    ),
    Case(
        "commit-revert-a",
        "変更を戻す…（A: index から外し、ファイルは残す）",
        COMMIT + menu("n.txt", "変更を戻す…") + yes("変更を戻しました"),
        [("n.txt は未追跡で残る", lambda r, h: "?? n.txt" in status(r) and read(r, "n.txt") == "added\n")],
        dirty=True,
    ),
    Case(
        "commit-revert-d",
        "変更を戻す…（D）",
        COMMIT + menu("b.txt", "変更を戻す…") + yes("変更を戻しました"),
        [("b.txt が戻る", lambda r, h: read(r, "b.txt") == "b1\n")],
        dirty=True,
    ),
    Case(
        "commit-revert-r",
        "変更を戻す…（R: 元の名前に戻す）",
        COMMIT + menu("c2.txt", "変更を戻す…") + yes("変更を戻しました"),
        [("c.txt が戻る", lambda r, h: read(r, "c.txt") == "c1\n"), ("c2.txt が無い", lambda r, h: read(r, "c2.txt") is None)],
        dirty=True,
    ),
    Case("commit-revert-untracked", "変更を戻す…（未追跡は戻せないと知らせる）", COMMIT + menu("u.txt", "変更を戻す…") + "<WaitFor:未追跡のファイルは戻せません>", [("u.txt は残る", lambda r, h: read(r, "u.txt") == "untracked\n")], dirty=True),
    Case("commit-revert-no", "変更を戻す…（いいえ）", COMMIT + menu("a.txt", "変更を戻す…") + NO, [("a.txt は変えたまま", lambda r, h: read(r, "a.txt") == "a1\na4\nlocal-mod\n")], dirty=True),
    Case(
        "commit-commit",
        "メッセージを書く → コミット",
        COMMIT + menu("a.txt", "メッセージを書く") + "e2e-commit<Esc><Esc>" + menu("a.txt", "コミット", "menu2") + "<WaitFor:コミットしました>",
        [("件名", lambda r, h: sub(r) == "e2e-commit"), ("親は c4", lambda r, h: rev(r, "HEAD~1") == h["c4-fourth"]), ("未追跡は残る", lambda r, h: "?? u.txt" in status(r))],
        dirty=True,
    ),
    Case(
        "commit-amend",
        "直前のコミットを直す → コミット",
        COMMIT + menu("a.txt", "直前のコミットを直す") + "<WaitFor:[x] 直前のコミットを直す>" + menu("a.txt", "メッセージを書く", "menu2") + "amended<Esc><Esc>" + menu("a.txt", "コミット", "menu3") + "<WaitFor:コミットしました>",
        [("件名", lambda r, h: sub(r) == "amended"), ("c4 を置き換えた（親は c3）", lambda r, h: rev(r, "HEAD~1") == h["c3-third"])],
        dirty=True,
    ),
    Case(
        "commit-amend-off",
        "直前のコミットを直す: やめる",
        COMMIT + menu("a.txt", "直前のコミットを直す") + "<WaitFor:[x] 直前のコミットを直す>" + menu("a.txt", "直前のコミットを直す: やめる", "menu2") + "<WaitFor:[ ] 直前のコミットを直す>",
        dirty=True,
    ),
    Case(
        "commit-push",
        "コミットしてプッシュ（はい）",
        COMMIT + menu("a.txt", "メッセージを書く") + "pushed<Esc><Esc>" + menu("a.txt", "コミットしてプッシュ", "menu2") + "<WaitFor:件のコミットを送りますか>" + yes("プッシュしました"),
        [("origin/main が HEAD", lambda r, h: rev(r, "origin/main") == rev(r, "HEAD") and sub(r) == "pushed"), ("素のリポジトリにも届いた", lambda r, h: git(r.parent / "remote.git", "rev-parse", "main") == rev(r, "HEAD"))],
        dirty=True,
    ),
    Case(
        "commit-push-no",
        "コミットしてプッシュ（いいえ）",
        COMMIT + menu("a.txt", "メッセージを書く") + "not-pushed<Esc><Esc>" + menu("a.txt", "コミットしてプッシュ", "menu2") + "<WaitFor:件のコミットを送りますか>" + NO + "<WaitFor:プッシュをやめました>",
        [("コミットはした", lambda r, h: sub(r) == "not-pushed"), ("送っていない", lambda r, h: rev(r, "origin/main") == h["c3-third"])],
        dirty=True,
    ),
    Case(
        "commit-push-new-branch",
        "コミットしてプッシュ（追跡先の無いブランチ: origin に作る）",
        COMMIT + menu("a.txt", "メッセージを書く") + "on-new<Esc><Esc>" + menu("a.txt", "コミットしてプッシュ", "menu2") + "<WaitFor:新しく作る>" + yes("プッシュしました"),
        [("origin/new-br ができた", lambda r, h: rev(r, "origin/new-br") == rev(r, "HEAD")), ("追跡する", lambda r, h: git(r, "rev-parse", "--abbrev-ref", "@{u}", check=False) == "origin/new-br")],
        dirty=True,
        prepare=lambda r, h: git(r, "switch", "-q", "-c", "new-br"),
    ),
    Case("commit-reload", "読み直す", COMMIT + menu("a.txt", "読み直す") + "<WaitFor:変更 (4)><See:u.txt>", dirty=True),
    Case("commit-close", "欄を閉じる", COMMIT + menu("a.txt", "欄を閉じる") + "<WaitGone:コミットメッセージ（>", dirty=True),
    Case("commit-header", "見出しの行（▾ 変更）の右クリック（観察）", COMMIT + "<RClick:▾ 変更 (4)><Wait:300><Shot:menu>", dirty=True),
    Case(
        "commit-after-commit-refresh",
        "コミットの後、一覧が作り直される（タブを切り替えなくても）",
        COMMIT + menu("a.txt", "メッセージを書く") + "refresh<Esc><Esc>" + menu("a.txt", "コミット", "menu2") + "<WaitFor:コミットしました><WaitGone:M a.txt><WaitFor:▾ 変更 (0)>",
        dirty=True,
    ),
    Case(
        "commit-outside-refresh",
        "外（ターミナルなど）でコミットしても、一覧が作り直される",
        COMMIT + "<Wait:1500><WaitGone:M a.txt><Shot:after>",
        [("外のコミット", lambda r, h: sub(r) == "outside")],
        dirty=True,
        during=("OK shown 変更 (4)", lambda r: git(r, "commit", "-qam", "outside")),
    ),
    # ---------------- 履歴: ブランチの列 ----------------
    Case("br-checkout-local", "ローカル: チェックアウト", LOG + menu("topic-br", "チェックアウト") + "<WaitFor:topic-br をチェックアウトしました>", [("今のブランチ", lambda r, h: branch(r) == "topic-br")]),
    Case(
        "br-checkout-remote",
        "リモート: チェックアウト（ローカルを作って追跡）",
        LOG + menu("origin/remote-br", "チェックアウト") + "<WaitFor:remote-br を作って origin/remote-br を追跡します>",
        [("今のブランチ", lambda r, h: branch(r) == "remote-br"), ("追跡先", lambda r, h: git(r, "rev-parse", "--abbrev-ref", "@{u}", check=False) == "origin/remote-br")],
    ),
    Case(
        "br-checkout-remote-existing",
        "リモート: チェックアウト（同じ名前のローカルがある）",
        LOG + menu("origin/remote-br", "チェックアウト") + "<WaitFor:remote-br をチェックアウトしました>",
        [("今のブランチ", lambda r, h: branch(r) == "remote-br")],
        prepare=lambda r, h: git(r, "branch", "remote-br", "origin/remote-br"),
    ),
    Case("br-checkout-tag", "タグ: チェックアウト（HEAD を切り離す）", LOG + menu("v1-tag", "チェックアウト") + yes("チェックアウトしました"), [("HEAD は c2", lambda r, h: rev(r, "HEAD") == h["c2-second"] and branch(r) == "HEAD")]),
    Case("br-new-from", "X から新しいブランチ…", LOG + menu("topic-br", "topic-br から新しいブランチ…") + "<Shot:cmdline>from-topic<CR><Wait:500>", [("from-topic は t1", lambda r, h: rev(r, "from-topic") == h["t1-topic"])]),
    Case(
        "br-checkout-rebase",
        "チェックアウトして main の上へリベース",
        LOG + menu("topic-br", "チェックアウトして main の上へリベース") + yes("topic-br を main の上へリベースしました"),
        [("今のブランチ", lambda r, h: branch(r) == "topic-br"), ("main の上", lambda r, h: rev(r, "HEAD~1") == h["c4-fourth"])],
    ),
    Case("br-compare", "main と比べる", LOG + menu("topic-br", "main と比べる") + "<WaitGone:c4-fourth><WaitFor:t1-topic><See:main..topic-br>"),
    Case("br-rebase-current", "main を X の上へリベース", LOG + menu("topic-br", "main を topic-br の上へリベース") + yes("リベースしました"), [("main は topic-br の上", lambda r, h: ancestor(r, h["t1-topic"], "main") and branch(r) == "main")]),
    Case("br-merge", "X を main へマージ", LOG + menu("topic-br", "topic-br を main へマージ") + "<WaitFor:topic-br をマージしました>", [("マージした", lambda r, h: ancestor(r, h["t1-topic"], "main") and len(git(r, "log", "-1", "--format=%P").split()) == 2)]),
    Case("br-merge-conflict", "X を main へマージ（衝突: 衝突したまま止まる）", LOG + menu("conflict-br", "conflict-br を main へマージ") + "<WaitFor:Automatic merge failed>", [("衝突の途中で止まる", lambda r, h: (r / ".git" / "MERGE_HEAD").exists())]),
    Case(
        "br-rebase-conflict",
        "main を X の上へリベース（衝突: マージと同じく途中で止める）",
        LOG + menu("conflict-br", "main を conflict-br の上へリベース") + yes("リベースの途中で止めました"),
        [("リベースの途中", lambda r, h: (r / ".git" / "rebase-merge").exists() and any(l.startswith("UU") for l in status(r)))],
    ),
    Case(
        "br-checkout-rebase-conflict",
        "チェックアウトして main の上へリベース（衝突: 途中で止める）",
        LOG + menu("conflict-br", "チェックアウトして main の上へリベース") + yes("リベースの途中で止めました"),
        [("リベースの途中", lambda r, h: (r / ".git" / "rebase-merge").exists())],
    ),
    Case("br-rename", "名前を変える…", LOG + menu("topic-br", "名前を変える…") + "<Shot:cmdline>renamed-br<CR><WaitFor:topic-br を renamed-br に変えました>", [("名前が変わった", lambda r, h: rev(r, "renamed-br") == h["t1-topic"] and rev(r, "topic-br") == "")]),
    Case("br-push", "プッシュ", LOG + menu("topic-br", "プッシュ") + "<WaitFor:push しました>", [("origin に届いた", lambda r, h: git(r.parent / "remote.git", "rev-parse", "topic-br", check=False) == h["t1-topic"])]),
    Case("br-delete-merged", "削除（マージ済み）", LOG + menu("merged-br", "削除") + yes("ブランチ merged-br を消しました"), [("消えた", lambda r, h: rev(r, "merged-br") == "")]),
    Case(
        "br-delete-unmerged",
        "削除（マージしていない: 2 回目の確認で -D）",
        LOG + menu("topic-br", "削除") + "<WaitFor:はい（Y）><Click:はい（Y）><WaitFor:マージされていません>" + yes("ブランチ topic-br を消しました"),
        [("消えた", lambda r, h: rev(r, "topic-br") == "")],
    ),
    Case("br-delete-no", "削除（いいえ）", LOG + menu("merged-br", "削除") + NO, [("残る", lambda r, h: rev(r, "merged-br") == h["c2-second"])]),
    Case("br-menu-current", "今のブランチのメニュー（チェックアウト・比べる・削除が無い）", LOG + "<RClick:* main><WaitFor:main から新しいブランチ…><Shot:menu><NotSee:main と比べる><NotSee:削除><See:名前を変える…><See:プッシュ>"),
    Case("br-menu-remote", "リモートのメニュー（名前を変える・プッシュ・削除が無い）", LOG + "<RClick:origin/remote-br><WaitFor:origin/remote-br から新しいブランチ…><Shot:menu><NotSee:名前を変える…><NotSee:削除>"),
    Case("br-menu-tag", "タグのメニュー（比べる・リベース・マージが無い）", LOG + "<RClick:v1-tag><WaitFor:v1-tag から新しいブランチ…><Shot:menu><NotSee:main と比べる><NotSee:マージ>"),
    # ---------------- 履歴: コミットの列 ----------------
    Case("log-copy-hash", "ハッシュをコピー", LOG + menu("c2-second", "ハッシュをコピー") + "<WaitFor:コピーしました>"),
    Case("log-cherry-pick", "チェリーピック", LOG + menu("t1-topic", "チェリーピック") + yes("チェリーピックしました"), [("main に t1 の中身", lambda r, h: sub(r) == "t1-topic" and rev(r, "HEAD~1") == h["c4-fourth"])]),
    Case(
        "log-cherry-pick-conflict",
        "チェリーピック（衝突: やめて元に戻す）",
        LOG + menu("x1-conflict", "チェリーピック") + yes("チェリーピックをやめて元に戻しました"),
        [("元のまま", lambda r, h: rev(r, "HEAD") == h["c4-fourth"] and status(r) == [] and not (r / ".git" / "CHERRY_PICK_HEAD").exists())],
    ),
    Case("log-checkout", "このリビジョンをチェックアウト", LOG + menu("c2-second", "このリビジョンをチェックアウト") + yes("チェックアウトしました"), [("HEAD は c2（切り離し）", lambda r, h: rev(r, "HEAD") == h["c2-second"] and branch(r) == "HEAD")]),
    Case("log-new-branch", "新しいブランチ…", LOG + menu("c2-second", "新しいブランチ…") + "<Shot:cmdline>from-c2<CR><Wait:500>", [("from-c2 は c2", lambda r, h: rev(r, "from-c2") == h["c2-second"])]),
    Case("log-new-tag", "新しいタグ…", LOG + menu("c2-second", "新しいタグ…") + "<Shot:cmdline>tag-e2e<CR><WaitFor:タグ tag-e2e を付けました>", [("タグは c2", lambda r, h: rev(r, "tag-e2e") == h["c2-second"])]),
    Case("log-reset-soft", "リセット（--soft）", LOG + menu("c2-second", "main をここまでリセット（--soft）") + yes("リセットしました（--soft）"), [("HEAD は c2", lambda r, h: rev(r, "HEAD") == h["c2-second"]), ("変更は index に", lambda r, h: "M  a.txt" in status(r) and "A  c.txt" in status(r))]),
    Case("log-reset-mixed", "リセット（--mixed）", LOG + menu("c2-second", "main をここまでリセット（--mixed）") + yes("リセットしました（--mixed）"), [("HEAD は c2", lambda r, h: rev(r, "HEAD") == h["c2-second"]), ("変更は作業ツリーに", lambda r, h: " M a.txt" in status(r) and "?? c.txt" in status(r))]),
    Case("log-reset-hard", "リセット（--hard）", LOG + menu("c2-second", "main をここまでリセット（--hard）") + yes("リセットしました（--hard）"), [("HEAD は c2", lambda r, h: rev(r, "HEAD") == h["c2-second"]), ("作業ツリーも c2", lambda r, h: status(r) == [] and read(r, "a.txt") == "a1\na2\n")]),
    Case("log-reset-keep", "リセット（--keep）", LOG + menu("c2-second", "main をここまでリセット（--keep）") + yes("リセットしました（--keep）"), [("HEAD は c2", lambda r, h: rev(r, "HEAD") == h["c2-second"])]),
    Case(
        "log-reset-keep-conflict",
        "リセット（--keep。作業中の変更とぶつかる: 失敗する）",
        LOG + menu("c2-second", "main をここまでリセット（--keep）") + yes("git:"),
        [("HEAD は動かない", lambda r, h: rev(r, "HEAD") == h["c4-fourth"]), ("変更は残る", lambda r, h: read(r, "a.txt") == "a1\na4\nlocal\n")],
        prepare=dirty_wt,
    ),
    Case("log-revert", "コミットを取り消す（revert）", LOG + menu("c3-third", "コミットを取り消す（revert）") + yes("取り消すコミットを足しました"), [("c.txt が消えた", lambda r, h: read(r, "c.txt") is None and rev(r, "HEAD~1") == h["c4-fourth"])]),
    Case("log-undo", "コミットを元に戻す（undo。HEAD だけ）", LOG + menu("c4-fourth", "コミットを元に戻す（undo）") + yes("コミットを取り消しました"), [("HEAD は c3", lambda r, h: rev(r, "HEAD") == h["c3-third"]), ("変更は残る", lambda r, h: "M  a.txt" in status(r))]),
    Case("log-undo-hidden", "HEAD 以外には undo が無い", LOG + "<RClick:c3-third><WaitFor:チェリーピック><Shot:menu><NotSee:コミットを元に戻す（undo）>"),
    Case("log-reword-head", "メッセージを編集…（HEAD）", LOG + menu("c4-fourth", "メッセージを編集…") + "<Shot:cmdline>new-<CR><WaitFor:メッセージを直しました>", [("件名", lambda r, h: sub(r) == "new-c4-fourth")]),
    Case("log-reword-old", "メッセージを編集…（古いコミット）", LOG + menu("c2-second", "メッセージを編集…") + "new-<CR><WaitFor:メッセージを直しました>", [("件名", lambda r, h: subjects(r) == ["c4-fourth", "c3-third", "new-c2-second", "c1-first"])]),
    Case("log-fixup", "1 つ前にまとめる（フィックスアップ）", LOG + menu("c3-third", "1 つ前にまとめる（フィックスアップ）") + yes("1 つ前のコミットにまとめました"), [("件名の並び", lambda r, h: subjects(r) == ["c4-fourth", "c2-second", "c1-first"]), ("c.txt は残る", lambda r, h: read(r, "c.txt") == "c1\n")]),
    Case("log-squash", "1 つ前にまとめる（スカッシュ）", LOG + menu("c3-third", "1 つ前にまとめる（スカッシュ）") + yes("1 つ前のコミットにまとめました"), [("メッセージをつなぐ", lambda r, h: "c3-third" in git(r, "log", "-1", "--format=%B", "HEAD~1") and sub(r, "HEAD~1") == "c2-second")]),
    Case("log-fixup-root", "1 つ前にまとめる（最初のコミット: できない）", LOG + menu("c1-first", "1 つ前にまとめる（フィックスアップ）") + yes("git:"), [("変わらない", lambda r, h: rev(r, "HEAD") == h["c4-fourth"])]),
    Case("log-drop", "コミットを消す", LOG + menu("c3-third", "コミットを消す") + yes("コミットを消しました"), [("件名の並び", lambda r, h: subjects(r) == ["c4-fourth", "c2-second", "c1-first"]), ("c.txt は無い", lambda r, h: read(r, "c.txt") is None)]),
    Case("log-rebase", "main をここへリベース", LOG + menu("t1-topic", "main をここへリベース") + yes("リベースしました"), [("main は t1 の上", lambda r, h: ancestor(r, h["t1-topic"], "main"))]),
    Case("log-push-to", "ここまでプッシュ（origin/main）", LOG + menu("c4-fourth", "ここまでプッシュ（origin/main）") + yes("push しました"), [("origin の main は c4", lambda r, h: git(r.parent / "remote.git", "rev-parse", "main") == h["c4-fourth"])]),
    Case("log-push-to-hidden", "追跡先が無ければ「ここまでプッシュ」が無い", LOG + "<RClick:c4-fourth><WaitFor:チェリーピック><Shot:menu><NotSee:ここまでプッシュ>", prepare=lambda r, h: git(r, "branch", "--unset-upstream")),
    Case("log-parent", "親のコミットへ", LOG + "<Click:c3-third><Wait:500>" + menu("c3-third", "親のコミットへ") + "<WaitFor:親 {c1-first}>"),
    # ---------------- 履歴: ファイルの列（選んだコミットの変えたファイル） ----------------
    Case("file-diff", "差分を見る（このコミットでの変更）", C2 + menu("a.txt", "差分を見る（このコミットでの変更）") + "<WaitFor:{c2-second}^  a.txt><See:a2>"),
    Case("file-compare", "作業中のファイルと比べる", C2 + menu("a.txt", "作業中のファイルと比べる") + "<WaitFor:作業中  a.txt><See:local>", prepare=dirty_wt),
    Case("file-open", "ファイルを開く", C2 + menu("a.txt", "ファイルを開く") + "<WaitGone:-- 履歴 --><WaitFor:a4>"),
    Case("file-open-missing", "ファイルを開く（作業ツリーに無い）", LOG + "<Click:c3-third><WaitFor:親 {c2-second}>" + menu("c.txt", "ファイルを開く") + "<WaitFor:c.txt は作業ツリーにありません>", prepare=lambda r, h: (r / "c.txt").unlink()),
    Case("file-history", "このファイルの履歴", C2 + menu("a.txt", "このファイルの履歴") + "<WaitGone:c3-third><WaitFor:c4-fourth><See:c1-first>"),
    Case("file-restore", "この版に戻す…", C2 + menu("a.txt", "この版に戻す…") + yes("a.txt を {c2-second} の版に戻しました"), [("a.txt が c2 の版", lambda r, h: read(r, "a.txt") == "a1\na2\n")]),
    Case("file-restore-no", "この版に戻す…（いいえ）", C2 + menu("a.txt", "この版に戻す…") + NO, [("a.txt はそのまま", lambda r, h: read(r, "a.txt") == "a1\na4\n")]),
    Case("file-copy-path", "パスをコピー", C2 + menu("a.txt", "パスをコピー") + "<WaitFor:コピーしました>"),
    # ---------------- 変更の差分の小窓（参考） ----------------
]


def run_case(c: Case, base: Path, exe: Path) -> dict:
    d = base / c.name
    d.mkdir(parents=True)
    h = make_repo(d, c.dirty)
    if c.prepare:
        c.prepare(d / "repo", h)
    appdata = d / "appdata" / "nagi"
    appdata.mkdir(parents=True)
    (d / "appdata" / "empty.vimrc").write_text("", encoding="utf-8")
    (appdata / "settings.conf").write_text(f"vim.enabled = true\nvim.rc_path = {winpath(d / 'appdata' / 'empty.vimrc')}\n", encoding="utf-8")
    (d / "out").mkdir()
    # {コミットの名前} は短いハッシュに
    steps = re.sub(r"\{([a-z0-9]+-[a-z]+)\}", lambda m: h[m.group(1)][:7], c.steps)
    (d / "steps.txt").write_text(steps + "<Shot:end><Quit>", encoding="utf-8")
    env = dict(os.environ)
    env.update(
        LOCALAPPDATA=winpath(d / "appdata"),
        NAGI_REPLAY_LOG=winpath(d) + "\\log.txt",
        NAGI_REPLAY_OUT=winpath(d / "out"),
        WSLENV="LOCALAPPDATA:NAGI_REPLAY_LOG:NAGI_REPLAY_OUT",
    )
    logf = d / "log.txt"
    # 窓は表に出さない（ほかの作業の入力を取らない）
    p = subprocess.Popen([str(exe), winpath(d / "repo" / OPEN), "--headless", "--replay-file", winpath(d / "steps.txt")], env=env)
    start, during = time.time(), c.during
    while p.poll() is None and time.time() - start < 120:
        if during and logf.exists() and during[0] in logf.read_text(encoding="utf-8"):
            during[1](d / "repo")
            during = None
        time.sleep(0.1)
    if p.poll() is None:
        p.kill()
        exit_code = "timeout"
    else:
        exit_code = p.returncode
    log = logf.read_text(encoding="utf-8") if logf.exists() else ""
    problems = [l for l in log.splitlines() if l.startswith("NG")]
    if "INFO quit visible=true" in log:
        problems.append("NG 窓が表に出ていた（--headless なのに）")
    if "INFO quit" not in log:
        problems.append("NG 手順の最後（<Quit>）まで行っていない（記録が欠けた・窓が閉じられた）")
    if exit_code != 0:
        problems.append(f"NG nagi の終わり方: {exit_code}")
    repo = d / "repo"
    for desc, ok in c.checks:
        try:
            good = ok(repo, h)
        except Exception as e:  # noqa: BLE001 確かめの失敗も NG として残す
            good, desc = False, f"{desc}（{e}）"
        if not good:
            problems.append(f"NG {desc}")
    if any(p.startswith("NG") for p in problems):
        problems.append("    状態: " + " / ".join(status(repo)) + "   履歴: " + git(repo, "log", "--oneline", "--all", "-8", check=False).replace("\n", " / "))
    shots = []
    for bmp in sorted((d / "out").glob("*.bmp")):
        try:
            from PIL import Image

            Image.open(bmp).save(bmp.with_suffix(".png"))
            bmp.unlink()
            shots.append(bmp.with_suffix(".png"))
        except ImportError:
            shots.append(bmp)
    return {"name": c.name, "what": c.what, "ok": not any(p.startswith("NG") for p in problems), "problems": problems, "shots": shots, "dir": d}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--exe", default=str(ROOT / "target" / "debug" / "nagi.exe"))
    ap.add_argument("only", nargs="*")
    a = ap.parse_args()
    base = ROOT / "target" / "e2e" / datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    cases = [c for c in CASES if not a.only or any(o in c.name for o in a.only)]
    # 「ハッシュをコピー」はクリップボードを書き換えるので、今の文字を控えて最後に戻す
    ps = ["powershell.exe", "-NoProfile", "-Command"]
    clip = subprocess.run(ps + ["Get-Clipboard -Raw"], capture_output=True, text=True, encoding="utf-8").stdout
    results = []
    for c in cases:
        r = run_case(c, base, Path(a.exe))
        results.append(r)
        print(f"{'OK' if r['ok'] else 'NG'}  {c.name}  {c.what}", flush=True)
        for p in r["problems"]:
            print(f"      {p}", flush=True)
    lines = [f"# git の右クリックのメニュー E2E（{base.name}）", "", f"{sum(r['ok'] for r in results)} / {len(results)} 件 OK", ""]
    for r in results:
        lines.append(f"## {'OK' if r['ok'] else 'NG'} {r['name']}  {r['what']}")
        lines += [f"- {p}" for p in r["problems"]]
        lines += [f"- 写し: {s.relative_to(base)}" for s in r["shots"]]
        lines.append("")
    if clip:
        subprocess.run(ps + ["$input | Set-Clipboard"], input=clip.rstrip("\r\n"), text=True, encoding="utf-8")
    (base / "summary.md").write_text("\n".join(lines), encoding="utf-8")
    print(f"\n{sum(r['ok'] for r in results)} / {len(results)} 件 OK  →  {base}")
    return 0 if all(r["ok"] for r in results) else 1


if __name__ == "__main__":
    sys.exit(main())
