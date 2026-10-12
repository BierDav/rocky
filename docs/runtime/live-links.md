# Live links

Links carry synchronization requests and committed-frame pushes. The
[phase 3 brief](../features_08/03-data-over-the-link.md) records qualification;
[decision 0064](../decisions/0064-sync-on-live-links.md) defines the request
lifecycle. Native per-pass sync remains only for a typed busy refusal until
phase 4. Pair, Renew and Command retain their own streams.

## Authentication and ownership

`rocky/link/1` reuses credential admission, revocation exchange and
session-bound holding proofs, with a distinct signed handshake identity.
Other versions are refused. Every data request retains the native authorization,
page, chain, dependency and transaction checks. A changed authority ends the link.
A handshake that learns authority changes sends a signed authority close and
restarts; it cannot continue with its old proof context. Reconciliation retains
the pre-handshake policy digest so newly learned revocations require key renewal
before row transfer. Coverage from the prior policy does not skip that retry. An admitted envelope
batch finishes local key installation even when an epoch change cancels its link;
a lost receipt leaves the sender's durable delivery available for retry.

Proved holdings and advertised state agree exactly. A proof of a peer's
partition does not subscribe a reader to it. Unknown partitions are registered
only by authorized keyless custody; readers install them through envelopes.

The database's network service owns links. Detach, database close, offline mode
and disabling the worker close persistent links. Borrowed application transports,
endpoints and routers remain open. Both workers must request persistence to
keep a link. Explicit sync, or a peer with its worker disabled, uses a short
link with the same protocol and handlers. Short links close after reconciliation;
they do not restart continuously at idle. An automatic worker reconciles after
an incoming short exchange finishes.

One registry entry exists per authenticated node. Simultaneous dials retain
the stream initiated by the lower node key. Replacement cancels old requests;
a bounded retry uses the surviving link without classifying it as unreachable.

## Requests and live push

Connecting starts reconciliation. Rows, existence logs and bases, checkpoints,
horizons, results and their acknowledgments, envelopes and contact exchange, including requests for an unknown peer route,
use logical request channels. Both directions can request concurrently. Each
channel inherits the fresh link admission and holding binding and has its own
signed sequence, chain table and byte budget. Direction, slot and generation
bind its messages. A slot is reused only after both ends finish.

Reconcile start/end markers cover the gaps between channels, including an
external coordinator lookup. A short link waits for both directions to finish;
its generation-bound final marker must flush before closing. A stale finish cannot
release a newer reconcile. Completing a handler never cancels a partly read signed
frame. Changed token checks run beside request handlers, so a handler awaiting
storage keeps progressing. Control remains independent of request handling.

After commit and flush, the publisher exports one bounded page after its last
committed scope cursor. A push carries self-contained frames and their chains
for a proved scope. The receiver uses transactional `receive_all`, as catch-up
does. Missing dependencies stay pending and request catch-up. Receipt prefixes
suppress echoes without hiding concurrent local writes.

A small edit needs one message, with no dial or reconcile at the receiver.
An overflowing page, or a frame too large for push, sends `Changed`; catch-up
uses the existing link. Non-frame changes also reconcile over it. Signals carry
at most 64 proved scopes and an opaque state token; duplicates are ignored.
Push does not replace durable custody acknowledgments: frontiers exchanged by
reconciliation still govern release of retained operations.

## Liveness and bounds

| Resource | Bound |
| --- | --- |
| Inbound links per service | 32, separate from transient streams |
| Signed link frame | 64 KiB |
| Control / signals and pushes | Channels 0 / 1 |
| Request slots | Channels 2–5, four per direction |
| Channel data fragment | 8 KiB |
| Native message / exchange | 256 KiB / 32 MiB, unchanged |
| Completed-message queue per slot | 64 messages |
| Outgoing data queue / control replies | 64 fragments or pushes / 4 replies |
| Reconcile leases | One per direction |
| Changed scopes / pushed frames | 64 / 32 per message, within the byte bound |
| Idle ping / missed pong limit | 15 seconds / 3 |
| Started frame, request wait or write | 10 seconds |

A queue, reassembly or byte overflow fails explicitly. Control is selected
before data; a stalled write ends before the next ping interval. An idle read
has no deadline, but a started frame has ten seconds. After unanswered pings
at 15, 30 and 45 seconds, the third miss closes at 60 seconds. Errors reconnect
with per-peer back-off. Busy refusal alone permits a native data pass.

The iroh link ALPN keeps its independent connection and stream budgets. See
[SDK transports](sdk-transports.md#bounds). The memory hub counts complete
link messages and injects byte cuts, write muting and stalled dials under the
paused clock. Internet traversal and mobile lifecycle belong to later phases.
