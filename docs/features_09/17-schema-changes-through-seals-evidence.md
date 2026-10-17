# Phase 17 implementation evidence

Status: repairs implemented; current-format requalification and fresh independent
HIGH acceptance pending (2026-10-18). Round 7 rejected acceptance of the earlier
tree. Work is in change `pvvozwkw`, together with phase 19. The earlier v31
qualification below is baseline evidence, not qualification of the v32 repairs.

## What changed

- **Graph schemas** (`rocky-credentials`, `rocky-core`). An opening commits
  its own schema (opening format 2); a `continue` may name another schema;
  a recovery seal keeps its tip's. An issuer may open a migration successor
  on an authenticated `close`, with another schema and its line's signed
  minimum, verified structurally; active resolution additionally requires
  canonical, unrevoked ancestry. `granted_like` is part of the manifest.
  Transitions (`GenerationTransition`, `SignedTransition`, `ItemChanges`)
  are removed.
- **Replica** (`rocky-secure`, file `rocky.encrypted-replica.v32`, link
  `rocky.link.v14`). Every domain has a graph from install; the graph-less
  explicit-generation mode and arbitrary-cut checkpoints are gone. A
  catalog (`secure_tip_schemas`) and `required_schemas()` replace
  `secure_generations`; a followed bookmark reaching an unknown schema
  records a pending, read-only switch. `PathFields` replaces names,
  baselines and pull-again; bases at seals keep their own tip's fields.
  Additive switches apply in place. Staged schema publications stay private
  and are released after endorsement of their journaled migration descriptor
  and, when coordinated, after
  verified finality of the exact close a source-tip trusted node relays.
  A migration that fails while it runs withdraws its stage atomically.
- **SDK** (Settings v19, enrollment format 2). Open classifies the declared
  schema as current, ahead or behind against the followed tip; issuers
  publish at open before the network starts. Records travel over
  `rocky.sdk.tip-schema.v1`; enrollment carries the inviter's graph, its
  bookmark and record. Joins with no offered base use frames.
- **Examples and Kotlin.** `TipOpening.schema`; the shopping app follows
  `main` and signs its genesis minimum at creation; `SchemaUpgradeTest`
  follows the 6 → 7 migration and re-authors the held write.

The round40 withdrawal and final staging corrections are recorded separately in
[staging atomicity evidence](17-staging-atomicity-evidence.md); round41 held-source
validation and incarnation deletes are in
[their focused evidence](17-held-source-delete-evidence.md). Fresh independent
review and final-source qualification remain pending.

## Completion gate

The round25 carry-order repair adds permanent concrete own-birth floor O and
signed `CoveredMark.own` inside the one unreleased checkpoint16 format. P still
includes carries at both scopes; ordinary receive, S and L use own births only.
The delivery-order regression checks P=3 before own birth4 and P=O=4 afterwards;
the earlier P=3/O=4 example was an arithmetic error, not an invariant exception.
Current file32 requires O; link14, row11 and backup2 are unchanged. Focused
development passes do not replace the complete matrix or fresh HIGH acceptance.

| Gate | Test |
| --- | --- |
| Old device offline across a migration gets its writes back, re-authors, converges | SDK `generations::an_old_device_offline_across_a_migration_gets_its_writes_back_and_reauthors_them`; shopping `SchemaUpgradeTest` |
| Offline writes across an additive seal apply everywhere | SDK `generations::a_member_ahead_waits_for_the_issuer_and_its_unsent_writes_cross_the_seal`, `an_older_application_follows_two_published_schemas_and_keeps_its_unsent_rows` |
| A nullable field on 100,000 rows rewrites none | secure `tip_schemas::an_additive_switch_changes_the_same_storage_for_ten_or_a_thousand_rows` (SQLite's `total_changes()` over the switch is equal at 10 and 1,000 rows, with zero observer events) and `an_additive_switch_preserves_rows_observers_cursors_and_existing_sessions` (cursor, open session, observer); SDK `migrations::a_migration_of_100000_rows_converges_on_every_member` compares the switch transaction's row changes for 8 and 100,000 rows, and checks used data-page growth under 1 MiB |
| Concurrent schemas or migrations end on one tip without duplicates | SDK `generations::issuers_that_publish_one_schema_apart_end_on_one_tip`, `migrations::issuers_that_cannot_reach_each_other_both_migrate_and_converge`; credentials race tests |
| Offline staged migration while others write `main` | SDK `generations::an_issuer_migrating_offline_publishes_on_reconnect_and_meanwhile_writes_are_held`, `coordinated::an_offline_coordinated_migration_is_released_by_the_exact_close_s_finality` |
| A device on `main` never fetches a branch schema | SDK `generations::a_device_on_main_never_fetches_or_installs_a_dev_branch_schema` (transcript), with the positive control in `a_member_fetches_a_required_schema_from_another_member_with_no_issuer_reachable` |
| A re-added name never receives an earlier value | secure `generations::switches::a_readded_field_never_receives_a_value_written_before_its_removal` (with a base at A imported into C); SDK `removal::a_readded_field_never_receives_a_value_written_before_its_removal` |

Crash points: the SDK switch on either side of the replica commit
(`a_crash_on_either_side_of_the_switch_resumes_on_the_followed_schema`), a
migration failing before its commit and never repeating after it
(`migrations::a_failed_migration_runs_again_and_a_committed_one_never`),
and the secure pending-switch reopen (`tip_schemas`).

## Admission evidence (implementation in progress)

Returning admitted frames after pruning or a lower base need exact local
evidence beside the permanent identity marks; see
[admission evidence](../runtime/admission-evidence.md). The implementation is
under focused validation; it has no completed qualification or acceptance.
Its acceptance requires,
besides the behavioral regressions:

- the phase 15 plateau tuple `((1, 0), 1, 0, 0, 4, 1, 1)` unchanged at cycles
  16 and 64;
- the churn fixture's exact growth from cycle 16 to 64: 48 receipts, 48
  original assignments and 48 chain values, with permanent marks, summary,
  ledger and bindings unchanged;
- encoded bytes, allocated pages per table and index, and file and WAL bytes
  reported separately, with backups and restore cuts counted and graph
  history reported apart.

Its implementation is in progress; the corrected rules for filtered
restoration, the replay ledger and kept views are in that guide.

## Frontier cache repair

Committed frontiers use an `Inner`-owned generation, scoped by resource and
workspace tip. Ordinary write attempts invalidate before execution. Transactional
reads bypass lookup and insertion; failed attempts and context restoration clear
both frontier and field-path caches. Only `offer`, peer-frontier bookkeeping and
individual acknowledgments use the private cache-neutral transaction helper.
Secure SQL begin, rollback, cancellation, savepoint rollback and drop follow the
same lifecycle. Neither SQLite's `total_changes()` nor graph lengths identify
cached state.

Deterministic tests cover rollback after a transactional read, retry after writes,
a deferred constraint failing at COMMIT, SQL cancellation and savepoints,
equal-sized authenticated revocation replacement without SQL writes, graph reload,
failed graph admission, branch return, pending bundles and gaps, signed voids,
private-stage withdrawal, and warmed paths across removal/re-add and checkpoints.
The serving test asserts one full scan and 66 header decodes for 66 previously
unoffered frames across multiple pages, with frontier reports and individual
acknowledgments between them. It also checks authorization and same-tip refusal.

## Qualification

Replica v32 / link v14 repairs are being requalified sequentially. Fresh
independent HIGH review is required before accepting phase 17. See the
[current repair qualification](17-schema-changes-through-seals-qualification.md)
for commands, logs, counts, timings and open review points. Phases 14 and 19 share
these gates; their earlier passes do not qualify this format change.

### Before round 7 (v31 baseline)

Earlier tree, 2026-10-18; these gates passed before the review repairs, with
budgets unchanged. They must be rerun for replica v32 / link v14:

- `./scripts/lint` and `scripts/commitlint --jj` passed.
- `./scripts/check`: clippy, 166 unit tests (budget 10 s) and 1,055
  integration tests in both SDK builds, wall 158.06 s (budget 300 s).
- `./scripts/check --e2e`: 73 tests passed in both builds, wall 796.96 s
  (budget 900 s): 447.1 s workspace and 348.9 s without default features.
- `a_migration_of_100000_rows_converges_on_every_member`: 367.2 s and
  348.4 s in the suite (limit 600 s); alone 393.2 s with seed open 48.8 s,
  additive open 12.7 s, fill open 124.4 s and member pull 203.4 s in one
  sync. The additive switch counted 8 row changes for 100,000 rows, as for
  the 8-row control, and data pages grew by 12 KB. The member retained
  628,301,677 bytes, all of it migration history; its ordinary history was
  0 bytes, the issuer's 19,108, within the unchanged 64 MiB bound. No
  checkpoint covers the open migration tip.
- `./sdks/native/check`, `sdks/kotlin/gradlew -p sdks/kotlin check` (4 m
  10 s) and `publishToMavenLocal` passed.
- `./examples/shoppinglist-app/check`: shared JVM 58/58 including
  `SchemaUpgradeTest`, Android `shared` 25/25, `desktopApp` and
  `androidApp` builds.

Before the repairs below the 100,000-row test timed out at 600.044 s (seed
open 124.5 s, additive 18.1 s, fill 237.1 s, member pull unfinished). Runs at
20,000 and 30,000 rows showed it was CPU-bound, not the earlier retention
deadlock: the member converged in one sync with no failures, both
publications (seed and fill) classified, ordinary history 0 bytes and
123 MB and 185 MB of migration history. The pull grew as n².
Sampled repairs, none changing admission, audit or atomicity:

- A covering index serves header scans: reading `status` from
  `secure_operations` walked each frame body's overflow pages.
- An `(author, scope, status, sequence)` index makes an author's newest held
  or next pending frame one seek instead of a log scan per authored bundle.
- Frontier computation caches decoded header fields by frame hash (bounded;
  headers never change) and walks the tip path once per source tip instead
  of once per frame; `catch_up` reuses the frontier it just reported.
- Closed-source held classification reads bodies only for frames its header
  places as held, unless a recovery seal exists.
- A frame keeps its verified operation once decoded, and its bytes are
  private to `wire.rs`, so no caller can change them under the cache; a
  payload decodes once, its format check runs only on failure.

Round 7 rejected the baseline's common-ancestor filtering and its assumption
that scoped trusted nodes could not take a successor's first slot. The target
path contract and signed placements now replace those workarounds; the
[repair qualification](17-schema-changes-through-seals-qualification.md) records
their regression results. Phase 20's schema 8 artifacts and physical-device
qualification remain outside this evidence.

## Migration descriptors

The accepted bulk-history refinement adds format-1 migration descriptors and
Merkle-sum inclusion proofs, a paged disk-backed construction journal, durable
per-frame classifications and counters, external-provider endorsement after SQL
commit, proof-bearing receive and exact descriptor lookup. Ordinary admission
uses checked `T - M`; generic publications receive no exemption. Operation-delete
triggers debit classifications atomically across all operation-removal paths.
Open-time and explicit audits validate proof provenance and reconcile counters.
Descriptors landed at replica v31 / link v13. The placement and switch-journal
repairs advance them to replica v32 / link v14; v31/v13 are refused.

## History: retention diagnosis before descriptors

A bounded diagnostic resumed the failed fixture, without reseeding, using the
already-built SDK in `/tmp/p17-resume`. Its source audit took 37.15 s and member
open completed at 40.58 s. Every following sync returned retry-pending with
`storage queue is full; retry when pending work finishes`. The member remained
at 20,480 rows, 640 applied frames and 34 pending frames. Its retained counter
was **67,056,712 bytes**, against the unchanged **67,108,864-byte** default hard
bound. Neither source nor member had a checkpoint. The source retained about
620 MB. This is a retention deadlock, not evidence that frontier caching is
incorrect or that more CPU time will converge. Diagnostic logs and samples are
`/tmp/p17-resumed-pull.log`, `/tmp/p17-resumed-pull.sample` and
`/tmp/p17-resume-open.sample`; they are not qualification. The diagnostic was
stopped after confirming the repeated failure.

The owning paths are:

- `replica/group.rs::receive_step` calls `check_budget` before storing a new frame.
- `replica.rs::check_budget` exempts local `publishing`; ordinary incoming
  migration frames still hit the hard bound.
- `replica/checkpoint/create.rs::maintain_scope` now only finalizes ready seals;
  it does not build an unsealed checkpoint or prune one.
- The current migration tip remains open. Seal-only compaction therefore cannot
  cover all its migration frames, and a member can hit the bound even before
  acquiring the full ancestor frontier.

The accepted descriptor design above resolved this deadlock; the
qualification section records the converging runs.
