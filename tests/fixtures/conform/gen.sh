#!/bin/sh
# Regenerate the expectations of tests/conform.rs with the reference LuaJIT (JIT disabled
# build). Usage: tests/fixtures/conform/gen.sh [path/to/luajit]
set -e
LJ=${1:-${LUAJIT_REF:?Pass a reference LuaJIT binary or set LUAJIT_REF}}
LJDIR=$(cd "$(dirname "$LJ")" && pwd)
LJ="$LJDIR/$(basename "$LJ")"
cd "$(dirname "$0")"
FIX=$(pwd)
grep -v '^#' cases.txt | while IFS='|' read -r name env args; do
  name=$(echo $name)
  tmp=$(mktemp -d)
  cp -r src "$tmp/"
  stdin=/dev/null
  [ -f "$FIX/$name.stdin" ] && stdin="$FIX/$name.stdin"
  (
    cd "$tmp"
    for kv in $(echo "$env" | tr ',' ' '); do export "$kv"; done
    set +e
    LUA_PATH="$LJDIR/?.lua;;" "$LJ" $args < "$stdin" > out.txt 2> err.txt
    echo $? > code.txt
  )
  {
    cat "$tmp/out.txt"
    if [ -s "$tmp/err.txt" ]; then echo "--- stderr"; sed "s|$LJ|luajit|g" "$tmp/err.txt"; fi
    echo "--- exit $(cat "$tmp/code.txt")"
  } | sed 's/0x[0-9a-f][0-9a-f]*/0x?/g' > "$name.expected"
  rm -rf "$tmp"
done
