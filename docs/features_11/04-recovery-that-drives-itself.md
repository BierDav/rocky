# 4. Recovery that drives itself

Status: proposed. Owner: `rocky-sdk` node scheduling, sync and bootstrap;
`rocky-secure` apply, rebootstrap, restoration and maintenance. Depends on
[phase 1](01-see-every-stall.md) and [phase 2](02-deterministic-simulation.md).
Effort: two to three weeks. Follow the
[execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read the [review](00-reliability-review.md), findings 3, 4 and 11. A sync
that succeeds clears the peer's retry while blockers remain; local recovery
runs only inside an admitted peer session; one failing scope or frame stops
unrelated work; some guards have no clearing path. On devices this left a
Full phone unwritable for more than eight hours and delivered frames
pending until a restart.

After this phase every blocker has a driver that runs until it clears or
becomes a terminal status, and the simulation's healing check passes for
every seed.

## Settled

- **A retry is owed while blocked.** An exchange reports a retry when a
  blocker that any permitted peer or this peer clears remains for a scope
  the peer may serve, or bootstrap found no peer on the followed tip. The
  existing backoff, with phase 3's jitter, spaces retries. Background
  restrictions on mobile platforms stay.
- **Local drivers.** `recover_locally` runs local frame replay,
  restoration checks, `finish_seal_rebootstrap` and pending application.
  It runs at open, after a tip graph change, after a commit that touches a
  blocked scope and every 30 s while a blocker that local work clears
  remains. It is never called inside a network call or a peer session's
  transaction.
- **Per-scope recovery.** `rebootstrap_at_seals` and bootstrap continue
  past a failing scope. Failures are counted durably per scope and
  checkpoint; after three, staging for that checkpoint is cancelled and the
  scope falls back to frames, as today, but surviving restart.
  `maintain` and `install_pulled` never clear another import's staging.
- **Quarantine instead of a stopped pass.** A frame whose apply fails with
  an error that is neither retryable nor a semantic refusal is rolled back
  to its savepoint and recorded in `secure_quarantine` (hash, reason,
  attempts, first and last seen). Its log is skipped for the rest of the
  pass; other logs continue. It is retried when keys, grants, schemas or
  the tip graph change, and otherwise with backoff. A frame that fails to
  decrypt under the key generation its header names is quarantined as
  undecryptable, not left waiting and not rejected. Quarantine appears as
  a blocker; it never admits or deletes a frame.
- **Guards with an exit.** `secure_dropped_tip_frames` entries clear when a
  verified, listed base at a seal covers them. A scope whose only path is
  `Unavailable` asks trusted peers that hold an unlisted base for it to
  list it; until one does, the blocker reads "no peer can serve".
- **Peer-dependent guards are justified.** The first slice lists every
  guard that waits for a specific peer (the inviter wait, restoration,
  replay from a non-pruning peer) with the reason that peer is required.
  Where safety does not need that specific peer, any peer permitted to
  serve the scope on the followed tip satisfies it.
- **Bounded maintenance.** `maintain` deduplicates by domain, finalizes at
  most one seal per call, backs off after an error and removes stale
  `secure_pulled` rows.
- **Format.** The replica moves one version for `secure_quarantine` and the
  durable import failure counts; the link moves one version if the listing
  request needs a new message.

## Ordered slices

1. Inventory of peer-dependent guards with their justification, reviewed
   before code changes.
2. Owed retries and the exchange result.
3. `recover_locally` and its triggers.
4. Per-scope rebootstrap and bootstrap, durable failure counts, staging
   ownership.
5. Quarantine and the undecryptable classification.
6. Exits for dropped tip frames and unavailable scopes.
7. Bounded `maintain`.
8. Evidence, including the three-device check.

## Completion gate

- Each device-testing blocked state has a regression that clears it with
  no new incoming frame, no user action and no restart, once a permitted
  peer is reachable or local work suffices.
- Two delivered frames whose keys arrive later apply in the same session.
- A quarantined frame in one log does not delay a frame in another log of
  the same group; the quarantine clears when its cause changes.
- Every simulation seed in the integration set and the e2e sweep passes the
  healing check; the known liveness seeds from phase 2 now pass.
- Three devices: a Full phone joining a household with seals, a branch on
  another desktop and an unused partition becomes writable within two
  minutes of foreground time with one reachable trusted peer, and its new
  list reaches both desktops on the same tip.
