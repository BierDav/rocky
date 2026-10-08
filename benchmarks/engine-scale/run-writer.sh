#!/bin/sh
# SQLite batched-writer probes, one process at a time. Build first; see README.md.
# usage: ./run-writer.sh [results-dir] [repetitions] [sections]
#   sections: any of switch batched layout layout-extra one-client (default: all)
#   data goes to $ENGINE_SCALE_DATA, default /tmp/engine-scale-writer
set -eu
cd "$(dirname "$0")"
out=${1:-results/raw}
reps=${2:-3}
sections=${3:-switch batched layout layout-extra one-client}
data=${ENGINE_SCALE_DATA:-/tmp/engine-scale-writer}
bin=target/release/sqlite-writer
mkdir -p "$out"

# probe <label> <wal-checkpoint-MiB> <mode> <args...>
probe() {
    label=$1 wal=$2 mode=$3
    shift 3
    rm -rf "$data"
    echo "== $label $mode $*" >&2
    env WRITER_WAL_CHECKPOINT_BYTES=$((wal * 1048576)) ENGINE_SCALE_VARIANT="$label" \
        ENGINE_SCALE_OUT="$out/sqlite-writer.jsonl" "$bin" "$mode" "$data" "$@" >/dev/null
    rm -rf "$data"
}

has() { case " $sections " in *" $1 "*) return 0 ;; *) return 1 ;; esac; }

rep=1
while [ "$rep" -le "$reps" ]; do
    if has switch; then probe sync-switch 0 sync-switch; fi
    # One file, 2,000 tables and 500,000 rows split over 1, 8 or 16 domains.
    if has batched; then
        for wal in 4 64; do
            for sync in full normal; do
                for clients in 1 8 16; do
                    probe "batched-wal$wal" "$wal" batched 1 "$clients" $((2000 / clients)) 250 3 "$sync"
                done
            done
        done
    fi
    # 16 domains of 8 tables, 1,000 rows each, in 1, 4 or 16 files.
    if has layout; then
        for sync in full normal; do
            for files in 1 4 16; do
                probe layout-wal64 64 batched "$files" 16 8 1000 3 "$sync"
            done
        done
    fi
    # The same with checkpoints at 4 MiB, and with none inside the window.
    if has layout-extra; then
        for wal in 4 1024; do
            for sync in full normal; do
                for files in 1 4 16; do
                    probe "layout-wal$wal" "$wal" batched "$files" 16 8 1000 3 "$sync"
                done
            done
        done
    fi
    # One durable client at 20 and 2,000 tables, against the single-writer figures.
    if has one-client; then
        for tables in 20 2000; do
            probe batched-1client-wal64 64 batched 1 1 "$tables" $((500000 / tables)) 3 full
        done
    fi
    rep=$((rep + 1))
done
