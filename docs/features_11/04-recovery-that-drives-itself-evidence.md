# 4. Recovery that drives itself: evidence

Status: implemented; local qualification passed (2026-10-26).
Device qualification not verified.
Base: `main` at `623ef5e2`, Jujutsu workspace `recovery`. The replica moves
to format **38**; no link message was added, so the link stays at phase 8's
handshake **18**.

## Peer-dependent guards (slice 1)

Each guard that waits for a specific peer, and why. Where safety does not
need that peer, any peer permitted to serve the scope on the followed tip
satisfies it.

| Guard | Who clears it | Why |
| --- | --- | --- |
| Inviter wait (bootstrap marker with `inviter`) | That peer | Only the inviter holds the key envelopes it shared after enrollment. The awaited inviter is never skipped as `covered` any more. |
| Restoration (`secure_evicted_logs`) | Local work | The completion check (`restored`) is local, and `recover_locally` runs it. The frames come from any peer. |
| Frame replay from a peer (`FrameReplay::Peers`) | Any permitted peer that pruned none | Pruned frames must come back whole. `retains_history` asks each peer on the followed tip. |
| Import-source stickiness (`keep`) | The staged import's signer, for at most `IMPORT_SOURCE` (180 s) | Pages of one checkpoint come from one signer. After that, any peer serves it. |

## Implementation

- **Owed retries.** An exchange owes a retry while a blocker remains that
  this peer or any permitted peer clears, for a scope the peer served or
  for its domain (`Node::owes_retry`). The existing jittered backoff spaces
  the retries. Keyless custody waits for read authorization rather than
  owing retries for a key it is not authorized to receive.
- **Local drivers.** `EncryptedReplica::recover_locally` settles pending
  frames, replays seal rebootstraps whose frames are local, ends
  restorations, forgets dropped tip frames that a listed base covers and
  ends a finished seal rebootstrap. One scope's failure does not stop the
  others. The SDK's `drive_recovery` task runs it at open; on a change of
  keys, grants, schemas or the tip graph (`observe_recovery`); after
  commits, frames and sync passes; and every 30 s while local work remains,
  at most once a second. It is aborted before shutdown and never makes a
  network call. Network work stays in the sync pass (`publish_for_members`).
- **Per-scope recovery.** `rebootstrap_scopes_at_seals` and bootstrap
  continue past a failing scope. `secure_import_failures` counts failed
  imports per scope and checkpoint, durably. The third cancels that
  checkpoint's staging, and the scope falls back to frames. This replaced
  the SDK's in-memory `state_failures`. Transport errors, timeouts and an
  unfinished budget are not counted. `install_pulled` returns `Deferred`
  instead of clearing another import's staging.
- **Quarantine.** Each frame applies in `SAVEPOINT quarantine`. A failure
  that is neither retryable, nor a semantic refusal, nor a storage failure
  is rolled back to the savepoint and recorded in `secure_quarantine`. So
  is a frame that does not decrypt under the key its header names
  (`undecryptable`). Only that frame's log waits. It is due again when the
  recovery context changes (keys, grants, schemas, graph counts, followed
  tip), or else after an exponential backoff of up to an hour. It is
  reported as `BlockerKind::Quarantined`, and it never admits or deletes a
  frame. A failing head is attempted only once in a recovery pass,
  even when another log makes progress.
- **Guards with an exit.** A dropped tip frame is forgotten once a
  verified base listed at a seal of the followed path covers it.
  `secure_dropped_tip_frames` gained the `scope` column for that. A
  `SealRebootstrap` whose replay is `Unavailable` and that lists nothing
  clears by `ClearsBy::NoPeer` ("no peer can serve").
- **Bounded `maintain`.** It deduplicates by domain, finalizes at most one
  seal per call, backs off after an error (up to 60 s) and drops stale
  `secure_pulled` and quarantine rows.

## Design decisions beyond the brief

- **A revocation seals and compacts.** `revoke` runs `seal_for_members`:
  it seals, runs `compact(0)` and lists the base. Members refuse a revoked
  author's frames, even ones whose dependencies never apply, and do not
  get the earlier key epoch. Those rows reach them only through a
  checkpoint at a later seal. Revoking a trust-only grant rotates no key
  and creates no extra seal. A member that learns of revocations by
  renewal writes the bootstrap marker. A bootstrap falls back to frames
  only if the scope holds no revoked author's frames.
- **Signers keep members' bases current.** In each sync pass a signer:
  - seals again when writes of an earlier key epoch arrived past the last
    seal;
  - compacts at the newest finalized seal when such writes are sealed but
    lie past the base it serves (`EarlierEpochWrites::Unbased`);
  - re-makes a lost base, or one signed under an earlier epoch;
  - lists its bases, including empty concrete partitions learned later.
  A scope learned after a seal was finalized is accounted there only when
  retained history verifies its cut. Semantic refusals remain refusals;
  quarantine excludes them and storage failures.
- **Listed checkpoints replace history that never arrives.** When
  finalizing the next seal waits on history the replica lacks, or frames
  wait for a key epoch, `replace_with_listed` imports a checkpoint that
  the graph lists at a later seal of the path.
- **Following a new path.** A follow is allowed when each scope whose
  base leaves the path either replays its own frames (no pruned prefix,
  no released existence records, and its opening key is held) or has a checkpoint the graph lists at a
  seal of the new path. A follow that would need frames only peers hold
  stays refused. Replaying a checkpointed scope's births as frames would
  conflict with the identity marks the checkpoint raised.
- **Interrupted switches resume.** A staged import is revisited even if
  frontiers match. Listed replacement respects an import progressing with
  another signer until its source-retention interval ends.
- **Retained writes survive imports on another branch.** A checkpoint
  switch retains the applied position and row/birth indexes of an already
  admitted operation outside the followed path. It does not re-admit that
  operation against the current branch and permanently reject it. Returning
  to its branch rebuilds its rows from the retained history.
- **Empty bases advance their proof.** Compaction signs an empty-history concrete
  partition at the new seal too: a newly granted reader may not hold the
  other scopes' frames that prove coverage through newer authors. Other
  unchanged bases keep their existing identity.
- **Unused own partitions retain their proof.** Preparing an own identity
  retains the known frontier even before the first seal. Importing another
  scope's base cannot erase the prepared partition's empty-history accounting.

- **Drain after releasing the import slot.** Reconciliation releases the
  shared checkpoint-import slot before waiting for reverse requests to
  drain. Holding it across that wait creates a cycle among three nodes'
  concurrent exchanges. The unchanged offline-peer/live-link regression
  passed 31 consecutive runs after the fix; before it, repeated runs hung
  during initial reconciliation on a paused clock.

## Defects the sweep found

| Seed | What broke | Fix |
| --- | --- | --- |
| 10335 (safety) | The audit refused a member's projection. A listed import, then a tip move that dropped the imported base, left seal bounds at positions inside that base. The rebuild and the audit replay stopped there. | A switch to a staged checkpoint clears the scope's seal bounds and drops. A tip rebuild that drops a base forgets bounds within it. |
| 10961 | A member that joined on a branch could not follow `main`: the start seal's state was missing, and peers on another tip serve nothing. | The follow is allowed when `main`'s path lists a checkpoint of each off-path scope; the seal rebootstrap imports it. |
| 10123 | A member without the earlier key epoch waited for sealed writes of that epoch past the signer's served base. | The signer moves its base to the newest finalized seal and lists it. |
| 10355 | An empty partition reused its base at an old seal; a member could not prove coverage through a later author. | Compaction advances the signed seal for an empty-history concrete partition even when no rows changed. |
| 10641, 10834 | A checkpoint switch on a branch rejected an already admitted write of another branch; returning there lost its rows. | Keep its applied history and indexes beyond the imported base without projecting it on the current branch. |

## Regressions

- `rocky-secure` `tests::recovery`:
  - two frames whose key arrives later apply without another frame;
  - a quarantined frame waits alone while another log of the same group
    applies, records one attempt per pass, and clears once a key arrives,
    not before;
  - import failure counts are kept per scope across a restart;
  - `maintain` finalizes one seal per step.
- `rocky-sdk` `simulation::regressions`:
  - leaving an imported base drops stale seal bounds (10335);
  - a member on another branch recovers from listed bases (10961);
  - a member recovers earlier-epoch writes from a newer base (10123);
  - a lone signer recovers after leaving a compacted branch (10957);
  - a late empty partition gets a verified replay base (10532);
  - a crash at an import switch resumes without another write (10730);
  - an empty partition base advances through a new author (10355);
  - importing on another branch preserves an admitted main write
    (10641, 10834).
- Both new regressions fail when their corresponding fix is removed:
  the empty partition waits for seal coverage, and the branch import loses
  main rows. Both fixes are restored for final verification.
- The paused-clock healing rounds allow the autonomous recovery timer to
  run. No frame, restart or direct recovery call supplies that local work.
  A round that switches from another tip exchanges on the new tip before
  measuring progress on the common path; blocker-growth detection stays
  enabled and `KNOWN_LIVENESS` stays empty.
- The simulation's healing check gates the sweep: a shard fails on a seed
  whose liveness differs from `KNOWN_LIVENESS`, which stays empty.

## Measurements

The inherited 1,000-seed sweep had no safety failures and nine liveness
failures. Continuation replays clear the local-work stalls in 10432,
10600, 10839 and 10957 after fixing driver wake-ups and allowing its timer
on the paused clock. The next full sweep ran all 1,000 seeds in 838.80 s:
no safety failures, with three liveness failures (10355, 10641, 10834).
Those schedules now converge in targeted replay after the fixes above.
The final sweep passes all **1,000 seeds (10000–10999)** in **892.84 s**:
16 shards, zero safety failures and zero liveness failures. The seed set,
healing-round count and blocker-growth check are unchanged; `KNOWN_LIVENESS`
is empty.

## Qualification limits

- **Seeded partition writes.** `simulation::cluster::Member::insert`
  allocates a partitioned identity without `partition_values`, so the
  generated partition-write actions exercise refusal. The sweep compares
  item rows and checks per-scope blockers and audits; it does not qualify
  successful partition writes under random faults. The SDK's shared-list
  and partition integration regressions cover actual partition writes
  separately.
- **Three-device check.** A Full phone joining a sealed household, with a
  branch on another desktop and an unused partition, becoming writable
  within two minutes of foreground time and its list reaching both
  desktops. Not run: no devices were available.

The revocation fault fixture separates reader-secret stops from partition-
secret stops. All nine original fault points and both reopen checks remain;
the focused runs take 0.62 s and 1.28 s within the existing 2 s tier budget.

Full lint passes with the main checkout's installed tools on PATH.
`NEXTEST_TEST_THREADS=8 ./scripts/check` passes: 200 unit tests and 1,403
integration tests (1,090 workspace and 313 SDK without default features).
Integration wall time is 244.80 s against the unchanged 300 s budget;
individual outliers pass their isolated tier budgets. Workspace clippy
passes for all targets with warnings denied.
