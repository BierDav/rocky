# 4. Small fixes

Status: not started.
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
