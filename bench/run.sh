#!/usr/bin/env bash
# Compare benchmark timings (best of N runs) of luajit-rs against the reference.
#
# Usage: bench/run.sh [-n RUNS] [-c] [-p CPU] [[LABEL=]BIN...] [-- BENCH...]
#   -n     runs per benchmark and binary (default 3); the best one is reported
#   -c     report user-mode CPU cycles (10^9, via `perf stat`) instead of wall-clock seconds;
#          less sensitive to machine load (combine with -p on hybrid CPUs)
#   -p     pin the benchmarked process to CPU number CPU (`taskset`), e.g. a performance core
#   BIN    binaries to time (default: ref=reference, rs=target/release/luajit)
#   BENCH  benchmark names without .lua (default: all bench/*.lua)
# Env: LUAJIT_REF (default ~/projects/luajit-ref/luajit/src/luajit),
#      CARGO_TARGET_DIR (to locate the release binary).
# Each benchmark's output must equal the reference's; mismatches are flagged with '!'.
set -euo pipefail
cd "$(dirname "$0")"

runs=3
cycles=
pin=()
while [[ ${1-} == -[ncp] ]]; do
    case $1 in
        -n) runs=$2; shift 2 ;;
        -c) cycles=1; shift ;;
        -p) pin=(taskset -c "$2"); shift 2 ;;
    esac
done
ref=${LUAJIT_REF:-$HOME/projects/luajit-ref/luajit/src/luajit}
bins=()
while [[ $# -gt 0 && $1 != -- ]]; do bins+=("$1"); shift; done
[[ ${1-} == -- ]] && shift
if [[ ${#bins[@]} -eq 0 ]]; then
    bins=("ref=$ref" "rs=${CARGO_TARGET_DIR:-../target}/release/luajit")
fi
benches=("$@")
if [[ ${#benches[@]} -eq 0 ]]; then
    for f in *.lua; do benches+=("${f%.lua}"); done
fi

measure() { # bin script -> milliseconds or megacycles of one run
    if [[ -n $cycles ]]; then
        # Hybrid CPUs report one line per core type; sum the counted ones.
        local sum=0 line
        while IFS=, read -r line _; do
            [[ $line =~ ^[0-9]+$ ]] && sum=$((sum + line))
        done < <("${pin[@]}" perf stat -x, -e cycles:u -- "$1" "$2" 2>&1 > /dev/null)
        echo $((sum / 1000000))
    else
        local t0 t1
        t0=$(date +%s%N)
        "${pin[@]}" "$1" "$2" > /dev/null
        t1=$(date +%s%N)
        echo $(((t1 - t0) / 1000000))
    fi
}

best_time() { # bin script -> best result, in seconds or 10^9 cycles
    local best=
    for ((i = 0; i < runs; i++)); do
        local t
        t=$(measure "$1" "$2")
        if [[ -z $best || $t -lt $best ]]; then best=$t; fi
    done
    printf '%d.%03d' $((best / 1000)) $((best % 1000))
}

printf '%-12s' benchmark
labels=()
for i in "${!bins[@]}"; do
    b=${bins[$i]}
    if [[ $b == *=* ]]; then labels[i]=${b%%=*}; bins[i]=${b#*=}; else labels[i]=$(basename "$b"); fi
done
for l in "${labels[@]}"; do printf ' %12s' "$l"; done
printf '\n'
for name in "${benches[@]}"; do
    script=$name.lua
    expect=$("$ref" "$script")
    printf '%-12s' "$name"
    for b in "${bins[@]}"; do
        mark=' '
        [[ $("$b" "$script") == "$expect" ]] || mark='!'
        printf ' %11s%s' "$(best_time "$b" "$script")" "$mark"
    done
    printf '\n'
done
