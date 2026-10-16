# End-to-end qualification evidence

Native macOS arm64 loopback evidence for the composed SDK, coordination service
and Kotlin package. Start with the [starter guide](getting-started.md) for what
an application may rely on. The shopping application is not yet qualified; see
[its audit](shopping-qualification.md).

## Reproduce

```sh
cargo test -p rocky-sdk --locked --release --test commands -- --ignored --nocapture
```

This runs the two latency probes below. The scenarios and the soak first ran a
Kotlin-only market application, since removed, as separate JVM processes
killed with SIGKILL against three `rocky-voter` processes; the last column names
the command that reproduces each scenario today.

## Scenarios

All passed on 2026-10-07; the last rerun on 2026-10-08 passed 42 checks.

| # | Result | Pointers | Reproduce |
| --- | --- | --- | --- |
| 1 Two owners, one service | Two tenants each hold one open domain on the same voters. Another tenant's control and registration are `denied`. Voter files hold no application plaintext | [Public coordination](public-coordination.md) | Not reproducible as processes; `ServiceOwnerTest` in `sdks/kotlin/gradlew -p sdks/kotlin :rocky-core:jvmTest` covers the tenant quota and a refused non-owner |
| 2 Offline mergeable data | Edits on two members survive SIGKILL, converge directly while the creator is stopped, and the restarted creator catches up. A scoped writer is `denied` other collections and holds none of their bytes | [Permissions and sync](permissions-and-sync.md) | In part: `./examples/shoppinglist-app/qualify` covers offline edits through custody and creator loss ([shopping](shopping-qualification.md)); the SIGKILL and scoped-writer steps are not reproducible as processes |
| 3 Competing commands | Of two reservations for the last item one is accepted, the other gets a typed rejection kept across kill and reopen. Command-controlled rows are read through a Room DAO; DAO and SQL writes to them are refused. Undeclared methods and versions, mistyped arguments, callers without invoke rights and non-executors are refused | [Typed commands](typed-commands.md) | Undeclared and mistyped calls are refused in process by `cargo test -p rocky-sdk --test commands` |
| 4 Loss, cancellation, takeover | A lost reply, a killed client, a cancelled wait and a killed executor each complete the original ticket, with one handler call per identity. The owner fences the executor and the new one continues at the same head. An unconfirmed identity reads `Expired`; a confirmed one whose reply was lost reads `Retired` with no outcome | [Coordinator retention](coordinator-retention.md) | `Expired` and `Retired` are not reproducible as processes |
| 5 Shared endpoint | Rocky and an application protocol share one application-owned iroh endpoint across a Rocky close and reopen | [SDK transports](sdk-transports.md#iroh) | Not reproducible as processes |
| 6 Service faults and custody | Strong work continues after leader loss, pauses at quorum loss while mergeable writes sync, and the pending identity applies once after restore. An executor without its results answers retryable `missing_head`, then recovers from the custodian; with the only copy removed it stays `missing_head` | [Coordinator operations](coordinator-operations.md), [custody](getting-started.md#optional-custody) | Not reproducible as processes |
| 7 Upgrades and revocation | An issuer upgrades additively while an old peer writes; strong work waits with `unsupported_generation` until the executor activates the generation. Logout ends authority. Revocation rotates keys and the revoked writer receives no later plaintext | [Generations](generations.md), [credentials](credentials.md) | Not reproducible as processes |
| 8 Retention soak | 669 commands in 21 windows of 20 s with a device kill and a new control revision per window. Voter responses, state bytes, sessions and retry records stayed within 125% of the largest earlier window (10–19 responses, 9–15 KB state) | [Coordinator retention](coordinator-retention.md) | Not reproducible |

Syncing while a configured peer is stopped took about 75 s in scenario 2,
because each dial runs to its timeout.

## Strong-command latency

`command_latency_does_not_grow_with_confirmed_history` confirms 400 commands on
two devices with an in-memory coordinator and transport. Mean per command stays
flat by hundred confirmed: 68–70 ms debug, 26 ms release. Execution and import
are incremental ([receiving](encrypted-commands.md#receiving-subsets-and-completeness)).

`first_command_after_reopen_is_bounded_by_strong_window` confirms 500 commands
with a 16-result window and reopens the invoking writer after each hundred.

| Confirmed prefix | First command after reopen, unpruned | With window 16 |
| --- | --- | --- |
| 100 | 177 ms | 34.8 ms |
| 400 | 503 ms | 46.8 ms |

Open took 16–38 ms and the first sync 25–28 ms. The retained payload audit is
bounded by the window, the base and pinned unobserved jobs. A separate
`rocky-secure` regression confirms 4,100 commands, reopens the reader, imports
a peer below the pruned prefix and confirms after takeover ([strong history](strong-history.md)).

## Device limits

- Strong command payloads are pruned behind the result window. Effect and
  recovery artifacts share 4,096 records and 32 MiB (`MAX_SECURE_RECORDS`,
  `MAX_SECURE_BYTES`) and are not pruned. Before pruning, an unpaced soak filled
  the byte budget at 938 confirmed commands of 17 KB results; the executor then
  stopped and a later ticket ended `Retired` without an outcome.
- Each new receipt rewrites the device's whole coordinator evidence file, up to
  4,096 receipts. In the three-voter soak, mean confirmation rose from 230 ms to
  512 ms over 20 windows. The executor prunes behind the result window and its
  records stay at 384; an invoking device keeps every result and stops near
  2,065 commands at its byte budget.
- Retry markers and effect observations are not claimed to plateau.

## Not qualified

No relay, NAT, WAN or public Internet run was made; everything is loopback on
one machine except one [phone on a LAN](shopping-devices.md#physical-device-run-2026-10-08).
iOS, web, physical power loss, disk-full recovery and Byzantine voters are
outside this evidence. The packaged native library is a development build, so
packaged timings are not release numbers.
