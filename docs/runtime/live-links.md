# Live links

Links carry every synchronization request and committed-frame push; no other
sync path exists. The [phase 3 brief](../features_08/03-data-over-the-link.md)
records qualification, the [phase 4 brief](../features_08/04-remove-per-pass-sessions.md)
the removal of per-pass sessions; [decision 0064](../decisions/0064-sync-on-live-links.md)
defines the request lifecycle. Pair, Renew and Command retain their own streams.

## Authentication and ownership

`rocky/link/2` reuses credential admission, revocation exchange and
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
keep a link. Explicit sync reuses the open link or opens one. When both workers
run, the link remains open; with either worker disabled it is a short link
using the same protocol and handlers. Contact handover retains at most 16 recently successful peers for later links;
an earlier pass does not consume those hints. Short links close after reconciliation;
they do not restart continuously at idle. An automatic worker reconciles after
an incoming short exchange finishes.

An accepted link stream whose first frame is a pinned contact reference's
resolution gets the signed proof of the node key and closes without a link.
`Link::drive` carries request channels for an embedder without the SDK worker,
such as the employees demo; it ignores signals and pushes.

One registry entry exists per authenticated node. Simultaneous dials retain
the stream initiated by the lower node key. Replacement cancels old requests;
a bounded retry uses the surviving link without classifying it as unreachable.
A requester uses the newly admitted link immediately, including a competing
inbound short link, instead of waiting for that peer's reconciliation to end.
Stream I/O failures and link channel close/timeout errors retain ordinary
retry/backoff; they do not trigger credential renewal, which would cancel
unrelated active links. Committed publication processes peers concurrently in
bounded batches and finishes healthy peers even when another publication fails.

## Status and lifecycle

Network status and readiness expose each peer's `link`: `state` is `open`,
`opening` during a dial/admission, or `closed`; `opened` is Unix seconds and
`round_trip_ms` the selected path's round trip, both decimal strings or null.
`paths` lists the transport's network paths, the selected one first, measured
when status is read:

| Field | Meaning |
| --- | --- |
| `selected` | Whether data uses this path now |
| `kind` | `direct` (IP) or `relay` |
| `remote`, `local` | The peer's socket address or relay URL; this node's address or null |
| `rtt_us` | Smoothed round trip in microseconds |
| `mtu`, `window` | Current MTU and congestion window, in bytes |
| `sent_bytes`, `sent_datagrams`, `received_bytes`, `received_datagrams` | Path lifetime totals |
| `lost_packets`, `lost_bytes`, `congestion_events` | Path lifetime totals |

Integers are decimal strings. Closed and opening links, and transports that
measure nothing, such as the test hub, have an empty list. Cancelling a dial
clears `opening`. Opening and closing wake the existing sync-status and
readiness waits. Measurements do not advance tokens; poll status to follow
them.
`link_opened` and `link_closed` activity includes the peer and reason; an error
close also carries the error. Duplicate replacement closes the displaced link.

Disabling the worker waits for its pass and links to drain. Android's
`enterBackground` uses that operation and blocks further inbound or outbound
link admission; foreground allows links and enables the worker again.
The file, sessions and application-owned transport stay open. Desktop workers
keep links open while online. See [Android](android.md#lifecycle).

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
| Outgoing data queue | 64 fragments or pushes |
| Reconcile leases | One per direction |
| Changed scopes / pushed frames | 64 / 32 per message, within the byte bound |
| Keep-alive / idle close | iroh's: 5 seconds / 30 seconds ([liveness](sdk-transports.md#liveness-and-paths)) |
| Started frame, request wait or write | 10 seconds |

A queue, reassembly or byte overflow fails explicitly. A stalled write ends
after ten seconds. An idle read has no deadline, but a started frame has ten
seconds. Rocky sends no keep-alive: the transport keeps an idle link's
connection open and closes one whose peer stopped answering, which fails the
link. Errors, a busy refusal included, reconnect with per-peer back-off;
nothing falls back to another sync path.

The iroh link ALPN keeps its independent connection and stream budgets. See
[SDK transports](sdk-transports.md#bounds). The memory hub counts complete
link messages and injects byte cuts, severed links and stalled dials under the
paused clock. Android lifecycle evidence is in the [phase 5 brief](../features_08/05-status-kotlin-lifecycle.md);
desktop and phone runs over cellular and an iroh relay are in the
[phase 6 evidence](../features_08/06-qualification-evidence.md).

Short-link cleanup names the admitted link identity. When simultaneous dials
replace a stream, a failed reconciliation on the losing stream cannot close
the replacement. Both peers can retry on the surviving link. Stream interruptions retry within
the existing four-attempt reconciliation bound, including when a remote
replacement is not yet visible in the local registry.

The automatic worker checks queued explicit work before each pass, even when
a revision or link event won its previous wait. A key share cannot lose its
wake-up to an unrelated notification on an already open link.
