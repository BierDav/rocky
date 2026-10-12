# 2. Link session and signals

Status: implemented (2026-10-12). All five slices and every completion-gate
bullet are verified. Workspace lint, checks and test budgets pass.
Owner: `rocky-core` `net.rs` (protocol name); `rocky-secure` native session
(handshake, framing); `rocky-sdk` sync worker in `node.rs` and a new link
registry; `rocky-iroh` stream budgets. Depends on [phase 1](01-result-retry-defects.md).
Follow the [execution contract](README.md#agent-execution-contract). Takes
decision record 0063.

## Inputs and objective

Read [change-driven sync](../runtime/always-on-peer.md#change-driven-sync),
[secure sessions](../runtime/secure-sessions.md#peer-sessions-on-links),
[SDK transports](../runtime/sdk-transports.md) and
[decision 0051](../decisions/0051-roster-free-admission.md).

Today every pass dials each peer, authenticates, exchanges and closes the
stream. A commit wakes only the local worker, so a peer that cannot be dialed
learns of it only when it dials. This phase keeps one authenticated stream
per peer open and sends signals over it. Rows still move through the existing
pass, which a signal starts.

## Settled

- A new protocol `Link` with its own versioned name. The handshake is the
  native session's: credentials, the revocation exchange and admission.
  A link is admitted exactly when a session would be.
- **Framing.** Every message carries a channel number and a direction bit, so
  both ends can have requests in flight on one ordered stream. A bounded
  number of channels per link, each with bounded bytes in flight; a slow
  channel cannot block the ping.
- **Messages in this phase:** `Changed { scopes, token }`, `Ping`, `Pong` and
  `Close { reason }`. `scopes` lists only scopes the receiver proved it
  holds in this link's handshake ([holdings](../runtime/replica-roles.md#proving-a-partition)).
  The `token` is opaque and lets the receiver drop a repeated signal.
- **Sending.** After a commit that touches a scope the peer holds, the worker
  sends `Changed` on that peer's link. Signals are batched over the existing
  settle window. A peer with no open link is woken as it is today.
- **Receiving.** `Changed` marks that peer pending, resets its back-off and
  wakes the worker for a pass to that peer alone.
- **Liveness.** A ping every 15 s on an idle link; three missed pongs, or a
  transport error, close it. A closed link marks the peer disconnected, and
  the peer is dialed again with its back-off. The interval is a constant,
  not a setting.
- **Authority.** A change of the replica's authority closes every link with
  `Close { reason: authority }`; the next pass dials again.
- **Budget.** Links have their own inbound budget, separate from transient
  streams, so open links never starve `Pair`, `Renew` or `Command`. Over the
  budget a link is refused as busy, and the peer keeps per-pass sync for
  that pair until a slot frees.
- **Ownership.** Links belong to the database's network service. Closing the
  database or detaching closes them, never the application's endpoint.
  Offline mode and a disabled worker close them.

## Ordered slices

1. Wire format and decision record 0063: link handshake, framing, the four
   messages and the version name. Unit tests for framing bounds and refusal
   of any other version.
2. Link registry in `rocky-sdk`: one entry per peer, the tie-break for links
   dialed by both sides at once, and close on authority change, detach and offline.
3. Signals: send after commit, receive into the per-peer pending state.
4. Liveness and budgets, including `rocky-iroh` limits for long-lived streams.
5. Guides: change-driven sync, SDK transports and a new runtime guide for links.

## Completion gate

All on the memory hub under the paused clock, unless noted:

- After a first pass, a commit on A reaches B through a signal, and B's pass
  is the only new stream; A dials nothing.
- B cannot be dialed (stalled in the hub), and a commit on A still reaches B
  over the link B opened.
- Both ends dialing at once leave exactly one link.
- A peer that stops answering is closed after three missed pings and dialed
  again with back-off. An idle pair with an open link runs no timer but the ping.
- A learned revocation closes the revoked peer's link; a renewal closes and
  reopens every link.
- A full link budget still admits `Pair` and `Command` streams.
- The same checks on TCP and on iroh between two local endpoints.

## Verification evidence (2026-10-12)

Slice 1 is committed as `rwxxwmpo`: the versioned protocol, native admission,
mutual holding proofs, committed scope-state exchange and signed bounded frames.
The four `rocky-secure` `network::link` tests pass, including rejection before
reading an oversized body. `link_handshake_refuses_native_session_identity`
verifies refusal across the actual handshake.

Slices 2–4 are committed as `mymwttnk` and exercised by the 21 tests in
`rocky-sdk`'s `node::sync_tests::change_driven::live_links`. All 32 tests in the
owning `change_driven` module passed concurrently in 4.38 s after the scheduling
fix. The gate maps to:

- `commit_signals_over_one_link_and_only_receiver_dials`: after commit, exactly
  one new native pass from the receiver; no publisher dial.
- `reverse_opened_link_delivers_when_receiver_cannot_be_dialed`: a hub-stalled
  receiver still receives on the link it opened.
- `simultaneous_dials_keep_the_lower_nodes_stream`: one registry entry on each
  side, with the lower node's outgoing stream selected.
- `three_missing_pongs_close_and_reconnect_with_backoff`: missing pongs at
  15/30/45 s close at 60 s; no early dial, then reconnect at the 1 s back-off.
  The signal test also advances idle ping intervals with no new streams.
- `learned_revocation_closes_the_revoked_peers_link`: an injected, verified
  incoming statement ends the link without renewal or key installation.
  `authority_offline_disable_and_detach_end_owned_links` checks renewal of all
  links to two peers, worker disable, offline mode and detach.
- `saturated_link_budget_leaves_pair_and_command_admission_free` and
  `busy_links_keep_native_sync_and_reopen_when_a_slot_frees`: busy refusal,
  independent transient admission, continued native sync and eventual link retry.
- The corresponding `tcp_*_gate` and `iroh_*_gate` tests repeat signal,
  reverse-link, simultaneous-dial, liveness/idle, authority and budget checks.

Memory and liveness faults use the paused clock. Other TCP/iroh socket gates
use a real clock so QUIC retransmission and flow-control timers can progress;
there are no arbitrary sleeps. On QUIC, the budget test writes the first byte
of each transient stream before asserting inbound admission. Link handshakes
have a separate service budget; a cached iroh connection allows four bounded
streams so duplicate handshakes can reach the registry's refusal or tie-break.
Link retry deadlines are separate from data-pass deadlines. The first-pass
regression `a_retry_wakeup_still_initializes_an_unattempted_peer` covers a queued
commit wakeup racing worker enablement: it must initialize the peer's retry
state through a data pass rather than skipping straight to link attempts.
Before the fix, the stalled-bootstrap test waited on peers that had never
received a data pass; its paused clock correctly did not advance their dial
failures. Its assertions and the 2 s integration budget are unchanged.

Slice 5 updates [change-driven sync](../runtime/always-on-peer.md#change-driven-sync),
[SDK transports](../runtime/sdk-transports.md),
[secure sessions](../runtime/secure-sessions.md#peer-sessions-on-links) and the new
[live-link guide](../runtime/live-links.md). Decision
[0063](../decisions/0063-live-link-sessions.md) records the wire and ownership rules.

Internet traversal, phones, Kotlin readiness/lifecycle and data over the link
are outside phase 2 and were not verified here.

Final verification passed: `./scripts/lint`, `./scripts/check --unit`,
`./scripts/check` and `./scripts/commitlint --jj`. The final full check ran 150
unit test executions (127.9 ms) and 904 integration test executions (145.16 s):
690 workspace integration tests plus 214 SDK tests without default features.
Every test met its tier budget, including the live-link gates and the existing
result, bootstrap and checkpoint regressions. No assertions or budgets were
weakened. Slice 5 and the finished decision record are committed as `oluozspt`.
