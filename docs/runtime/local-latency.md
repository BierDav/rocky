# Local latency

What a single local change costs on one device, from the DAO call until its
observer shows it, how to measure it and what still costs time. Implemented
behavior; the numbers are one 18-core Apple M5 Pro run of 2026-10-11 on the
debug native library the JVM SDK builds.

## The path of one change

Ticking an item in the [shopping example](../../examples/shoppinglist-app/README.md)
is one Room transaction on the [SQL driver](sql-driver.md):

1. `BEGIN`, `UPDATE`, `SELECT changes()` and `END TRANSACTION`, each one native
   call served on the database's reactor thread.
2. The commit seals the changed row as one signed bundle in the engine
   transaction and answers [buffered](sql-driver.md#durability).
3. Room reads its invalidation log and runs the observed query again.
4. In the background the database flushes, runs the commit's maintenance
   step, and the sync worker starts its pass 20 ms after the change, so
   the observer's read is served first and a burst of commits is one pass.

Every database task shares the reactor thread. Work that runs there between a
commit and its observer's read delays what the user sees.

## Measure

```sh
cd examples/shoppinglist-app
ROCKY_LATENCY_PROBE=1 ./kotlin test -p jvm -m shared \
  --include-test='org.example.examples.shopping.LocalLatencyProbe.ticking an item reaches its observer'
ROCKY_LATENCY_PROBE=1 ./kotlin test -p jvm -m shared \
  --include-test='org.example.examples.shopping.LocalLatencyProbe.ticking an item while a peer syncs'
cargo test -p rocky-sdk --test driver local_write_latency -- --ignored --nocapture
```

The Kotlin probe uses the application's own repository on Room: it ticks one
of 50 items and times the DAO call, the observed list and, with a second
device, that device's list. The Rust measurement times each statement of the
same transaction on a driver connection without the JVM. Both print
percentiles and assert nothing. `ROCKY_LATENCY_TAPS` sets the count and
`ROCKY_LATENCY_PAUSE_MS` the pause between taps, 20 ms by default; 0 measures
a burst, in which background work meets the next tap. Run one scenario at a
time on an otherwise idle machine, and publish the JVM SDK first
(`sdks/kotlin/gradlew -p sdks/kotlin publishToMavenLocal`).

## Results

Milliseconds as p50 / p95 / max over 200 taps, 100 at the 200 ms pause.

| Scenario | Write returns | List shows it |
| --- | --- | --- |
| One device, burst, before | 14.2 / 24.5 / 111.9 | 17.5 / 30.2 / 115.7 |
| One device, burst | 2.9 / 6.2 / 11.8 | 4.0 / 7.7 / 13.0 |
| One device, 20 ms pause | 3.1 / 5.9 / 7.2 | 4.4 / 7.5 / 17.2 |
| A peer syncing, 200 ms pause | 4.3 / 6.7 / 8.0 | 5.7 / 8.6 / 10.6 |
| A peer syncing, 20 ms pause | 6.0 / 7.7 / 11.0 | 8.8 / 12.8 / 20.7 |

The peer's list shows the tick after 80 / 92 / 106 ms at the 200 ms pause.
Without the JVM a statement costs 0.08 ms for `BEGIN`, 0.19 ms for the
`UPDATE`, 2.0 ms for the commit and 0.6 ms for the read.

What changed between the first two rows:

- **The commit no longer waits for the device.** A full flush took about 9 ms
  of each commit and all of its 100 ms outliers.
- **A statement no longer verifies the device's credentials.** A partitioned
  schema verified its own chain for every table of every statement, about
  1.7 ms each; the selectors are now kept per authority and second.
- **Maintenance follows the commit.** Checkpoint and pruning steps took up to
  10 ms of some commits.
- **Sync waits 20 ms.** With a peer connected the pass started at the commit
  and the observer's read queued behind it: 10.2 / 14.6 / 17.9 ms before.

## What still costs

- Sealing is about 1.5 ms of a commit: the author's chain is verified twice,
  once more for the frame it just signed, the row's stored record is decoded
  several times and the frame is hashed repeatedly.
- Every 33rd commit or so takes about 4 ms longer: after a WAL checkpoint,
  due each 4 MiB, SQLite syncs the header of the restarted log.
- One change writes about 30 pages to the WAL.
- A sync pass for one changed row keeps both devices' reactors busy for tens
  of milliseconds. Taps faster than that meet it, as the 20 ms rows show.
- A burst meets the background flush and maintenance on the replica lock.
