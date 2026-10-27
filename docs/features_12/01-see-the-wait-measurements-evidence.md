# Write probe measurements

Change `uuzywvqz`, 2026-10-27. All values below exclude warm-up.

Desktop: Apple M5 Pro, arm64 macOS; Pixel: physical Pixel 11 Pro, Android 17, arm64.
Release, `rocky-sdk --no-default-features --features qualification`; in-memory SDK transport.
Each row uses 600 writes paced at 100 ms, after 50 warm-up writes.
Finite L1/L3 loads may finish before the probe; long stalls extend the run beyond 60 seconds.
Sites are relative to `crates/rocky-secure/src/`; line numbers identify the measured revision.
These are latency baselines, not paired speedup claims. The desktop was shared with build/check work.

Reproduce on desktop (`ROCKY_PROBE_DURABILITY=Flushed` or `Buffered` selects one mode; default runs both):

```sh
cargo test --release -p rocky-sdk --no-default-features --features qualification \
  --test migrations write_probe:: -- --ignored --nocapture --test-threads=1
```

Android command (replace the test name with the scenario):

```sh
adb -s 66121FDKX001MF shell 'TMPDIR=/data/local/tmp/rocky-write-probe-uuzy-data /data/local/tmp/rocky-write-probe-uuzy --exact write_probe::l0_idle --ignored --nocapture --test-threads=1'
```

L1 uses the rebuilt `/data/local/tmp/rocky-write-probe-l1-fixed` binary.
Test names: `l0_idle`, `l1_bulk_receive`, `l2_small_receive`, `l3_local_maintenance`, `l4_checkpoint_pressure`.
Android runs the cross-compiled native test executable under `/data/local/tmp`, with a dedicated TMPDIR.
No application database is used. iPhone measurements are not available from this native Android harness.

## Desktop L0 Flushed

| Metric (microseconds) | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 76956 | 165647 | 319299 | 768925 |
| observed | 600 | 3 | 9 | 12 | 148 |
| write_lock_wait | 600 | 0 | 0 | 1 | 21 |
| write_lock_hold | 600 | 76684 | 165463 | 319130 | 768582 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| replica_flush_wait | 600 | 0 | 0 | 0 | 0 |

Load duration: 65.616 s; work: 0 (rows in L1/L3, authored rows in L2/L4).

Top holders by p99; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/sql.rs:749:14` | Interactive | 600 | 76300 | 163769 | 195740 | 476219 |
| `replica/sql.rs:1098:18` | Background | 600 | 283 | 726 | 1933 | 513619 |
| `replica/sql.rs:181:14` | Background | 600 | 101 | 172 | 334 | 613 |
| `replica/blockers.rs:510:14` | Background | 600 | 33 | 72 | 141 | 295 |
| `replica.rs:1498:36` | Background | 600 | 5 | 13 | 39 | 79 |
| `replica/session.rs:64:32` | Background | 600 | 4 | 6 | 38 | 122 |
| `replica/session.rs:298:14` | Background | 600 | 0 | 2 | 2 | 10 |
| `replica.rs:1478:36` | Background | 600 | 0 | 0 | 0 | 23 |

Top holders by max; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/sql.rs:1098:18` | Background | 600 | 283 | 726 | 1933 | 513619 |
| `replica/sql.rs:749:14` | Interactive | 600 | 76300 | 163769 | 195740 | 476219 |
| `replica/sql.rs:181:14` | Background | 600 | 101 | 172 | 334 | 613 |
| `replica/blockers.rs:510:14` | Background | 600 | 33 | 72 | 141 | 295 |
| `replica/session.rs:64:32` | Background | 600 | 4 | 6 | 38 | 122 |
| `replica.rs:1498:36` | Background | 600 | 5 | 13 | 39 | 79 |
| `replica.rs:1478:36` | Background | 600 | 0 | 0 | 0 | 23 |
| `replica/session.rs:298:14` | Background | 600 | 0 | 2 | 2 | 10 |

Per-unit costs: full acquisition hold divided by observed work, in nanoseconds.
Costs include other work within that acquisition; they are conservative amortized costs.

| Site | Class | Unit | Acquisitions | Work | p50 | p95 | p99 | Max |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |

`metric=checkpoint_passive samples=5 p50_us=429472 p95_us=513261 p99_us=513261 max_us=513261`

`metric=checkpoint_passive_ns_per_page samples=5 p50_us=52114 p95_us=62646 p99_us=62646 max_us=62646`

## Desktop L0 Buffered

| Metric (microseconds) | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 3485 | 20381 | 50558 | 123490 |
| observed | 600 | 2 | 4 | 7 | 12 |
| write_lock_wait | 600 | 0 | 0 | 0 | 1 |
| write_lock_hold | 600 | 3191 | 17964 | 46128 | 122982 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |

Load duration: 60.039 s; work: 0 (rows in L1/L3, authored rows in L2/L4).

Top holders by p99; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/sql.rs:749:14` | Interactive | 600 | 3166 | 17932 | 46126 | 122973 |
| `replica.rs:1498:36` | Background | 600 | 239 | 436 | 812 | 13102 |
| `replica/blockers.rs:510:14` | Background | 600 | 87 | 148 | 232 | 366 |
| `replica/session.rs:298:14` | Background | 600 | 2 | 28 | 36 | 92 |
| `replica.rs:1478:36` | Background | 600 | 0 | 0 | 0 | 1 |

Top holders by max; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/sql.rs:749:14` | Interactive | 600 | 3166 | 17932 | 46126 | 122973 |
| `replica.rs:1498:36` | Background | 600 | 239 | 436 | 812 | 13102 |
| `replica/blockers.rs:510:14` | Background | 600 | 87 | 148 | 232 | 366 |
| `replica/session.rs:298:14` | Background | 600 | 2 | 28 | 36 | 92 |
| `replica.rs:1478:36` | Background | 600 | 0 | 0 | 0 | 1 |

Per-unit costs: full acquisition hold divided by observed work, in nanoseconds.
Costs include other work within that acquisition; they are conservative amortized costs.

| Site | Class | Unit | Acquisitions | Work | p50 | p95 | p99 | Max |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |

`metric=checkpoint_passive samples=5 p50_us=12556 p95_us=12846 p99_us=12846 max_us=12846`

`metric=checkpoint_passive_ns_per_page samples=5 p50_us=1500 p95_us=1563 p99_us=1563 max_us=1563`

## Desktop L1 Flushed

| Metric (microseconds) | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 8580 | 325879 | 553401 | 682156 |
| observed | 600 | 4 | 7 | 12 | 1902 |
| write_lock_wait | 600 | 0 | 34 | 368 | 100850 |
| write_lock_hold | 600 | 5484 | 20292 | 44168 | 197092 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| replica_flush_wait | 603 | 0 | 0 | 0 | 0 |

Load duration: 39.438 s; work: 100000 (rows in L1/L3, authored rows in L2/L4).

Top holders by p99; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/group.rs:147:32` | Background | 261 | 105274 | 224529 | 252209 | 348832 |
| `replica.rs:1498:36` | Background | 861 | 7 | 71919 | 155697 | 230482 |
| `replica/bulk.rs:453:32` | Background | 3 | 616 | 30642 | 30642 | 30642 |
| `replica/sql.rs:749:14` | Interactive | 600 | 4787 | 17463 | 29510 | 196455 |
| `replica/schemas.rs:502:14` | Background | 2 | 27381 | 27422 | 27422 | 27422 |
| `replica/roles.rs:1083:40` | Background | 3 | 588 | 5896 | 5896 | 5896 |
| `replica/incoming.rs:15:40` | Background | 70 | 347 | 725 | 3581 | 3581 |
| `replica/sql.rs:1098:18` | Background | 600 | 454 | 1033 | 3330 | 119408 |
| `replica/sql.rs:726:52` | Background | 1 | 2432 | 2432 | 2432 | 2432 |
| `replica/sql.rs:181:14` | Background | 600 | 183 | 450 | 2291 | 10976 |

Top holders by max; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/group.rs:147:32` | Background | 261 | 105274 | 224529 | 252209 | 348832 |
| `replica.rs:1498:36` | Background | 861 | 7 | 71919 | 155697 | 230482 |
| `replica/sql.rs:749:14` | Interactive | 600 | 4787 | 17463 | 29510 | 196455 |
| `replica/sql.rs:1098:18` | Background | 600 | 454 | 1033 | 3330 | 119408 |
| `replica/bulk.rs:453:32` | Background | 3 | 616 | 30642 | 30642 | 30642 |
| `replica/schemas.rs:502:14` | Background | 2 | 27381 | 27422 | 27422 | 27422 |
| `replica/sql.rs:181:14` | Background | 600 | 183 | 450 | 2291 | 10976 |
| `replica.rs:1414:40` | Background | 3125 | 4 | 18 | 53 | 9024 |
| `replica/session.rs:64:32` | Background | 600 | 5 | 37 | 92 | 5916 |
| `replica/roles.rs:1083:40` | Background | 3 | 588 | 5896 | 5896 | 5896 |

Per-unit costs: full acquisition hold divided by observed work, in nanoseconds.
Costs include other work within that acquisition; they are conservative amortized costs.

| Site | Class | Unit | Acquisitions | Work | p50 | p95 | p99 | Max |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `replica/group.rs:147:32` | Background | encoded_byte | 261 | 33662389 | 814 | 1737 | 1947 | 2707 |
| `replica/group.rs:147:32` | Background | received_frame | 261 | 3125 | 8781666 | 18710750 | 21017416 | 29069333 |

`metric=checkpoint_passive samples=47 p50_us=99474 p95_us=167485 p99_us=230439 max_us=230439`

`metric=checkpoint_passive_ns_per_page samples=47 p50_us=8114 p95_us=14530 p99_us=15603 max_us=15603`

`metric=checkpoint_truncate samples=4 p50_us=119334 p95_us=188227 p99_us=188227 max_us=188227`

## Desktop L1 Buffered

| Metric (microseconds) | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 134964 | 452807 | 599187 | 1343433 |
| observed | 600 | 2 | 5 | 19 | 61 |
| write_lock_wait | 600 | 0 | 0 | 0 | 23 |
| write_lock_hold | 600 | 64130 | 428381 | 562718 | 680279 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| replica_flush_wait | 3 | 0 | 6811 | 6811 | 6811 |

Load duration: 37.687 s; work: 100000 (rows in L1/L3, authored rows in L2/L4).

Top holders by p99; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/sql.rs:749:14` | Interactive | 600 | 64129 | 428380 | 562716 | 680278 |
| `replica/group.rs:147:32` | Background | 261 | 111503 | 189415 | 253690 | 303560 |
| `replica/group.rs:185:36` | Background | 261 | 358 | 90520 | 159087 | 186419 |
| `replica.rs:1498:36` | Background | 860 | 303 | 1002 | 106255 | 1234950 |
| `replica/schemas.rs:502:14` | Background | 2 | 28844 | 29323 | 29323 | 29323 |
| `replica/bulk.rs:453:32` | Background | 3 | 2375 | 27977 | 27977 | 27977 |
| `replica/incoming.rs:15:40` | Background | 90 | 798 | 5988 | 10783 | 10783 |
| `replica/sql.rs:726:52` | Background | 1 | 3048 | 3048 | 3048 | 3048 |
| `replica/roles.rs:1083:40` | Background | 3 | 982 | 1577 | 1577 | 1577 |
| `replica/roles.rs:1103:14` | Background | 266 | 249 | 437 | 1005 | 1373 |

Top holders by max; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica.rs:1498:36` | Background | 860 | 303 | 1002 | 106255 | 1234950 |
| `replica/sql.rs:749:14` | Interactive | 600 | 64129 | 428380 | 562716 | 680278 |
| `replica/group.rs:147:32` | Background | 261 | 111503 | 189415 | 253690 | 303560 |
| `replica/group.rs:185:36` | Background | 261 | 358 | 90520 | 159087 | 186419 |
| `replica/schemas.rs:502:14` | Background | 2 | 28844 | 29323 | 29323 | 29323 |
| `replica/bulk.rs:453:32` | Background | 3 | 2375 | 27977 | 27977 | 27977 |
| `replica/blockers.rs:510:14` | Background | 181 | 168 | 413 | 795 | 25640 |
| `replica.rs:1414:40` | Background | 3125 | 4 | 17 | 46 | 13598 |
| `replica/incoming.rs:15:40` | Background | 90 | 798 | 5988 | 10783 | 10783 |
| `replica/sql.rs:726:52` | Background | 1 | 3048 | 3048 | 3048 | 3048 |

Per-unit costs: full acquisition hold divided by observed work, in nanoseconds.
Costs include other work within that acquisition; they are conservative amortized costs.

| Site | Class | Unit | Acquisitions | Work | p50 | p95 | p99 | Max |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `replica/group.rs:147:32` | Background | encoded_byte | 261 | 33647542 | 865 | 1485 | 1962 | 2350 |
| `replica/group.rs:147:32` | Background | received_frame | 261 | 3125 | 9315000 | 15966416 | 21140833 | 25296666 |

`metric=checkpoint_passive samples=44 p50_us=93795 p95_us=185860 p99_us=1233957 max_us=1233957`

`metric=checkpoint_passive_ns_per_page samples=44 p50_us=7744 p95_us=12938 p99_us=78401 max_us=78401`

`metric=checkpoint_truncate samples=5 p50_us=104492 p95_us=128556 p99_us=128556 max_us=128556`

## Desktop L2 Flushed

| Metric (microseconds) | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 31741 | 129004 | 219113 | 431930 |
| observed | 600 | 2 | 4 | 12 | 23 |
| write_lock_wait | 600 | 0 | 201 | 416 | 655 |
| write_lock_hold | 600 | 24351 | 77049 | 180394 | 431276 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| replica_flush_wait | 952 | 0 | 0 | 0 | 0 |

Load duration: 61.378 s; work: 1754 (rows in L1/L3, authored rows in L2/L4).

Top holders by p99; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/group.rs:147:32` | Background | 187 | 34959 | 134949 | 176336 | 200563 |
| `replica/sql.rs:749:14` | Interactive | 600 | 23694 | 73519 | 128500 | 188154 |
| `replica/sql.rs:1098:18` | Background | 600 | 239 | 483 | 60468 | 307176 |
| `replica/bulk.rs:453:32` | Background | 353 | 8934 | 17520 | 19648 | 26810 |
| `replica/schemas.rs:502:14` | Background | 374 | 8799 | 17394 | 19065 | 79521 |
| `replica/roles.rs:1083:40` | Background | 354 | 204 | 514 | 4553 | 9091 |
| `replica/roles.rs:1103:14` | Background | 873 | 130 | 337 | 3740 | 17828 |
| `network/frontiers.rs:114:22` | Background | 374 | 126 | 289 | 3515 | 5541 |
| `replica/compaction/rebootstrap.rs:365:36` | Background | 561 | 148 | 295 | 428 | 513 |
| `replica/sql.rs:181:14` | Background | 600 | 126 | 271 | 409 | 8083 |

Top holders by max; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/sql.rs:1098:18` | Background | 600 | 239 | 483 | 60468 | 307176 |
| `replica.rs:1498:36` | Background | 787 | 5 | 16 | 32 | 218245 |
| `replica/group.rs:147:32` | Background | 187 | 34959 | 134949 | 176336 | 200563 |
| `replica/sql.rs:749:14` | Interactive | 600 | 23694 | 73519 | 128500 | 188154 |
| `replica/schemas.rs:502:14` | Background | 374 | 8799 | 17394 | 19065 | 79521 |
| `replica/bulk.rs:453:32` | Background | 353 | 8934 | 17520 | 19648 | 26810 |
| `replica/roles.rs:1103:14` | Background | 873 | 130 | 337 | 3740 | 17828 |
| `replica/roles.rs:1083:40` | Background | 354 | 204 | 514 | 4553 | 9091 |
| `replica/sql.rs:181:14` | Background | 600 | 126 | 271 | 409 | 8083 |
| `network/frontiers.rs:114:22` | Background | 374 | 126 | 289 | 3515 | 5541 |

Per-unit costs: full acquisition hold divided by observed work, in nanoseconds.
Costs include other work within that acquisition; they are conservative amortized costs.

| Site | Class | Unit | Acquisitions | Work | p50 | p95 | p99 | Max |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `replica/group.rs:147:32` | Background | encoded_byte | 187 | 2501047 | 2956 | 8686 | 12919 | 14548 |
| `replica/group.rs:147:32` | Background | received_frame | 187 | 1752 | 4221714 | 12404777 | 18453666 | 20785714 |

`metric=checkpoint_passive samples=10 p50_us=103024 p95_us=306854 p99_us=306854 max_us=306854`

`metric=checkpoint_passive_ns_per_page samples=10 p50_us=12017 p95_us=37216 p99_us=37216 max_us=37216`

## Desktop L2 Buffered

| Metric (microseconds) | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 11247 | 52164 | 70443 | 96450 |
| observed | 600 | 2 | 5 | 17 | 30 |
| write_lock_wait | 600 | 0 | 0 | 0 | 1 |
| write_lock_hold | 600 | 3180 | 10451 | 15570 | 25776 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| replica_flush_wait | 1504 | 0 | 8074 | 15979 | 28563 |

Load duration: 60.187 s; work: 2985 (rows in L1/L3, authored rows in L2/L4).

Top holders by p99; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/bulk.rs:453:32` | Background | 1248 | 8579 | 26324 | 27626 | 30520 |
| `replica/schemas.rs:502:14` | Background | 1570 | 7021 | 25954 | 27317 | 33981 |
| `replica/group.rs:147:32` | Background | 785 | 5024 | 18401 | 24756 | 44082 |
| `replica/sql.rs:749:14` | Interactive | 600 | 3179 | 10449 | 15569 | 25774 |
| `replica/roles.rs:1103:14` | Background | 2960 | 176 | 391 | 3625 | 55799 |
| `replica/roles.rs:1083:40` | Background | 1249 | 143 | 434 | 1981 | 10669 |
| `network/frontiers.rs:114:22` | Background | 1570 | 207 | 406 | 1714 | 44435 |
| `replica.rs:1498:36` | Background | 1386 | 18 | 411 | 810 | 36357 |
| `replica/group.rs:185:36` | Background | 785 | 169 | 310 | 527 | 31545 |
| `replica/compaction/rebootstrap.rs:365:36` | Background | 2355 | 224 | 347 | 500 | 14820 |

Top holders by max; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/roles.rs:1103:14` | Background | 2960 | 176 | 391 | 3625 | 55799 |
| `network/frontiers.rs:114:22` | Background | 1570 | 207 | 406 | 1714 | 44435 |
| `replica/group.rs:147:32` | Background | 785 | 5024 | 18401 | 24756 | 44082 |
| `replica.rs:1498:36` | Background | 1386 | 18 | 411 | 810 | 36357 |
| `replica/schemas.rs:502:14` | Background | 1570 | 7021 | 25954 | 27317 | 33981 |
| `replica/group.rs:185:36` | Background | 785 | 169 | 310 | 527 | 31545 |
| `replica/bulk.rs:453:32` | Background | 1248 | 8579 | 26324 | 27626 | 30520 |
| `replica/sql.rs:749:14` | Interactive | 600 | 3179 | 10449 | 15569 | 25774 |
| `replica/compaction/rebootstrap.rs:365:36` | Background | 2355 | 224 | 347 | 500 | 14820 |
| `replica/roles.rs:1083:40` | Background | 1249 | 143 | 434 | 1981 | 10669 |

Per-unit costs: full acquisition hold divided by observed work, in nanoseconds.
Costs include other work within that acquisition; they are conservative amortized costs.

| Site | Class | Unit | Acquisitions | Work | p50 | p95 | p99 | Max |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `replica/group.rs:147:32` | Background | encoded_byte | 785 | 4237759 | 1000 | 2946 | 5917 | 8366 |
| `replica/group.rs:147:32` | Background | received_frame | 785 | 2985 | 1419900 | 4183000 | 8391000 | 11863000 |

`metric=checkpoint_passive samples=18 p50_us=23405 p95_us=54942 p99_us=54942 max_us=54942`

`metric=checkpoint_passive_ns_per_page samples=18 p50_us=2761 p95_us=6195 p99_us=6195 max_us=6195`

## Desktop L3 Flushed

| Metric (microseconds) | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 5552 | 28808 | 45714 | 88542549 |
| observed | 600 | 4 | 8 | 14 | 59 |
| write_lock_wait | 600 | 0 | 0 | 1 | 142 |
| write_lock_hold | 600 | 5122 | 24929 | 43908 | 200507 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| replica_flush_wait | 600 | 0 | 0 | 0 | 0 |

Load duration: 89.017 s; work: 100000 (rows in L1/L3, authored rows in L2/L4).

Top holders by p99; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/compaction.rs:1977:36` | Background | 3 | 4875100 | 70856988 | 70856988 | 70856988 |
| `replica/tips.rs:1319:36` | Background | 8 | 4555 | 6454190 | 6454190 | 6454190 |
| `replica/publication.rs:344:36` | Background | 4 | 137 | 383155 | 383155 | 383155 |
| `replica/schemas.rs:1260:36` | Background | 1 | 39355 | 39355 | 39355 | 39355 |
| `replica/sql.rs:749:14` | Interactive | 600 | 4295 | 20858 | 38628 | 57107 |
| `replica/tip_observation.rs:187:14` | Background | 2 | 4487 | 36502 | 36502 | 36502 |
| `replica/tip_observation.rs:198:36` | Background | 2 | 23459 | 29978 | 29978 | 29978 |
| `replica/compaction/rebootstrap.rs:318:32` | Background | 7 | 158 | 22933 | 22933 | 22933 |
| `replica/compaction.rs:2024:36` | Background | 1 | 16658 | 16658 | 16658 | 16658 |
| `replica/tip_observation.rs:289:32` | Background | 1 | 3786 | 3786 | 3786 | 3786 |

Top holders by max; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/compaction.rs:1977:36` | Background | 3 | 4875100 | 70856988 | 70856988 | 70856988 |
| `replica/tips.rs:1319:36` | Background | 8 | 4555 | 6454190 | 6454190 | 6454190 |
| `replica/publication.rs:344:36` | Background | 4 | 137 | 383155 | 383155 | 383155 |
| `replica/sql.rs:1098:18` | Background | 600 | 432 | 808 | 3010 | 170612 |
| `replica/sql.rs:749:14` | Interactive | 600 | 4295 | 20858 | 38628 | 57107 |
| `replica/schemas.rs:1260:36` | Background | 1 | 39355 | 39355 | 39355 | 39355 |
| `replica/tip_observation.rs:187:14` | Background | 2 | 4487 | 36502 | 36502 | 36502 |
| `replica/tip_observation.rs:198:36` | Background | 2 | 23459 | 29978 | 29978 | 29978 |
| `replica/compaction/rebootstrap.rs:318:32` | Background | 7 | 158 | 22933 | 22933 | 22933 |
| `replica/compaction.rs:2024:36` | Background | 1 | 16658 | 16658 | 16658 | 16658 |

Per-unit costs: full acquisition hold divided by observed work, in nanoseconds.
Costs include other work within that acquisition; they are conservative amortized costs.

| Site | Class | Unit | Acquisitions | Work | p50 | p95 | p99 | Max |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `replica/compaction.rs:1977:36` | Background | base_row | 3 | 100013 | 32544000 | 975020000 | 975020000 | 975020000 |
| `replica/compaction.rs:1977:36` | Background | replayed_frame | 3 | 3183 | 22310134 | 2437550000 | 2437550000 | 2437550000 |

`metric=checkpoint_passive samples=12 p50_us=97322 p95_us=224956 p99_us=224956 max_us=224956`

`metric=checkpoint_passive_ns_per_page samples=12 p50_us=7916 p95_us=23481 p99_us=23481 max_us=23481`

`metric=checkpoint_truncate samples=2 p50_us=97436 p95_us=104990 p99_us=104990 max_us=104990`

## Desktop L3 Buffered

| Metric (microseconds) | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 7202 | 31093 | 67291 | 132867002 |
| observed | 600 | 4 | 7 | 42 | 206 |
| write_lock_wait | 600 | 0 | 0 | 1 | 32 |
| write_lock_hold | 600 | 5817 | 22172 | 48279 | 81663 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| replica_flush_wait | 2 | 7309 | 10656 | 10656 | 10656 |

Load duration: 133.162 s; work: 100000 (rows in L1/L3, authored rows in L2/L4).

Top holders by p99; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/compaction.rs:1977:36` | Background | 3 | 9893675 | 96307174 | 96307174 | 96307174 |
| `replica/tips.rs:1319:36` | Background | 8 | 6282 | 15853637 | 15853637 | 15853637 |
| `replica/publication.rs:344:36` | Background | 4 | 142 | 967117 | 967117 | 967117 |
| `replica/tip_observation.rs:187:14` | Background | 2 | 4466 | 223211 | 223211 | 223211 |
| `replica/tip_observation.rs:198:36` | Background | 2 | 3618 | 95606 | 95606 | 95606 |
| `replica/sql.rs:749:14` | Interactive | 600 | 5815 | 22134 | 48274 | 81660 |
| `replica.rs:1498:36` | Background | 603 | 579 | 2898 | 11094 | 82573 |
| `replica/schemas.rs:1260:36` | Background | 1 | 9007 | 9007 | 9007 | 9007 |
| `replica/tip_observation.rs:289:32` | Background | 1 | 4168 | 4168 | 4168 | 4168 |
| `replica/schemas.rs:502:14` | Background | 4 | 3580 | 3879 | 3879 | 3879 |

Top holders by max; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/compaction.rs:1977:36` | Background | 3 | 9893675 | 96307174 | 96307174 | 96307174 |
| `replica/tips.rs:1319:36` | Background | 8 | 6282 | 15853637 | 15853637 | 15853637 |
| `replica/publication.rs:344:36` | Background | 4 | 142 | 967117 | 967117 | 967117 |
| `replica/tip_observation.rs:187:14` | Background | 2 | 4466 | 223211 | 223211 | 223211 |
| `replica/tip_observation.rs:198:36` | Background | 2 | 3618 | 95606 | 95606 | 95606 |
| `replica.rs:1498:36` | Background | 603 | 579 | 2898 | 11094 | 82573 |
| `replica/sql.rs:749:14` | Interactive | 600 | 5815 | 22134 | 48274 | 81660 |
| `replica/schemas.rs:1260:36` | Background | 1 | 9007 | 9007 | 9007 | 9007 |
| `replica/blockers.rs:510:14` | Background | 592 | 286 | 1054 | 2797 | 8177 |
| `replica/tip_observation.rs:289:32` | Background | 1 | 4168 | 4168 | 4168 | 4168 |

Per-unit costs: full acquisition hold divided by observed work, in nanoseconds.
Costs include other work within that acquisition; they are conservative amortized costs.

| Site | Class | Unit | Acquisitions | Work | p50 | p95 | p99 | Max |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `replica/compaction.rs:1977:36` | Background | base_row | 3 | 100013 | 10555000 | 1978735000 | 1978735000 | 1978735000 |
| `replica/compaction.rs:1977:36` | Background | replayed_frame | 3 | 3182 | 30323417 | 4946837500 | 4946837500 | 4946837500 |

`metric=checkpoint_passive samples=13 p50_us=79196 p95_us=130214 p99_us=130214 max_us=130214`

`metric=checkpoint_passive_ns_per_page samples=13 p50_us=9025 p95_us=12111 p99_us=12111 max_us=12111`

`metric=checkpoint_truncate samples=2 p50_us=95341 p95_us=134159 p99_us=134159 max_us=134159`

## Desktop L4 Flushed

| Metric (microseconds) | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 17537 | 379044 | 388552 | 758264 |
| observed | 600 | 3 | 6 | 12 | 23 |
| write_lock_wait | 600 | 0 | 6758 | 10418 | 62190 |
| write_lock_hold | 600 | 5272 | 128544 | 132665 | 238393 |
| write_flush_wait | 600 | 8298 | 243522 | 247388 | 271149 |
| replica_flush_wait | 4861 | 3959 | 11640 | 240789 | 271149 |

Load duration: 81.224 s; work: 136384 (rows in L1/L3, authored rows in L2/L4).

Top holders by p99; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/sql.rs:1098:18` | Background | 4862 | 263 | 576 | 122920 | 166505 |
| `replica.rs:1498:36` | Background | 4859 | 10 | 68 | 61927 | 76142 |
| `replica/sql.rs:749:14` | Interactive | 4862 | 6359 | 11200 | 14705 | 118655 |
| `replica/blockers.rs:510:14` | Background | 4862 | 89 | 125 | 212 | 2402 |
| `replica/sql.rs:181:14` | Background | 602 | 66 | 131 | 188 | 827 |
| `replica/session.rs:64:32` | Background | 602 | 3 | 5 | 8 | 28 |
| `replica/session.rs:298:14` | Background | 4862 | 1 | 1 | 2 | 28 |
| `replica.rs:1478:36` | Background | 4859 | 0 | 0 | 0 | 14 |

Top holders by max; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/sql.rs:1098:18` | Background | 4862 | 263 | 576 | 122920 | 166505 |
| `replica/sql.rs:749:14` | Interactive | 4862 | 6359 | 11200 | 14705 | 118655 |
| `replica.rs:1498:36` | Background | 4859 | 10 | 68 | 61927 | 76142 |
| `replica/blockers.rs:510:14` | Background | 4862 | 89 | 125 | 212 | 2402 |
| `replica/sql.rs:181:14` | Background | 602 | 66 | 131 | 188 | 827 |
| `replica/session.rs:298:14` | Background | 4862 | 1 | 1 | 2 | 28 |
| `replica/session.rs:64:32` | Background | 602 | 3 | 5 | 8 | 28 |
| `replica.rs:1478:36` | Background | 4859 | 0 | 0 | 0 | 14 |

Per-unit costs: full acquisition hold divided by observed work, in nanoseconds.
Costs include other work within that acquisition; they are conservative amortized costs.

| Site | Class | Unit | Acquisitions | Work | p50 | p95 | p99 | Max |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |

`metric=checkpoint_passive samples=130 p50_us=11 p95_us=50560 p99_us=74455 max_us=166135`

`metric=checkpoint_passive_ns_per_page samples=130 p50_us=3 p95_us=4440 p99_us=7177 max_us=13678`

`metric=checkpoint_truncate samples=456 p50_us=60727 p95_us=62593 p99_us=62635 max_us=76102`

`metric=checkpoint_truncate_ns_per_page samples=455 p50_us=17861 p95_us=18409 p99_us=18422 max_us=22382`

## Desktop L4 Buffered

| Metric (microseconds) | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 16866 | 317582 | 325527 | 373117 |
| observed | 600 | 3 | 9 | 17 | 50 |
| write_lock_wait | 600 | 0 | 7026 | 15514 | 66041 |
| write_lock_hold | 600 | 7806 | 19991 | 33951 | 66785 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| replica_flush_wait | 2791 | 4155 | 20687 | 180202 | 188569 |

Load duration: 77.807 s; work: 89312 (rows in L1/L3, authored rows in L2/L4).

Top holders by p99; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/sql.rs:1098:18` | Background | 2791 | 357 | 1716 | 123017 | 126318 |
| `replica.rs:1498:36` | Background | 3345 | 20 | 56796 | 122672 | 127649 |
| `replica/sql.rs:749:14` | Interactive | 3391 | 8549 | 19991 | 32073 | 178432 |
| `replica/blockers.rs:510:14` | Background | 3391 | 95 | 210 | 378 | 4537 |
| `replica/sql.rs:181:14` | Background | 2 | 55 | 61 | 61 | 61 |
| `replica/session.rs:298:14` | Background | 3391 | 1 | 2 | 7 | 392 |
| `replica/session.rs:64:32` | Background | 2 | 1 | 3 | 3 | 3 |
| `replica.rs:1478:36` | Background | 3344 | 0 | 0 | 0 | 22 |

Top holders by max; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/sql.rs:749:14` | Interactive | 3391 | 8549 | 19991 | 32073 | 178432 |
| `replica.rs:1498:36` | Background | 3345 | 20 | 56796 | 122672 | 127649 |
| `replica/sql.rs:1098:18` | Background | 2791 | 357 | 1716 | 123017 | 126318 |
| `replica/blockers.rs:510:14` | Background | 3391 | 95 | 210 | 378 | 4537 |
| `replica/session.rs:298:14` | Background | 3391 | 1 | 2 | 7 | 392 |
| `replica/sql.rs:181:14` | Background | 2 | 55 | 61 | 61 | 61 |
| `replica.rs:1478:36` | Background | 3344 | 0 | 0 | 0 | 22 |
| `replica/session.rs:64:32` | Background | 2 | 1 | 3 | 3 | 3 |

Per-unit costs: full acquisition hold divided by observed work, in nanoseconds.
Costs include other work within that acquisition; they are conservative amortized costs.

| Site | Class | Unit | Acquisitions | Work | p50 | p95 | p99 | Max |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |

`metric=checkpoint_passive samples=109 p50_us=11 p95_us=53930 p99_us=61647 max_us=102716`

`metric=checkpoint_passive_ns_per_page samples=109 p50_us=3 p95_us=5099 p99_us=6235 max_us=9261`

`metric=checkpoint_truncate samples=448 p50_us=60622 p95_us=62611 p99_us=62658 max_us=63230`

`metric=checkpoint_truncate_ns_per_page samples=447 p50_us=17655 p95_us=18232 p99_us=18246 max_us=18412`

## Pixel L0 Flushed

| Metric (microseconds) | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 28464 | 297995 | 391419 | 442488 |
| observed | 600 | 36 | 76 | 101 | 318 |
| write_lock_wait | 600 | 10 | 19 | 25 | 158 |
| write_lock_hold | 600 | 24107 | 35769 | 376957 | 439884 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| replica_flush_wait | 600 | 0 | 0 | 0 | 0 |

Load duration: 73.859 s; work: 0 (rows in L1/L3, authored rows in L2/L4).

Top holders by p99; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/sql.rs:749:14` | Interactive | 600 | 19832 | 28333 | 367981 | 403417 |
| `replica/sql.rs:1098:18` | Background | 600 | 2895 | 4254 | 9402 | 416196 |
| `replica/sql.rs:181:14` | Background | 600 | 1138 | 1701 | 3495 | 343019 |
| `replica/blockers.rs:510:14` | Background | 600 | 370 | 470 | 631 | 2296 |
| `replica/session.rs:64:32` | Background | 600 | 49 | 264 | 323 | 2259 |
| `replica.rs:1498:36` | Background | 600 | 45 | 57 | 107 | 981 |
| `replica/session.rs:298:14` | Background | 600 | 10 | 26 | 49 | 260 |
| `replica.rs:1478:36` | Background | 600 | 4 | 5 | 6 | 96 |

Top holders by max; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/sql.rs:1098:18` | Background | 600 | 2895 | 4254 | 9402 | 416196 |
| `replica/sql.rs:749:14` | Interactive | 600 | 19832 | 28333 | 367981 | 403417 |
| `replica/sql.rs:181:14` | Background | 600 | 1138 | 1701 | 3495 | 343019 |
| `replica/blockers.rs:510:14` | Background | 600 | 370 | 470 | 631 | 2296 |
| `replica/session.rs:64:32` | Background | 600 | 49 | 264 | 323 | 2259 |
| `replica.rs:1498:36` | Background | 600 | 45 | 57 | 107 | 981 |
| `replica/session.rs:298:14` | Background | 600 | 10 | 26 | 49 | 260 |
| `replica.rs:1478:36` | Background | 600 | 4 | 5 | 6 | 96 |

Per-unit costs: full acquisition hold divided by observed work, in nanoseconds.
Costs include other work within that acquisition; they are conservative amortized costs.

| Site | Class | Unit | Acquisitions | Work | p50 | p95 | p99 | Max |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |

`metric=checkpoint_passive samples=5 p50_us=74947 p95_us=412119 p99_us=412119 max_us=412119`

`metric=checkpoint_passive_ns_per_page samples=5 p50_us=9161 p95_us=49426 p99_us=49426 max_us=49426`

## Pixel L0 Buffered

| Metric (microseconds) | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 25738 | 359850 | 395632 | 412104 |
| observed | 600 | 38 | 75 | 176 | 6194 |
| write_lock_wait | 600 | 5 | 9 | 13 | 103 |
| write_lock_hold | 600 | 20329 | 25841 | 370047 | 402871 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |

Load duration: 86.330 s; work: 0 (rows in L1/L3, authored rows in L2/L4).

Top holders by p99; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/sql.rs:749:14` | Interactive | 600 | 20272 | 25675 | 369714 | 402850 |
| `replica.rs:1498:36` | Background | 600 | 3214 | 4131 | 23935 | 349617 |
| `replica/blockers.rs:510:14` | Background | 600 | 1276 | 1532 | 2907 | 8462 |
| `replica/session.rs:298:14` | Background | 600 | 19 | 260 | 385 | 16150 |
| `replica.rs:1478:36` | Background | 600 | 3 | 6 | 7 | 90 |

Top holders by max; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/sql.rs:749:14` | Interactive | 600 | 20272 | 25675 | 369714 | 402850 |
| `replica.rs:1498:36` | Background | 600 | 3214 | 4131 | 23935 | 349617 |
| `replica/session.rs:298:14` | Background | 600 | 19 | 260 | 385 | 16150 |
| `replica/blockers.rs:510:14` | Background | 600 | 1276 | 1532 | 2907 | 8462 |
| `replica.rs:1478:36` | Background | 600 | 3 | 6 | 7 | 90 |

Per-unit costs: full acquisition hold divided by observed work, in nanoseconds.
Costs include other work within that acquisition; they are conservative amortized costs.

| Site | Class | Unit | Acquisitions | Work | p50 | p95 | p99 | Max |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |

`metric=checkpoint_passive samples=5 p50_us=34521 p95_us=43167 p99_us=43167 max_us=43167`

`metric=checkpoint_passive_ns_per_page samples=5 p50_us=4210 p95_us=5188 p99_us=5188 max_us=5188`

## Pixel L1 Flushed

| Metric (microseconds) | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 38921 | 340441 | 565598 | 920541 |
| observed | 600 | 32 | 68 | 92 | 262 |
| write_lock_wait | 600 | 10 | 169 | 622 | 3574 |
| write_lock_hold | 600 | 28448 | 36616 | 109948 | 259616 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| replica_flush_wait | 603 | 0 | 0 | 0 | 0 |

Load duration: 54.301 s; work: 100000 (rows in L1/L3, authored rows in L2/L4).

Top holders by p99; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/group.rs:147:32` | Background | 261 | 131844 | 308439 | 349150 | 384504 |
| `replica/schemas.rs:502:14` | Background | 2 | 103682 | 181438 | 181438 | 181438 |
| `replica/bulk.rs:453:32` | Background | 3 | 5570 | 178373 | 178373 | 178373 |
| `replica.rs:1498:36` | Background | 860 | 21 | 136 | 137512 | 206373 |
| `replica/sql.rs:1098:18` | Background | 600 | 3409 | 5171 | 101597 | 230063 |
| `replica/sql.rs:749:14` | Interactive | 600 | 21905 | 29411 | 37105 | 86291 |
| `replica/sql.rs:726:52` | Background | 1 | 11028 | 11028 | 11028 | 11028 |
| `replica/incoming.rs:15:40` | Background | 73 | 745 | 3829 | 6298 | 6298 |
| `replica/compaction/rebootstrap.rs:365:36` | Background | 3 | 3837 | 5253 | 5253 | 5253 |
| `replica/roles.rs:1083:40` | Background | 3 | 1666 | 4518 | 4518 | 4518 |

Top holders by max; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/group.rs:147:32` | Background | 261 | 131844 | 308439 | 349150 | 384504 |
| `replica/sql.rs:1098:18` | Background | 600 | 3409 | 5171 | 101597 | 230063 |
| `replica.rs:1498:36` | Background | 860 | 21 | 136 | 137512 | 206373 |
| `replica/schemas.rs:502:14` | Background | 2 | 103682 | 181438 | 181438 | 181438 |
| `replica/bulk.rs:453:32` | Background | 3 | 5570 | 178373 | 178373 | 178373 |
| `replica/sql.rs:749:14` | Interactive | 600 | 21905 | 29411 | 37105 | 86291 |
| `replica/sql.rs:726:52` | Background | 1 | 11028 | 11028 | 11028 | 11028 |
| `replica/sql.rs:181:14` | Background | 600 | 877 | 1666 | 2517 | 10515 |
| `replica/group.rs:185:36` | Background | 261 | 492 | 2013 | 4388 | 6484 |
| `replica/incoming.rs:15:40` | Background | 73 | 745 | 3829 | 6298 | 6298 |

Per-unit costs: full acquisition hold divided by observed work, in nanoseconds.
Costs include other work within that acquisition; they are conservative amortized costs.

| Site | Class | Unit | Acquisitions | Work | p50 | p95 | p99 | Max |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `replica/group.rs:147:32` | Background | encoded_byte | 261 | 33661011 | 1019 | 2388 | 2902 | 3014 |
| `replica/group.rs:147:32` | Background | received_frame | 261 | 3125 | 10987000 | 25776666 | 31308333 | 32605800 |

`metric=checkpoint_passive samples=48 p50_us=102884 p95_us=178180 p99_us=226476 max_us=226476`

`metric=checkpoint_passive_ns_per_page samples=48 p50_us=8903 p95_us=14412 p99_us=16995 max_us=16995`

`metric=checkpoint_truncate samples=4 p50_us=140479 p95_us=206313 p99_us=206313 max_us=206313`

## Pixel L1 Buffered

| Metric (microseconds) | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 34183 | 509377 | 732484 | 917280 |
| observed | 600 | 37 | 71 | 237 | 400 |
| write_lock_wait | 600 | 4 | 9 | 10 | 61 |
| write_lock_hold | 600 | 25348 | 31028 | 38624 | 46687 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| replica_flush_wait | 4 | 0 | 183973 | 183973 | 183973 |

Load duration: 80.758 s; work: 100000 (rows in L1/L3, authored rows in L2/L4).

Top holders by p99; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/group.rs:147:32` | Background | 261 | 247147 | 336412 | 406537 | 489757 |
| `replica/schemas.rs:502:14` | Background | 2 | 89290 | 160786 | 160786 | 160786 |
| `replica.rs:1498:36` | Background | 832 | 914 | 71890 | 153919 | 191132 |
| `replica/bulk.rs:453:32` | Background | 3 | 11530 | 144261 | 144261 | 144261 |
| `replica/group.rs:185:36` | Background | 261 | 690 | 2929 | 118263 | 126004 |
| `replica/sql.rs:749:14` | Interactive | 600 | 25329 | 31011 | 38615 | 46666 |
| `replica/sql.rs:726:52` | Background | 1 | 12270 | 12270 | 12270 | 12270 |
| `replica/blockers.rs:510:14` | Background | 198 | 720 | 2876 | 7577 | 7902 |
| `replica/incoming.rs:15:40` | Background | 98 | 595 | 4073 | 6129 | 6129 |
| `network/frontiers.rs:114:22` | Background | 3 | 2587 | 5022 | 5022 | 5022 |

Top holders by max; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/group.rs:147:32` | Background | 261 | 247147 | 336412 | 406537 | 489757 |
| `replica.rs:1498:36` | Background | 832 | 914 | 71890 | 153919 | 191132 |
| `replica/schemas.rs:502:14` | Background | 2 | 89290 | 160786 | 160786 | 160786 |
| `replica/bulk.rs:453:32` | Background | 3 | 11530 | 144261 | 144261 | 144261 |
| `replica/group.rs:185:36` | Background | 261 | 690 | 2929 | 118263 | 126004 |
| `replica/roles.rs:1103:14` | Background | 266 | 579 | 1934 | 4742 | 69226 |
| `replica/sql.rs:749:14` | Interactive | 600 | 25329 | 31011 | 38615 | 46666 |
| `replica/sql.rs:726:52` | Background | 1 | 12270 | 12270 | 12270 | 12270 |
| `replica/blockers.rs:510:14` | Background | 198 | 720 | 2876 | 7577 | 7902 |
| `replica/incoming.rs:15:40` | Background | 98 | 595 | 4073 | 6129 | 6129 |

Per-unit costs: full acquisition hold divided by observed work, in nanoseconds.
Costs include other work within that acquisition; they are conservative amortized costs.

| Site | Class | Unit | Acquisitions | Work | p50 | p95 | p99 | Max |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `replica/group.rs:147:32` | Background | encoded_byte | 261 | 33658610 | 1910 | 2604 | 3139 | 3786 |
| `replica/group.rs:147:32` | Background | received_frame | 261 | 3125 | 20595583 | 28034333 | 33878083 | 40813083 |

`metric=checkpoint_passive samples=42 p50_us=107860 p95_us=159854 p99_us=186399 max_us=186399`

`metric=checkpoint_passive_ns_per_page samples=42 p50_us=8551 p95_us=13475 p99_us=16168 max_us=16168`

`metric=checkpoint_truncate samples=6 p50_us=155004 p95_us=191087 p99_us=191087 max_us=191087`

## Pixel L2 Flushed

| Metric (microseconds) | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 36202 | 578700 | 1013511 | 1112506 |
| observed | 600 | 22 | 34 | 113 | 4231 |
| write_lock_wait | 600 | 3 | 2293 | 4518 | 33805 |
| write_lock_hold | 600 | 19397 | 36663 | 374373 | 427252 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| replica_flush_wait | 734 | 0 | 0 | 0 | 0 |

Load duration: 106.889 s; work: 2809 (rows in L1/L3, authored rows in L2/L4).

Top holders by p99; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/group.rs:147:32` | Background | 124 | 123236 | 523895 | 552923 | 553624 |
| `replica/bulk.rs:453:32` | Background | 134 | 84864 | 464118 | 514861 | 516542 |
| `replica/schemas.rs:502:14` | Background | 134 | 80798 | 461226 | 501349 | 531941 |
| `replica/sql.rs:749:14` | Interactive | 600 | 16049 | 26959 | 363397 | 425307 |
| `replica/sql.rs:1098:18` | Background | 600 | 1984 | 4583 | 20510 | 380987 |
| `replica/roles.rs:1103:14` | Background | 393 | 1370 | 3407 | 15635 | 50714 |
| `replica/group.rs:185:36` | Background | 124 | 1444 | 4042 | 12967 | 36829 |
| `network/frontiers.rs:114:22` | Background | 134 | 1066 | 3607 | 9242 | 10969 |
| `replica/roles.rs:1083:40` | Background | 135 | 2115 | 6945 | 8981 | 9498 |
| `replica/tip_observation.rs:198:36` | Background | 67 | 1 | 3 | 7716 | 7716 |

Top holders by max; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/group.rs:147:32` | Background | 124 | 123236 | 523895 | 552923 | 553624 |
| `replica/schemas.rs:502:14` | Background | 134 | 80798 | 461226 | 501349 | 531941 |
| `replica/bulk.rs:453:32` | Background | 134 | 84864 | 464118 | 514861 | 516542 |
| `replica.rs:1498:36` | Background | 724 | 29 | 88 | 3373 | 434701 |
| `replica/sql.rs:749:14` | Interactive | 600 | 16049 | 26959 | 363397 | 425307 |
| `replica/sql.rs:1098:18` | Background | 600 | 1984 | 4583 | 20510 | 380987 |
| `replica/blockers.rs:510:14` | Background | 981 | 222 | 933 | 3051 | 363434 |
| `replica/tips.rs:1502:36` | Background | 335 | 244 | 794 | 1401 | 352798 |
| `replica/roles.rs:1103:14` | Background | 393 | 1370 | 3407 | 15635 | 50714 |
| `replica/group.rs:185:36` | Background | 124 | 1444 | 4042 | 12967 | 36829 |

Per-unit costs: full acquisition hold divided by observed work, in nanoseconds.
Costs include other work within that acquisition; they are conservative amortized costs.

| Site | Class | Unit | Acquisitions | Work | p50 | p95 | p99 | Max |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `replica/group.rs:147:32` | Background | encoded_byte | 124 | 3995910 | 3568 | 12098 | 47920 | 50003 |
| `replica/group.rs:147:32` | Background | received_frame | 124 | 2795 | 5097562 | 17300750 | 68565666 | 71530666 |

`metric=checkpoint_passive samples=11 p50_us=55634 p95_us=434654 p99_us=434654 max_us=434654`

`metric=checkpoint_passive_ns_per_page samples=11 p50_us=6290 p95_us=45652 p99_us=45652 max_us=45652`

## Pixel L2 Buffered

| Metric (microseconds) | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 29534 | 358353 | 543968 | 1063965 |
| observed | 600 | 17 | 33 | 77 | 2004 |
| write_lock_wait | 600 | 1 | 3 | 6 | 55 |
| write_lock_hold | 600 | 13366 | 25611 | 380325 | 422880 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| replica_flush_wait | 228 | 0 | 5716 | 39168 | 350915 |

Load duration: 79.310 s; work: 2941 (rows in L1/L3, authored rows in L2/L4).

Top holders by p99; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/group.rs:147:32` | Background | 136 | 57876 | 199569 | 520851 | 555911 |
| `replica/bulk.rs:453:32` | Background | 170 | 59283 | 138479 | 421050 | 426594 |
| `replica/schemas.rs:502:14` | Background | 170 | 52790 | 136469 | 417783 | 440912 |
| `replica/sql.rs:749:14` | Interactive | 600 | 13358 | 25599 | 380314 | 422873 |
| `replica/group.rs:185:36` | Background | 136 | 1244 | 2588 | 40259 | 46421 |
| `replica/roles.rs:1083:40` | Background | 171 | 1479 | 5532 | 12491 | 20590 |
| `network/frontiers.rs:114:22` | Background | 170 | 1518 | 4534 | 11875 | 88101 |
| `replica/held/late.rs:485:36` | Background | 170 | 1503 | 4156 | 10086 | 361404 |
| `replica/roles.rs:1103:14` | Background | 477 | 1326 | 3955 | 9648 | 70247 |
| `replica/compaction/rebootstrap.rs:365:36` | Background | 255 | 2330 | 4819 | 8487 | 14199 |

Top holders by max; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/group.rs:147:32` | Background | 136 | 57876 | 199569 | 520851 | 555911 |
| `replica/schemas.rs:502:14` | Background | 170 | 52790 | 136469 | 417783 | 440912 |
| `replica/bulk.rs:453:32` | Background | 170 | 59283 | 138479 | 421050 | 426594 |
| `replica/sql.rs:749:14` | Interactive | 600 | 13358 | 25599 | 380314 | 422873 |
| `replica/blockers.rs:510:14` | Background | 1041 | 329 | 1818 | 3290 | 369896 |
| `replica.rs:1498:36` | Background | 734 | 1188 | 3809 | 8042 | 365588 |
| `replica/held/late.rs:485:36` | Background | 170 | 1503 | 4156 | 10086 | 361404 |
| `replica/compaction/rebootstrap.rs:318:32` | Background | 255 | 556 | 1832 | 5286 | 355849 |
| `network/frontiers.rs:114:22` | Background | 170 | 1518 | 4534 | 11875 | 88101 |
| `replica/roles.rs:1103:14` | Background | 477 | 1326 | 3955 | 9648 | 70247 |

Per-unit costs: full acquisition hold divided by observed work, in nanoseconds.
Costs include other work within that acquisition; they are conservative amortized costs.

| Site | Class | Unit | Acquisitions | Work | p50 | p95 | p99 | Max |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `replica/group.rs:147:32` | Background | encoded_byte | 136 | 4184050 | 2897 | 6126 | 12208 | 43554 |
| `replica/group.rs:147:32` | Background | received_frame | 136 | 2941 | 4123666 | 8715312 | 17372218 | 61989857 |

`metric=checkpoint_passive samples=11 p50_us=42272 p95_us=87428 p99_us=87428 max_us=87428`

`metric=checkpoint_passive_ns_per_page samples=11 p50_us=4686 p95_us=8655 p99_us=8655 max_us=8655`

## Pixel L3 Flushed

| Metric (microseconds) | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 35177 | 44710 | 95064 | 185219594 |
| observed | 600 | 44 | 81 | 146 | 1179 |
| write_lock_wait | 600 | 12 | 24 | 383 | 2877 |
| write_lock_hold | 600 | 30292 | 37421 | 54633 | 121684 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| replica_flush_wait | 600 | 0 | 0 | 0 | 0 |

Load duration: 186.274 s; work: 100000 (rows in L1/L3, authored rows in L2/L4).

Top holders by p99; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/compaction.rs:1977:36` | Background | 3 | 32919531 | 70374819 | 70374819 | 70374819 |
| `replica/tips.rs:1319:36` | Background | 8 | 20231 | 41836539 | 41836539 | 41836539 |
| `replica/publication.rs:344:36` | Background | 4 | 599 | 874565 | 874565 | 874565 |
| `replica/tip_observation.rs:198:36` | Background | 2 | 18710 | 200433 | 200433 | 200433 |
| `replica/tip_observation.rs:187:14` | Background | 2 | 21230 | 124666 | 124666 | 124666 |
| `replica/schemas.rs:502:14` | Background | 4 | 24626 | 42841 | 42841 | 42841 |
| `replica/sql.rs:749:14` | Interactive | 600 | 24237 | 29576 | 40444 | 50107 |
| `replica/schemas.rs:1260:36` | Background | 1 | 31496 | 31496 | 31496 | 31496 |
| `replica/compaction.rs:2024:36` | Background | 1 | 30965 | 30965 | 30965 | 30965 |
| `replica/held/late.rs:485:36` | Background | 4 | 1274 | 21643 | 21643 | 21643 |

Top holders by max; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/compaction.rs:1977:36` | Background | 3 | 32919531 | 70374819 | 70374819 | 70374819 |
| `replica/tips.rs:1319:36` | Background | 8 | 20231 | 41836539 | 41836539 | 41836539 |
| `replica/publication.rs:344:36` | Background | 4 | 599 | 874565 | 874565 | 874565 |
| `replica/tip_observation.rs:198:36` | Background | 2 | 18710 | 200433 | 200433 | 200433 |
| `replica/tip_observation.rs:187:14` | Background | 2 | 21230 | 124666 | 124666 | 124666 |
| `replica/sql.rs:1098:18` | Background | 600 | 4436 | 5538 | 11817 | 95452 |
| `replica/sql.rs:749:14` | Interactive | 600 | 24237 | 29576 | 40444 | 50107 |
| `replica/schemas.rs:502:14` | Background | 4 | 24626 | 42841 | 42841 | 42841 |
| `replica/schemas.rs:1260:36` | Background | 1 | 31496 | 31496 | 31496 | 31496 |
| `replica/compaction.rs:2024:36` | Background | 1 | 30965 | 30965 | 30965 | 30965 |

Per-unit costs: full acquisition hold divided by observed work, in nanoseconds.
Costs include other work within that acquisition; they are conservative amortized costs.

| Site | Class | Unit | Acquisitions | Work | p50 | p95 | p99 | Max |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `replica/compaction.rs:1977:36` | Background | base_row | 3 | 100013 | 83828000 | 6583906200 | 6583906200 | 6583906200 |
| `replica/compaction.rs:1977:36` | Background | replayed_frame | 3 | 3186 | 22158318 | 16459765500 | 16459765500 | 16459765500 |

`metric=checkpoint_passive samples=12 p50_us=105945 p95_us=153160 p99_us=153160 max_us=153160`

`metric=checkpoint_passive_ns_per_page samples=12 p50_us=8634 p95_us=16029 p99_us=16029 max_us=16029`

`metric=checkpoint_truncate samples=2 p50_us=120595 p95_us=168815 p99_us=168815 max_us=168815`

## Pixel L3 Buffered

| Metric (microseconds) | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 30737 | 41721 | 64070 | 242509683 |
| observed | 600 | 36 | 68 | 188 | 1724 |
| write_lock_wait | 600 | 5 | 9 | 12 | 242 |
| write_lock_hold | 600 | 24826 | 32422 | 39827 | 53225 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| replica_flush_wait | 3 | 2575 | 2578 | 2578 | 2578 |

Load duration: 243.133 s; work: 100000 (rows in L1/L3, authored rows in L2/L4).

Top holders by p99; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/compaction.rs:1977:36` | Background | 3 | 44771295 | 89081073 | 89081073 | 89081073 |
| `replica/tips.rs:1319:36` | Background | 8 | 18676 | 54879111 | 54879111 | 54879111 |
| `replica/publication.rs:344:36` | Background | 4 | 524 | 995679 | 995679 | 995679 |
| `replica/tip_observation.rs:198:36` | Background | 2 | 29944 | 221224 | 221224 | 221224 |
| `replica/tip_observation.rs:187:14` | Background | 2 | 23057 | 146001 | 146001 | 146001 |
| `replica/sql.rs:749:14` | Interactive | 600 | 24808 | 32405 | 39752 | 53206 |
| `replica/compaction.rs:2024:36` | Background | 1 | 36619 | 36619 | 36619 | 36619 |
| `replica/schemas.rs:1260:36` | Background | 1 | 31956 | 31956 | 31956 | 31956 |
| `replica/schemas.rs:502:14` | Background | 4 | 20400 | 29784 | 29784 | 29784 |
| `replica/tip_observation.rs:289:32` | Background | 1 | 14681 | 14681 | 14681 | 14681 |

Top holders by max; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/compaction.rs:1977:36` | Background | 3 | 44771295 | 89081073 | 89081073 | 89081073 |
| `replica/tips.rs:1319:36` | Background | 8 | 18676 | 54879111 | 54879111 | 54879111 |
| `replica/publication.rs:344:36` | Background | 4 | 524 | 995679 | 995679 | 995679 |
| `replica/tip_observation.rs:198:36` | Background | 2 | 29944 | 221224 | 221224 | 221224 |
| `replica/tip_observation.rs:187:14` | Background | 2 | 23057 | 146001 | 146001 | 146001 |
| `replica/sql.rs:749:14` | Interactive | 600 | 24808 | 32405 | 39752 | 53206 |
| `replica.rs:1498:36` | Background | 601 | 4118 | 5719 | 9242 | 47696 |
| `replica/compaction.rs:2024:36` | Background | 1 | 36619 | 36619 | 36619 | 36619 |
| `replica/schemas.rs:1260:36` | Background | 1 | 31956 | 31956 | 31956 | 31956 |
| `replica/schemas.rs:502:14` | Background | 4 | 20400 | 29784 | 29784 | 29784 |

Per-unit costs: full acquisition hold divided by observed work, in nanoseconds.
Costs include other work within that acquisition; they are conservative amortized costs.

| Site | Class | Unit | Acquisitions | Work | p50 | p95 | p99 | Max |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `replica/compaction.rs:1977:36` | Background | base_row | 3 | 100013 | 41174000 | 8954259000 | 8954259000 | 8954259000 |
| `replica/compaction.rs:1977:36` | Background | replayed_frame | 3 | 3183 | 28048196 | 22385647500 | 22385647500 | 22385647500 |

`metric=checkpoint_passive samples=12 p50_us=42615 p95_us=147280 p99_us=147280 max_us=147280`

`metric=checkpoint_passive_ns_per_page samples=12 p50_us=5008 p95_us=11318 p99_us=11318 max_us=11318`

`metric=checkpoint_truncate samples=2 p50_us=180213 p95_us=197251 p99_us=197251 max_us=197251`

## Pixel L4 Flushed

| Metric (microseconds) | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 59274 | 434358 | 585164 | 730162 |
| observed | 600 | 33 | 50 | 121 | 549 |
| write_lock_wait | 600 | 3 | 74744 | 123066 | 253418 |
| write_lock_hold | 600 | 17707 | 133067 | 137543 | 141624 |
| write_flush_wait | 600 | 3215 | 56030 | 220576 | 224494 |
| replica_flush_wait | 1467 | 3601 | 55107 | 220468 | 226639 |

Load duration: 78.597 s; work: 27744 (rows in L1/L3, authored rows in L2/L4).

Top holders by p99; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/sql.rs:1098:18` | Background | 1467 | 2127 | 108761 | 113200 | 152791 |
| `replica/sql.rs:749:14` | Interactive | 1467 | 39213 | 57090 | 81523 | 84880 |
| `replica.rs:1498:36` | Background | 1467 | 31 | 52537 | 55694 | 60093 |
| `replica/blockers.rs:510:14` | Background | 1467 | 489 | 1009 | 1745 | 3674 |
| `replica/sql.rs:181:14` | Background | 602 | 316 | 1154 | 1517 | 2637 |
| `replica/session.rs:64:32` | Background | 602 | 19 | 52 | 60 | 168 |
| `replica/session.rs:298:14` | Background | 1467 | 7 | 15 | 38 | 255 |
| `replica.rs:1478:36` | Background | 1467 | 1 | 3 | 5 | 82 |

Top holders by max; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/sql.rs:1098:18` | Background | 1467 | 2127 | 108761 | 113200 | 152791 |
| `replica/sql.rs:749:14` | Interactive | 1467 | 39213 | 57090 | 81523 | 84880 |
| `replica.rs:1498:36` | Background | 1467 | 31 | 52537 | 55694 | 60093 |
| `replica/blockers.rs:510:14` | Background | 1467 | 489 | 1009 | 1745 | 3674 |
| `replica/sql.rs:181:14` | Background | 602 | 316 | 1154 | 1517 | 2637 |
| `replica/session.rs:298:14` | Background | 1467 | 7 | 15 | 38 | 255 |
| `replica/session.rs:64:32` | Background | 602 | 19 | 52 | 60 | 168 |
| `replica.rs:1478:36` | Background | 1467 | 1 | 3 | 5 | 82 |

Per-unit costs: full acquisition hold divided by observed work, in nanoseconds.
Costs include other work within that acquisition; they are conservative amortized costs.

| Site | Class | Unit | Acquisitions | Work | p50 | p95 | p99 | Max |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |

`metric=checkpoint_passive samples=71 p50_us=42 p95_us=53190 p99_us=59689 max_us=59689`

`metric=checkpoint_passive_ns_per_page samples=71 p50_us=12 p95_us=5902 p99_us=6643 max_us=6643`

`metric=checkpoint_truncate samples=334 p50_us=52893 p95_us=56067 p99_us=58044 max_us=149388`

`metric=checkpoint_truncate_ns_per_page samples=333 p50_us=15676 p95_us=16611 p99_us=17150 max_us=17778`

## Pixel L4 Buffered

| Metric (microseconds) | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 51194 | 375585 | 430992 | 487550 |
| observed | 600 | 38 | 66 | 212 | 637 |
| write_lock_wait | 600 | 2 | 11 | 113649 | 184199 |
| write_lock_hold | 600 | 15571 | 24438 | 27101 | 31018 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| replica_flush_wait | 783 | 5742 | 112212 | 163673 | 170431 |

Load duration: 76.531 s; work: 25056 (rows in L1/L3, authored rows in L2/L4).

Top holders by p99; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/sql.rs:1098:18` | Background | 783 | 2262 | 110014 | 115046 | 118280 |
| `replica.rs:1498:36` | Background | 1370 | 94 | 65097 | 112119 | 278085 |
| `replica/sql.rs:749:14` | Interactive | 1383 | 49279 | 59731 | 80616 | 86492 |
| `replica/blockers.rs:510:14` | Background | 1383 | 510 | 1260 | 1888 | 2648 |
| `replica/sql.rs:181:14` | Background | 2 | 739 | 1058 | 1058 | 1058 |
| `replica/session.rs:64:32` | Background | 2 | 39 | 39 | 39 | 39 |
| `replica/session.rs:298:14` | Background | 1383 | 8 | 18 | 37 | 333 |
| `replica.rs:1478:36` | Background | 1370 | 1 | 3 | 5 | 91 |

Top holders by max; hold percentiles in microseconds. Unit: acquisition.

| Site | Class | Acquisitions | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica.rs:1498:36` | Background | 1370 | 94 | 65097 | 112119 | 278085 |
| `replica/sql.rs:1098:18` | Background | 783 | 2262 | 110014 | 115046 | 118280 |
| `replica/sql.rs:749:14` | Interactive | 1383 | 49279 | 59731 | 80616 | 86492 |
| `replica/blockers.rs:510:14` | Background | 1383 | 510 | 1260 | 1888 | 2648 |
| `replica/sql.rs:181:14` | Background | 2 | 739 | 1058 | 1058 | 1058 |
| `replica/session.rs:298:14` | Background | 1383 | 8 | 18 | 37 | 333 |
| `replica.rs:1478:36` | Background | 1370 | 1 | 3 | 5 | 91 |
| `replica/session.rs:64:32` | Background | 2 | 39 | 39 | 39 | 39 |

Per-unit costs: full acquisition hold divided by observed work, in nanoseconds.
Costs include other work within that acquisition; they are conservative amortized costs.

| Site | Class | Unit | Acquisitions | Work | p50 | p95 | p99 | Max |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |

`metric=checkpoint_passive samples=84 p50_us=36 p95_us=59445 p99_us=64997 max_us=64997`

`metric=checkpoint_passive_ns_per_page samples=84 p50_us=10 p95_us=6816 p99_us=7623 max_us=7623`

`metric=checkpoint_truncate samples=314 p50_us=52914 p95_us=56197 p99_us=58079 max_us=219246`

`metric=checkpoint_truncate_ns_per_page samples=313 p50_us=15499 p95_us=16456 p99_us=16910 max_us=17092`

## Desktop checkpoint bootstrap 1

Release, 100k fixture; outside the timed write probe.
Holds in microseconds; per-unit costs in nanoseconds per row/frame/byte.

Top holders by p99.

| Site | Class | Holds | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/checkpoint/transfer.rs:1715:36` | Background | 1 | 2356266 | 2356266 | 2356266 | 2356266 |
| `replica/checkpoint/transfer.rs:1661:36` | Background | 847 | 11242 | 31191 | 64778 | 134637 |
| `replica/schemas.rs:1152:14` | Background | 1 | 19207 | 19207 | 19207 | 19207 |
| `replica/checkpoint/transfer.rs:1638:36` | Background | 847 | 606 | 4244 | 16966 | 32439 |
| `replica/sql.rs:726:52` | Background | 2 | 39 | 15715 | 15715 | 15715 |
| `replica/checkpoint/transfer.rs:1495:36` | Background | 1 | 8562 | 8562 | 8562 | 8562 |
| `replica/schemas.rs:523:36` | Background | 3 | 1632 | 3882 | 3882 | 3882 |
| `replica/identity.rs:110:36` | Background | 1 | 3392 | 3392 | 3392 | 3392 |
| `replica/tips.rs:1613:36` | Background | 1 | 2596 | 2596 | 2596 | 2596 |
| `replica/placements.rs:632:36` | Background | 17 | 345 | 2012 | 2012 | 2012 |

Top holders by max.

| Site | Class | Holds | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/checkpoint/transfer.rs:1715:36` | Background | 1 | 2356266 | 2356266 | 2356266 | 2356266 |
| `replica/checkpoint/transfer.rs:1661:36` | Background | 847 | 11242 | 31191 | 64778 | 134637 |
| `replica/checkpoint/transfer.rs:1638:36` | Background | 847 | 606 | 4244 | 16966 | 32439 |
| `replica/schemas.rs:1152:14` | Background | 1 | 19207 | 19207 | 19207 | 19207 |
| `replica/sql.rs:726:52` | Background | 2 | 39 | 15715 | 15715 | 15715 |
| `replica/checkpoint/transfer.rs:1495:36` | Background | 1 | 8562 | 8562 | 8562 | 8562 |
| `replica/schemas.rs:523:36` | Background | 3 | 1632 | 3882 | 3882 | 3882 |
| `replica/identity.rs:110:36` | Background | 1 | 3392 | 3392 | 3392 | 3392 |
| `replica/checkpoint/transfer.rs:1616:36` | Background | 847 | 141 | 198 | 432 | 2973 |
| `replica/tips.rs:1613:36` | Background | 1 | 2596 | 2596 | 2596 | 2596 |

| Site | Class | Unit | Holds | Work | p50 | p95 | p99 | Max |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `replica/checkpoint/transfer.rs:1661:36` | Background | checkpoint_row | 847 | 100000 | 93511 | 441300 | 859353 | 1517821 |
| `replica/checkpoint/transfer.rs:1715:36` | Background | base_row | 1 | 100000 | 23562 | 23562 | 23562 | 23562 |

## Pixel checkpoint bootstrap 1

Release, 100k fixture; outside the timed write probe.
Holds in microseconds; per-unit costs in nanoseconds per row/frame/byte.

Top holders by p99.

| Site | Class | Holds | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/checkpoint/transfer.rs:1715:36` | Background | 2 | 149181 | 2635374 | 2635374 | 2635374 |
| `replica/compaction/rebootstrap.rs:327:36` | Background | 1 | 155047 | 155047 | 155047 | 155047 |
| `replica/checkpoint/transfer.rs:1661:36` | Background | 769 | 65315 | 102456 | 138880 | 166146 |
| `replica/sql.rs:1098:18` | Background | 1 | 70337 | 70337 | 70337 | 70337 |
| `replica/schemas.rs:1152:14` | Background | 1 | 59370 | 59370 | 59370 | 59370 |
| `replica/placements.rs:632:36` | Background | 66 | 2337 | 42718 | 46902 | 46902 |
| `replica/sql.rs:749:14` | Interactive | 2 | 11865 | 30099 | 30099 | 30099 |
| `replica/incoming.rs:15:40` | Background | 85 | 785 | 3638 | 26830 | 26830 |
| `replica/checkpoint/transfer.rs:1638:36` | Background | 769 | 5732 | 10055 | 13021 | 21126 |
| `replica/checkpoint/transfer.rs:1495:36` | Background | 2 | 9125 | 10993 | 10993 | 10993 |

Top holders by max.

| Site | Class | Holds | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/checkpoint/transfer.rs:1715:36` | Background | 2 | 149181 | 2635374 | 2635374 | 2635374 |
| `replica/checkpoint/transfer.rs:1661:36` | Background | 769 | 65315 | 102456 | 138880 | 166146 |
| `replica/compaction/rebootstrap.rs:327:36` | Background | 1 | 155047 | 155047 | 155047 | 155047 |
| `replica/sql.rs:1098:18` | Background | 1 | 70337 | 70337 | 70337 | 70337 |
| `replica/schemas.rs:1152:14` | Background | 1 | 59370 | 59370 | 59370 | 59370 |
| `replica/placements.rs:632:36` | Background | 66 | 2337 | 42718 | 46902 | 46902 |
| `replica/sql.rs:749:14` | Interactive | 2 | 11865 | 30099 | 30099 | 30099 |
| `replica/incoming.rs:15:40` | Background | 85 | 785 | 3638 | 26830 | 26830 |
| `replica/checkpoint/transfer.rs:1638:36` | Background | 769 | 5732 | 10055 | 13021 | 21126 |
| `replica/checkpoint/transfer.rs:1495:36` | Background | 2 | 9125 | 10993 | 10993 | 10993 |

| Site | Class | Unit | Holds | Work | p50 | p95 | p99 | Max |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `replica/checkpoint/transfer.rs:1661:36` | Background | checkpoint_row | 769 | 100001 | 476080 | 973891 | 1445538 | 13041000 |
| `replica/checkpoint/transfer.rs:1715:36` | Background | base_row | 2 | 100001 | 26353 | 149181000 | 149181000 | 149181000 |
| `replica/incoming.rs:15:40` | Background | applied_frame | 1 | 1 | 26830000 | 26830000 | 26830000 | 26830000 |
| `replica/schemas.rs:1152:14` | Background | replayed_frame | 1 | 1 | 59370000 | 59370000 | 59370000 | 59370000 |
| `replica/sql.rs:1098:18` | Background | replayed_frame | 1 | 1 | 70337000 | 70337000 | 70337000 | 70337000 |

## Pixel checkpoint bootstrap 2

Release, 100k fixture; outside the timed write probe.
Holds in microseconds; per-unit costs in nanoseconds per row/frame/byte.

Top holders by p99.

| Site | Class | Holds | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/checkpoint/transfer.rs:1715:36` | Background | 1 | 39520465 | 39520465 | 39520465 | 39520465 |
| `replica/group.rs:147:32` | Background | 313 | 41194 | 3622843 | 3968132 | 4051561 |
| `replica/incoming.rs:15:40` | Background | 53 | 3569 | 242509 | 290558 | 290558 |
| `replica/checkpoint/transfer.rs:1661:36` | Background | 768 | 139541 | 189984 | 208811 | 256917 |
| `replica.rs:1498:36` | Background | 54 | 93 | 315 | 177444 | 177444 |
| `replica/bulk.rs:453:32` | Background | 4 | 1453 | 174454 | 174454 | 174454 |
| `replica/group.rs:185:36` | Background | 313 | 2000 | 4595 | 167346 | 171389 |
| `replica/roles.rs:958:36` | Background | 7 | 1158 | 124205 | 124205 | 124205 |
| `replica/placements.rs:632:36` | Background | 18 | 1187 | 88214 | 88214 | 88214 |
| `replica/schemas.rs:1152:14` | Background | 1 | 60128 | 60128 | 60128 | 60128 |

Top holders by max.

| Site | Class | Holds | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `replica/checkpoint/transfer.rs:1715:36` | Background | 1 | 39520465 | 39520465 | 39520465 | 39520465 |
| `replica/group.rs:147:32` | Background | 313 | 41194 | 3622843 | 3968132 | 4051561 |
| `replica/roles.rs:1103:14` | Background | 317 | 1032 | 6853 | 11882 | 320297 |
| `replica/incoming.rs:15:40` | Background | 53 | 3569 | 242509 | 290558 | 290558 |
| `replica/checkpoint/transfer.rs:1661:36` | Background | 768 | 139541 | 189984 | 208811 | 256917 |
| `replica.rs:1498:36` | Background | 54 | 93 | 315 | 177444 | 177444 |
| `replica/bulk.rs:453:32` | Background | 4 | 1453 | 174454 | 174454 | 174454 |
| `replica/group.rs:185:36` | Background | 313 | 2000 | 4595 | 167346 | 171389 |
| `replica/roles.rs:958:36` | Background | 7 | 1158 | 124205 | 124205 | 124205 |
| `replica/placements.rs:632:36` | Background | 18 | 1187 | 88214 | 88214 | 88214 |

| Site | Class | Unit | Holds | Work | p50 | p95 | p99 | Max |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `replica/checkpoint/transfer.rs:1661:36` | Background | checkpoint_row | 768 | 100000 | 979436 | 1737111 | 2104282 | 4071782 |
| `replica/checkpoint/transfer.rs:1715:36` | Background | base_row | 1 | 100000 | 395204 | 395204 | 395204 | 395204 |
| `replica/group.rs:147:32` | Background | encoded_byte | 313 | 40439599 | 317 | 28179 | 30726 | 233050 |
| `replica/group.rs:147:32` | Background | received_frame | 313 | 3125 | 4119400 | 365268900 | 398572300 | 2670520000 |
| `replica/schemas.rs:1152:14` | Background | replayed_frame | 1 | 1 | 60128000 | 60128000 | 60128000 | 60128000 |
