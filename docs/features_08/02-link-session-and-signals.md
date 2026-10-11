# 2. Link session and signals

Status: not started.
Owner: `rocky-core` `net.rs` (protocol name); `rocky-secure` native session
(handshake, framing); `rocky-sdk` sync worker in `node.rs` and a new link
registry; `rocky-iroh` stream budgets. Depends on [phase 1](01-result-retry-defects.md).
Follow the [execution contract](README.md#agent-execution-contract). Takes
decision record 0063.

## Inputs and objective

Read [change-driven sync](../runtime/always-on-peer.md#change-driven-sync),
[secure sessions](../runtime/secure-sessions.md#native-peer-sessions),
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
