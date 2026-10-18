#!/usr/bin/env bash
# 文法の DLL を作り、GitHub のリリースに置く zip と、nagi に埋め込む SHA-256 の表を書く。
#
#   grammars/build.sh            21 個すべて
#   grammars/build.sh c go       指定したものだけ
#
# 出来るもの:
#   target/grammars/dist/grammar-<id>.dll   試験で使う（NAGI_GRAMMARS_DIR）
#   target/grammars/dist/grammar-<id>.zip   リリースに置く
#   crates/nagi-syntax/grammars.lock        <id> <DLL の SHA-256> <zip の大きさ>（nagi はこれと合う DLL だけを読む）
#
# 置き方（文法を作り直したら、リリースの名前の版 grammars-vN を上げて nagi-syntax/src/grammar.rs の RELEASE も変える）:
#   gh release create grammars-v1 target/grammars/dist/grammar-*.zip --repo hashiohiro/nagi-grammars --title "Grammars v1"
set -euo pipefail
cd "$(dirname "$0")/.."
CARGO="${CARGO:-/mnt/c/Users/hashi/.cargo/bin/cargo.exe}"
ALL="c cpp go csharp php ruby lua scala swift dart zig haskell elixir r sql toml xml make cmake dockerfile groovy"
IDS="${*:-$ALL}"
DIST=target/grammars/dist
mkdir -p "$DIST"
for id in $IDS; do
  echo "== $id"
  for try in 1 2 3; do
    if "$CARGO" build -q --release --manifest-path grammars/Cargo.toml --features "$id" --target-dir target/grammars/build; then break; fi
    sleep 2
  done
  cp target/grammars/build/release/nagi_grammar.dll "$DIST/grammar-$id.dll"
  rm -f "$DIST/grammar-$id.zip"
  python3 -c "import zipfile,sys; z=zipfile.ZipFile(sys.argv[1],'w',zipfile.ZIP_DEFLATED,compresslevel=9); z.write(sys.argv[2],sys.argv[3]); z.close()" "$DIST/grammar-$id.zip" "$DIST/grammar-$id.dll" "grammar-$id.dll"
done
# 表はいつも 21 個すべてを書く（作っていないものは dist にある前のもの）
{
  echo "# 文法の DLL の SHA-256 と、リリースの zip の大きさ（grammars/build.sh が書く。手で直さない）"
  for id in $ALL; do
    [ -f "$DIST/grammar-$id.dll" ] || continue
    echo "$id $(sha256sum "$DIST/grammar-$id.dll" | cut -d' ' -f1) $(stat -c %s "$DIST/grammar-$id.zip")"
  done
} > crates/nagi-syntax/grammars.lock
cat crates/nagi-syntax/grammars.lock
