# Phase 4 — Checkpoints and history pruning

> Historical foundation plan/evidence. Implemented contracts and recorded
> limits remain valid; future sequence and ownership follow
> [architecture V4](../../proposals/architecture-proposal-v4.md) and
> [features_04](../../features_04/README.md). Old future-tense instructions do not
> override that roadmap or require optional Room work before generic services.

Status: implemented, 2026-10-04, on both paths with the limits in the
[outcome](#outcome-2026-10-04). The brief text up to it is unchanged.
Proposed after [phase 3](03-stored-state-and-reads.md). Takes over the
compaction gate of [features 1 phase 6](../features_01/06-scaling-upgrades-and-sdks.md#scaling-and-retained-history).
Context: [scaling and rebuilds](../../runtime/scaling-and-rebuilds.md),
[recovery and effects](../../runtime/recovery-and-effects.md),
[two-replica sync](../../runtime/two-replica-sync.md).

Every operation is kept forever, loaded into memory at open and replayed. That is
why a domain is limited to 4,096 operations. This phase bounds retained history
without losing convergence or anti-resurrection evidence.

## What a checkpoint must contain

Write a decision record that fixes the canonical format. At minimum it binds:

- scope, definition, schema/handler generations and policy/key epochs;
- the applied frontier per author and the highest clock;
- the stored projection at that frontier, or a commitment to it plus the rows;
- deletion evidence for every removed entity that an older operation could recreate;
- a deduplication summary for mutation and command handles that may still be retried;
- the strong head and every unresolved request, preparation and pending job;
- which recovery copies the checkpoint supersedes and which remain sole copies.

A checkpoint is verified before use: recomputing it from the prior checkpoint plus
retained operations must give the same commitment.

## Slices

1. **Checkpoint creation.** Produce a checkpoint in bounded steps without holding
   a write transaction across the whole domain. Store it atomically with a marker
   naming the frontier it covers. Keep all history; nothing is pruned yet.
2. **Open from checkpoint.** Open loads the latest verified checkpoint and replays
   only operations after it. Remove the full in-memory operation map and full
   history replay. Full verification becomes an explicit, bounded audit call.
3. **Old-client horizon.** Declare a horizon per domain: operations at or before it
   may be pruned. A peer whose frontier is behind the horizon cannot receive the
   pruned operations and must be told so explicitly. Define the wire signal.
4. **Rebootstrap.** A replica behind the horizon imports a checkpoint, then syncs
   operations after it. Its unsent local operations must be preserved and
   reapplied or reported as rejected with a reason, never dropped silently.
5. **Pruning.** Delete operations, acknowledgments and outbox entries at or before
   the horizon once the checkpoint is durable and every configured delivery target
   has either acknowledged them or been declared behind the horizon by policy.
   Pruning is idempotent and resumable after a crash at any step.
6. **Replace the quotas.** Remove the 4,096 operation and strong-record quotas.
   Replace them with a retained-window policy and a hard bound on retained bytes
   that triggers checkpointing, then backpressure, never silent loss.
7. **Engine reclamation.** Measure log and file growth across repeated
   checkpoint/prune cycles and add whatever truncation or vacuum the pinned engine
   needs for the file to stay bounded. Record the result.
8. **Encrypted path.** The same for `EncryptedReplica`: signed checkpoints, pruning
   of operation bodies and headers, and release of its 4,096-record quota. Keep
   the existing custody rule: the last recovery copy is never released by pruning.

## Gate

- A domain accepts 2,000,000 operations over 500,000 live rows; open time,
  resident memory and write cost at the end are within the target budgets.
- File size stays bounded across at least ten checkpoint/prune cycles.
- A replica that was offline across a prune either converges or reports that it
  needs rebootstrap; after rebootstrap it converges and its local edits are
  accounted for.
- A deleted entity is not resurrected by a late operation from before the horizon.
- A retried mutation or command handle from before the horizon does not execute
  twice.
- Crash injection at every step of checkpoint creation, pruning and rebootstrap
  leaves a file that opens and passes the audit call.
- Missing custody or an unverifiable checkpoint blocks pruning.

## Out of scope

Application generation migration and signed compatibility transforms, which stay
in features 1 phase 6. Checkpoints here keep the same definition.

## Outcome, 2026-10-04

Design: [decision 0010](../../decisions/0010-checkpoints-and-pruning.md) for the
trusted path, [decision 0011](../../decisions/0011-encrypted-checkpoints.md) for
the encrypted path; the two share no code or format. Guides:
[checkpoints](../../runtime/checkpoints.md),
[encrypted checkpoints](../../runtime/encrypted-checkpoints.md).

| Slice | Trusted path | Encrypted path |
| --- | --- | --- |
| 1 Creation, verification | `worker/retention.rs`; row digests in `digest.rs` from the pre-update hook | `secure/replica/checkpoint/create.rs`; LtHash set digests maintained per transaction |
| 2 Open from checkpoint, audit | `worker/incoming.rs`, `worker/audit.rs`, `Replica::audit` | `EncryptedReplica::audit`, `checkpoint/audit.rs`; files migrate at open |
| 3 Horizon, wire signal | `worker/horizon.rs`, `HorizonNotice`, `catch_up` | `pruned` on transfer pages, `horizon_page`, `NativeSession::pull_page`; session v2 |
| 4 Rebootstrap | `worker/rebootstrap.rs`, `import_checkpoint` | `checkpoint/transfer.rs`, `NativeSession::rebootstrap` |
| 5 Pruning | `worker/horizon.rs` | `checkpoint/prune.rs` |
| 6 Quotas | `RetentionPolicy`, `Error::Retention` | `RetentionLimits`, `Error::Backpressure` |
| 7 Engine reclamation | Freed pages are reused; no vacuum | The same |

`StoredApplication` gained the required `checkpoint_tables()`. The desktop
transport uses `catch_up` and rebootstraps a peer behind the horizon.

### Gate

Probe runs are in [scaling limits](../../runtime/scaling-limits.md#after-checkpoints)
and the [encrypted baseline](../../runtime/encrypted-baseline.md#after-checkpoints).
Tests are in `crates/rocky-runtime/src/retention_tests.rs` and
`crates/rocky-secure/src/tests/checkpoint.rs`.

| Gate | Trusted path | Encrypted path |
| --- | --- | --- |
| 2,000,000 operations over 500,000 rows within budgets | Met at 20 and 2,000 tables: write 1.16× at 2,000 tables, reopen 117 ms, 50 MiB | Not run at this size (about 3.6 hours at 155 operations/s); 14,000 operations over 2,000 rows |
| File size bounded across ten cycles | Met: 211.3 to 211.4 MB at 2,000 tables | Met: 11.19 to 11.25 MB over 12 cycles with a handle window of 2,000 |
| Offline replica converges or rebootstraps, local edits accounted | `an_offline_replica_converges_or_rebootstraps_with_its_local_edits` | `offline_replica_learns_it_is_behind_and_rebootstraps_with_its_local_edits` |
| No resurrection by a late operation | `a_late_operation_from_before_the_horizon_cannot_resurrect_a_deleted_row` | `late_frames_and_retried_handles_from_before_the_horizon_do_nothing` |
| Retried handles do not execute twice | Met inside the 65,536-handle window | Met inside the same window; `retried_command_from_before_the_horizon_keeps_its_one_result` |
| Crash injection at every step | Injected failure and process death in every step | Injected failure and process exit in every step, then reopen and audit; `process_exit_inside_any_checkpoint_step_leaves_a_file_that_opens_and_audits` |
| Missing custody or unverifiable checkpoint blocks pruning | Met | `pruning_needs_a_verified_checkpoint_acknowledgments_and_custody`, `rebootstrap_refuses_unverifiable_checkpoints` |

### Decisions

The user chose on 2026-10-04 that handles keep a retry window of 65,536 on both
paths; a retry of an older handle executes again. The user then reviewed the
remaining trade-offs of decision 0010 the same day
([review](../../decisions/0010-checkpoints-and-pruning.md#review-2026-10-04)):

- Confirmed: the checkpoint is a second copy of the projection, double storage
  and each operation applied twice, in exchange for real recomputation. It
  roughly doubles the install time of a 2,000-table application (8.5 s against
  4.8 s). A leaner mode is [deferred](../../optimizations_on_demand/lean-checkpoints.md).
- Confirmed: the build needs libclang, since `rusqlite`'s `preupdate_hook`
  feature uses `bindgen`.
- Amended: an unresolved strong request stops checkpoints and pruning until it
  is applied or released, by `release_command` or after `request_hold`. One
  with an unknown outcome is never released. Storing cut rows instead is
  [deferred](../../optimizations_on_demand/stored-cut-rows.md).
- Amended: a peer is declared behind by `declare_behind`, or after
  `behind_after` if set; otherwise retained history grows to the bound and
  writes fail with `Error::Retention`.

Tests: `a_released_or_expired_request_stops_holding_checkpoints`,
`a_stuck_request_pushes_writes_back_until_it_is_released` and
`a_target_holding_back_the_horizon_too_long_is_declared_behind`. The encrypted
path is unchanged: it has no unresolved-request hold, and its retention policy
already lists peers declared behind explicitly.

Decision 0011 leaves two questions open: whether a custody-only replica may
prune under a checkpoint from a trusted reader, and whether recovery packages
should carry a checkpoint so that domains with required custodians can prune.

### Limits

- The trusted path's limits are in [checkpoints](../../runtime/checkpoints.md#limits),
  the encrypted path's in [encrypted checkpoints](../../runtime/encrypted-checkpoints.md#limits).
- Not covered by a test: automatic maintenance with delivery targets under
  concurrent load, and a strong anchor adopted during an import. A
  `WITHOUT ROWID` table imported into one domain of a multi-domain file is
  covered by `a_rowless_checkpoint_imports_into_one_domain_of_a_shared_file`.
- The interactive desktop demo was not rerun; `./sdks/kotlin/check` passes.
