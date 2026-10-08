# Fix later

Known defects that are not scheduled. Each entry states the scenario, the
site and a proposed fix. Scheduled work is in
[the roadmap](../features_06/README.md); local defects with an obvious fix are
in its [small fixes](../features_06/12-small-fixes.md). Remove an entry when it
is fixed or moved into a phase.

## Coordinator client writes a cache it cannot reopen

`rocky-coordinator/src/client.rs` appends an observed head per scope without a
cap, while opening refuses a cache with more than 64 heads. On a public service
(up to 4,096 domains, closed ones never dropped) a tenant owner that has seen
65 scopes persists a file that fails to open with `Configuration`.

Fix: bound the heads on insert and on open with one constant, sized for a
public genesis, and drop heads of closed domains.

## Stale fence leaves a strong request unreleasable

`rocky-runtime/src/strong.rs`, `execute_pending`: `Prepare` records the attempt
as `unknown` before `confirm`. On `StaleHead` or `StaleEpoch` the function
returns `StrongPending(StaleFence)` and the attempt stays `unknown`, so
`release_command` refuses the request and its checkpoint hold persists until a
later attempt applies.

Fix: a stale fence is a definite refusal of that confirmation. Record it with
a `Settle`-style request that discards the preparation and returns the request
to queued, as `Retired` and `Expired` already do for their outcomes; the next
pass prepares against the new head. First confirm in `rocky-coordinator` that
a confirmation certified with a lost reply is answered with its receipt and
never with a stale error.

## Kotlin automatic sync polls every peer

`sdks/kotlin/rocky-core/src/native/RockyDatabase.kt`: the default
`automaticSync` calls `syncOnce` every 500 ms while foregrounded, which dials
every peer. `enterBackground` waits for the running pass under the lifecycle
mutex, so an unreachable peer delays the pause by up to 15 s per route.

Fix: drive it from the native change-driven worker that `syncContinuously()`
already uses, and make the pause cancel the running pass.

## Domains cannot be closed outside Rust

`rocky-coordinator/src/client.rs`, `close_domain`, has no JSON or Kotlin call;
`command_service_*` covers register, control, activate and usage. A non-Rust
tenant can never close a domain or free its quota.

Fix: add the command to the service surface and the Kotlin binding.

## Reads and status do whole-table work on the writer thread

`query()`, `observe()`, `strong_status` and `strong_results` scan a whole
projection or ledger on the writer thread. Listed under
[known costs](../runtime/scaling-limits.md#known-costs).

Fix: page them, or serve them from the read connection.
