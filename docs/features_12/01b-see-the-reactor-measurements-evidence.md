# Reactor attribution measurements

Work in progress, 2026-10-27. Initial implementation: change `nktmmvzx`.

Desktop: Apple M5 Pro, 18 cores, arm64 macOS. Phone: physical Pixel 11 Pro.
Release, `rocky-sdk --no-default-features --features qualification`, in-memory SDK transport.
Each scenario and durability uses 600 measured writes after 50 warm-up writes, paced at 100 ms.
L1 and L3 use the 100k-row fixture. Finite loads may end before the probe.

The host is shared with builds: desktop initial load averages exceeded 149 on 18 cores.
Poll durations include OS descheduling, and these are attribution measurements, not controlled speedup claims.
Only completed 600-sample blocks appear below. Missing blocks are still pending.

Reproduce with:

```sh
cargo test --release -p rocky-sdk --no-default-features --features qualification \
  --test migrations write_probe:: -- --ignored --nocapture --test-threads=1
```

All durations are microseconds. Poll percentiles include only polls above 1 ms.
`write_sql_in_call` overlaps lock and flush time. Queue time can overlap the transaction-wide lock hold;
`write_attribution_overlap` reports the excess when the requested sum exceeds the full write time.

## Desktop before interactive changes: L0 Flushed

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 6247 | 23690 | 59481 | 96050 |
| observed | 600 | 6 | 9 | 12 | 282 |
| write_lock_wait | 600 | 0 | 1 | 2 | 97 |
| write_lock_hold | 600 | 5648 | 19204 | 46942 | 85717 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 1441 | 15362 | 42426 | 85153 |
| write_sql_in_call | 600 | 5734 | 18688 | 47026 | 85629 |
| write_unattributed | 600 | 85 | 786 | 7401 | 15972 |
| write_attribution_overlap | 600 | 0 | 12325 | 32898 | 84633 |
| replica_flush_wait | 600 | 0 | 0 | 0 | 0 |
| checkpoint_passive | 5 | 28420 | 39279 | 39279 | 39279 |
| checkpoint_passive_ns_per_page | 5 | 3475 | 4755 | 4755 | 4755 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| sql | 621 | 4695 | 17025 | 45958 | 84775 |
| connect | 11 | 1725 | 4370 | 4370 | 4370 |
| database | 2 | 2615 | 4267 | 4267 | 4267 |
| publish_links | 1 | 3065 | 3065 | 3065 | 3065 |

Load elapsed: 60011082 us.

## Desktop before interactive changes: L0 Buffered

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 5557 | 14018 | 28931 | 59208 |
| observed | 600 | 6 | 9 | 30 | 150 |
| write_lock_wait | 600 | 0 | 1 | 2 | 44 |
| write_lock_hold | 600 | 4908 | 12154 | 23048 | 53265 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 260 | 652 | 3052 | 38754 |
| write_sql_in_call | 600 | 5081 | 12481 | 22884 | 53190 |
| write_unattributed | 600 | 337 | 648 | 3592 | 11283 |
| write_attribution_overlap | 600 | 0 | 0 | 110 | 2350 |
| replica_flush_wait | 0 | 0 | 0 | 0 | 0 |
| checkpoint_passive | 5 | 28412 | 106035 | 106035 | 106035 |
| checkpoint_passive_ns_per_page | 5 | 3389 | 12717 | 12717 | 12717 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| settle_commits | 17 | 2336 | 106771 | 106771 | 106771 |
| sql | 625 | 4181 | 11265 | 22321 | 52076 |

Load elapsed: 60014702 us.

## Pixel before interactive changes: L0 Flushed

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 29378 | 68034 | 391313 | 415539 |
| observed | 600 | 38 | 83 | 110 | 640 |
| write_lock_wait | 600 | 13 | 23 | 33 | 172 |
| write_lock_hold | 600 | 23876 | 31721 | 370752 | 411524 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 7628 | 25348 | 353552 | 704895 |
| write_sql_in_call | 600 | 23143 | 33218 | 358961 | 410793 |
| write_unattributed | 600 | 0 | 2864 | 327965 | 349442 |
| write_attribution_overlap | 600 | 198 | 19492 | 33186 | 700783 |
| replica_flush_wait | 600 | 0 | 0 | 0 | 0 |
| checkpoint_passive | 5 | 43760 | 70702 | 70702 | 70702 |
| checkpoint_passive_ns_per_page | 5 | 5330 | 8620 | 8620 | 8620 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| sql | 1204 | 8680 | 20531 | 47505 | 405482 |
| connect | 574 | 2106 | 2650 | 25052 | 347233 |
| database | 18 | 1916 | 339187 | 339187 | 339187 |
| recovery | 2 | 1081 | 1265 | 1265 | 1265 |

Load elapsed: 72457025 us.
