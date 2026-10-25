# 6. A database thread that never blocks the link

Status: proposed. Owner: `rocky-sdk` database start-up and live links;
`rocky-secure` group commit, flushing, receive and apply; `rocky-credentials`
chain verification; `rocky-storage` cursors. Depends on
[phase 3](03-links-that-outlive-a-request.md). Effort: two to three weeks.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read the [review](00-reliability-review.md), findings 1, 5 and 9. One
thread runs storage, crypto, fsync and the link; a page apply or a seal
finalization starves the link and its peer's deadlines expire. A frame is
decrypted four or five times and its chain verified at least twice under
the replica lock.

Start with a sampled CPU profile of the 100k member catch-up, the phone's
100k bootstrap and the issuer's fill, recorded in evidence. After this
phase the link keeps running while the replica works, each frame is
opened and verified once, and reads do not queue behind the writer.

## Settled

- **The link leaves the database thread.** Link readers, writers, channel
  plumbing and transport tasks run on the transport runtime. They reach
  the replica through bounded channels; the replica's work stays on the
  database thread. The single writer and its commit order are unchanged.
- **Fsync leaves the lock.** Group flushes and every `inner.flush()` run
  on blocking threads with the replica lock released, as
  `EncryptedReplica::flush` already does. A reply that may leave the device
  still follows the flush that makes it durable.
- **Open once.** `apply_frame` passes its decoded payload to `materialize`
  and `write_materialized`; `dependencies_ready` hands on what it opened.
  Cheap waiting checks (direct import, opening key, dependencies) run before
  authority verification.
- **Verify once.** A per-replica cache holds verified chains keyed by
  chain digest, publisher and revocation-set digest, with their leaf,
  grants and validity window; time and current grants are checked per
  frame. Revoked references are indexed instead of scanned per link. The
  duplicate chain checks in tip and manifest verification go.
- **Pre-open outside the lock.** Signature checks and decryption that do
  not depend on stored state run on blocking threads before the group
  takes the lock, as checkpoint pages already do.
- **A read pool.** Secure cursors take connections from a bounded pool of
  read-only connections instead of a new thread and connection each, and
  readiness for a read is answered from a snapshot taken without the
  writer lock, consistent with the authority it checks.

## Ordered slices

1. Profiles and their evidence.
2. Link tasks on the transport runtime.
3. Fsync off the lock and off the database thread.
4. Single open and single verification, with the chain cache.
5. Pre-open outside the lock.
6. The cursor read pool.
7. Evidence: profiles again, gates, and the three-device check.

## Completion gate

- While the replica finalizes a seal on a 100k scope, a link heartbeat
  round trip on the same node stays at or below 500 ms.
- Counting hooks show one AEAD open and at most one chain verification per
  applied frame on a cold cache; revoking a link in a cached chain refuses
  the next frame that names it.
- The 100k member catch-up is at or below 18 s; the phone's 100k bootstrap
  is at or below 120 s.
- Opening and closing 1,000 cursors creates no thread per cursor.
- Three devices: the 100k load bootstraps the phone with no admission,
  read or channel timeout recorded.
