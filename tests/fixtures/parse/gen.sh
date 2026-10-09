#!/usr/bin/env bash
# Regenerate parser goldens from the reference LuaJIT binary.
#   *.lua         -> *.lua.bl   (exact stdout of `luajit -bl NAME.lua`, run in this dir)
#   *.lua         -> *.lua.blw  (same with -bWl: non-native FR1 frame layout)
#   *.lua         -> *.lua.raw  (`luajit -bgd -t raw`: deterministic dump with debug info)
#   errors/*.lua  -> errors/*.err (exact loadstring() error message, no trailing newline)
# Override the binary with $LUAJIT_REF.
set -euo pipefail

LUAJIT_REF="${LUAJIT_REF:-$HOME/projects/luajit-ref/luajit/src/luajit}"
REF_SRC="$(cd "$(dirname "$LUAJIT_REF")" && pwd)"
export LUA_PATH="$REF_SRC/?.lua;;"

cd "$(dirname "${BASH_SOURCE[0]}")"

for f in *.lua; do
  "$LUAJIT_REF" -bl "$f" > "$f.bl.tmp"
  if [ ! -s "$f.bl.tmp" ]; then
    echo "gen.sh: empty listing for $f" >&2
    exit 1
  fi
  mv "$f.bl.tmp" "$f.bl"
  "$LUAJIT_REF" -bWl "$f" > "$f.blw"
  "$LUAJIT_REF" -bgd -t raw "$f" "$f.raw"
done

cd errors
for f in *.lua; do
  "$LUAJIT_REF" - "$f" > "${f%.lua}.err" <<'EOF'
local f = assert(io.open(arg[1], 'rb'))
local s = f:read('*a')
f:close()
local fn, err = loadstring(s)
assert(not fn, 'compiled OK: ' .. arg[1])
io.write(err)
EOF
done
