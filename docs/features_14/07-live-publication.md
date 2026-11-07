# 7. Bounded live publication

Status: proposed (2026-11-07); investigation complete, runtime changes not
implemented. Independent of phase 6; integrate against the database-bound
membership and admission behavior of phases 3 and 5a. Source findings and
external measurements are in [the investigation evidence](07-live-publication-evidence.md).

## Problem and decision

A small application transaction can produce several encrypted frames across
several scopes. Direct pushes repeat the full credential chain per frame,
size their input by storage weight, and fall back at the signed link message's
64 KiB limit. An early frame waiting for its siblings also schedules a full
peer reconciliation. That reconciliation opens a session per held scope and
repeats discovery before finding which scopes changed.

Use **one bounded publication over an existing authenticated request channel**
for the peer's committed changes. Reuse native chain and escrow transfer,
fragmentation, authorization and receive admission. Give the publication an
explicit completion exchange. Repair remaining dependencies after that
exchange, with the affected scopes preserved through the scheduler.

This lifts the direct path's dependence on a single 64 KiB message without
raising the link allocation limit. Native channels already fragment logical
messages into 8 KiB pieces and reassemble messages up to 256 KiB. The values
serve different purposes; neither is a network MTU or an application
transaction size. Rename/document their roles when changing the code. Remove
the `FRAME_BYTES / 8` storage-to-wire guess from publication planning.

Do not globally change `EncryptedFrame::weight()`: its exclusion of shared
credentials is useful for storage and existing range accounting. Transport
planning must count the actual encoded messages, including credentials,
escrow, migration proofs, signatures and framing.

For frames sharing a chain of `C` bytes, today's credential cost is `n * C`;
carrying it once costs `C`. This removes repeated bytes before any compression
or packet-size tuning. It does not remove the cost of that first chain.

## Publication contract

1. **Capture and preflight.** Capture the changed scopes and per-log upper
   positions before export. Export only durable, authorized records up to
   those positions. Build the complete bounded publication before sending
   its data; size the encoded signed messages, with worst-case sequence
   encoding. Keep voids before the frames whose positions they consume.
   A compacted cursor, incomplete range page, individually overfull message
   or over-budget publication produces scoped repair work. Send none of
   that publication's data in these cases. A commit racing export stays
   pending for the next publication.
2. **Send without a discovery round trip.** Open a request channel using the
   live link's existing admission. Send the known-cursor deltas immediately;
   do not call the SDK's general `sync::session`, which refreshes discovery,
   or `NativeSession::push`, which first asks for the peer's frontier.
   Share each distinct credential chain once within the publication using
   the existing digest mechanism. Carry and keep the corresponding root
   escrow records through the existing verified path. Avoid a second chain
   dictionary or a permanently growing link cache.
3. **Send related scopes together.** A publication may cover several scopes;
   ending a scope page does not end the publication. Preserve runtime bundle
   admission and atomic projection/receipt/revision behavior. Feed bounded
   pages through the existing receive path; transport chunk boundaries do
   not become application commit boundaries.
4. **Complete after intake.** Send a terminal publication request after the
   last data page on the same ordered request channel. The receiver handles
   it only after all preceding pages have completed durable intake and the
   runtime has settled their ready dependencies. Reinspect the final
   frontiers and unresolved dependency/key state for the publication;
   neither an earlier `PendingDependencies` result nor a later successful
   frame alone decides completeness. Return one bounded summary with held
   and applied positions and remaining repair scopes. Do not add a
   round trip before applying or notifying observers of ready transactions.
5. **Separate delivery from repair.** Replace the push boolean with an
   explicit outcome: published positions, repair required with scopes and
   reason, or failure. Advance the sender's cursor only over exported,
   durably acknowledged positions. Held receipt is distinct from applied
   receipt and custody. Failed or cancelled work stays eligible for retry.
   Keep receive ownership and the link generation guard through receipt;
   completion on an old stream must not update its replacement's cursors.

Native `RowMessage::Push` currently replies per page. A publication mode
therefore needs a deliberate protocol change: carry pages without waiting
for each page's reply, then answer the terminal request once. Simply calling
the existing push method in a loop would add a network round trip per page
and would not solve the scheduler problem. Retire the separate direct-push
encoding when this mode replaces it. Bump the authenticated link version
from the then-current value and reject other versions; no compatibility
reader or persisted-format change is needed for transient publication state.

Opening a channel and returning completion adds fixed framing traffic to the
current single-message push for tiny schemas. Measure that case explicitly;
preserve immediate one-way application and notification, independent of the
receipt round trip. Do not claim fewer messages for every workload. If this
overhead matters, an inline form of the same publication representation can
be an optimization after the common admission and completion logic is proven.
Keep bulk publication from monopolizing control or all request lanes.

### Initial bounds and recovery

Start conservatively with one active publication per peer, at most 64 proved
scopes, and at most 256 KiB of signed native messages in the whole publication,
including its unique credentials and escrow. Keep the existing 32-item page
bound, 128 KiB native range budget, 8 KiB channel fragments and 64 KiB signed
link-message maximum. These are initial engineering bounds, not measured
optima; qualify them before tuning. Bound queued bytes as well as item counts.
The 32 MiB session ceiling is not a sensible per-peer publication allocation.

The receiver must reject a declaration over its publication budget before
retaining the body, and account for actual received and decoded metadata.
The terminal request must identify the expected scope positions and data
count; reject missing, duplicate, out-of-order or conflicting publication
parts. Scope and chain references are not authority. Recheck current
membership, revocation, tip and read permissions through existing admission.

Use the existing request progress and absolute lifetime deadlines. A missing
terminal request, handler overflow, cancellation, EOF or stalled sender ends
the publication and preserves repair work. Trickle traffic must not extend
the absolute lifetime. Do not suppress all `PendingDependencies`, use an
arbitrary grace sleep, or infer completion from an empty in-flight queue.
Reliable transport loss is retransmitted by the transport; an interrupted
application publication recovers through the ordinary scheduler and reconnect.

A frame exceeding the direct 64 KiB envelope may fit a native channel message.
A frame exceeding native range/message limits still needs the existing repair
or an explicit quota error. Audit authored, range and signed-message limits
together before claiming support for larger individual operations; raising
only the direct limit proves nothing about their recoverability.

## Scoped reconciliation

Replace peer-only pending work with coalesced `All` or a bounded set of scopes
and reasons. Changed hints already carry scopes; retain them. Add unresolved
bundle/dependency scopes using the runtime's existing dependency and frontier
grouping rules, restricted to authorized holdings. Unknown requirements,
set overflow, reconnect, explicit full sync and control-plane changes can
widen work to `All`. Metadata changes can affect scopes beyond the hint.

Take a work generation at the start of a pass. A completion clears only that
generation's work, never a newer hint, a different publication's recovery or
an independent full sync. Keep failed scopes and use the existing backoff.

For a reconciliation pass:

- Exchange revocations, deliveries, discovery and the tip graph once at the
  appropriate peer/control generation, with authority checks still enforced
  on every request. A revocation learned during the pass invalidates old work.
- Use the existing `changed_scopes`/`frontier_batches` planning across the
  selected authorized scopes. The planner already supports batches of up to
  64 scopes and partition-group expansion; preserve its proofs and byte bounds.
- Schedule row-transfer channels only for scopes requiring data or unfinished
  imports. Keep bounded parallel transfer so one stalled scope does not block
  the rest. Empty equality checks should not each run contact discovery.

This also improves real recovery; the performance fix must not depend on
reconciliation becoming unreachable.

## Implementation order and acceptance

Land publication planning/metadata reuse, completion semantics and scheduler
handling together before describing the live refresh problem as fixed.
Batch the recovery planner in a separate change if needed. A temporary
wire-compatible splitter can mitigate packet rejection, but needs the
completion/recovery change before it satisfies this phase.

Correctness tests use deterministic gates, cancellation, fault injection and
paused deadlines. Keep durations diagnostic. Cover:

- A production-shaped schema with 32 held scopes and large credentials;
  ordinary atomic contact/receipt/version writes and repeated burst edits.
  After idle links, a complete in-budget publication causes no full peer
  reconciliation, no discovery exchange and no extra dial. At notification,
  a snapshot contains the associated receipt/version and complete transaction.
- Actual signed byte boundaries, worst-case sequence widths, zero and many
  voids, multiple chains, migration proofs and escrow. One copy of each chain
  per publication; no data sent when preflight rejects the publication.
- A bundle split between pages and scopes. Hold the last sibling explicitly:
  no repair before the terminal barrier; all siblings arriving means no repair.
  Hold an unrelated missing dependency: the final summary still requests it.
- A missing key, withheld chain, absent sibling, missing terminal request,
  saturated handler, sender disappearance, cancellation and reconnect.
  Recovery converges or surfaces a specific error and does not leave stale data.
- A frame larger than 64 KiB that fits native limits, and an individual item
  beyond the supported limit. Exercise the actual fallback, not just its return
  value. Check restricted peers, mid-publication revocation and a root-escrow
  backup/recovery using changes delivered only through live publication.
- Hints racing completion, a new commit during export, overlapping authors,
  scopes beyond one planning batch and unfinished imports. No newer dirty work
  is cleared, no cursor skips data and unrelated held scopes need no row session.

Run the disposable Paperless diagnostic against the new dependency after the
Rocky regressions pass. Measure source commit return → receiver notification
→ snapshot, complete signed bytes, protocol messages by family, sessions,
repair reasons and queue residence. Include a real relay run and native/wasm
targets. Injected delay per write is useful fault pressure, not calibrated RTT.
No commit-to-render claim follows from reconciliation-start timestamps.

## Research worth applying

- [COPS, SOSP 2011](https://www.cs.cmu.edu/~dga/papers/cops-sosp2011.pdf),
  especially sections 4.2–4.4: asynchronously deliver updates and make them
  visible when their dependencies are satisfied. Apply the dependency
  discipline to publication completion; retain Rocky's own replay and bundle
  semantics. COPS is not a proof of this proposed protocol's recovery.
- [Range-Based Set Reconciliation, Meyer, 2023 draft](https://arxiv.org/abs/2212.13567):
  compare range fingerprints and descend into differences. First use Rocky's
  existing batched exact frontiers. Investigate range reconciliation only if
  large authorized scope/metadata inventories dominate afterward. The paper
  explicitly warns about adversarial fingerprints; summaries are not proofs
  of authority or a substitute for exact final validation.
- [Practical Rateless Set Reconciliation, SIGCOMM 2024](https://arxiv.org/abs/2402.02668):
  a promising experiment for large sparse differences in metadata/artifact
  inventories when their size is unknown. Contiguous author-log frontiers
  already describe ordinary row deltas efficiently; do not replace them just
  to fix this bug. Bound decoder work and verify received artifacts normally.
- [TLS Certificate Compression, RFC 8879](https://www.rfc-editor.org/rfc/rfc8879):
  measure compressing a unique credential chain after eliminating repetition.
  Bound decompressed size before allocation and preserve the digest of the
  canonical uncompressed bytes. Compression of encrypted row bodies is
  unlikely to help. Add a codec only if native and wasm measurements justify it.
- [QUIC, RFC 9000](https://www.rfc-editor.org/rfc/rfc9000), sections 2 and 14:
  streams and network datagrams are different layers. Let the transport own
  MTU and congestion control; keep Rocky's message and queue bounds explicit.

After the initial fix, measure sharing known chains across publications.
That could remove most credential bytes from warm edits, beyond sharing
within a batch. Use a bounded dictionary with explicit reset/eviction semantics
and fresh authority checks. Keeping a native channel open indefinitely is
not a free shortcut: current link drain waits for request slots to close,
and native sessions have a finite byte budget. Include those lifetimes in
any follow-up design.
