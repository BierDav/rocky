#!/bin/sh
# Strong commands on one long-lived domain under default retention (phase 11).
set -eu
cd "$(dirname "$0")/../../../.."
cargo build --release -p runtime-scaling --bin scaling_probe --locked
./target/release/scaling_probe strong-cut | tee benchmarks/runtime-scaling/evidence/strong-cut-2026-10-08/run.log
