# `david-server` phase 5 recheck

Captured 2026-10-05 after the trusted full soak completed. This fresh-directory
run used one file, full durability, 2,000 tables, 16 domains × 8 clients, group
128, 16,384 checkpoint frames and 600 seconds. `profile.json` records the source
archive and binary hashes, platform, filesystem, exact command and exit status.

The executable exited 0, but the throughput and latency budgets were missed:
local writes averaged 3,291/s, the slowest 10-second interval was 2,100/s, and
write p99 was 158.7 ms. Strong-command p99 was 919.5 ms. Percentiles are
conservative histogram bucket upper bounds. The raw output is `gate.txt`.
