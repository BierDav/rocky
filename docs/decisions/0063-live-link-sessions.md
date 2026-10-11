# 0063: Live link sessions and signals

Status: accepted.
Date: 2026-10-12.

## Decision

`rocky/link/1` is a distinct protocol. Its hello uses identity 65537 and
signs under `rocky.link.v1`, preventing confusion with native sync sessions.
It reuses credential admission, mutual challenge signatures, the revocation
exchange and session-bound holding proofs. Both ends exchange holdings in
handshake order before sending signals, including granted whole scopes that
hold no rows yet. They then exchange a committed state token for each proved
scope, so a write between reconciliation and link registration is signalled.
No other version is accepted.

Every subsequent signed frame carries the request channel and direction.
Channel 0 carries Ping, Pong and Close; channel 1 carries Changed. The direction
is false for requests started by the dialer, true for the acceptor. Pong echoes
the ping's direction. Each frame is at most 64 KiB and a Changed names at most
64 distinct scopes. Control and signals have independent bounded queues;
control is selected first. A blocked transport write has a ten-second deadline.
Idle reads have no deadline; a started frame has ten seconds to finish.

One registry entry exists per authenticated node. A simultaneous dial keeps
the stream initiated by the lower node key. Replacement closes the loser
without recording a reachability error. Registry entries belong to a network
service, which also owns a separate inbound link budget.

Signals name only handshake-proved holdings. The worker flushes committed
state before publication and compares scope frontiers to coalesce changes.
Tokens are opaque commitments to receipt frontiers, existence, result heads and
horizons; duplicate signals are ignored. Taking a pending batch clears it so
later changed scopes cannot be hidden by an earlier batch.
Receiving a new token makes only that peer pending and resets its back-off.
Data continues through native sync sessions during this phase.

Idle links ping every 15 seconds. Three consecutive unanswered pings end the
link and schedule a reconnect through per-peer back-off. Link retry deadlines
are independent of data-pass retries, so busy links retain working native sync.
Authority changes, offline mode, disabling the worker and detach close the
service's links.
Borrowed application transports and endpoints remain owned by the application.

## Evidence

See the [phase 2 brief](../features_08/02-link-session-and-signals.md) for slice
status and completion-gate evidence. No subsequent roadmap phase is included.
