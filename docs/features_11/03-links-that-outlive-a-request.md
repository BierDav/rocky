# 3. Links that outlive a request

Status: implemented; qualification incomplete. See the
[evidence](03-links-that-outlive-a-request-evidence.md). Owner: `rocky-secure` link channels, network sessions and
serving; `rocky-sdk` sync, live links and retry scheduling. Depends on
nothing; its loss scenarios use [phase 2](02-deterministic-simulation.md)
when available. Effort: one to two weeks. Follow the
[execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read the [review](00-reliability-review.md), findings 2, 3, 7 and 14. On
devices, a request that timed out locally closed the link when its reply
arrived; a server error closed the link without telling the client; one
failing scope ended the sync for all scopes; a placement response
exceeded the message limit.

After this phase a request that fails costs that request, a scope that
fails costs that scope, and no response can exceed a message.

## Settled

- **Retired channels.** When a channel's `Io` drops or times out, its slot
  is retired for that generation. Later `Data` and `End` for a retired
  generation are discarded and counted, and the slot is removed on `End` or
  when the link ends. Delivery to a full queue and delivery to a closed
  receiver are distinct errors; only a full queue of a live receiver is a
  quota violation.
- **Refusal replies.** Every request family can be answered with
  `Refused { reason }`, a closed enum: not permitted, unavailable,
  over budget, malformed, wrong tip. `serve_all` answers refusals instead of
  returning `Err`; a session error never ends the link. A link ends only for
  framing violations, quota violations by a live receiver, expired
  credentials and transport loss.
- **Progress, not wall time.** A server preparing a response sends
  `Working` on its channel every 4 s; the requester's deadline renews on
  `Working` or data for that channel. A single request may last at most
  120 s. Idle links rely on QUIC keepalive.
- **Per-scope outcomes.** `sync.rs` collects an outcome per scope instead
  of using `?` across scopes. A failing scope is recorded in the peer's
  sync result and as a blocker with `clears_by` that peer or any permitted
  peer, then the pass continues. A bootstrap failure in one scope does not
  skip row reconciliation of the others. A scope this node imports still
  takes the checkpoint, not history, from that peer.
- **Pushes yield.** Over the pending-handler limit, pushes and signals are
  dropped and the peer marked pending instead of closing the link. Pushes
  do not wait on the publishing lock held while pages are built.
- **Byte budgets everywhere.** Tip pages, the handshake's `Statements` and
  `CandidatePush`, frontier batches, holdings `Prove` and existence pages
  are cut by encoded size against `MAX_SECURE_FRAME` minus a 4 KiB framing
  reserve, as range, entries, delivery, chains and placements already are.
  An item that cannot fit one message is refused when authored. The phase
  measures the largest acknowledgment with an eight-link chain and sets
  `MAX_TERMINAL_ACK_BYTES` so that `MAX_FINALITY_BYTES` fits; if it cannot,
  `MAX_FINALITY_ACKS` drops to what fits.
- **Backoff that spreads.** `Retry::failed` adds ±25% jitter from a
  per-node seed. Backoff resets only after a link survives 60 s or
  completes a sync pass, and failures are counted per peer across link
  incarnations.
- **Format.** The link moves one version for the refusal reply and
  `Working`. Checkpoint, row and replica formats are unchanged.
- **Dead code.** `pull` and `pull_page`, with their per-frame
  acknowledgment, are used only by tests; remove them and move those tests
  to `catch_up`.

## Ordered slices

1. Retired channel generations and distinct full and closed errors.
2. Refusal replies, `Working`, and link-ending rules.
3. Per-scope outcomes in `sync.rs` and the bootstrap path.
4. Push backpressure.
5. Byte budgets for every listed family; authoring refusal for oversized
   items; the finality bound.
6. Jittered backoff with the survival rule.
7. Evidence, including the three-device check.

## Completion gate

- A requester that times out, followed by the server's late reply, keeps
  the link open and completes its next request on it.
- A server that refuses one scope answers with a refusal; the same pass
  syncs the peer's other scopes; the link stays open.
- Each listed message family, filled to its item limit with maximal items,
  fits `MAX_SECURE_FRAME`; an oversized tip statement is refused at
  authoring.
- A burst of pushes over the handler limit leaves the link open and
  delivers every frame within the next pass.
- Two nodes failing against the same peer with the same seed spread their
  retries; a link that opens and fails within 60 s backs off.
- Simulation seeds with stalls and cuts on every link show no link closure
  attributed to a timeout or refusal.
- Three devices: 30 minutes of normal shopping use with one phone on Wi-Fi
  records no link closure from a timeout, refusal or queue quota.
