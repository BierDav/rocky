# Mesh benchmark results

Measured 2026-11-21 on Rocky `c376b5653` (with this benchmark uncommitted on
top), release builds, Rust 1.99.0. Raw lines are in `results/2026-11-21/`, and
`python3 analyze.py results/2026-11-21` prints the tables below.

| name | host | hardware | OS | network |
| --- | --- | --- | --- | --- |
| mb | david-mb (driver) | Apple M5 Pro, 18 cores, 48 GB | macOS 27.2 | Wi-Fi, home LAN |
| mm | david-mm-linz | Apple M4, 10 cores, 16 GB | macOS | wired, home LAN |
| tp | david-tp | AMD Ryzen 5 4500U, 6 GB | NixOS | home LAN |
| srv | david-server | Intel i5-1235U, 11 GB | NixOS | other site, behind NAT |

mb, mm and tp share one LAN and public address. srv is reached by iroh hole
punching. `p2p` binds each host's LAN address so the WireGuard overlay is not
used. `relay` disables IP transports, so every byte goes through the default
n0 relay (euc1). Rows carry 256-byte bodies and are written 100 per
transaction.

## Summary

| question | answer |
| --- | --- |
| small change, direct paths | p50 66 ms, p90 95 ms, p99 154 ms one-way across all 12 pairs; 59–68 ms even on wired LAN with 2 ms RTT |
| small change, relay only | p50 139 ms, p90 203 ms, p99 325 ms |
| small change, same machine | p50 38 ms between two processes, which is Rocky's floor |
| fresh bootstrap of 50k rows (15 MB payload, 30 MB on the wire) | p2p: 8.6 s on LAN (3.5 MB/s, 28 Mbit/s), 17–18 s from srv (1.7–1.8 MB/s); relay: 64–92 s (0.33–0.47 MB/s, 3–4 Mbit/s) |
| migration of every row (50k) | issuer open on v2 in 7.4 s; v1 members have every migrated row 20–27 s later, after receiving only 1–2.4 MB because the program is replayed rather than shipped; upgrading a member then takes about 3.4 s |
| peer returning with offline changes | 1k changes converge in 6–9 s, 10k in 48–88 s, 50k in 268–469 s (about 110–180 rows/s, 20–40× slower than bootstrapping the same rows fresh); other peers' small changes keep p50 ≈ 97 ms during catch-up, but the tail reaches 4 s at p99 and 25 s at the maximum |
| Rocky vs plain SQLite, one machine | a single-row commit costs 4.9–6.8 ms against 6–19 µs (about 400×), 1.5–11× against SQLite `synchronous=FULL`; point reads 0.26–1 ms against 1–3 µs; batched inserts 2.8–8.6k rows/s against 376–563k; about 6.5 KB on disk per row against 317 bytes |

## Local commit cost: plain SQLite vs Rocky

| host | variant | insert p50 µs | insert p99 µs | update p50 µs | point read p50 µs | batched rows/s | disk bytes/row | commit() p50 µs |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| mb | sqlite-normal | 12 | 65 | 6 | 1 | 521,155 | 317 | – |
| mb | rocky-buffered | 4,908 | 8,989 | 5,015 | 281 | 6,859 | 6,532 | 4,174 |
| mb | sqlite-full | 3,030 | 6,076 | 3,036 | 3 | 26,886 | 317 | – |
| mb | rocky-flushed | 7,794 | 16,975 | 8,027 | 285 | 5,332 | 6,533 | 8,085 |
| mm | sqlite-normal | 19 | 63 | 8 | 1 | 376,236 | 317 | – |
| mm | rocky-buffered | 5,621 | 6,602 | 5,212 | 262 | 8,629 | 6,526 | 4,211 |
| mm | sqlite-full | 3,992 | 4,956 | 3,998 | 7 | 21,921 | 317 | – |
| mm | rocky-flushed | 9,987 | 16,098 | 10,435 | 261 | 6,335 | 6,539 | 9,153 |
| tp | sqlite-normal | 10 | 28 | 9 | 3 | 461,252 | 317 | – |
| tp | rocky-buffered | 6,812 | 8,309 | 8,017 | 1,014 | 2,824 | 6,529 | 7,237 |
| tp | sqlite-full | 633 | 863 | 322 | 3 | 77,925 | 317 | – |
| tp | rocky-flushed | 7,126 | 9,756 | 8,947 | 1,020 | 2,754 | 6,513 | 8,855 |
| srv | sqlite-normal | 6 | 26 | 5 | 1 | 562,660 | 317 | – |
| srv | rocky-buffered | 5,503 | 9,931 | 6,098 | 609 | 3,060 | 6,519 | 5,510 |
| srv | sqlite-full | 3,334 | 6,890 | 2,956 | 6 | 28,338 | 317 | – |
| srv | rocky-flushed | 8,339 | 14,282 | 9,376 | 609 | 2,985 | 6,533 | 9,326 |

Most of a buffered commit is `commit()` itself (4.2 ms of 4.9 ms on mb). A
sampled profile of the SDK's reactor thread on mb splits it roughly into:

- `write_materialized`: 19%
- the per-commit `applied_heads` query: 16%
- ed25519 verification of the credential chain: 16%
- SQLite `COMMIT` and WAL writes: 15%

The 20 ms `LOCAL_SETTLE` in `crates/rocky-sdk/src/node.rs` does not affect
commit latency, but it delays the outbound announcement. The macOS
`sqlite-full` row uses `F_FULLFSYNC`. tp's disk ignores flushes, which explains
its fast `sqlite-full`.

## Small-change latency

A probe is one committed row. Latency runs from just before the writer's
`transaction()` to the receiver's `observe_tables` callback, both on the
driver's clock. The pair-symmetric column cancels the remaining clock offset.

| path | pair | n | p50 ms | p90 | p99 | max | pair-symmetric p50 | selected path | path RTT ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| p2p | mb → mm | 20 | 54.7 | 79.8 | 128.3 | 128.3 | 59.2 | direct | 10.2 |
| p2p | mb → srv | 20 | 82.1 | 101.8 | 153.6 | 153.6 | 75.5 | direct | 86.8 |
| p2p | mb → tp | 20 | 50.1 | 75.8 | 123.3 | 123.3 | 59.1 | direct | 12.0 |
| p2p | mm → mb | 20 | 63.8 | 65.7 | 66.4 | 66.4 | 59.2 | direct | 13.4 |
| p2p | mm → srv | 20 | 87.9 | 94.2 | 96.3 | 96.3 | 77.3 | direct | 66.3 |
| p2p | mm → tp | 20 | 64.2 | 65.5 | 66.2 | 66.2 | 68.2 | direct | 2.0 |
| p2p | srv → mb | 20 | 68.9 | 92.8 | 150.9 | 150.9 | 75.5 | direct | 45.8 |
| p2p | srv → mm | 20 | 66.7 | 77.5 | 157.0 | 157.0 | 77.3 | direct | 60.5 |
| p2p | srv → tp | 20 | 64.3 | 69.9 | 153.5 | 153.5 | 72.0 | direct | 60.9 |
| p2p | tp → mb | 20 | 68.1 | 80.3 | 115.3 | 115.3 | 59.1 | direct | 16.7 |
| p2p | tp → mm | 20 | 72.3 | 74.7 | 81.9 | 81.9 | 68.2 | direct | 1.8 |
| p2p | tp → srv | 20 | 79.7 | 120.0 | 150.0 | 150.0 | 72.0 | direct | 68.1 |
| relay | mb → mm | 20 | 144.1 | 156.4 | 274.2 | 274.2 | 178.3 | relay | 98.0 |
| relay | mb → srv | 20 | 130.9 | 168.0 | 253.5 | 253.5 | 118.7 | relay | 69.9 |
| relay | mb → tp | 20 | 138.6 | 188.7 | 287.9 | 287.9 | 150.5 | relay | 84.5 |
| relay | mm → mb | 20 | 212.6 | 285.0 | 304.4 | 304.4 | 178.3 | relay | 104.8 |
| relay | mm → srv | 20 | 147.2 | 187.9 | 213.1 | 213.1 | 123.4 | relay | 59.2 |
| relay | mm → tp | 20 | 183.5 | 203.3 | 247.7 | 247.7 | 169.5 | relay | 73.3 |
| relay | srv → mb | 20 | 106.4 | 119.4 | 191.4 | 191.4 | 118.7 | relay | 71.7 |
| relay | srv → mm | 20 | 99.6 | 110.0 | 125.0 | 125.0 | 123.4 | relay | 63.9 |
| relay | srv → tp | 20 | 89.7 | 100.9 | 184.8 | 184.8 | 114.8 | relay | 56.9 |
| relay | tp → mb | 20 | 162.5 | 228.5 | 324.8 | 324.8 | 150.5 | relay | 106.3 |
| relay | tp → mm | 20 | 155.5 | 179.7 | 485.5 | 485.5 | 169.5 | relay | 76.4 |
| relay | tp → srv | 20 | 140.0 | 155.1 | 472.7 | 472.7 | 114.8 | relay | 60.7 |

| path | all pairs n | p50 ms | p90 | p99 | max |
| --- | ---: | ---: | ---: | ---: | ---: |
| loopback (two processes on mb) | 80 | 37.8 | 40.3 | 42.0 | 43.3 |
| p2p | 240 | 65.9 | 94.6 | 153.5 | 157.0 |
| relay | 240 | 138.6 | 203.3 | 324.8 | 485.5 |

No probe was lost. On the wire, a probe takes one RTT or less, so roughly
40–60 ms of every delivery is spent inside Rocky. That is about 8–16 ms of
commit, the 20 ms local settle, and the receiver's apply and observe. The same
gap shows on wired mm↔tp with a 2 ms RTT. Commit p50 with three live peers is
8 ms on mb, 12 ms on srv and 16 ms on mm and tp.

## Fresh bootstrap

A new device joins with an invitation and syncs 50,000 rows of 256-byte
bodies. `total` runs from the start of `join()` until a count of the joined
database reaches every row. Bytes are taken from the joining endpoint's iroh
socket counters, split by transport.

| path | pair | join() s | total s | rows/s | received MB | direct MB | relay MB | MB/s | Mbit/s | RTT ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| p2p | mm → tp | 4.18 | 8.6 | 5,833 | 30.3 | 30.3 | 0.0 | 3.53 | 28.3 | 1.7 |
| p2p | mm → mb | 3.33 | 10.4 | 4,811 | 30.3 | 30.3 | 0.0 | 2.92 | 23.3 | 10.5 |
| p2p | srv → mm | 3.75 | 16.9 | 2,959 | 30.4 | 30.3 | 0.0 | 1.80 | 14.4 | 50.6 |
| p2p | srv → tp | 3.72 | 16.6 | 3,004 | 30.3 | 30.3 | 0.0 | 1.82 | 14.6 | 28.2 |
| p2p | srv → mb | 3.68 | 18.2 | 2,745 | 30.3 | 30.3 | 0.0 | 1.66 | 13.3 | 48.2 |
| relay | mm → tp | 1.94 | 79.5 | 629 | 30.3 | 0.0 | 30.3 | 0.38 | 3.0 | 161.0 |
| relay | mm → mb | 2.02 | 91.6 | 546 | 30.3 | 0.0 | 30.3 | 0.33 | 2.6 | 185.0 |
| relay | srv → mm | 2.59 | 65.5 | 763 | 30.3 | 0.0 | 30.3 | 0.46 | 3.7 | 161.8 |
| relay | srv → tp | 1.57 | 64.4 | 776 | 30.3 | 0.0 | 30.3 | 0.47 | 3.8 | 117.7 |
| relay | srv → mb | 1.89 | 65.9 | 759 | 30.3 | 0.0 | 30.3 | 0.46 | 3.7 | 131.9 |

| path | mean MB/s | mean Mbit/s |
| --- | ---: | ---: |
| p2p, same LAN | 3.2 | 25.8 |
| p2p, across sites | 1.76 | 14.1 |
| relay | 0.42 | 3.4 |

The wire carries about twice the 15 MB of row payload, from operation
envelopes, signatures and materialized-state bundles. On LAN, the direct path
is bound by CPU rather than the network: tp applies 5.8k rows/s, close to its
local batched rate. srv, the source for three pairs, serves at 1.7–1.8 MB/s,
the same as its own seed rate of 3k rows/s. The relay path ran at 3–4 Mbit/s
and the relay rate-limited at least one session. Expect the default n0 relays
to cap a fresh bootstrap at about 0.4 MB/s, which is 30–90 s per 15 MB of
data.

## Migration of every row

50,000 rows are partitioned by `grp` into 40 partitions of 1,250 rows. Each
partition is shared with every member, and all members are writers. mm, the
creator, reopens with schema v2, which adds `tag` with an index and runs
`UPDATE items SET tag = …` over every row. Members keep running v1 first and
then upgrade one at a time.

- Issuer (mm) open with v2: 7.4 s with 50,000 rows tagged. The agent had used
  76 s of CPU in total, with 374 MB RSS.
- Members still on v1 have every migrated row 19.7 s (mb), 24.6 s (tp) and
  26.5 s (srv) after the issuer reopened. They received 1.0–2.4 MB, far less
  than the 30 MB a bootstrap of the same rows costs: the migration program is
  replayed, not shipped row by row.
- A member's own upgrade open takes 3.3–3.6 s, with the data already present.

| phase | n | p50 ms | p90 | p99 | max | probe errors |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| before | 60 | 122.2 | 158.7 | 272.1 | 354.6 | 0 |
| issuer migrating | 28 | 107.2 | 142.8 | 184.9 | 184.9 | 0 |
| issuer migrated, members replaying | 15 | 2,567.5 | 15,364.5 | 15,543.3 | 15,543.3 | 10 |
| upgrading srv | 4 | 105.5 | 129.9 | 129.9 | 129.9 | 5 |
| after | 21 | 163.4 | 207.7 | 4,020.7 | 4,020.7 | 20 |

While members replay, small changes are delayed by seconds: p50 2.6 s, max
15.5 s. Every probe error is the bug in item 2 under Defects found.

## A peer returning with offline changes

The mesh holds 20,000 base rows. tp goes offline, inserts N rows, and updates
N/2 base rows. Meanwhile mm inserts 2,000 rows and seals three times. tp then
reconnects. A node converges when it has every row from the other side. The
other three peers keep exchanging probes throughout.

| tier | offline inserts | insert s | update half s | online inserts | seals s |
| --- | ---: | ---: | ---: | ---: | ---: |
| 0 | 1,000 | 0.4 | 0.1 | 2,000 | 0.29, 0.29, 0.39 |
| 1 | 10,000 | 4.1 | 0.7 | 2,000 | 0.54, 0.51, 0.50 |

| tier | offline changes | node | converged after s | received MB | sent MB |
| --- | ---: | ---: | ---: | ---: | ---: |
| 0 | 1,000 | mm | 6.5 | 2.6 | 5.0 |
| 0 | 1,000 | mb | 6.4 | 2.3 | 1.0 |
| 0 | 1,000 | srv | 8.8 | 3.2 | 3.2 |
| 0 | 1,000 | tp (returning) | 6.3 | 3.2 | 2.2 |
| 1 | 10,000 | mm | 56.1 | 14.0 | 6.4 |
| 1 | 10,000 | mb | 60.3 | 11.0 | 1.3 |
| 1 | 10,000 | srv | 88.0 | 17.7 | 16.6 |
| 1 | 10,000 | tp (returning) | 48.0 | 12.1 | 32.0 |

| phase | n | p50 ms | p90 | p99 | max |
| --- | ---: | ---: | ---: | ---: | ---: |
| baseline | 32 | 72.6 | 101.9 | 141.4 | 141.4 |
| 1k offline | 4 | 186.5 | 1,193.0 | 1,193.0 | 1,193.0 |
| 1k catch-up | 10 | 122.1 | 3,762.2 | 4,554.2 | 4,554.2 |
| 1k after | 32 | 72.4 | 100.2 | 148.4 | 148.4 |
| 10k offline | 16 | 136.3 | 489.6 | 1,987.2 | 1,987.2 |
| 10k catch-up | 192 | 98.4 | 295.8 | 728.7 | 15,474.6 |

The 50,000-change tier ran separately on the same day, after the original run
was interrupted (`results/2026-11-21/offline-50k/`):

| offline inserts | insert s | update 20k s | node | converged after s | received MB | sent MB |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 50,000 | 24.7 | 5.2 | mm | 351.7 | 61.4 | 69.1 |
| 50,000 | 24.7 | 5.2 | mb | 357.0 | 53.7 | 22.4 |
| 50,000 | 24.7 | 5.2 | srv | 469.4 | 114.3 | 85.3 |
| 50,000 | 24.7 | 5.2 | tp (returning) | 267.5 | 49.3 | 84.5 |

| phase | n | p50 ms | p90 | p99 | max |
| --- | ---: | ---: | ---: | ---: | ---: |
| baseline | 32 | 102.9 | 157.6 | 176.0 | 176.0 |
| 50k offline | 90 | 73.6 | 220.5 | 995.5 | 1,917.9 |
| 50k catch-up | 1,018 | 96.7 | 239.1 | 4,314.3 | 25,156.6 |
| 50k after | 12 | 95.6 | 940.9 | 1,231.3 | 1,231.3 |

Catch-up is far slower than a fresh bootstrap:

- 10k changes take 48–88 s and 50k changes take 268–469 s. That is about
  110–180 rows/s, while a fresh bootstrap of the same volume runs at 3–6k
  rows/s.
- It does not trickle evenly. With 50k changes, mm, mb and srv hold only 100 of
  tp's rows for the first ~260 s, then receive the rest in a burst over the last
  60–170 s.
- The returning peer receives mm's 2,000 online rows slowly the whole time.
- Traffic is two to four times the payload, and srv receives 114 MB for 13 MB
  of new rows.

During catch-up, the p50 of other peers' small changes stays near baseline,
but the tail grows to seconds. Probes are not lost, because the 60 s probe
timeout was never hit.

## Defects found

1. **A migration UPDATE over more than 2,048 rows in one partition is silently
   dropped.** The open succeeds and the schema reports v2, but no row changes.
   Bisected: 2,048 rows apply and 2,049 do not.
   - Cause: `REPLAY_CAPTURE` is bounded by `MAX_BUNDLE_ROWS` (64 × 32) in
     `crates/rocky-secure/src/replica/replay.rs`, and the `(Ok(None), Some(_))`
     arm returns `Ok(None)` instead of refusing the migration.
   - The migration scenario above partitions the data to stay under the limit.
2. **After an issuer migrates, writer members that joined through an
   invitation cannot write.** Every write fails with "replication data
   rejected: node author credential missing". Some fail with "followed
   bookmark moved or was deleted; workspace is read-only".
   - This holds both before and after the member upgrades to v2 and calls
     `renew()`.
   - It reproduces on one machine with `drive.py migration --local --rows 5000`.
   - The SDK test
     `a_full_writer_on_an_older_application_writes_after_the_migration` covers
     an identity-supplied writer, so it does not catch this.
