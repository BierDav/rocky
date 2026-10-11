# 12. Small fixes

Status: complete on 2026-10-08: all 11 items, behavior regressions and owning
guides completed; `./scripts/check` passed (618 workspace tests, 120 SDK tests
without default features).
Owner: the crate named on each item. Depends on nothing; items touching a
format land after [phase 1](01-remove-compatibility-code.md). Wave 0.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Defects found while pruning the guides on 2026-10-08. Each is local, has an
obvious fix and needs a regression test. Larger or undecided defects are in
[fix later](../fix_later/README.md).

## Items

1. `rocky-coordinator/src/client.rs`, `persist`: an evidence file over
   `MAX_STORE` returns `Denied`. Return a capacity error the caller can tell
   from an authorization refusal.
2. `rocky-coordinator/src/client.rs`, eviction: "newest of its scope" scans
   every receipt for each candidate, quadratic in a full cache while the cache
   lock is held. Precompute the newest sequence per scope.
3. `rocky-coordinator/src/service.rs`, voter listener: a connection beyond the
   64 slots is dropped silently. Count the refusal in `status`.
4. `rocky-sdk/src/enrollment.rs`, `exchange`: one deadline covers dialing every
   route and the exchange, so later routes are never tried when the first two
   stall. Scope the deadline to the exchange after `dial`.
5. `rocky-sdk/src/transport/tcp.rs`: every issuer defaults to
   `127.0.0.1:47470`, so two issuing apps on one host fail with "address in
   use". Default to an ephemeral port or require `listen`.
6. `rocky-sdk/src/connection.rs`: `rocky_next_id` is recognized only as a whole
   statement by token shape. `SELECT rocky_next_id('t') AS x` and a negative
   argument must be refused with a precise message, not reach the engine as an
   unknown function.
7. `rocky-core/src/logical.rs`: column lookup is case-sensitive while duplicate
   detection lowercases, and the foreign-key target check mixes both. Use one
   comparison and report the column, not `Reference`.
8. `rocky-core/src/logical.rs`: the non-mergeable foreign-key branch is
   unreachable because `LogicalTable::installable` refuses first. Remove it. A
   hand-written `REFERENCES` in a local table's `create_sql` is not inspected;
   refuse it.
9. `rocky-secure/src/replica/keys.rs`: a duplicate key under another scope
   reports "key requires read permission". Name the real cause.
10. `rocky-secure/src/replica/constraints.rs`: add the regression test for
    the equal-stamp unique tie fixed on 2026-10-08 (one frame writing
    different columns of two rows).
11. `crates/rocky-coordinator/Dockerfile` and
    `.github/workflows/coordinator-image.yaml` disagree on whether the image
    is public. State one, and match
    [coordinator operations](../runtime/coordinator-operations.md).

## Completion gate

- One regression test per item that changes behavior.
- `./scripts/check` passes; the brief's status line lists what was done.

## Implementation evidence

- Client persistence uses `Backpressure` for the encoded capacity limit and
  leaves the durable file intact; eviction computes scope watermarks once.
- Voter slot saturation contributes to `status.metrics.busy` without adding a
  wire field. The regression fills and releases all 64 permits deterministically.
- Enrollment gives every route its own dial deadline, then a separate exchange
  deadline. A paused-clock regression reaches a third route after two stalls.
- TCP issuer defaults use an ephemeral loopback port; two concurrent defaults
  connect using the advertised bound address.
- `rocky_next_id` refuses aliases, expressions, malformed argument lists and
  negative literals explicitly; negative values may be bound parameters.
- SQL identifier comparison is ASCII case insensitive through schema validation,
  row capture and constraint consumers. Missing reference columns are named.
  A Rust regression installs mixed-case references, replicates and cascades.
- Non-mergeable tables refuse raw `REFERENCES`; the dead target branch is removed.
- Duplicate key material names its collision without changing installed epochs.
- Equal unique-index stamps choose the larger row id after opposite delivery
  orders, projection rebuild and reopen.
- Dockerfile, image workflow and operations guide specify a private GHCR package.

No persisted or wire format changed, so this phase needs no decision record.
Phase 1 still owns removal of the base's pre-existing compatibility code.
The completion gate adds no Kotlin, native ABI, packaged consumer, Room/schema
or measurement command beyond the Rust workspace checks.

## Verification record

All commands ran from `/Users/david-bieregger/AgentWorkspaces/rocky-db-p12-small-fixes`.

- `cargo fmt --all` and `cargo fmt --all -- --check`: passed.
- `cargo test -p rocky-core --lib --locked`: 45 passed.
- `cargo test -p rocky-secure --lib case_insensitive_schema --locked`: 1 passed,
  162 filtered out, after correcting the test-stream type and a borrowed name.
- `cargo test -p rocky-core -p rocky-coordinator -p rocky-sdk -p rocky-secure --lib --locked`:
  coordinator 23 passed / 1 ignored, core 45 passed, SDK 20 passed; the secure
  batch was superseded by the final workspace run after its phase regressions
  passed. The initial attempt failed compiling the test's unboxed stream.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: a later
  argument-shape check initially hit `manual_is_multiple_of`; fixed with the
  standard method. Final Clippy runs are part of `./scripts/check`.
- `./scripts/check`: the first preliminary run was superseded. A completed
  attempt then failed the existing coordinator durable-batch mean gate at
  52.230329 ms (limit 50 ms) while sibling workspaces were compiling and testing.
  The unchanged retry passed that probe; no threshold or assertion was weakened.

Final `./scripts/check` retry exited 0:

- `cargo fmt --all -- --check`: passed.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: passed.
- `cargo test --workspace --locked`: 615 unit/integration tests and 3 doc tests
  passed; 11 ignored entries, 0 failures. Crash-helper entries are invoked by
  their parent regressions; standalone measurements/container gates stay ignored.
- `cargo test -p rocky-sdk --no-default-features --locked`: 120 passed,
  4 ignored, 0 failures.

The completed changes are described with `fix: correct coordinator, schema and SDK edge cases`.
`scripts/commitlint --jj` checks that description. Raw logs are kept under the
ignored workspace directory `local/phase12-verification/`.

Kotlin, excluded native ABI, packaged consumer, Room/schema qualification,
Android, device/real-network scenarios and the standalone ignored measurements
were not run. Docker build/container gate, workflow execution and live GHCR
visibility were not verified. Local checks do not establish those qualification claims.
