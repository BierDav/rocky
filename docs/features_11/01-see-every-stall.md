# 1. See every stall

Status: implemented 2026-10-25; overhead and manual device gates open, see the
[evidence](01-see-every-stall-evidence.md). Owner: `rocky-secure`
readiness, compaction and apply gates; `rocky-sdk` status and refusals;
the native binding's event sink.
Depends on nothing. Effort: one to two weeks. Follow the
[execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read the [review](00-reliability-review.md), findings 4 to 6. Today a node
that cannot write or read reports the first gate that refused it, a frame
that waits records no reason, and Rust emits no events at all; each device
blocker took a probe build to see.

After this phase every refusal and every status names all of its blockers,
every pending frame says why it waits, and one sync pass can be followed as
a tree of spans without exposing application data.

## Settled

- **One source for gates and diagnostics.** `EncryptedReplica::blockers()`
  returns `Vec<Blocker>`; `require_workspace_base`, `require_scope_readable`,
  `ensure_tip_restored`, the bootstrap and import checks and `table_complete`
  answer from the same predicates. A refusal error carries the full list.
  Blockers are derived from stored state on demand, not stored.
- **A blocker is** `{ scope: short hash, kind, clears_by, since }`.
  `kind` is a closed enum covering at least: bootstrap marker, awaiting
  inviter, importing, staged import, lost compaction, seal rebootstrap,
  restoration guard, dropped tip frames, pending frames (with a wait reason
  and count) and finalization waiting. `clears_by` is one of: any permitted
  peer, a named peer, local work, the application. `since` is the earliest
  stored time that explains it, or absent.
- **Wait reasons are values.** `apply_frame`'s `Waiting` carries a
  `WaitReason` (schema, direct import, opening key, read grant, own chain,
  dependencies, undecryptable). The reasons for pending heads are kept per
  replica process and recomputed by the pass that sees them; no format
  change.
- **Status shows blockers.** SDK `DataStatus` gains `blockers`, and
  `bootstrap_pending` stays as a summary. Kotlin status exposes the same
  list. Each peer's last sync result keeps per-scope outcomes, bytes and
  frames sent and received, and timeouts by kind.
- **Spans with fixed fields.** Add `tracing` as a workspace dependency of
  `rocky-secure`, `rocky-sdk`, `rocky-iroh` and `rocky-coordinator`. Spans
  wrap: sync pass, dial, admission, ready, session, request (channel,
  family, bytes, duration, outcome), page apply (frames, rows), checkpoint
  page, finalize, compact, maintain, coordinated append, acknowledge and
  resolve. Fields are short hashes, counts, durations and error categories;
  row values, keys, invitation codes and credential bytes are never fields.
  The library installs no subscriber.
- **Applications opt in to events.** The native binding gains an event
  listener with a level filter, delivering rendered events off the database
  thread through a bounded queue that drops and counts on overflow. The
  shopping app writes them to its existing rotated diagnostics file.

## Ordered slices

1. `Blocker`, `WaitReason` and `blockers()`; move every listed gate onto
   the shared predicates; refusals carry the list.
2. SDK and Kotlin status: `blockers`, per-peer sync outcomes and counters.
3. `tracing` spans and events in the listed places.
4. The native event listener and the shopping app's diagnostics sink.
5. Evidence in this directory.

## Completion gate

- For each blocked state reproduced by an existing regression from the
  2026-10-24 device testing (reset replay, import intent, line signers,
  unshared proof, unused partition, own-id preparation), `blockers()` names
  it with the right kind and `clears_by`, and the refused write lists it.
- No frame is pending without a wait reason after an apply pass.
- A test subscriber records one complete sync pass between two nodes as a
  span tree; a scan of every recorded field finds none of the test's row
  values, keys or invitation bytes.
- With no subscriber installed, the 100k member catch-up stays within 3% of
  its phase-start measurement.
- The shopping app's diagnostics file contains the events of one manual
  sync on desktop and Android.
