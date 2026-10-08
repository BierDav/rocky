#!/bin/sh
# Matched observer/history growth arms proposed for the phase 5/8 investigation.
# One arm at a time on the same machine; each stores its own raw output and a
# qualification.py profile. Instrumentation (subphases + observer deliveries) is
# on for every arm so the arms are comparable.
set -u
cd "$(dirname "$0")/../../../.." || exit 1
ROOT=$(pwd)
EVID="$ROOT/benchmarks/runtime-scaling/evidence/observer-growth-2026-10-06"
BIN="$ROOT/target/release/scaling_probe"
SOURCE_SHA=$(cat "$EVID/source-sha256.txt" 2>/dev/null || printf '%s' unknown)

run_arm() {
  dir="$EVID/$1"
  rows="$2"
  observers="$3"
  seconds="$4"
  mkdir -p "$dir"
  started=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  echo "=== arm $1 rows=$rows observers=$observers seconds=$seconds started=$started ==="
  "$BIN" soak \
    --directory "$dir/data" \
    --instrument \
    --tables 2000 \
    --observers "$observers" \
    --rows "$rows" \
    --seconds "$seconds" \
    --operations 5000000 \
    --interval 30 \
    --durability full > "$dir/soak.txt" 2>&1
  code=$?
  python3 benchmarks/runtime-scaling/qualification.py record-probe \
    --profile "Local Apple M5 Pro observer-growth rows-$rows-observers-$observers" \
    --directory "$dir" \
    --binary "$BIN" \
    --source-sha "$SOURCE_SHA" \
    --raw "$dir/soak.txt" \
    --started-at "$started" \
    --exit-code "$code" \
    --command "$BIN" soak \
      --directory "$dir/data" \
      --instrument \
      --tables 2000 \
      --observers "$observers" \
      --rows "$rows" \
      --seconds "$seconds" \
      --operations 5000000 \
      --interval 30 \
      --durability full
  echo "=== arm $1 exit=$code ==="
}

run_arm rows-100k-observers-100 100000 100 600
run_arm rows-100k-observers-0 100000 0 600
run_arm rows-500k-observers-100 500000 100 900
run_arm rows-500k-observers-0 500000 0 900
echo "observer-growth runs complete"
