# 7. Scope-local gates: evidence

Status: implemented (2026-10-26). The shopping-app profile is not verified.
Base: `main` at `d9964fe6`, Jujutsu workspace `scope-local-gates`. No
persisted or wire format changed: the replica identity stays at **39**, and
no link message changed.

## Gate inventory (slice 1)

Each gate that `workspace_blockers` held for every scope of a domain, and
what it holds back now. A gate stays domain-wide only for a cross-scope
dependency.

| Gate | Was | Now | Why |
| --- | --- | --- | --- |
| `SealRebootstrap` (a base off the followed path, or no verified base) | Every write in the domain; reads of the scope itself | Writes and reads of that scope | Its base and rows are its own. Authoring in another scope reads only that scope's log and base. Author sequences come from the node-local counter `secure_author_sequences`, and the seal frontier is per author, so a frame in a verified scope never needs another scope's base. |
| `StagedImport { sealed: false }` (an import outside a seal) | Every write and read in the domain | Writes and reads of that scope | Staging rows sit in `secure_rebootstrap*` under the scope's id. An unsealed import replaces only that scope's base. |
| `LostCompaction` (a compacted seal left the followed path) | Every write and read in the domain | Unchanged: domain-wide | `secure_finalized` and the identity ledger it folds belong to the domain. When the last rebootstrap ends, `settle_seal_rebootstrap` deletes them and rebuilds the whole tip workspace (`rebuild_tip_workspace`). Until then, finalized state from the lost path is visible to every scope. |
| `DroppedTipFrames` | Every write | Unchanged: domain-wide | The frames return into the followed tip's ancestry. Their seal frontier is per author across scopes, so a new frame in any scope may sit after them in an author's sequence. |
| `LineNode` | Every write | Unchanged: domain-wide | No node of this identity is registered for the line, so nothing can be signed. |
| `ensure_tip_writable` (followed bookmark moved) | Every write | Unchanged: domain-wide | The workspace is read-only. |
| Pending frames, restoration guard, quarantine | That scope | Unchanged | These were already per scope (`has_pending`, `restoring`, `secure_quarantine`). |
| Partition identity preparation (`next_partition_identity`) | Every scope of the domain | Domain blockers and the prepared group's new scopes | An identity is allocated per table group. Another group's state is never read. |

Partitioned tables used to set `unavailable` when any held partition was
pending or restoring, which made every read of the table incomplete. Now
`TablePartitions::waiting` lists only the partitions that wait (pending,
restoring or blocked). A read is refused as `Incomplete` only if its
partition proof may reach one of them. A read with no proof (`row_kind=0`
alone) may reach every partition, so it is refused while any partition
waits.

## Implementation

- **Derive once (slice 2).** `blockers::Readiness` caches a
  `DomainReadiness` per domain: the domain-wide blockers plus each scope's
  own. A cached entry is valid for one generation, one count of watched
  state-table writes and one context commitment. The generation advances
  at every transaction begin, commit and rollback. The watched tables are
  `secure_rebootstrap`, `secure_compaction_lost`, `secure_finalized`, seal
  anchors, bounds and drops, `secure_checkpoints`, existence bases,
  subscriptions, roles, evicted logs, partitions, the followed tip, and
  `secure_scope_state` inserts and deletes. A preupdate hook counts them
  (`sqlite::Writes`), so a write inside a transaction also invalidates.
  `secure_scope_state`'s per-frame updates are not counted, because
  readiness reads only which scopes have state. The context commitment
  covers authority, manifest, tip, followed bookmark, graph counts and
  custody scopes. Per-frame tables (`secure_operations`, `secure_logs`,
  position ranges) are covered by the transaction-boundary invalidation.
- **Per-domain checks.** SQL `begin` checks each domain once
  (`require_domain_ready`). When it refuses, the refusal also names the
  blockers of the scopes the session may write. Each written scope's own
  blockers refuse its frames at commit (`author_frame_carried` →
  `scope_blockers`). `partition_rules` and `table_complete` fetch the domain
  readiness once per statement, not once per partition.
- **Scope-local refusals (slice 3).** `scope_blockers(scope)` is the domain's
  blockers plus `scope`'s own. `readable_blockers` is the same, except that
  an authenticated direct import reads its verified pages.
- **Parse once (slice 4).** `rocky_storage::parsed::parse` keeps parsed
  statements by SQL text in two generations of 256, for texts up to 16 KiB.
  `statement::analyze` and `partition_sql` both use it.
  `partition_sql::prepare` keeps the view-expanded statement and its
  relations per text and view set. `Prepared::check` returns `Checked
  { sql, params }`: held partition values bind as `?N` after the caller's
  parameters, and anonymous `?` are numbered. The text then depends only on
  the number of held partitions. A filtered statement given the wrong number
  of parameters is refused instead of being padded. A per-table rule cache
  was not added: rules are built from the cached readiness, and the parse
  and view expansion they used to repeat are now cached.
- **Simulation (slice 5).** Each write step counts a write accepted while
  some scope of the node waited for a seal rebootstrap or an unsealed import
  (`Report::scope_local`). Before this phase, a domain-wide gate refused such
  a write. Once a seed converges, every scope on every live node must audit
  with no refusal at all (`Simulation::audited`). The sweep prints the
  seeds that exercised a scope-local acceptance.

## Completion gate

| Gate | Evidence |
| --- | --- |
| One scope awaits a seal rebootstrap: another scope writes and reads; the blocked scope's write and read are refused with its blocker | `tests::compaction::scope_local::a_scope_awaiting_a_seal_rebootstrap_holds_back_only_itself`: a joiner with an imported `items` base writes and reads `items` while `tags` waits. The `tags` write and read are refused with only `tags`' `SealRebootstrap`. After the import, both scopes audit. `a_lost_compaction_holds_back_every_scope_of_its_domain` keeps the domain-wide case. |
| One readiness derivation per domain per revision for a transaction over 64 scopes; one parse per distinct statement | `tests::partitions::sixty_four_partitions_derive_readiness_once_and_parse_each_statement_once`: a 64-scope bundle derives once, and a 64-scope SQL transaction derives once. Three repeats of a filtered statement parse nothing. 64 autocommit reads derive at most once. `parsed::tests` and `partition_sql::tests` cover the cache bound, bound held values and stable text. |
| Audit oracle and e2e sweep pass with scope-local gates | See [Sweep](#sweep). |
| Shopping-app control SQL on a household with ten partitions spends no sampled time in readiness checks of unrelated partitions | Not verified. No device or shopping-household profile was taken. The counting test above is a deterministic proxy, not a profile. |

## Tests changed with the inventory

These tests asserted the domain-wide gate. They now assert the scope-local
one:
`partial_reads::a_verified_scope_reads_and_writes_while_an_unbegun_one_waits`
(was `…_and_authoring_stays_blocked`),
`no_frames::seal_recovery_skips_a_peer_outside_the_missing_scope` (the
public scope authors while the private one waits), and the SDK's
`shared_lists::preparing_an_own_id_does_not_claim_an_unrelated_group_members_state`
(the prepared list writes, and its unverified `items` partition refuses
with its own blocker).

## Sweep

Measured 2026-10-26 on macOS with 18 cores, in a debug build.
`cargo test -p rocky-sdk --lib simulation::tests::sweep_ -- --ignored
--test-threads 16` ran seeds 10000–10999 (3 or 4 nodes, 32 steps, 5 healing
rounds) in 599 s wall over 16 shards.

| Result | Value |
| --- | --- |
| Safety failures, including the post-convergence audit | 0 of 1,000 |
| Seeds that did not converge | 0 (`KNOWN_LIVENESS` stays empty) |
| Seeds with a write a scope-local gate accepted | 8: 10084 (1), 10090 (1), 10347 (3), 10501 (1), 10537 (2), 10743 (1), 10747 (1), 10819 (2) |

Each of those 12 writes was refused before this phase. Every node of each
of those seeds converged, and all of its scopes audited after healing.
`./scripts/lint` and `./scripts/check` passed: 203 unit tests, and 1,407
integration tests in 271.6 s against a 300 s budget.

## Not verified

- The shopping-app profile on a ten-partition household.
- Device qualification is not part of this phase.
