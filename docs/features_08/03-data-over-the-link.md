# 3. Data over the link

Status: not started.
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
