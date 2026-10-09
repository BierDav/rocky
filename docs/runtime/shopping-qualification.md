# Shopping qualification

Incomplete; audited on 2026-10-08. This is not a release qualification. The [current roadmap](../features_06/README.md)
schedules the missing partition-aware constraints.

## Required schema installs

```sh
./kotlin run -m requiredSchema
```

Since [phase 7](../features_06/07-builder-room-and-shopping-schema.md) the
Kotlin-only [required definition](../../examples/shoppinglist-app/requiredSchema/src/RequiredSchema.kt)
builds: `lists`, `categories` and `items` in the `list` group, a `LatestWins`
unique index on `(list, title)`, a cascading list reference and a nullable
`(list, category)` reference with `SET NULL`. It equals the import of the app's
Room export, and the native runtime installs either and reopens with the other.
`./examples/shoppinglist-app/check` runs it first. Partitioned `SetNull`
indexes, references across groups, existence logs and command control stay
refused ([mergeable constraints](mergeable-constraints.md)).

## Acceptance scenarios

The deliverable is the shopping-list application as a packaged Kotlin app with no
Rust source: Room DAOs on Rocky's file through the driver, `lists` partitioned by
its own `id` and `items` by `list`, per-list sharing through partition grants and
key envelopes, two Android devices, a packaged always-on custody peer and a second
issuing device. It is mergeable-only and needs no coordinator; the market app
covers strong commands.

1. Two phones never online together share one list through the custody peer; a
   third device joins from a checkpoint and catches up.
2. A list is shared with one device and not another; the other never receives
   its frames. Revocation rotates the partition key.
3. An item is unlinked and relinked offline on two devices and converges; a
   duplicate title under the list's unique index converges by policy.
4. The creator phone is lost; the second device invites and renews.
5. A horizon drops tombstones on every device and a long-offline phone
   rebootstraps.
6. Market: 4,096 confirmed commands without hitting the budget; a DAO reads
   confirmed stock; a handler reads by key.
7. An ERP-shaped schema with a cross-scope foreign key and 200 partitions on one
   device, against [the budget](partitioned-scopes.md#limits-and-budget).

The deterministic multi-process scenarios run in CI; one real Android run with a
real network is recorded separately. Loopback evidence does not establish
Internet readiness.

## Remaining acceptance work

| Scenario | Status |
| --- | --- |
| 1 Phones alternate through custody; checkpoint join | Partitioned Room app and multi-process scenario outstanding; one-phone LAN run only |
| 2 Per-list exclusion and revocation | The app shares a list with one envelope; JVM tests share, rotate and reshare; per-device exclusion on phones outstanding |
| 3 Offline relink and duplicate title policy | Schema installs from Room; native per-partition convergence tested in `rocky-secure`; app scenario outstanding |
| 4 Creator loss; second issuer invites and renews | `IssuerRecoveryTest` runs it on the partitioned app schema on the JVM; devices outstanding |
| 5 All-device tombstone finalization and rebootstrap | Horizon regressions exist; app scenario outstanding |
| 6 Market 4,096 commands, DAO stock, keyed read | Secure 4,100-command test exists; packaged scenario outstanding |
| 7 ERP cross-scope foreign key with 200 partitions | Partition probe and unpartitioned existence tests are separate; combined fixture outstanding |

None of the seven multi-process scenarios is implemented or run in CI. The
Android app now builds on the required version-3 definition, but no phone run
used it, and no device driver is in the tree. A single-phone LAN run cannot
establish the two-phone custody topology or Internet/NAT/relay readiness.

## Physical-device run, 2026-10-08

The since-removed Android device driver passed on a Pixel 11 Pro (Android 17,
API 37, `arm64-v8a`) using direct TCP to `192.168.0.186`. Apps were installed over USB; their traffic
used direct TCP over home Wi-Fi to a macOS arm64 host peer on the packaged JVM SDK.
Three apps ran: the Gradle `todo` and `shopping` consumers of the packaged AAR, and `compose`,
the [Compose Multiplatform app](../../examples/shoppinglist-app/androidApp) built by the
Kotlin toolchain from the shared shopping module.

| Check | Result |
| --- | --- |
| Instrumentation, debug and R8 release | Packaged consumer, Keystore store (non-exportable key, security level 1), missing-secret refusal and lifecycle reopen passed |
| todo: join / phone→host / background then foreground / kill then restart | 7.2 s / 2.3 s / 7.3 s / 7.3 s |
| shopping (Room DAOs on Rocky's file): same steps | 3.7 s / 0.2 s / 3.7 s / 1.9 s |
| compose (same DAOs, Compose UI): same steps | 3.7 s / 2.5 s / 2.5 s / 1.3 s |
| Secrets | 7 sealed secrets per app under `no_backup/rocky-secrets`; no plaintext directory |

During the background step, the host wrote a row and the paused app did not receive
it within 3 s; it received the row after returning to the foreground. Timings are single
samples of the polling driver on a lossy home network, not latency measurements; an
earlier passing run measured 0.7 s for each todo step.

Findings fixed on the way:

- Each issuing app claimed the SDK's default `127.0.0.1:47470`; a cold-started second
  app failed with `Address already in use`. The examples now listen on an ephemeral
  loopback port, since the phone only dials.
- Android 17 gates LAN connections behind the runtime `ACCESS_LOCAL_NETWORK`
  permission for apps targeting API 37. The Compose app targets it and requests the
  permission before opening; the Gradle apps target API 35. Without it, joins time out
  as `peer unreachable`.

One run failed while the Wi-Fi was degraded (phone→host pings 100% then 50% lost): the
todo join's 30 s pairing exchange (`enrollment::exchange`) elapsed and the app
reported `open failed`; a repeat at 50% loss failed the same way. The same build passed
every step in under a second over USB loopback (`adb reverse`), so these are network
failures. The examples do not retry a failed join. Such timeouts surface as
`deadline has elapsed` rather than a classified unreachable error.

This is one phone and one host on one LAN, with the version-2 unpartitioned shopping schema.

## Resumption boundary

Implement partition-aware unique/FK resolution, row identities in candidate and
support bookkeeping, and checkpoint/horizon finalization without crossing
partition authority. Qualify refusal and convergence before lifting the guards.
Then migrate the Room entities and repository to the required schema, compose
explicit per-list grants/envelopes and custody, build all seven process
scenarios, wire them into CI and record the Android topology separately.
