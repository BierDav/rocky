# Local Apple M5 Pro phase 5 recheck

Captured 2026-10-05 after the completed M5 trusted soak. This fresh-directory
run used one full-durability file, 2,000 tables, 16 domains × 8 clients, group
128, 16,384 checkpoint frames and 600 seconds. `profile.json` records the
machine, compiler, filesystem, source and binary hashes, exact arguments and
exit status. The executable exited 0, but the performance budgets were missed:
5,076 local writes/s average, 4,368/s in the slowest 10-second interval, 40 of
60 intervals below 5,000/s, and 128.9 ms local write p99. Raw output is in
`gate.txt`.
