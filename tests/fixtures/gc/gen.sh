#!/bin/sh
# Regenerate the expectations of tests/gc.rs with the reference LuaJIT (JIT disabled build).
# Usage: tests/fixtures/gc/gen.sh [path/to/luajit]
set -e
LJ=${1:-${LUAJIT_REF:?Pass a reference LuaJIT binary or set LUAJIT_REF}}
LJDIR=$(cd "$(dirname "$LJ")" && pwd)
LJ="$LJDIR/$(basename "$LJ")"
cd "$(dirname "$0")"
tmp=$(mktemp -d)
for f in *.lua; do
  name=${f%.lua}
  set +e
  "$LJ" "$f" < /dev/null > "$tmp/out.txt" 2> "$tmp/err.txt"
  code=$?
  set -e
  {
    cat "$tmp/out.txt"
    if [ -s "$tmp/err.txt" ]; then echo "--- stderr"; sed "s|$LJ|luajit|g" "$tmp/err.txt"; fi
    echo "--- exit $code"
  } | sed 's/0x[0-9a-f][0-9a-f]*/0x?/g' > "$name.expected"
done
rm -rf "$tmp"
