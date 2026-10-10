# Market qualification

The [market example](../../examples/market-app/README.md) runs strong
commands against a coordination service of three `rocky-voter` processes.
Qualified on 2026-10-10 by [phase 19](../features_07/19-market-example.md) on
the JVM desktop profile (macOS 27 arm64, 18 cores, loopback iroh without
relays), each device in its own JVM, and with one reservation on an Android
emulator. This is not an Internet, NAT, relay or phone claim; the two-phone run
is [phase 22](../features_07/22-qualification.md)'s.

## Commands

| Command | Runs |
| --- | --- |
| `./examples/market-app/check` | The commands, Room DAOs and schema through the packaged runtime against the in-memory coordinator; compiles the desktop app and the harness |
| `./examples/market-app/qualify` | Builds `rocky-voter` and the packaged `rocky-peer`, then the [harness](../../examples/market-app/marketQualification/src/Qualification.kt): three voters provisioned as `scripts/coordinator-local` does, devices as JVMs running the app's `MarketNode`; evidence in `local/market-qualification/evidence.json` |
| `./examples/market-app/soak [COMMANDS]` | The [soak](#soak), outside every default check; evidence in `local/market-soak/soak.json` |
| `./examples/market-app/emulator-service DIR` | Voters whose bundle an Android emulator can reach; see [below](#android-emulator) |

`ROCKY_SCENARIOS=last-item,loss` selects scenarios; `ROCKY_VOTER` and
`ROCKY_PEER` take prebuilt binaries. Each scenario registers its own domain on
the run's service and closes it with `command_service_close` at the end. The CI
`market` job runs all five and keeps `evidence.json`. Each evidence entry names
the host, the build and the voter count.

## Scenarios

All five passed on 2026-10-10 in one run of 2.5 minutes.

| Scenario | What passed |
| --- | --- |
| `last-item` | Two buyers reserve the last lamp at once from their own processes: one `Accepted(remaining 0)`, the other `Rejected(Shortfall(0, 1))`. The rejected buyer is killed with SIGKILL and reopened; its ticket still reads the same rejection. Every device reads `lamp=0` through the Room DAO; one handler call per reservation |
| `loss` | A lost reply (the executor confirmed, its answer dropped), a killed caller (SIGKILL after its invocation left) and a killed executor (SIGKILL after confirming, the caller meanwhile `unreachable`) each complete the original ticket after rerunning it, with one handler call per identity |
| `takeover` | The owner's control revision names a second issuer as executor. The fenced seller answers a delivered invocation without deciding. The backup acquires the domain with its own first command at the sequence after the old head, then decides the caller's original ticket once |
| `refused-writes` | On the seller and a buyer, a DAO upsert to `stock` fails with `SQLITE_AUTH` (23) and SQL `UPDATE`, `INSERT` and `DELETE` on `stock` and `holds` are refused; the confirmed head does not move, while a memo and a listing sync both ways |
| `voters` | Killing the leader voter: strong work continued after re-election (21 s). Killing a second: the ticket pauses as `Queued` with `unknown_confirmation` while a buyer's memo syncs to the seller with the custody peer present. Restarting both: the same ticket applies once |

## Soak

The seller restocks one lamp and a buyer reserves it, alternately, so the
command tables stay two rows; every reservation crosses processes. Each window
of 256 commands records latency and both devices' retained command records and
bytes, counted as the runtime's budget check counts them (`MAX_SECURE_RECORDS`
4,096, `MAX_SECURE_BYTES` 32 MiB).

The soak stopped at a named budget in 13 minutes: after 2,065 of 4,096
commands the buyer's next result was refused with `retained command quota`,
its retained command bytes at 33,539,450 of the 33,554,432 budget. An earlier
run stopped the same way after 2,071.

| Commands | Mean per command | Seller records, MB | Buyer records, MB | Evidence file, seller and buyer |
| --- | --- | --- | --- | --- |
| 256 | 232 ms | 384, 7.8 | 256, 4.2 | 234 and 117 KB |
| 1,024 | 411 ms | 384, 7.8 | 1,024, 16.6 | 936 and 468 KB |
| 1,536 | 260 ms | 384, 7.8 | 1,536, 25.0 | 1,404 and 702 KB |
| 2,048 | 503 ms | 384, 7.8 | 2,048, 33.3 | 1,872 and 936 KB |

The executor's records stay at 384 because it prunes behind the result window;
the buyer, which invokes but does not execute, keeps every result of about
16 KB and is not pruned. Latency varied with other builds on the host and did
not grow steadily. The coordinator evidence file grows by about 0.9 KB per
receipt on the seller and is rewritten whole on each.

## Android emulator

On 2026-10-10 the Android app on an API 35 emulator (arm64) created a market
against three voters on the host, listed and restocked a lamp and completed one
accepted reservation through the screen (`Accepted: 0 left`). Procedure:

```sh
sdks/kotlin/android-runtime/gradlew -p sdks/kotlin/android-runtime publishToMavenLocal
./kotlin build -m marketAndroid
./examples/market-app/emulator-service local/market-emulator
adb install -r build/tasks/_marketAndroid_buildAndroidDebug/gradle-project-debug.apk
adb shell am start -n org.example.examples.market/.MainActivity \
    --es bundle "$(base64 < local/market-emulator/service/bundle.json | tr -d '\n')" \
    --ez create true --ez loopback true
```

`emulator-service` certifies each voter's client address twice: `127.0.0.1`
for the host and `10.0.2.2`, the host's loopback as the emulator reaches it.
On the screen, add a listing, press Restock, then Reserve. The phone in this
run was the owner and executor; a phone reaching an executor it never met is
[phase 16](../features_07/16-strong-commands-in-the-mesh.md)'s.

## Limits

- An idle executor named by a new control acquires the domain only when it
  runs a ticket of its own; until then invokers get `executor_unavailable`.
- Invokers retain every result unpruned: the soak's buyer stops near 2,065
  commands at its byte budget.
- Leader re-election took 21 s of client retries; quorum loss costs each run
  the client request deadline.
- Loopback only; relays, NAT and phones are not covered.
