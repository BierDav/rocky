# 8. Pipelined sync

Status: implemented; performance and device qualification incomplete. See
[evidence](08-pipelined-sync-evidence.md). Owner: `rocky-secure` network sessions, rows, tips and
checkpoint transfer; `rocky-iroh` streams; `rocky-sdk` sync. Depends on
[phase 3](03-links-that-outlive-a-request.md) and
[phase 6](06-database-thread.md). Effort: two to three weeks. Follow the
[execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read the [review](00-reliability-review.md), finding 8. A range page costs
a request, the page, a separate acknowledgment round trip and its apply,
with nothing in flight meanwhile; scopes sync one at a time on one channel;
every session exchanges the whole tip graph when digests differ; a
checkpoint import keeps one page in flight; all channels share one QUIC
stream. An empty sync over relay took 10.43 s and the phone's 100k
bootstrap 215 s.

After this phase round trips overlap the work they wait for, a sync with
nothing to do costs a few round trips, and one lost packet stalls only its
stream.

## Settled

- **Windowed catch-up.** The `Held`/`Acknowledged` round trip goes: the
  next `Range` request carries the acknowledged frontier, and a final
  acknowledgment closes the scope. Up to two range requests are in flight
  per scope while a page applies.
- **Checkpoint pages in a window.** Up to four checkpoint pages are in
  flight, verified off the lock as today, and stored in order.
- **Scopes in parallel.** Up to four scopes sync at once over the request
  channels, keeping per-scope order and the session byte budget.
- **Tip graph differences.** A session exchanges the set of statement
  digests the other side lacks, found by comparing digests over sorted
  ranges, instead of the whole graph; scopes whose held-change token is
  unchanged skip held changes.
- **Handshake reuse.** Scope tokens computed for link states are reused
  for admission instead of computed twice.
- **A stream per channel.** The link opens one QUIC stream per request
  channel plus a control stream, so control and signals never queue behind
  bulk data and a lost packet stalls one stream.
- **Format.** The link moves one version.

## Ordered slices

1. Windowed catch-up.
2. Checkpoint page window.
3. Parallel scopes.
4. Tip graph differences, held-change skipping and token reuse.
5. Streams per channel.
6. Evidence, including relay and LAN measurements on devices.

## Completion gate

- The phone's 100k bootstrap is at or below 60 s on LAN.
- One-row sync between the phone and a desktop on LAN is at or below 1 s at
  p95 over 20 runs.
- An empty reconciliation over relay is at or below 2 s.
- With 100 ms simulated latency per hop, the 100k member catch-up takes no
  more than 1.5 times its zero-latency time.
- A sync with no changes and an unchanged tip graph sends no statement and
  no frame.
