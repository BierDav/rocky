# 12. Small fixes

Status: not started.
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
