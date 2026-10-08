# `ccs-oracle-2` phase 5 recheck

Captured 2026-10-05 after the trusted full soak. This fresh-directory run used
one full-durability file, 2,000 tables, 16 domains × 8 clients, group 128,
16,384 checkpoint frames and 600 seconds. `profile.json` links to the soak
manifest for hardware and Rust toolchain provenance and records the exact
command, source and binary hashes. The executable exited 0; throughput and
latency budgets were missed. Raw interval and summary output is in `gate.txt`.
