# Phase 6 evidence

Status: implemented; performance and device qualification pending (2026-10-25).
Base: `26f8ebe6`; isolated Jujutsu workspace `rocky-database-thread`.

## Implementation

- Transport workers own admitted-link readers, writers and channel routing.
  Requests cross a bounded 32-entry channel to the database executor. Transport
  cancellation also watches authority and network-service lifecycle changes.
  Channel ownership closes explicitly, waking reconcile waiters even while the
  writer retains its queue. Publication back to a push origin waits for its
  durable intake to update the shared cursor; other peers keep progressing.
- Exposed group replies follow a blocking flush whose closure owns the replies,
  so cancelling the leader cannot strand them. Other exposing paths release
  the replica for fsync and recheck the durable watermark after reacquisition.
  Compaction flushes its checkpoint before deleting covered history.
- Immutable frame signatures and available-key decryption are prepared on blocking
  workers before intake. A FIFO cache bounded to 256 frames and 16 MiB shares the result
  across receive, dependency checks, bundles and apply. Apply passes its decoded
  payload through materialization and storage.
- Verified chains are cached under chain, publisher and full revocation-set
  digests. Every use checks the leaf binding, author, validity window and concrete
  grants. Revoked node references have a set index. Tip and manifest endorsement
  checks no longer verify the same chain twice.
- Cursors lease reusable read-only connections and workers, bounded to 64 per
  host. Each lease resets hooks, temporary views and its transaction before reuse.
  SQL sessions register committed readiness snapshots, including partition rules
  and kept-history metadata. A cursor compares the snapshot epoch inside its
  read transaction; racing admission changes fail closed. Credentials and authority
  are checked before opening and before delivering pages.
- The persisted replica identity is now **37** for the read-admission epoch.
  Earlier and future identities are refused; wire and row formats are unchanged.

## CPU samples and the 100k fixture

Built with `cargo test -p rocky-sdk --test migrations --release --no-run` and ran
`a_migration_of_100000_rows_converges_on_every_member --exact --nocapture`.
macOS `sample PID 5 2 -file PATH` captured five-second samples at seed, issuer fill
and returning-member catch-up. Symbols were decoded with
`xcrun llvm-cxxfilt --no-strip-underscore`. Raw logs and samples are under
`/tmp/rocky-phase6` on the execution host; the meaningful results are retained here.

The initial before/after runs both checked every key/value, matching closing seals,
member convergence and retention accounting. These runs overlapped other local
builds and another workspace's tests, so they are observations, not an isolated
performance comparison.

- Baseline: seed **84.866 s**, additive open **7.810 s**, issuer fill **333.595 s**,
  publication **72.708 s**, closing compaction **50.279 s**, member catch-up
  **38.560 s**, member file **170,205,184 bytes**.
- Initial implementation: seed **129.903 s**, additive open **16.081 s**, fill
  **513.525 s**, publication **71.724 s**, closing compaction **35.777 s**, member
  catch-up **23.380 s**, member file **171,094,016 bytes**. This sample predates
  the final pruning-order and dependency-payload corrections.

Samples retain substantial work outside repeated frame opening: row/header proof
signature verification, CBOR packing/unpacking, record encoding, SQLite B-tree
writes and WAL I/O. For example, the baseline member's collapsed leaves include
548 field-arithmetic samples, 222 `pread`, 178 `fcntl` and 145 `pwrite` samples;
its initial implementation counterpart has 596, 193, 292 and 176 respectively.
These are counts from all sampled threads, **not CPU percentages**. The member
checkpoint path still verifies row and checkpoint proofs: caching an incoming
frame's credential chain does not remove those checks. No scope or admission
check was weakened to meet a timing target.

## Verification and remaining gates

- Cursor tests: **7 passed**, including 1,000 sequential cursors reusing exactly
  one worker, temporary-view isolation, stale readiness, cancellation and snapshots.
- Focused security regression run: **78 passed**, covering group durability,
  pending application, checkpoint pruning, crash recovery and kept seals. The
  one-open/one-cold-chain, cached revocation and paused-fsync tests passed.
- Focused SDK admission, lifecycle, change-driven sync and registry run:
  **47 passed**, including real isolated QUIC idle-link behavior, shutdown,
  renewal, cancellation, simultaneous writers and a blocked-executor round trip.
- Channel tests: **13 passed**, including dropping the channel owner while its
  writer retains the outgoing queue; reconcile wakes immediately and new
  requests are refused.
- Static lint and workspace Clippy (`--all-targets --locked -- -D warnings`)
  passed. Conventional Commit validation passes for the implementation change.
- The original checkout's existing binary independently fails
  `tests::tips::frontiers::headers::excluded_and_other_domain_headers_still_decode_after_warming`
  with “expected rejection invalid secure frame, got success”. Phase 6 does not
  change that stored-header scan. This baseline failure is not waived or hidden.
- Resumed `./scripts/lint` and workspace Clippy passed. The full check passed
  **198 unit tests** (183 workspace and 15 SDK without default features).
  Workspace integration passed **1,053 of 1,054 tests**; its only failure was
  the baseline stored-header test above, so `./scripts/check` is not green.
  SDK integration without default features passed **289 of 289 tests**.
  Integration finished with **1,342 passing executions** in **257.31 s**, within
  its 300 s suite budget. The isolated rerun of
  `races::revoking_the_identity_whose_seal_won_makes_every_device_follow_the_next_winner`
  took **2.02 s**, exceeding its 2 s per-test budget; a subsequent focused run
  passed functionally in **2.144 s** but did not meet that budget either.

The **18 s member target is not demonstrated** by either measured run. Physical
phone profiles, the **120 s phone bootstrap**, and the required two-desktop plus
phone shopping/100k check are **not verified**: device discovery exposed only
local iOS simulators and an unbooted Android emulator, with no physical phone.
The transport control test blocks the database executor and checks a round trip
against 500 ms; it is not the physical 100k-seal heartbeat qualification.
