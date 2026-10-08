#!/bin/sh
# Runs the matrix one process at a time. Build first; see README.md.
# usage: ./run.sh [results-dir]   (data goes to $ENGINE_SCALE_DATA, default /tmp/engine-scale)
set -u
cd "$(dirname "$0")"
out=${1:-results/raw}
data=${ENGINE_SCALE_DATA:-/tmp/engine-scale}
mkdir -p "$out"
bin=target/release

# case <label> <binary> <tables> <rows> [ENV=value ...]
case_() {
    label=$1 binary=$2 tables=$3 rows=$4
    shift 4
    dir="$data/$label-$tables-$rows"
    rm -rf "$dir"
    for mode in load reopen reopen-durable; do
        echo "== $label $tables $rows $mode" >&2
        # 40 minutes per process; a slower engine is recorded as a timeout.
        env "$@" ENGINE_SCALE_VARIANT="$label" ENGINE_SCALE_OUT="$out/$label.jsonl" \
            perl -e 'alarm 2400; exec @ARGV' /usr/bin/time -l "$binary" "$mode" "$dir" "$tables" "$rows" \
            >/dev/null 2>"$out/$label-$tables-$rows-$mode.err"
        status=$?
        peak=$(awk '/maximum resident set size/ {print int($1/1048576)}' "$out/$label-$tables-$rows-$mode.err")
        printf '{"variant":"%s","tables":%s,"rows":%s,"mode":"%s","metric":"process","exit":%s,"peak_resident_mib":%s}\n' \
            "$label" "$tables" "$rows" "$mode" "$status" "${peak:-0}" >>"$out/$label.jsonl"
        [ "$status" -eq 0 ] || break
    done
    rm -rf "$dir"
}

sizes="20:500000 2000:500000 2000:5000000"
for size in $sizes 16000:500000; do
    case_ sqlite $bin/sqlite-engine "${size%:*}" "${size#*:}"
    case_ redb $bin/redb-engine "${size%:*}" "${size#*:}"
done
for size in $sizes; do
    case_ turso-patched turso-patched/target/release/turso-patched-engine "${size%:*}" "${size#*:}"
    case_ turso-patched-cache256 turso-patched/target/release/turso-patched-engine "${size%:*}" "${size#*:}" TURSO_CACHE_KIB=262144
    case_ fjall-prefixed $bin/fjall-engine "${size%:*}" "${size#*:}" KV_LAYOUT=prefixed
done
# Slow or memory-hungry at 2,000 tables; the 5,000,000-row case is left out.
for size in 20:500000 2000:500000; do
    case_ turso $bin/turso-engine "${size%:*}" "${size#*:}"
    case_ duckdb $bin/duckdb-engine "${size%:*}" "${size#*:}"
done
