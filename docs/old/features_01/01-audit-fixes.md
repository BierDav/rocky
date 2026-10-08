# Phase 1 — Fix all audit findings

> Historical foundation plan/evidence. Implemented contracts and recorded
> limits remain valid; future sequence and ownership follow
> [architecture V4](../../proposals/architecture-proposal-v4.md) and
> [features_04](../../features_04/README.md). Old future-tense instructions do not
> override that roadmap or require optional Room work before generic services.

Status: implemented; see [resolution evidence](audit.md#phase-1-resolution-evidence)
and the [supported API](../../runtime/supported-api.md). Required before V3 feature work. Read the [audit](audit.md),
[local replica](../../runtime/local-replica.md), [strong contract](../../protocol/strong-command-contract.md)
and [development workflow](../../development.md). The IDs below account for every
finding. Completion requires implementation and regressions, not documentation
of a defect as a limitation.

## Slice 1: make rejection and restoration safe

Owners: `rocky-runtime`, with `rocky-core` protocol validation as needed.

- **F01:** introduce explicit pending/verified/rejected result verification states
  with retained rejection evidence. An invalid receipt/package is isolated from
  valid work. Preserve a legitimate preparation even if an attacker supplies an
  invalid receipt for its hash; rejection evidence must not erase that preparation
  or prevent a later genuine receipt. Unavailability remains pending. An authenticated confirmed result
  that cannot be reconstructed blocks its chain rather than being skipped.
- **F02:** restore strong metadata, request counter, preparation hash keys,
  package/receipt bindings, verification/apply flags, contiguous confirmed chain,
  exact input replay and strong outbox/ack consistency. Reconcile inconsistent
  flags conservatively or return a typed corruption error; never expose them as
  confirmed. Do not require coordinator availability to reopen trusted local data.
- **F04:** introduce persisted storage/application descriptors and compare the
  installed definition before any migration/schema write. Initialization and
  additive migrations must be transactional and interruption-safe. Unsupported
  versions leave the file intact and explain the required recovery/upgrade.

Regression gates: forged unrelated receipt followed by valid command, then reopen;
transient verification outage; missing real predecessor; independent corruption
of head, receipt, preparation key, flags, command counter and outbox; interrupted
migration and incompatible application/projection reopen. Preserve existing
lost-reply, old-epoch historical receipt and sealed-input tests.

## Slice 2: remove fixture and application coupling

Owners: `rocky-core`, `rocky-runtime`, `todo-app`, `rocky-coordinator`; desktop
composes fixtures rather than core/runtime selecting them.

- **F03:** configure trusted development peers/principals explicitly and validate
  the same configured/authenticated author model at receive and reopen. Track
  strong acknowledgments by named peer; define configured outbox delivery targets.
  Do not clear all delivery obligations after the first peer receipt.
- **F05:** split mergeable application/projection contracts from optional command
  handlers. Add a supported command registration/dispatch boundary and relocate
  the snapshot-specific convenience API to todo composition. No dummy strong
  implementations for local-only applications.
- **F06:** define state-aware candidate admission, whole-row read selection and
  paired row/field write checks, with explicit policy/version context. Keep
  historical replay distinct from a current authorization decision. Static tests
  implement the contract now; signed delegation and encryption belong to phase 3.
- **F11:** make the coordinator port async, scope-bind heads/receipts and separate
  caller invocation permission from executor eligibility/takeover. Timeouts after
  submission mean outcome unknown; look up the original request before retrying.
- **F12:** make projection semantics consistent between one local operation and
  incoming batches. Prefer changes produced at each deterministic transition,
  committed together by the runtime; document any final-state alternative fully.

Regression gates: custom scope/node/caller persist and reopen; three named peers
with separate acknowledgments; client-only command origin; local-only second app;
second app with multiple tables/state-sensitive projection; allowed/forbidden
mixed patch and ownership change rejected at local/incoming boundaries; delayed
coordinator does not block unrelated async tasks or hold database transactions.
Retain the current trusted demo without claiming authentication.

## Slice 3: make the supported API bounded and usable

Owners: `rocky-runtime` and `rocky-core`; todo and desktop migrate to the resulting
API. Write a focused supported-API guide and compiling second-app example.

- **F07:** separate query snapshots from publication transfer/history. Introduce
  explicit query identity/scope and named-peer progress. Track local durable,
  verification pending, coordinator confirmed, local applied and peer evidence
  independently. Document local reads as potentially stale; no fabricated
  completeness or strong-read guarantees.
- **F08:** cap input/frame bytes, retained records and per-pass processing; add
  continuation-based operation/request/result pages and queue byte accounting.
  Yield between processing passes so queries/close can run. Return quota/pending
  states before accepting work that cannot be retained. Document how retained
  history limits are reached safely until phase 6 adds compaction.
- **F09:** extract database ownership/lifecycle from per-domain state and execution
  scheduling. Keep a single-domain mode working and tested. Define scoped author
  and revision keys, with no shared mutable cursor required by the domain API.
  Phase 2 supplies actual same-file concurrent execution and measurements.
- **F10:** expose stable accepted mutation/request handles and structured failure
  reasons (denial, unsupported generation, missing dependencies/head, quota,
  corruption, conflict, closed and unknown confirmation). Preserve error sources
  without requiring SDK callers to match engine strings. Define retryable versus
  terminal outcomes and optional baseline/precondition semantics.

Regression gates: waiter cancellation followed by handle lookup; deliberate
backpressure; scope/query observation registration race and coalescing; oversized
transfer/status work; pending processing cannot monopolize the worker; bounded
result pages with no omissions/duplicates; close drains accepted work. Failure
injection must demonstrate no partial projection/outbox/revision change.

## Completion checklist

| Finding | Required evidence |
| --- | --- |
| F01–F02 | Rejection isolation plus strong-ledger reopen/corruption suite |
| F03–F04 | Custom-identity reopen and installed-definition compatibility |
| F05–F06 | Local-only app and state-aware admission contract tests |
| F07–F08 | Scoped snapshots/progress and bounded continuation/fairness tests |
| F09 | File/domain ownership separation with documented single-domain limits |
| F10–F11 | Durable handle/error behavior and async coordination/client roles |
| F12 | Second-app local/batched/replay projection equivalence |

Run targeted packages, `./scripts/check`, and the native todo smoke scenarios if
composition/UI behavior changes. Update the audit with resolution links and actual
verification results. Record API/wire compatibility breaks and an explicit MVP
file migration or rejection path. Phase 1 does not require Raft, production keys,
a SQL bridge, checkpoint compaction or a Kotlin binding.
