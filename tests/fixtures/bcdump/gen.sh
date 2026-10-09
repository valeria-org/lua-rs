#!/bin/sh
# Regenerate the bytecode dump goldens with the reference LuaJIT (JIT disabled build).
# Usage: tests/fixtures/bcdump/gen.sh [path/to/luajit]
set -e
LJ=${1:-$HOME/projects/luajit-ref/luajit/src/luajit}
LJDIR=$(cd "$(dirname "$LJ")" && pwd)
export LUA_PATH="$LJDIR/?.lua;;"
cd "$(dirname "$0")"
rm -f golden/*
cd corpus
for src in *.lua; do
  name=${src%.lua}
  "$LJ" -b -s -t raw "$src" "../golden/$name.s.raw"
  "$LJ" -b -g -t raw "$src" "../golden/$name.g.raw"
  "$LJ" -b -s -d -t raw "$src" "../golden/$name.sd.raw"
  "$LJ" -b -g -d -t raw "$src" "../golden/$name.gd.raw"
  "$LJ" ../describe.lua "../golden/$name.s.raw" > "../golden/$name.s.txt"
  "$LJ" ../describe.lua "../golden/$name.g.raw" > "../golden/$name.g.txt"
done
cd ..
LOADCHECK=$(mktemp)
trap 'rm -f "$LOADCHECK"' EXIT
cc -O1 -I"$LJDIR" -o "$LOADCHECK" loadcheck.c "$LJDIR/libluajit.a" -lm -ldl
LOADCHECK="$LOADCHECK" "$LJ" mkerrors.lua > golden/errors.txt
