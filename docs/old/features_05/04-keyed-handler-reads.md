# 4. Keyed handler reads

Status: implemented and qualified on 2026-10-07; required result met.
Owner: `rocky-core` command host contract, `sdks/native`
handler ABI and the Kotlin handler interface. Depends on nothing; wave 0.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [keyed handler reads](agreed-scope.md#keyed-handler-reads) of the scope,
[typed commands](../runtime/typed-commands.md) and
[encrypted commands](../runtime/encrypted-commands.md). Inspect how the host
passes every confirmed row of a command's tables to a handler today.

Offer keyed reads and bounded scans in the host-language handler interface,
served from the complete rows the runtime already holds, so handlers do not
change when tables grow. The 128 KiB envelope and
[large command tables](../optimizations_on_demand/large-command-tables.md)
stay deferred; this phase changes the interface, not the storage.

## Ordered slices

1. Define the read contract: get by primary key, scan by key range with a
   limit, in `rocky-core`, deterministic for equal confirmed state.
2. Serve it through `sdks/native` without a JSON copy of whole tables per
   invocation.
3. Kotlin handler interface on top; the market app's handlers move to keyed
   reads.
4. Keep the whole-table path only as long as a shipped handler needs it, then
   remove it. Two maintained read paths are not kept.

## Completion gate

- A handler that reads one key does not receive the rest of its table.
- Equal confirmed state gives equal results on every executor; a scan is
  bounded by its limit.
- The market app's commands produce the same results as before the change.
- [Typed commands](../runtime/typed-commands.md) documents the read contract.

## Measured evidence

- `cargo test -p rocky-core --locked`: passed, including deterministic compound
  key ordering, absent and empty reads, exclusive upper bounds, bounded scans,
  invalid keys/ranges/limits and duplicate-state refusal.
- `cargo test -p rocky-secure typed_commands --locked`: 6 passed; admission,
  scoped results, predecessor fencing, retry/reopen and takeover remain intact.
- `cargo clippy -p rocky-core -p rocky-sdk --all-targets --locked -- -D warnings`:
  passed.
- `./sdks/native/check`: passed with default features and without iroh; read
  tests prove only selected rows cross and handles expire on return/unwind.

The core contract and ABI are one atomic slice because replacing `HandlerInput`
requires migrating its JSON adapter at the same time. The Kotlin interface,
shipped handler migrations and whole-table removal are a second atomic slice:
there is no compatibility whole-table handler path.

- `cargo test -p rocky-sdk --locked`: passed, including keyed Rust handlers,
  ticket retry/reopen, deadline, generations, backup and transport regressions.
  Two pre-existing measurement tests remain ignored.
- `./sdks/kotlin/check`: passed, including exact integer/blob keyed-read codecs,
  bounded range forwarding and refusal of old whole-table handler frames, plus
  the existing compile-fail type fixtures and process qualification.
- `./examples/shoppinglist-app/check`: 21 tests discovered and passed, none
  skipped, plus desktop compilation. The new native-backed Kotlin test covers
  exact/absent keys, ordered bounded ranges, exclusive upper bounds, invalid
  resources/keys/limits and retained-context expiry.
- `./scripts/check`: passed formatting, Clippy with warnings denied, workspace
  tests/doctests and `rocky-sdk` tests without default features.

- Historical market application gate (removed 2026-10-08): passed 41 checks using the packaged JVM SDK
  and three provisioned voters on loopback. Existing assertions still give one
  accepted reservation and `Shortfall(remaining=0, requested=1)` for its
  competitor; retry/reopen, fencing, upgrade and custody recovery also pass.
  The existing earlier-policy-epoch history limitation after revocation was
  reproduced, not counted as a new capability.

Prerequisites were verified in `TypedCommands::run_command` and the secure
execution/preparation path: complete authorized confirmed state is required,
handlers run outside the replica lock/transaction, and preparation rechecks the
input and fence. The read adapter changes none of those boundaries or persisted
command formats. No prerequisite or phase 4 contract remains blocked.

All four completion gates are met. Large command tables, retention/performance
qualification and real-network evidence remain deferred; complete runtime rows,
128 KiB envelopes and existing retained-command budgets are unchanged. No
shopping DAO or Android packaging work beyond the handler regression was added.
