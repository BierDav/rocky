# 4. Small fixes

Status: complete; all completion gates passed locally (2026-10-10).
Owner: the `rocky-coordinator` client cache, `rocky-runtime` strong requests,
the service commands in `rocky-sdk` and `ServiceOwner.kt` in Kotlin
`rocky-core`. No dependency. Wave 0.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [fix later](../fix_later/README.md) and the
[service calls and bounds](../runtime/public-coordination.md). The market
application ([phase 19](19-market-example.md)) runs strong commands against a
coordination service from Kotlin and meets three of the entries. Fix each
with a regression test and remove its entry.

## Settled

- The head cap is the public-domain limit, `MAX_PUBLIC_DOMAINS` (4,096), as
  one constant used on insert and on open. Nothing is evicted: an evicted
  head would disable the client's rollback check for that scope. A head
  beyond the cap is refused with `Configuration` before the file is written.
- The cache format does not change; its format number stays and no decision
  record is taken.
- `StrongHead` carries no closed marker, so a client cannot recognise a
  closed domain it only observed. A head is dropped only after this client's
  own `close_domain` succeeded.
- Closing outside Rust is the request `command_service_close` with `tenant`
  and `id`, over the existing JSON call path, owner only. The device reads
  the current head itself and the result is the closed head. No UniFFI
  export is added.
- Closing frees the tenant's open-domain quota only. Closed domains still
  count toward accounted bytes and toward the 4,096 domains of the service,
  and a closed scope is never registered again.
- Tests of a certified confirmation with a lost reply exist
  (`lost_reply_is_reconciled_without_another_sequence` and the runtime's
  lost-reply tests). Fix 2 extends them; it does not start from nothing.

## Fixes

1. **Coordinator client cache.** `client.rs` appends an observed head per
   scope without a cap, and opening refuses more than 64. Apply the settled
   constant in both places and drop the head when `close_domain` returns.
2. **Stale fence.** In `execute_pending`, `Prepare` records the attempt as
   outcome unknown before `confirm`. On `StaleHead` or `StaleEpoch` the
   function returns `StrongPending(StaleFence)` and the attempt stays
   unknown, so `release_command` refuses the request and its checkpoint hold
   persists. First show in `rocky-coordinator` that a confirmation certified
   with a lost reply is answered with its receipt and never with a stale
   error, on the development coordinator and on the voter group. Then record
   a stale fence with a `Settle`-style request that discards the preparation
   and returns the request to queued, as `Retired` and `Expired` settle
   theirs. The pass still returns `StaleFence`; the next pass prepares
   against the new head.
3. **Closing a domain outside Rust.** Add `command_service_close` beside the
   other `command_service_*` requests, `ServiceOwner.close` in Kotlin and its
   row in the request table of the public coordination guide. The Kotlin edit
   lands before [phase 17](17-kotlin-native-binding.md) moves the native
   sources.
4. Remove the three entries from fix later.

## Completion gate

- A client that observed 65 scopes reopens. A client that observed 4,096
  reopens, and its cache stays within the store limit. The 4,097th head is
  refused and the file on disk still opens.
- A client that closed a domain reopens without that head; a lower head for
  a scope it still holds is refused as before.
- A request whose confirmation met a stale fence is queued again, can be
  released while queued, and otherwise is prepared against the new head and
  applies once. A cut after the settle and before the next pass loses
  nothing.
- A lost reply to a certified confirmation yields the receipt, with the head
  advanced by another executor in between.
- A Kotlin tenant at its open-domain quota closes a domain and registers
  another; a member that is not the owner is refused.
- The three entries are gone from fix later; `cargo test -p
  rocky-coordinator -p rocky-runtime -p rocky-sdk` and `./kotlin check` pass.

## Not in this phase

- Evicting heads, a closed marker on `StrongHead` or a new cache format.
- Reopening or re-registering a closed scope, or freeing its bytes.
- Takeover, control revisions and retention on the service.
- Reads and status on the writer thread; that entry stays in fix later.
- The checkpoint page's credential chains
  ([phase 11](11-chains-by-reference.md)).
- The sync entries: automatic sync is
  [phase 2](02-change-driven-sync.md), the Room write timeout
  [phase 3](03-catch-up-cost.md).

## Evidence (2026-10-10)

- Prerequisites verified against the cache, request ledger and service call
  paths. Retained confirmations precede stale-fence checks in the coordinator.
- An exhaustive development run observed and persisted 65 then 4,096 scopes,
  reopened both, refused head 4,097 without changing disk, and rejected lower
  heads after reopen. At 4,096 it encoded 974,570 bytes against the
  33,554,432-byte cap.
  The retained regression seeds the intermediate snapshot after scope 65,
  exercises the final insert and reopen, and refuses an oversized file on open;
  it passed in 1.915 seconds in the workspace check.
- Development and three-voter lost-reply regressions passed after a takeover
  and another confirmation advanced the head. The voter test cut the leader
  at `reply.before_send` and verified the recovered receipt's signature.
- Both stale errors requeue across reopen; release frees the cut, or a fresh
  preparation applies the original request once. Injected commit failure
  preserves the original unknown preparation atomically.
- Three-voter closure reopens without that cached head, retains another scope,
  denies a non-owner, frees open quota and refuses the closed identity.
  The SDK JSON service regression covers closure and retry after three commands.
- The Kotlin three-voter regression reached the 16-domain tenant quota,
  refused registration 17, closed and retried a domain, then registered another
  (16 → 15 → 16 open domains). Non-owner closure and re-registration of the
  closed identity were denied; closed bytes remained accounted.
- Closure's head read required allowing the registering tenant owner to read
  its own head without executor/invoker membership. Cross-tenant reads remain
  denied, with a regression assertion.
- No cache, persisted ledger or wire format changed; Kotlin edits stay in
  `ServiceOwner.kt` beside a JVM regression, before phase 17's platform move.

The first full Kotlin gate exposed stale qualification fixtures from before the
current issuer registry: author allocation numbers, table-name installation
order, the issuer-only scope, rejection of the current identity version and
removal of the last issuer contact. These fixture assertions now exercise the
current contracts without changing production behavior or admitting old formats.
The Android gate also required rebuilding the source-matched Maven-local runtime.

Serial reruns exposed two household timeouts: a second invitation replaced the
single pending invitation before the first enrollment was recorded. Reading the
live fixture confirmed a missing `Removed` row and an unnamed partial device
alongside `Remaining`. Those fixtures now await the first recorded enrollment
before issuing the next invitation, using a Flow barrier with unchanged time
limits. Production sync and household behavior are unchanged.

Verification from this phase's workspace:

- `cargo test -p rocky-coordinator -p rocky-runtime -p rocky-sdk --locked`:
  363 passed, zero failed, six ignored soaks.
- `./scripts/check`: 696 workspace tests and three doc tests passed, followed
  by 154 SDK tests without default features; zero failures. The two nextest
  runs skipped 15 and four tests, respectively. Formatting and workspace
  Clippy passed.
- `./scripts/lint` and `./scripts/commitlint --jj`: passed.
- `./kotlin check`: passed in 215.56 seconds, with 54 host test executions,
  zero failures, schema/inventory qualifications, the clean packaged consumer
  and Android compilation against the rebuilt current-source runtime.
- `./kotlin test -m shared -p jvm
  --include-classes='org.example.examples.shopping.HouseholdTest'`: all five
  tests passed in 103.024 seconds after the enrollment barrier repair.

Hosted CI, physical-device execution and the ignored/skipped tests were not run.
There are no remaining phase 4 blockers. The three fixed entries are removed
from fix later.
