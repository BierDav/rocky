# 3. Data over the link

Status: completed and verified (2026-10-12). All five slices and all five
completion gates pass. Phases 4–6 remain outside this work.
Owner: `rocky-secure` native session messages; `rocky-sdk` `sync.rs`,
`delivery.rs`, `mesh.rs` and the sync worker. Depends on
[phase 2](02-link-session-and-signals.md). Follow the
[execution contract](README.md#agent-execution-contract). Takes decision record 0064.

## Inputs and objective

Read the phase 2 decision record, [secure sessions](../runtime/secure-sessions.md),
[checkpoints](../runtime/checkpoints.md), [existence logs](../runtime/existence-logs.md),
[mergeable horizon](../runtime/mergeable-horizon.md) and
[encrypted commands](../runtime/encrypted-commands.md).

After phase 2, a signal still starts a pass that dials a new session. This
phase runs every exchange of that pass on the open link instead. A committed
frame is pushed to the peer as it commits, so a small edit costs one message
and no reconcile.

## Settled

- **Everything a session carries moves.** Changed-scope exchange, catch-up,
  push, checkpoint bootstrap, existence logs, horizon acknowledgments, result
  pulls and acknowledgments, envelope delivery and the contact exchange each
  run as a request on a link channel. Admission and bounds are unchanged:
  the same checks run on the same messages.
- **Live push.** After a commit, the frames of a scope the peer holds are
  sent on the link, up to the per-channel bound; this replaces `Changed`
  for those scopes. A receiver applies them as it applies a catch-up page.
  A frame it cannot apply yet (a missing dependency or chain) is stored
  as received is today, and a later catch-up on the link completes it.
- `Changed` remains for anything that is not a frame: results, horizon
  statements, revocations and envelopes. It starts a reconcile on the link.
- **Reconcile on connect.** A new link starts with the exchange a pass makes
  today, so a peer that was away catches up before live pushes.
- **Both directions at once.** Each end can pull and push concurrently on
  separate channels. A pass on the link and an incoming push for the same
  scope serialize at the replica, as two sessions do today.
- A dial fallback remains only for a peer whose link was refused as busy.
  Phase 4 removes it.

## Ordered slices

1. Decision record 0064: the request set on link channels and the live push
   message.
2. Move the reconcile in `sync.rs` onto a link handle: rows, existence,
   horizon, results, bootstrap.
3. Move envelope delivery and the contact exchange.
4. Live push of committed frames and its bounds.
5. Guides: links, secure sessions and change-driven sync.

## Completion gate

- One edit on A appears in B's projection with no new stream and without a
  reconcile on B, measured as message count on the memory hub.
- A burst larger than the channel bound arrives completely, partly by push
  and partly by catch-up, in commit order per scope.
- Bootstrap of a newly enrolled device over a link restores the same rows as
  the existing bootstrap tests.
- Revocation and key rotation tests that run across sessions today pass
  across links, including a revocation learned in the middle of a push.
- Two devices each writing to the same scope at the same time converge.

## Development evidence

Slice 1: decision [0064](../decisions/0064-sync-on-live-links.md) records inherited
admission, channel bounds and the self-contained live push.

Slices 2–3: the session factory opens a logical request channel on the link.
Rows, existence, horizons, results and checkpoint bootstrap retain their native
handlers; envelopes and contacts use the same factory. Busy refusal is typed;
other link errors do not silently fall back. Explicit sync uses a short link.
The large-row checkpoint regression `join_restores_large_rows_across_checkpoint_pages`
passed (1.61 s). The reverse-opened memory-link row regression passed (0.22 s).
Concurrent issuer renewal invalidates inherited channels; reconciliation retries
when authority or the selected link changed. The pre-handshake policy digest
preserves key renewal before rows under newly learned revocations. Retry drops
newly revoked targets and repeats coverage under the replacement authority.

Slice 4 implements bounded committed-frame push. The memory gates
`committed_edit_pushes_one_message_without_a_stream_or_receiver_reconcile`,
`a_burst_exceeds_push_bound_and_finishes_by_link_catch_up_in_log_order`,
`two_writers_push_concurrently_and_converge_on_the_same_scope` and
`revocation_learned_mid_push_cancels_the_partial_frame_and_its_authority` have
passed individually. The hub counts complete link frames across fragmented
writes. Incoming frames retain normal receive, dependency and transaction rules;
received author prefixes suppress echoes without hiding concurrent local writes.

Short-link readiness distinguishes a disabled worker from a persistent peer.
The worker does not repeatedly reopen a short link at idle. Reconcile markers
protect the gaps between request channels; completion waits for the final marker
to flush. A frame read survives completion of a concurrent handler. Channel
reassembly, queue, direction and generation checks have six passing native-link
checks, including a stale reconcile finish that cannot release a newer lease.
The eight representative cancellation offsets passed in 1.19 s, with
an explicit 100 ms close deadline under the paused clock. Completion racing the
requested byte cut is checked separately because link framing length can vary.
The exhaustive ignored historical helper was not run.

All nine SDK bootstrap regressions passed over link channels, including
checkpoint page restoration, interrupted scope cursors, authority intent and
missed-epoch renewal. The fault fixture pauses logical channel opens instead of
per-pass dials, preserving its assertions. Earlier failed full runs are superseded
by the final qualification below.


Slice 3 also moves explicit contact requests onto the shared link factory.
`a_contact_request_reuses_the_open_link` verifies zero native sync dials and no
new stream. All 16 delivery tests pass, including the nine crash cases, mailbox
bounds and recipient privacy. An admitted envelope batch completes local key
installation after its link ends; a lost receipt remains durable retry work.

The two-worker regression exposed a reader deadlock: Changed token checks could
wait for a replica lock while the same reader stopped polling its owner. They
now run beside request handlers. The unchanged-epoch regression prevents contact
metadata from invalidating admission. A verified renewal releases only its
answering issuer's backoff, tested under the paused clock without advancing it.
Removing a peer also forgets its met history and closes its current link.

Slice 5 updates live links, secure sessions, change-driven sync, transport
measurement and module ownership. Decision 0064 and the decision index describe
the implemented request lifecycle.

## Final qualification (2026-10-12)

All completion gates passed on the final implementation:

- One edit: `committed_edit_pushes_one_message_without_a_stream_or_receiver_reconcile`
  asserts one complete hub message, unchanged stream counts and no receiver
  reconciliation traffic.
- Overflow: `a_burst_exceeds_push_bound_and_finishes_by_link_catch_up_in_log_order`
  applies a bounded prefix of 40 commits, then catches up completely on the same
  link, checking contiguous applied author order and equal projections.
- Bootstrap: all nine SDK bootstrap regressions pass, including large checkpoint
  pages, interrupted scope cursors, authority intent and missed epochs.
- Authority: SDK revocation, registry revocation and concurrent rotation tests
  pass over links; `revocation_learned_mid_push_cancels_the_partial_frame_and_its_authority`
  retains the local commit, applies no partial remote frame and refuses revoked
  admission after the 100 ms close deadline.
- Concurrent writers: `two_writers_push_concurrently_and_converge_on_the_same_scope`
  checks equal projections and author frontiers with no new stream. The existing
  two-worker test also converges and stops dialing for a paused-clock hour.

`./scripts/lint` passes. `./scripts/check --unit` passes 137 workspace unit tests.
The full `./scripts/check` passes Clippy, 153 unit tests and 916 integration tests:
696 workspace cases and 220 SDK cases without default features. Integration
wall time is 150.25 s against the unchanged 300 s budget. The run used
`NEXTEST_TEST_THREADS=6` because other agents shared the host (load about 19 on
18 cores); no budget or assertion was relaxed. The four link gates and the
representative cancellation offsets pass in both builds. Separate targeted runs
pass all 16 delivery cases, 43 delivery and live-link cases together, the 19
bootstrap, revocation, registry revocation and concurrent rotation cases, and
seven native-link checks in `rocky-secure`.

The host volume was nearly full, so only this workspace's rebuildable
incremental cache was removed before the final run. The historical ignored
exhaustive cancellation helper and the complete e2e tier were not run.
Internet, mobile and phases 4–6 are not claimed by this qualification.
