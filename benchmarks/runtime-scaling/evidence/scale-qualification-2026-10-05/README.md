# Phase 8 qualification evidence

Recorded 2026-10-05. These are short trusted storage runs, not a phase 8 pass.
Each successful run used three domains, 2,000 logical tables per domain,
5,000 main-domain live rows, 100 observers and full durability. Raw output and
hardware/source/binary manifests are in the named directories.

The initial `x86` and `arm` runs failed with `Storage(Deadline)` before load.
`audit-after.txt` records independent recovery inspection; it does not qualify
the failed run. SQLite schema loading was moved to read-only connection setup,
before the unchanged application snapshot deadline. `x86-after` and `arm-after`
are fresh runs of the fixed build, with final close/reopen/audits and applied
frontiers checked. `mac` predates that fix. Source hashes in each manifest
identify the actual build; do not treat all runs as one identical binary.

ARM's 2,735 ms smoke reopen already exceeds the 2 s target. Installation covers
three wide domains and cannot be compared directly to the 10 s single-domain
budget. CPU/storage are shared VM/cloud profiles; no physical SSD power-loss
claim follows from them.

The deployed archive SHA256 is
`0f82b39d9c0e6c42890017fd81bac8d556180ea39bdb077af0e4879e4330d393`.
The initial local full run was interrupted; its [raw output and recovery audit](local-mac-full-interrupted/README.md)
are preserved as failed qualification evidence. A fresh local run is owned by
launchd. Its local toolchain differs from the remote builds, as recorded there.
All six runs have completed at three hours and at least 5,000,000 main-domain
operations. Remote raw output, profile manifests, final audit lines and
launcher statuses are in the `*-full` directories. The local M5 profile and
raw output are in `local-mac-full`; its launchd wrapper retried after the
successful run and overwrote its status file, as documented there. The trusted
runs do not qualify other topologies, encrypted storage or power-loss behavior. See the
[qualification guide](../../../../docs/runtime/scaling-limits.md#scale-qualification).

| Profile | Tables/domain | Live rows | Main operations | Observers | Peak RSS MiB | Reopen ms | Slice |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| Apple M5 Pro / APFS / Rust 1.95.0 / full durability | 2000 | 5000 | 53348 | 100 | 189 | 323 | passed |
| x86_64-KVM-i5-1235U-ext4 | 2000 | — | — | 100 | 62 | — | incomplete/failed |
| arm64-Neoverse-N1-cloud-block-ext4 | 2000 | — | — | 100 | 61 | — | incomplete/failed |
| x86_64-KVM-i5-1235U-ext4 | 2000 | 5000 | 53982 | 100 | 187 | 1470 | passed |
| arm64-Neoverse-N1-cloud-block-ext4 | 2000 | 5000 | 50302 | 100 | 186 | 2735 | passed |

## Completed 500,000-row trusted soaks

All five remote runs used three domains, 2,000 logical tables per domain,
100 observers, full durability and the pinned source archive above. Elapsed
time and operations both exceeded their configured minima. The operation count
below is from the final domain-1 audit. Peak RSS and database size are observed
values, not capacity limits.

| Host/profile | Elapsed s | Main operations | Main live rows | Reopen ms | Peak RSS MiB | Peak DB MiB | Point-read p50 upper bound µs | Step µs/write, first → last |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `david-server` / x86-64 KVM i5-1235U / ext4 | 10,804 | 6,448,914 | 500,000 | 1,567 | 210 | 369 | 511 | 253.19 → 411.42 |
| `ccs-oracle-1` / Neoverse-N1 cloud block / ext4 | 10,804 | 9,387,974 | 500,000 | 2,499 | 207 | 385 | 1,023 | 238.08 → 415.62 |
| `ccs-oracle-2` / Neoverse-N1 cloud block / ext4 | 10,805 | 9,236,361 | 500,000 | 2,483 | 327 | 384 | 511 | 232.49 → 392.02 |
| `ccs-oracle-3` / Neoverse-N1 cloud block / ext4 | 10,804 | 8,699,317 | 500,000 | 2,370 | 209 | 381 | 511 | 217.57 → 420.82 |
| `david-mm-linz` / Apple M4 / APFS | 10,805 | 8,682,443 | 500,000 | 1,033 | 201 | 381 | 511 | 260.14 → 381.78 |
| Local Apple M5 Pro / APFS / Homebrew Rust 1.99 | 10,807 | 11,402,819 | 500,000 | 626 | 253 | 388 | 255 | 218.90 → 288.82 |

Each final audit reported 500,000 live rows in domain 1 and 5,000 in each
smaller domain; all three audit lines reported recomputed state. The per-run
`profile.json` records exit status 0. Reopen exceeded the 2 s budget on all
three Oracle profiles; `ccs-oracle-2` also exceeded the 256 MiB RSS budget during
reopen. `ccs-oracle-1`'s conservative point-read p50 bound was 1,023 µs, so the
under-1-ms target is not demonstrated on that profile. Step time rose over each
run; it is a writer diagnostic and does not by itself identify the cost or
prove a plateau.

## Phase 5 recheck

All six 10-minute phase 5 gates used one full-durability file, 2,000 tables,
16 domains × 8 clients, group 128 and 16,384 checkpoint frames. All executables
returned status 0, but every profile missed the 5,000 writes/s in every
10-second interval target, and each missed its approximately 100 ms p99 target:

| Host | Local writes/s | Slowest 10 s writes/s | Intervals <5,000/s | Local write p99 | Checkpoint file flush mean |
| --- | ---: | ---: | ---: | ---: | ---: |
| `david-server` | 3,291 | 2,100 | 58 / 60 | 158.7 ms | 77.3 ms |
| `ccs-oracle-1` | 2,233 | 1,471 | 60 / 60 | 332.3 ms | 203.2 ms |
| `ccs-oracle-2` | 2,197 | 1,498 | 60 / 60 | 329.1 ms | 206.5 ms |
| `ccs-oracle-3` | 2,165 | 1,375 | 60 / 60 | 351.0 ms | 228.4 ms |
| `david-mm-linz` | 3,085 | 1,778 | 56 / 60 | 310.0 ms | 191.2 ms |
| Local Apple M5 Pro | 5,076 | 4,368 | 40 / 60 | 128.9 ms | 66.4 ms |

Raw gate outputs and provenance are in the host-named `*-phase5` directories.

### Diagnosis of the six phase 5 misses

The writer counters point to two limits. First, every file has one writer
thread; its per-operation step work occupied 40–56% of wall time, while commit
work occupied 27–46%. Those counters combine several costs: step time includes
admission, reduction, encoding and SQLite statements; commit time includes the
device flush for full-durability groups. They do not separate CPU from storage
waits, so they cannot identify which operation inside a step dominates.

Second, checkpointing was a substantial tail and throughput cost on the four
slowest profiles. Passive checkpoints occupied 11–23% of wall time there, and
the database-file flush averaged 191–228 ms per checkpoint. `david-server` and
the M5 averaged 77 ms and 66 ms respectively. WAL flush time was near zero on
the Oracle guests and only a few milliseconds on the Macs; page-copy time was
12–28 ms. Checkpoint maxima reached 590 ms on Oracle 1, 485 ms on Oracle 2,
636 ms on Oracle 3 and 1,028 ms on the M4, consistent with long writer stalls.

This explains why checkpoint handling deserves attention for p99 on the slower
profiles, but it is not a complete causal attribution: the reported mean flush
and maximum checkpoint durations do not identify which individual mutation
samples overlapped them. The M5 also missed its p99 target at 128.9 ms despite
better throughput; its 66 ms mean checkpoint file flush remains material.
Raising checkpoint thresholds or changing durability to improve the numbers
would trade WAL growth or durability behavior and is not justified by this
diagnosis. No settings were changed. A next diagnostic run should sample the
writer step and commit subphases and correlate individual write latency with
checkpoint intervals before selecting a performance change.

Phase 8 gate: pending. These are observed dimensions, not maximum supported limits.

| Budget or slice | Evidence still required |
| --- | --- |
| 2,000 tables; 10 s install | Single-domain install separated from the three-domain install |
| 5,000,000 live rows | The soak defaults to 500,000; a distinct target-row run is needed |
| Unbounded lifetime; bounded history | Six three-hour/5M-operation soaks pass at 500K live rows; 5M-row target and longer plateau analysis remain |
| Write cost within 2x | Matched empty/full workload, plus CPU attribution of growing step cost |
| Point read median <1 ms | `ccs-oracle-1` p50 upper bound 1,023 µs; target not demonstrated there; other five were <=511 µs |
| Observer cost | Changed-row and observer-count comparison at target size |
| Open <2 s | Missed on Oracle 1/2/3: 2,499 / 2,483 / 2,370 ms; interval opens remain unmeasured |
| RSS <256 MiB | `ccs-oracle-2` reached 327 MiB on reopen; other profiles peaked at 253 MiB or lower |
| 10,000-operation catch-up | Concurrent native topology with three voters and two partial replicas |
| 5,000 writes/s each interval; profile p99 | All six profiles missed; interval shortfalls and p99 are tabulated above |
| Fault correctness | Kill/disconnect/queue/checkpoint/prune/rebootstrap in the combined topology |
| Strong commands and effects | Run throughout the same workload; existing separate probes are insufficient |
| Encrypted scale | Same workload with real grants/keys; trusted measurements cannot qualify it |
| Durability | Power-loss/fault-device evidence for both classes; process death is insufficient |
