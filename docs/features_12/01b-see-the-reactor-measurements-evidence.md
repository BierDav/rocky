# Reactor attribution measurements

Qualification completed 2026-10-27. Initial measurement: `nktmmvzx`; final implementation: `orswmwyu`.

Desktop: Apple M5 Pro, 18 cores, arm64 macOS. Phone: physical Pixel 11 Pro.
Release, `rocky-sdk --no-default-features --features qualification`, in-memory SDK transport.
Each scenario and durability uses 600 measured writes after 50 warm-up writes, paced at 100 ms.
L1 and L3 use the 100k-row fixture. Finite loads may end before the probe.

The host is shared with builds: desktop initial load averages exceeded 149 on 18 cores.
Poll durations include OS descheduling, and these are attribution measurements, not controlled speedup claims.
Desktop baseline and final matrices are complete. The phone baseline is partial; its final matrix is complete. Only completed 600-sample blocks appear below.

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

## Desktop before interactive changes: L1 Flushed

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 9251 | 171847 | 195377 | 372678 |
| observed | 600 | 4 | 7 | 8 | 27 |
| write_lock_wait | 600 | 1 | 27 | 33 | 43 |
| write_lock_hold | 600 | 7236 | 9449 | 75080 | 95728 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 7274 | 164280 | 191751 | 371335 |
| write_sql_in_call | 600 | 7458 | 9699 | 78826 | 167248 |
| write_unattributed | 600 | 0 | 297 | 331 | 351 |
| write_attribution_overlap | 600 | 1974 | 7222 | 8005 | 10466 |
| replica_flush_wait | 603 | 0 | 0 | 0 | 0 |
| checkpoint_passive | 49 | 72746 | 89814 | 91740 | 91740 |
| checkpoint_passive_ns_per_page | 49 | 6355 | 7568 | 8845 | 8845 |
| checkpoint_truncate | 3 | 83675 | 97307 | 97307 | 97307 |
| checkpoint_truncate_ns_per_page | 0 | 0 | 0 | 0 | 0 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| call | 342 | 86321 | 96052 | 104367 | 151692 |
| settle_commits | 41 | 73975 | 89846 | 97345 | 97345 |
| sql | 616 | 6117 | 8205 | 71870 | 94309 |
| recovery | 14 | 1121 | 1294 | 1294 | 1294 |

Load elapsed: 28360119 us.

## Desktop before interactive changes: L1 Buffered

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 8636 | 202513 | 284928 | 373525 |
| observed | 600 | 3 | 7 | 10 | 27 |
| write_lock_wait | 600 | 0 | 1 | 1 | 2 |
| write_lock_hold | 600 | 4623 | 9069 | 10141 | 17751 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 198 | 195452 | 281887 | 370441 |
| write_sql_in_call | 600 | 4675 | 9320 | 11075 | 162712 |
| write_unattributed | 600 | 81 | 343 | 1250 | 159994 |
| write_attribution_overlap | 600 | 0 | 0 | 0 | 0 |
| replica_flush_wait | 4 | 0 | 5013 | 5013 | 5013 |
| checkpoint_passive | 45 | 72101 | 89309 | 95157 | 95157 |
| checkpoint_passive_ns_per_page | 45 | 5583 | 7687 | 8866 | 8866 |
| checkpoint_truncate | 4 | 67525 | 87601 | 87601 | 87601 |
| checkpoint_truncate_ns_per_page | 0 | 0 | 0 | 0 | 0 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| call | 349 | 89392 | 100530 | 135325 | 179710 |
| settle_commits | 24 | 1338 | 86147 | 92913 | 92913 |
| sql | 699 | 3471 | 7962 | 9090 | 17328 |
| recovery | 21 | 1121 | 1478 | 1484 | 1484 |

Load elapsed: 28782310 us.

## Desktop before interactive changes: L2 Flushed

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 13629 | 59094 | 74030 | 98291 |
| observed | 600 | 2 | 3 | 3 | 3 |
| write_lock_wait | 600 | 0 | 232 | 257 | 332 |
| write_lock_hold | 600 | 2904 | 11587 | 15802 | 35426 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 11677 | 57077 | 73196 | 103396 |
| write_sql_in_call | 600 | 3080 | 11616 | 15826 | 35485 |
| write_unattributed | 600 | 0 | 79 | 101 | 129 |
| write_attribution_overlap | 600 | 1587 | 8213 | 12820 | 46145 |
| replica_flush_wait | 1792 | 0 | 0 | 0 | 0 |
| checkpoint_passive | 18 | 24410 | 39079 | 39079 | 39079 |
| checkpoint_passive_ns_per_page | 18 | 2905 | 4530 | 4530 | 4530 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| call | 3189 | 5882 | 52807 | 68771 | 76389 |
| settle_commits | 9 | 25723 | 39088 | 39088 | 39088 |
| sql | 605 | 2341 | 11105 | 14998 | 33320 |

Load elapsed: 60217811 us.

## Desktop before interactive changes: L2 Buffered

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 13240 | 56739 | 74665 | 133886 |
| observed | 600 | 2 | 3 | 5 | 34 |
| write_lock_wait | 600 | 0 | 0 | 0 | 0 |
| write_lock_hold | 600 | 2697 | 10396 | 12424 | 15490 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 9338 | 53568 | 71918 | 123460 |
| write_sql_in_call | 600 | 2659 | 10907 | 13835 | 45504 |
| write_unattributed | 600 | 25 | 599 | 5794 | 33131 |
| write_attribution_overlap | 600 | 0 | 141 | 147 | 203 |
| replica_flush_wait | 1376 | 0 | 7821 | 16276 | 31037 |
| checkpoint_passive | 17 | 25524 | 40222 | 40222 | 40222 |
| checkpoint_passive_ns_per_page | 17 | 2802 | 4244 | 4244 | 4244 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| call | 2986 | 6821 | 52199 | 68958 | 75666 |
| settle_commits | 7 | 20510 | 31344 | 31344 | 31344 |
| sql | 600 | 2308 | 10104 | 12107 | 14845 |

Load elapsed: 60146312 us.

## Desktop before interactive changes: L3 Flushed

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 7605 | 56166 | 95444 | 196833062 |
| observed | 600 | 6 | 11 | 25 | 1053 |
| write_lock_wait | 600 | 0 | 1 | 3 | 68 |
| write_lock_hold | 600 | 6725 | 31602 | 57819 | 146944 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 4645 | 26324 | 60920 | 196801230 |
| write_sql_in_call | 600 | 6628 | 21430 | 55645 | 145670 |
| write_unattributed | 600 | 107 | 7329 | 20530 | 32764 |
| write_attribution_overlap | 600 | 0 | 13202 | 29199 | 70018 |
| replica_flush_wait | 600 | 0 | 0 | 0 | 0 |
| checkpoint_passive | 12 | 136953 | 200087 | 200087 | 200087 |
| checkpoint_passive_ns_per_page | 12 | 13262 | 18593 | 18593 | 18593 |
| checkpoint_truncate | 2 | 127126 | 130090 | 130090 | 130090 |
| checkpoint_truncate_ns_per_page | 0 | 0 | 0 | 0 | 0 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| call | 10 | 25223 | 196834277 | 196834277 | 196834277 |
| sql | 639 | 5358 | 18815 | 51557 | 144431 |
| connect | 64 | 2714 | 16824 | 36304 | 36304 |
| database | 6 | 1133 | 13935 | 13935 | 13935 |
| recovery | 3 | 1663 | 1870 | 1870 | 1870 |

Load elapsed: 197253173 us.

## Desktop before interactive changes: L3 Buffered

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 6977 | 52192 | 222361 | 202707571 |
| observed | 600 | 5 | 12 | 106 | 656 |
| write_lock_wait | 600 | 0 | 1 | 1 | 6 |
| write_lock_hold | 600 | 5935 | 31603 | 144147 | 245233 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 343 | 13983 | 46112 | 202701123 |
| write_sql_in_call | 600 | 5915 | 29204 | 158513 | 259860 |
| write_unattributed | 600 | 324 | 4906 | 19719 | 126918 |
| write_attribution_overlap | 600 | 0 | 123 | 6606 | 16785 |
| replica_flush_wait | 1 | 12081 | 12081 | 12081 | 12081 |
| checkpoint_passive | 12 | 108028 | 157801 | 157801 | 157801 |
| checkpoint_passive_ns_per_page | 12 | 10509 | 13899 | 13899 | 13899 |
| checkpoint_truncate | 2 | 137411 | 181011 | 181011 | 181011 |
| checkpoint_truncate_ns_per_page | 0 | 0 | 0 | 0 | 0 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| call | 12 | 7397 | 202800106 | 202800106 | 202800106 |
| sql | 683 | 4668 | 24800 | 127844 | 252139 |
| publish_links | 2 | 1056 | 117811 | 117811 | 117811 |
| settle_commits | 128 | 1541 | 16977 | 35250 | 92087 |
| database | 12 | 2002 | 25476 | 25476 | 25476 |
| recovery | 27 | 2285 | 6883 | 8840 | 8840 |

Load elapsed: 203119380 us.

## Desktop before interactive changes: L4 Flushed

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 39133 | 373081 | 389524 | 552442 |
| observed | 600 | 4 | 6 | 6 | 45 |
| write_lock_wait | 600 | 0 | 12881 | 28606 | 94185 |
| write_lock_hold | 600 | 12142 | 128828 | 136589 | 168894 |
| write_flush_wait | 600 | 14016 | 235749 | 244341 | 250648 |
| write_reactor_delay | 600 | 9533 | 66137 | 120002 | 418496 |
| write_sql_in_call | 600 | 37423 | 510459 | 735148 | 756953 |
| write_unattributed | 600 | 0 | 292 | 2866 | 21197 |
| write_attribution_overlap | 600 | 3559 | 21308 | 51269 | 99091 |
| replica_flush_wait | 2782 | 5844 | 36738 | 236816 | 250648 |
| checkpoint_passive | 96 | 18 | 80831 | 137654 | 137654 |
| checkpoint_passive_ns_per_page | 96 | 5 | 7630 | 14004 | 14004 |
| checkpoint_truncate | 433 | 59812 | 63414 | 69363 | 88833 |
| checkpoint_truncate_ns_per_page | 432 | 17371 | 18396 | 20146 | 25801 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| sql | 3127 | 10256 | 115622 | 123160 | 162315 |
| settle_commits | 89 | 62446 | 123220 | 127162 | 127162 |
| recovery | 9 | 3859 | 28225 | 28225 | 28225 |
| publish_links | 2 | 12196 | 25354 | 25354 | 25354 |
| database | 20 | 4805 | 12022 | 18707 | 18707 |
| synchronize | 5 | 7844 | 18515 | 18515 | 18515 |
| connect | 13 | 4305 | 11157 | 11157 | 11157 |

Load elapsed: 79767105 us.

## Desktop before interactive changes: L4 Buffered

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 15870 | 259835 | 279702 | 334709 |
| observed | 600 | 4 | 6 | 33 | 133 |
| write_lock_wait | 600 | 0 | 14303 | 38522 | 142599 |
| write_lock_hold | 600 | 6730 | 17575 | 33500 | 64990 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 7658 | 237530 | 254140 | 303814 |
| write_sql_in_call | 600 | 6836 | 31432 | 136788 | 269052 |
| write_unattributed | 600 | 78 | 1755 | 115975 | 212769 |
| write_attribution_overlap | 600 | 0 | 96 | 3986 | 26753 |
| replica_flush_wait | 2208 | 4194 | 13090 | 53798 | 133621 |
| checkpoint_passive | 94 | 15 | 68562 | 210869 | 210869 |
| checkpoint_passive_ns_per_page | 94 | 4 | 6817 | 22055 | 22055 |
| checkpoint_truncate | 394 | 58662 | 62846 | 64634 | 74852 |
| checkpoint_truncate_ns_per_page | 393 | 17192 | 18419 | 18943 | 21937 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| sql | 3012 | 10185 | 47450 | 120981 | 211505 |
| settle_commits | 105 | 116616 | 125629 | 173967 | 183694 |
| synchronize | 12 | 7791 | 49040 | 49040 | 49040 |
| publish_links | 2 | 28858 | 37982 | 37982 | 37982 |
| recovery | 28 | 8565 | 23017 | 33057 | 33057 |
| database | 6 | 2717 | 16309 | 16309 | 16309 |

Load elapsed: 71335832 us.

## Desktop after interactive changes: L0 Flushed

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 6831 | 50163 | 116212 | 367023 |
| observed | 600 | 6 | 11 | 22 | 193 |
| write_lock_wait | 600 | 0 | 1 | 2 | 42 |
| write_lock_hold | 600 | 5411 | 26957 | 90035 | 315756 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 4532 | 27646 | 62468 | 278276 |
| write_sql_in_call | 600 | 5442 | 21698 | 99430 | 319762 |
| write_unattributed | 600 | 0 | 3412 | 11221 | 171498 |
| write_attribution_overlap | 600 | 1562 | 11900 | 49009 | 227009 |
| write_server_remainder | 600 | 109 | 191 | 1462 | 175003 |
| write_client_remainder | 600 | 140 | 13897 | 26480 | 36788 |
| write_sql_covered | 600 | 6414 | 34448 | 109535 | 365871 |
| replica_flush_wait | 600 | 0 | 0 | 0 | 0 |
| checkpoint_passive | 5 | 18930 | 38245 | 38245 | 38245 |
| checkpoint_passive_ns_per_page | 5 | 2279 | 4566 | 4566 | 4566 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| sql | 637 | 4375 | 18428 | 89431 | 232093 |
| connect | 23 | 1899 | 87326 | 181796 | 181796 |
| synchronize | 2 | 3623 | 76940 | 76940 | 76940 |
| settle_commits | 23 | 3059 | 22383 | 38781 | 38781 |
| database | 8 | 1459 | 5906 | 5906 | 5906 |
| recovery | 4 | 2808 | 4012 | 4012 | 4012 |
| publish_links | 1 | 1305 | 1305 | 1305 | 1305 |

Load elapsed: 60406695 us.

## Desktop after interactive changes: L0 Buffered

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 3389 | 12612 | 20252 | 54329 |
| observed | 600 | 4 | 5 | 5 | 18 |
| write_lock_wait | 600 | 0 | 0 | 0 | 1 |
| write_lock_hold | 600 | 3024 | 11161 | 17599 | 44749 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 93 | 389 | 6305 | 23994 |
| write_sql_in_call | 600 | 3140 | 11307 | 17878 | 23450 |
| write_unattributed | 600 | 212 | 377 | 1561 | 17848 |
| write_attribution_overlap | 600 | 0 | 0 | 133 | 14414 |
| write_server_remainder | 600 | 154 | 209 | 402 | 885 |
| write_client_remainder | 600 | 51 | 312 | 3652 | 21414 |
| write_sql_covered | 600 | 3310 | 12100 | 17966 | 43545 |
| replica_flush_wait | 0 | 0 | 0 | 0 | 0 |
| checkpoint_passive | 5 | 13847 | 30828 | 30828 | 30828 |
| checkpoint_passive_ns_per_page | 5 | 1667 | 3680 | 3680 | 3680 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| settle_commits | 6 | 13175 | 31295 | 31295 | 31295 |
| sql | 605 | 2616 | 10628 | 17128 | 22075 |
| database | 1 | 1956 | 1956 | 1956 | 1956 |
| recovery | 1 | 1058 | 1058 | 1058 | 1058 |

Load elapsed: 60038571 us.

## Desktop after interactive changes: L1 Flushed

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 112881 | 1738848 | 4384805 | 7103000 |
| observed | 600 | 3 | 5 | 10 | 81 |
| write_lock_wait | 600 | 0 | 0 | 0 | 4 |
| write_lock_hold | 600 | 66717 | 244985 | 430412 | 577940 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 71252 | 1686083 | 4384387 | 6950214 |
| write_sql_in_call | 600 | 66845 | 245044 | 430469 | 577984 |
| write_unattributed | 600 | 0 | 175 | 373 | 1152 |
| write_attribution_overlap | 600 | 1892 | 173759 | 282408 | 521530 |
| write_server_remainder | 600 | 57 | 98 | 257 | 384 |
| write_client_remainder | 600 | 46 | 159 | 633 | 12736 |
| write_sql_covered | 600 | 112835 | 1738790 | 4384767 | 7102946 |
| replica_flush_wait | 603 | 0 | 0 | 0 | 0 |
| checkpoint_passive | 47 | 144767 | 548011 | 1064248 | 1064248 |
| checkpoint_passive_ns_per_page | 47 | 10772 | 44208 | 67708 | 67708 |
| checkpoint_truncate | 4 | 147756 | 703328 | 703328 | 703328 |
| checkpoint_truncate_ns_per_page | 0 | 0 | 0 | 0 | 0 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| call | 341 | 77207 | 2213159 | 3398750 | 4663346 |
| settle_commits | 74 | 80383 | 548460 | 1064794 | 1064794 |
| sql | 600 | 66359 | 244608 | 430072 | 577638 |
| recovery | 3 | 1273 | 2340 | 2340 | 2340 |
| connect | 1 | 1110 | 1110 | 1110 | 1110 |

Load elapsed: 174744475 us.

## Desktop after interactive changes: L1 Buffered

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 6144 | 201136 | 271232 | 359369 |
| observed | 600 | 3 | 7 | 20 | 35 |
| write_lock_wait | 600 | 0 | 0 | 0 | 17 |
| write_lock_hold | 600 | 4279 | 10275 | 30771 | 98655 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 228 | 187424 | 267206 | 356301 |
| write_sql_in_call | 600 | 4307 | 9558 | 71062 | 118974 |
| write_unattributed | 600 | 81 | 2279 | 13883 | 116249 |
| write_attribution_overlap | 600 | 0 | 0 | 374 | 1409 |
| write_server_remainder | 600 | -1 | 291 | 1724 | 116155 |
| write_client_remainder | 600 | 62 | 3594 | 12125 | 22226 |
| write_sql_covered | 600 | 5760 | 201084 | 271157 | 359316 |
| replica_flush_wait | 3 | 0 | 0 | 0 | 0 |
| checkpoint_passive | 44 | 64115 | 95670 | 102644 | 102644 |
| checkpoint_passive_ns_per_page | 44 | 5000 | 6845 | 7811 | 7811 |
| checkpoint_truncate | 6 | 76854 | 92234 | 92234 | 92234 |
| checkpoint_truncate_ns_per_page | 0 | 0 | 0 | 0 | 0 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| call | 311 | 69821 | 93763 | 103799 | 119766 |
| sql | 611 | 3757 | 8156 | 14934 | 97940 |
| settle_commits | 71 | 1600 | 73891 | 95716 | 95716 |
| recovery | 14 | 1096 | 2580 | 2580 | 2580 |
| database | 1 | 2567 | 2567 | 2567 | 2567 |

Load elapsed: 23457093 us.

## Desktop after interactive changes: L2 Flushed

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 23893 | 131317 | 181598 | 277963 |
| observed | 600 | 4 | 6 | 27 | 2610 |
| write_lock_wait | 600 | 0 | 0 | 1 | 61994 |
| write_lock_hold | 600 | 7094 | 31640 | 69923 | 100798 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 16270 | 122080 | 178363 | 248151 |
| write_sql_in_call | 600 | 6434 | 30314 | 75226 | 109355 |
| write_unattributed | 600 | 0 | 912 | 5142 | 19815 |
| write_attribution_overlap | 600 | 2552 | 19147 | 53735 | 97819 |
| write_server_remainder | 600 | 14 | 105 | 506 | 17114 |
| write_client_remainder | 600 | 86 | 2129 | 9087 | 49777 |
| write_sql_covered | 600 | 22559 | 131227 | 180865 | 276698 |
| replica_flush_wait | 1340 | 0 | 0 | 0 | 0 |
| checkpoint_passive | 14 | 40233 | 134438 | 134438 | 134438 |
| checkpoint_passive_ns_per_page | 14 | 4020 | 16203 | 16203 | 16203 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| call | 2308 | 4751 | 71657 | 104030 | 238112 |
| settle_commits | 62 | 4187 | 43492 | 135548 | 135548 |
| sql | 681 | 4419 | 24493 | 66927 | 98219 |
| connect | 56 | 3690 | 19718 | 26527 | 26527 |
| synchronize | 6 | 4466 | 24313 | 24313 | 24313 |
| recovery | 37 | 3222 | 14347 | 22740 | 22740 |
| publish_links | 2 | 9729 | 16372 | 16372 | 16372 |
| live_link | 4 | 1653 | 5728 | 5728 | 5728 |
| receive_deliveries | 4 | 1853 | 2760 | 2760 | 2760 |
| database | 3 | 1747 | 2285 | 2285 | 2285 |

Load elapsed: 60551216 us.

## Desktop after interactive changes: L2 Buffered

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 16125 | 44921 | 83071 | 290264 |
| observed | 600 | 3 | 6 | 21 | 71 |
| write_lock_wait | 600 | 0 | 0 | 0 | 9 |
| write_lock_hold | 600 | 3705 | 14819 | 27454 | 73286 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 9483 | 37317 | 70541 | 233351 |
| write_sql_in_call | 600 | 3614 | 15862 | 32141 | 80432 |
| write_unattributed | 600 | 39 | 3065 | 10711 | 44677 |
| write_attribution_overlap | 600 | 0 | 89 | 1622 | 4090 |
| write_server_remainder | 600 | -31 | 700 | 7179 | 41012 |
| write_client_remainder | 600 | 57 | 3531 | 6291 | 18773 |
| write_sql_covered | 600 | 15743 | 42936 | 82929 | 286968 |
| replica_flush_wait | 1159 | 0 | 8918 | 17194 | 70589 |
| checkpoint_passive | 15 | 31926 | 65595 | 65595 | 65595 |
| checkpoint_passive_ns_per_page | 15 | 3434 | 7871 | 7871 | 7871 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| call | 2834 | 8413 | 40845 | 48040 | 204951 |
| sql | 644 | 2950 | 13247 | 24281 | 70108 |
| settle_commits | 36 | 2287 | 48291 | 66295 | 66295 |
| synchronize | 1 | 2944 | 2944 | 2944 | 2944 |
| database | 1 | 2875 | 2875 | 2875 | 2875 |
| recovery | 6 | 1194 | 1875 | 1875 | 1875 |
| live_link | 2 | 1001 | 1452 | 1452 | 1452 |

Load elapsed: 60346459 us.

## Desktop after interactive changes: L3 Flushed

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 6361 | 8388 | 13760 | 29636742 |
| observed | 600 | 4 | 6 | 7 | 11 |
| write_lock_wait | 600 | 0 | 0 | 1 | 2 |
| write_lock_hold | 600 | 5258 | 6863 | 8634 | 12609 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 2248 | 6565 | 11565 | 29634189 |
| write_sql_in_call | 600 | 5362 | 7013 | 8769 | 12689 |
| write_unattributed | 600 | 0 | 228 | 274 | 374 |
| write_attribution_overlap | 600 | 1569 | 5222 | 6150 | 10328 |
| write_server_remainder | 600 | 109 | 169 | 196 | 326 |
| write_client_remainder | 600 | 66 | 98 | 122 | 187 |
| write_sql_covered | 600 | 6286 | 8278 | 13679 | 29636683 |
| replica_flush_wait | 600 | 0 | 0 | 0 | 0 |
| checkpoint_passive | 12 | 53187 | 62378 | 62378 | 62378 |
| checkpoint_passive_ns_per_page | 12 | 4603 | 6796 | 6796 | 6796 |
| checkpoint_truncate | 2 | 66725 | 70231 | 70231 | 70231 |
| checkpoint_truncate_ns_per_page | 0 | 0 | 0 | 0 | 0 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| call | 6 | 8392 | 29734769 | 29734769 | 29734769 |
| settle_commits | 7 | 25862 | 57345 | 57345 | 57345 |
| sql | 609 | 4314 | 5742 | 7539 | 11768 |
| database | 1 | 1365 | 1365 | 1365 | 1365 |
| recovery | 1 | 1020 | 1020 | 1020 | 1020 |

Load elapsed: 29806318 us.

## Desktop after interactive changes: L3 Buffered

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 6405 | 7732 | 8753 | 30445173 |
| observed | 600 | 4 | 8 | 13 | 41 |
| write_lock_wait | 600 | 0 | 1 | 1 | 4 |
| write_lock_hold | 600 | 5635 | 6942 | 7621 | 11106 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 133 | 690 | 830 | 30442948 |
| write_sql_in_call | 600 | 6004 | 7408 | 8051 | 11655 |
| write_unattributed | 600 | 438 | 734 | 869 | 1238 |
| write_attribution_overlap | 600 | 0 | 0 | 0 | 98 |
| write_server_remainder | 600 | 368 | 645 | 766 | 1166 |
| write_client_remainder | 600 | 72 | 102 | 128 | 245 |
| write_sql_covered | 600 | 6329 | 7629 | 8707 | 30445128 |
| replica_flush_wait | 1 | 4288 | 4288 | 4288 | 4288 |
| checkpoint_passive | 11 | 58079 | 63586 | 63586 | 63586 |
| checkpoint_passive_ns_per_page | 11 | 4957 | 6970 | 6970 | 6970 |
| checkpoint_truncate | 1 | 66902 | 66902 | 66902 | 66902 |
| checkpoint_truncate_ns_per_page | 0 | 0 | 0 | 0 | 0 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| call | 8 | 4835 | 30542730 | 30542730 | 30542730 |
| settle_commits | 6 | 1144 | 59156 | 59156 | 59156 |
| sql | 687 | 4609 | 5962 | 6787 | 10272 |
| database | 1 | 2917 | 2917 | 2917 | 2917 |
| recovery | 3 | 1804 | 1896 | 1896 | 1896 |

Load elapsed: 30618017 us.

## Desktop after interactive changes: L4 Flushed

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 15036 | 129385 | 165636 | 190524 |
| observed | 600 | 2 | 5 | 8 | 13 |
| write_lock_wait | 600 | 0 | 0 | 5396 | 12573 |
| write_lock_hold | 600 | 4570 | 7182 | 9541 | 17959 |
| write_flush_wait | 600 | 6899 | 12598 | 14227 | 17454 |
| write_reactor_delay | 600 | 5231 | 114794 | 156023 | 179458 |
| write_sql_in_call | 600 | 15424 | 30111 | 34534 | 47484 |
| write_unattributed | 600 | 53 | 179 | 218 | 269 |
| write_attribution_overlap | 600 | 0 | 5476 | 8739 | 17553 |
| write_server_remainder | 600 | 142 | 12066 | 13308 | 17514 |
| write_client_remainder | 600 | 38 | 94 | 118 | 203 |
| write_sql_covered | 600 | 14978 | 129314 | 165581 | 190446 |
| replica_flush_wait | 3991 | 3987 | 11973 | 127569 | 189137 |
| checkpoint_passive | 107 | 4 | 36811 | 41807 | 41836 |
| checkpoint_passive_ns_per_page | 107 | 1 | 3307 | 3565 | 3748 |
| checkpoint_truncate | 391 | 61265 | 62625 | 62645 | 75282 |
| checkpoint_truncate_ns_per_page | 390 | 18386 | 18811 | 18816 | 18824 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| settle_commits | 237 | 121113 | 175166 | 186615 | 187408 |
| sql | 3994 | 5026 | 8755 | 11521 | 17652 |

Load elapsed: 60025786 us.

## Desktop after interactive changes: L4 Buffered

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 10327 | 122109 | 168132 | 179515 |
| observed | 600 | 3 | 6 | 10 | 19 |
| write_lock_wait | 600 | 0 | 0 | 2 | 11482 |
| write_lock_hold | 600 | 4911 | 7402 | 10014 | 14413 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 4846 | 116790 | 163986 | 173842 |
| write_sql_in_call | 600 | 4926 | 7852 | 11712 | 16550 |
| write_unattributed | 600 | 48 | 158 | 751 | 881 |
| write_attribution_overlap | 600 | 0 | 0 | 0 | 0 |
| write_server_remainder | 600 | -11 | 106 | 675 | 830 |
| write_client_remainder | 600 | 42 | 93 | 133 | 589 |
| write_sql_covered | 600 | 10301 | 122021 | 168084 | 179440 |
| replica_flush_wait | 3378 | 3930 | 10527 | 128394 | 190994 |
| checkpoint_passive | 102 | 4 | 38590 | 40843 | 57542 |
| checkpoint_passive_ns_per_page | 102 | 1 | 3090 | 3568 | 4940 |
| checkpoint_truncate | 388 | 60904 | 62633 | 62678 | 78292 |
| checkpoint_truncate_ns_per_page | 387 | 17797 | 18301 | 18316 | 18321 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| settle_commits | 227 | 121474 | 179225 | 183488 | 186841 |
| sql | 3984 | 5058 | 9019 | 11719 | 18044 |

Load elapsed: 60014188 us.

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

## Pixel before interactive changes: L0 Buffered

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 26643 | 54004 | 385139 | 432342 |
| observed | 600 | 38 | 87 | 222 | 441 |
| write_lock_wait | 600 | 6 | 10 | 12 | 100 |
| write_lock_hold | 600 | 20463 | 31667 | 377452 | 404621 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 3699 | 7518 | 20341 | 334500 |
| write_sql_in_call | 600 | 19882 | 29341 | 371338 | 401767 |
| write_unattributed | 600 | 1675 | 4974 | 329444 | 381017 |
| write_attribution_overlap | 600 | 0 | 413 | 4443 | 17393 |
| replica_flush_wait | 0 | 0 | 0 | 0 | 0 |
| checkpoint_passive | 5 | 32814 | 46738 | 46738 | 46738 |
| checkpoint_passive_ns_per_page | 5 | 3995 | 5614 | 5614 | 5614 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| sql | 1267 | 6087 | 19030 | 329731 | 397349 |
| database | 19 | 1665 | 100198 | 100198 | 100198 |
| settle_commits | 558 | 3297 | 4252 | 21822 | 51186 |
| recovery | 444 | 1492 | 1925 | 3789 | 8360 |
| publish_links | 1 | 1589 | 1589 | 1589 | 1589 |

Load elapsed: 81008760 us.

## Pixel before interactive changes: L1 Flushed

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 39817 | 525931 | 903575 | 2693590 |
| observed | 600 | 39 | 87 | 120 | 373 |
| write_lock_wait | 600 | 12 | 321 | 1560 | 2941 |
| write_lock_hold | 600 | 30550 | 35445 | 124906 | 152551 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 30259 | 506252 | 893566 | 2647624 |
| write_sql_in_call | 600 | 30356 | 35151 | 124557 | 151012 |
| write_unattributed | 600 | 0 | 3160 | 5249 | 11040 |
| write_attribution_overlap | 600 | 27 | 26362 | 31176 | 147960 |
| replica_flush_wait | 603 | 0 | 0 | 0 | 0 |
| checkpoint_passive | 50 | 118734 | 173074 | 173961 | 173961 |
| checkpoint_passive_ns_per_page | 50 | 10653 | 13393 | 20289 | 20289 |
| checkpoint_truncate | 4 | 175700 | 193914 | 193914 | 193914 |
| checkpoint_truncate_ns_per_page | 0 | 0 | 0 | 0 | 0 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| call | 588 | 5483 | 382036 | 863092 | 1340795 |
| settle_commits | 43 | 129306 | 175810 | 193975 | 193975 |
| sql | 1153 | 4893 | 27342 | 37968 | 149149 |
| recovery | 104 | 1734 | 7487 | 7602 | 8864 |
| connect | 379 | 2207 | 2465 | 3357 | 8792 |
| database | 2 | 1124 | 1493 | 1493 | 1493 |

Load elapsed: 80539380 us.

## Pixel after interactive changes: L0 Flushed

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 29557 | 33240 | 53970 | 138173 |
| observed | 600 | 52 | 90 | 190 | 799 |
| write_lock_wait | 600 | 11 | 26 | 42 | 137 |
| write_lock_hold | 600 | 20999 | 23535 | 29923 | 127878 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 7173 | 23649 | 30581 | 114351 |
| write_sql_in_call | 600 | 20666 | 23457 | 27660 | 135538 |
| write_unattributed | 600 | 1323 | 3685 | 4735 | 7864 |
| write_attribution_overlap | 600 | 0 | 14779 | 17556 | 24100 |
| write_server_remainder | 600 | 105 | 736 | 1014 | 7658 |
| write_client_remainder | 600 | 2757 | 4218 | 4908 | 10868 |
| write_sql_covered | 600 | 26808 | 29755 | 49062 | 137946 |
| replica_flush_wait | 600 | 0 | 0 | 0 | 0 |
| checkpoint_passive | 5 | 72509 | 92798 | 92798 | 92798 |
| checkpoint_passive_ns_per_page | 5 | 8898 | 11218 | 11218 | 11218 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| sql | 1200 | 7042 | 16535 | 19239 | 126244 |
| settle_commits | 598 | 3085 | 3375 | 5686 | 96460 |
| connect | 599 | 2135 | 2444 | 4437 | 8310 |
| recovery | 290 | 1536 | 1686 | 1917 | 3248 |

Load elapsed: 60037166 us.

## Pixel after interactive changes: L0 Buffered

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 24834 | 29444 | 41198 | 376360 |
| observed | 600 | 44 | 88 | 147 | 239 |
| write_lock_wait | 600 | 6 | 9 | 13 | 63 |
| write_lock_hold | 600 | 19276 | 22850 | 30586 | 372901 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 2671 | 4543 | 8710 | 20712 |
| write_sql_in_call | 600 | 19473 | 22386 | 30895 | 372208 |
| write_unattributed | 600 | 2551 | 4793 | 6095 | 330745 |
| write_attribution_overlap | 600 | 0 | 0 | 177 | 11945 |
| write_server_remainder | 600 | -142 | 1780 | 2124 | 3528 |
| write_client_remainder | 600 | 2694 | 4698 | 6154 | 330809 |
| write_sql_covered | 600 | 22242 | 25372 | 36214 | 373086 |
| replica_flush_wait | 0 | 0 | 0 | 0 | 0 |
| checkpoint_passive | 5 | 39033 | 49409 | 49409 | 49409 |
| checkpoint_passive_ns_per_page | 5 | 4684 | 5892 | 5892 | 5892 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| sql | 1293 | 3915 | 16533 | 20075 | 370017 |
| settle_commits | 593 | 3752 | 4050 | 37202 | 335828 |
| recovery | 578 | 1481 | 1661 | 2293 | 4927 |
| database | 1 | 2634 | 2634 | 2634 | 2634 |

Load elapsed: 61731391 us.

## Pixel after interactive changes: L1 Flushed

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 41639 | 765135 | 1321306 | 2627138 |
| observed | 600 | 34 | 81 | 123 | 356 |
| write_lock_wait | 600 | 5 | 13 | 21 | 583196 |
| write_lock_hold | 600 | 24038 | 35380 | 172929 | 404972 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 29283 | 743471 | 1306613 | 2588842 |
| write_sql_in_call | 600 | 23726 | 35101 | 170817 | 587485 |
| write_unattributed | 600 | 449 | 4805 | 148183 | 345593 |
| write_attribution_overlap | 600 | 0 | 21678 | 33898 | 663047 |
| write_server_remainder | 600 | -104 | 812 | 991 | 30498 |
| write_client_remainder | 600 | 2487 | 6217 | 287192 | 361295 |
| write_sql_covered | 600 | 38065 | 745599 | 1307413 | 2611888 |
| replica_flush_wait | 603 | 0 | 0 | 0 | 0 |
| checkpoint_passive | 48 | 204842 | 637052 | 662526 | 662526 |
| checkpoint_passive_ns_per_page | 48 | 18558 | 55456 | 64923 | 64923 |
| checkpoint_truncate | 5 | 249659 | 397126 | 397126 | 397126 |
| checkpoint_truncate_ns_per_page | 0 | 0 | 0 | 0 | 0 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| call | 598 | 7553 | 625518 | 1091253 | 1519641 |
| settle_commits | 527 | 4599 | 212133 | 548703 | 662688 |
| sql | 1084 | 6162 | 24443 | 36548 | 397576 |
| recovery | 128 | 2373 | 10630 | 23412 | 348288 |
| connect | 368 | 2283 | 3021 | 12801 | 31296 |
| database | 2 | 2198 | 4002 | 4002 | 4002 |
| receive_deliveries | 1 | 2626 | 2626 | 2626 | 2626 |

Load elapsed: 101030890 us.

## Pixel after interactive changes: L1 Buffered

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 35863 | 564213 | 1449495 | 2473435 |
| observed | 600 | 30 | 63 | 105 | 1083 |
| write_lock_wait | 600 | 4 | 10 | 12 | 174065 |
| write_lock_hold | 600 | 20251 | 31877 | 361046 | 397566 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 4006 | 542025 | 1397695 | 2417728 |
| write_sql_in_call | 600 | 19842 | 33982 | 361135 | 1017826 |
| write_unattributed | 600 | 1758 | 8333 | 357788 | 639624 |
| write_attribution_overlap | 600 | 0 | 166 | 2398 | 320849 |
| write_server_remainder | 600 | -397 | 1205 | 7490 | 638550 |
| write_client_remainder | 600 | 2574 | 7480 | 354045 | 364614 |
| write_sql_covered | 600 | 31373 | 559315 | 1408565 | 2469747 |
| replica_flush_wait | 3 | 0 | 4159 | 4159 | 4159 |
| checkpoint_passive | 47 | 126741 | 237955 | 590523 | 590523 |
| checkpoint_passive_ns_per_page | 47 | 10876 | 21947 | 47738 | 47738 |
| checkpoint_truncate | 6 | 162120 | 189608 | 189608 | 189608 |
| checkpoint_truncate_ns_per_page | 0 | 0 | 0 | 0 | 0 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| call | 601 | 6430 | 470529 | 1093697 | 1587720 |
| settle_commits | 342 | 5005 | 155011 | 358072 | 590684 |
| sql | 1110 | 5592 | 24441 | 49976 | 378340 |
| recovery | 132 | 2438 | 11406 | 178889 | 355758 |
| database | 4 | 2749 | 11651 | 11651 | 11651 |
| publish_links | 1 | 2064 | 2064 | 2064 | 2064 |

Load elapsed: 88086114 us.

## Pixel after interactive changes: L2 Flushed

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 34715 | 573291 | 1009007 | 1493395 |
| observed | 600 | 22 | 36 | 91 | 922 |
| write_lock_wait | 600 | 2 | 5 | 9 | 509 |
| write_lock_hold | 600 | 17535 | 39795 | 396937 | 403914 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 23189 | 560432 | 973165 | 1487995 |
| write_sql_in_call | 600 | 17373 | 38758 | 397699 | 404327 |
| write_unattributed | 600 | 0 | 3461 | 338596 | 366771 |
| write_attribution_overlap | 600 | 977 | 21155 | 387853 | 737465 |
| write_server_remainder | 600 | 206 | 890 | 2479 | 358798 |
| write_client_remainder | 600 | 727 | 5824 | 350058 | 366912 |
| write_sql_covered | 600 | 33035 | 564204 | 988725 | 1490464 |
| replica_flush_wait | 734 | 0 | 0 | 0 | 0 |
| checkpoint_passive | 11 | 68707 | 450871 | 450871 | 450871 |
| checkpoint_passive_ns_per_page | 11 | 7965 | 51925 | 51925 | 51925 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| call | 1965 | 2710 | 177268 | 526405 | 1056423 |
| settle_commits | 566 | 2030 | 4913 | 91298 | 453098 |
| sql | 1184 | 5653 | 20732 | 384839 | 403779 |
| connect | 383 | 1654 | 5378 | 17491 | 355906 |
| database | 23 | 2225 | 5930 | 344501 | 344501 |
| live_link | 33 | 2244 | 5358 | 335513 | 335513 |
| recovery | 235 | 1318 | 3504 | 5641 | 322385 |
| publish_links | 4 | 2211 | 14679 | 14679 | 14679 |
| synchronize | 4 | 1641 | 1939 | 1939 | 1939 |

Load elapsed: 117193389 us.

## Pixel after interactive changes: L2 Buffered

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 33252 | 574849 | 1041243 | 2481112 |
| observed | 600 | 21 | 38 | 114 | 239 |
| write_lock_wait | 600 | 1 | 4 | 6 | 105 |
| write_lock_hold | 600 | 17221 | 30457 | 384622 | 430101 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 9931 | 544024 | 1010419 | 2455042 |
| write_sql_in_call | 600 | 16758 | 29760 | 374132 | 484506 |
| write_unattributed | 600 | 301 | 5507 | 122282 | 357046 |
| write_attribution_overlap | 600 | 0 | 1168 | 3652 | 366930 |
| write_server_remainder | 600 | -168 | 944 | 7873 | 119930 |
| write_client_remainder | 600 | 655 | 5386 | 342147 | 360833 |
| write_sql_covered | 600 | 31510 | 573581 | 1039325 | 2478496 |
| replica_flush_wait | 201 | 407 | 20672 | 88273 | 348365 |
| checkpoint_passive | 11 | 69981 | 465596 | 465596 | 465596 |
| checkpoint_passive_ns_per_page | 11 | 7936 | 48905 | 48905 | 48905 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| call | 1923 | 2982 | 180369 | 528377 | 1005295 |
| settle_commits | 498 | 2245 | 5486 | 58177 | 443722 |
| sql | 1218 | 6352 | 19695 | 175995 | 425035 |
| database | 8 | 1256 | 25871 | 25871 | 25871 |
| synchronize | 6 | 2012 | 22680 | 22680 | 22680 |
| recovery | 332 | 1439 | 3801 | 6135 | 9100 |
| live_link | 38 | 2108 | 6548 | 7354 | 7354 |
| publish_links | 7 | 1744 | 3141 | 3141 | 3141 |

Load elapsed: 115749118 us.

## Pixel after interactive changes: L3 Flushed

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 36702 | 370699 | 406342 | 203880304 |
| observed | 600 | 46 | 82 | 101 | 254 |
| write_lock_wait | 600 | 8 | 15 | 31 | 170 |
| write_lock_hold | 600 | 25527 | 31780 | 390548 | 428531 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 10632 | 32692 | 238266 | 203872762 |
| write_sql_in_call | 600 | 25509 | 31648 | 390870 | 403746 |
| write_unattributed | 600 | 795 | 20090 | 344365 | 365296 |
| write_attribution_overlap | 600 | 0 | 19500 | 26277 | 394782 |
| write_server_remainder | 600 | 183 | 933 | 3296 | 347231 |
| write_client_remainder | 600 | 2798 | 12427 | 347351 | 386026 |
| write_sql_covered | 600 | 33580 | 50034 | 395778 | 203877107 |
| replica_flush_wait | 600 | 0 | 0 | 0 | 0 |
| checkpoint_passive | 11 | 107751 | 619610 | 619610 | 619610 |
| checkpoint_passive_ns_per_page | 11 | 11650 | 43305 | 43305 | 43305 |
| checkpoint_truncate | 2 | 209072 | 264427 | 264427 | 264427 |
| checkpoint_truncate_ns_per_page | 0 | 0 | 0 | 0 | 0 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| call | 37 | 2311 | 425539 | 203944456 | 203944456 |
| sql | 1204 | 7902 | 21331 | 30783 | 397279 |
| connect | 579 | 2426 | 4418 | 275399 | 372306 |
| recovery | 369 | 2797 | 3235 | 3978 | 364400 |
| settle_commits | 599 | 3888 | 4815 | 45168 | 337419 |
| database | 5 | 1210 | 10115 | 10115 | 10115 |
| synchronize | 1 | 9942 | 9942 | 9942 | 9942 |

Load elapsed: 205961347 us.

## Pixel after interactive changes: L3 Buffered

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 31951 | 374321 | 398717 | 277170889 |
| observed | 600 | 41 | 80 | 224 | 642 |
| write_lock_wait | 600 | 6 | 11 | 14 | 147 |
| write_lock_hold | 600 | 24320 | 36905 | 381509 | 421501 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 4109 | 13865 | 31519 | 277155376 |
| write_sql_in_call | 600 | 24176 | 37434 | 378847 | 414878 |
| write_unattributed | 600 | 2573 | 6909 | 354478 | 374133 |
| write_attribution_overlap | 600 | 0 | 147 | 3265 | 23079 |
| write_server_remainder | 600 | -212 | 3128 | 3668 | 357545 |
| write_client_remainder | 600 | 2759 | 6539 | 355619 | 374751 |
| write_sql_covered | 600 | 28757 | 64835 | 391887 | 277167760 |
| replica_flush_wait | 2 | 5008 | 5573 | 5573 | 5573 |
| checkpoint_passive | 11 | 159945 | 538153 | 538153 | 538153 |
| checkpoint_passive_ns_per_page | 11 | 14257 | 46782 | 46782 | 46782 |
| checkpoint_truncate | 2 | 259409 | 619155 | 619155 | 619155 |
| checkpoint_truncate_ns_per_page | 0 | 0 | 0 | 0 | 0 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| call | 49 | 2115 | 101881 | 277250788 | 277250788 |
| settle_commits | 514 | 4422 | 5996 | 51746 | 399737 |
| sql | 1283 | 6278 | 22315 | 348536 | 394882 |
| recovery | 578 | 2841 | 3267 | 4059 | 354860 |
| database | 4 | 3104 | 344450 | 344450 | 344450 |
| synchronize | 3 | 3195 | 3693 | 3693 | 3693 |
| publish_links | 2 | 2251 | 2494 | 2494 | 2494 |

Load elapsed: 278810552 us.

## Pixel after interactive changes: L4 Flushed

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 63764 | 398162 | 476812 | 899877 |
| observed | 600 | 30 | 52 | 97 | 6642 |
| write_lock_wait | 600 | 2 | 54109 | 77250 | 407153 |
| write_lock_hold | 600 | 16260 | 25165 | 29732 | 44563 |
| write_flush_wait | 600 | 5131 | 78037 | 410476 | 489115 |
| write_reactor_delay | 600 | 27608 | 111792 | 403485 | 672402 |
| write_sql_in_call | 600 | 25715 | 176584 | 729840 | 888362 |
| write_unattributed | 600 | 467 | 2823 | 10785 | 368859 |
| write_attribution_overlap | 600 | 0 | 15329 | 18418 | 26636 |
| write_server_remainder | 600 | 276 | 48102 | 354813 | 434151 |
| write_client_remainder | 600 | 1461 | 4415 | 14728 | 368802 |
| write_sql_covered | 600 | 61869 | 371305 | 475903 | 690693 |
| replica_flush_wait | 1372 | 6535 | 59415 | 375066 | 659789 |
| checkpoint_passive | 39 | 35 | 409385 | 438626 | 438626 |
| checkpoint_passive_ns_per_page | 39 | 10 | 37561 | 41266 | 41266 |
| checkpoint_truncate | 83 | 54649 | 246269 | 606753 | 606753 |
| checkpoint_truncate_ns_per_page | 82 | 15983 | 54082 | 177516 | 177516 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| settle_commits | 283 | 2402 | 115305 | 488486 | 672462 |
| sql | 2744 | 6397 | 56911 | 397589 | 464664 |
| recovery | 181 | 1116 | 2170 | 5623 | 14861 |
| database | 3 | 2121 | 14153 | 14153 | 14153 |
| synchronize | 1 | 7933 | 7933 | 7933 | 7933 |
| publish_links | 1 | 5677 | 5677 | 5677 | 5677 |
| connect | 139 | 1313 | 2776 | 3252 | 3510 |

Load elapsed: 81915636 us.

## Pixel after interactive changes: L4 Buffered

| Metric | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| write | 600 | 50016 | 401678 | 455584 | 590943 |
| observed | 600 | 32 | 62 | 141 | 417 |
| write_lock_wait | 600 | 2 | 53253 | 64166 | 389260 |
| write_lock_hold | 600 | 15963 | 23902 | 35878 | 410263 |
| write_flush_wait | 600 | 0 | 0 | 0 | 0 |
| write_reactor_delay | 600 | 25564 | 101302 | 416697 | 538878 |
| write_sql_in_call | 600 | 15712 | 69905 | 110437 | 403250 |
| write_unattributed | 600 | 1623 | 6309 | 356230 | 389121 |
| write_attribution_overlap | 600 | 0 | 67 | 1917 | 22885 |
| write_server_remainder | 600 | -489 | 502 | 1760 | 325866 |
| write_client_remainder | 600 | 2405 | 9004 | 358240 | 389257 |
| write_sql_covered | 600 | 45502 | 140473 | 428157 | 587598 |
| replica_flush_wait | 679 | 15280 | 67457 | 384209 | 487677 |
| checkpoint_passive | 26 | 53 | 76702 | 395054 | 395054 |
| checkpoint_passive_ns_per_page | 26 | 15 | 6383 | 33399 | 33399 |
| checkpoint_truncate | 58 | 55054 | 389258 | 391988 | 391988 |
| checkpoint_truncate_ns_per_page | 57 | 16022 | 113288 | 114082 | 114082 |

| Task | Long polls | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| settle_commits | 207 | 2776 | 170298 | 448237 | 510777 |
| sql | 2552 | 6784 | 55952 | 405653 | 436604 |
| recovery | 238 | 1088 | 2247 | 4706 | 364714 |
| database | 1 | 3146 | 3146 | 3146 | 3146 |
| connect | 2 | 1458 | 1783 | 1783 | 1783 |
| synchronize | 1 | 1182 | 1182 | 1182 | 1182 |

Load elapsed: 85483746 us.
