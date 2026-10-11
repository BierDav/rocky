# 0064: Sync on live links

Status: accepted design; implementation and qualification pending.
Date: 2026-10-12.

## Decision

The authenticated link carries the native request families on bounded logical
channels: holdings and changed frontiers, rows and catch-up, checkpoints and
existence bases, existence logs, horizon statements and acknowledgments, result
pulls and acknowledgments, envelope delivery and contacts. They reuse the native
request handlers, admission checks, page limits and transaction boundaries.
Each channel inherits the link's admitted identity and fresh handshake binding;
its request direction and generation also bind its signed messages. It does not
dial or authenticate another transport stream.

Control and Changed retain channels 0 and 1. Four request slots per direction
use channels 2–5. A monotonically increasing generation identifies each use of
a slot; a slot remains occupied until both ends close. Channel data fragments
are bounded by the signed link frame limit. Native message and exchange byte
limits remain unchanged. Channel queues and reassembly are bounded; exceeding a
bound fails explicitly. Control is scheduled before data.

A committed frame push is distinct from reconciliation. It names one proved
scope and carries self-contained signed frames with their credential chains,
within the link frame bound. The receiver uses the catch-up page's admission and
transactional receive path. Dependencies may remain pending. Overflow schedules
Changed and bounded catch-up on the link, preserving author order per scope.
Non-frame changes still schedule reconciliation. Nothing leaves before flush.

A new link reconciles in both directions. Authority changes cancel channels
and invalidate the inherited admission. Renewal and Command retain their own
protocols. The native Sync path remains only for busy refusal until phase 4;
this phase does not remove its protocol or server.

## Evidence

Qualification is tracked in the [phase 3 brief](../features_08/03-data-over-the-link.md).
No completion gate is claimed by this design record.
