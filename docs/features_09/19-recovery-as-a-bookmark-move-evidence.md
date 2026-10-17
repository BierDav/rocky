# Phase 19 evidence

Status: current-format requalification and fresh independent acceptance pending
(2026-10-18). Round 7's scoped-readiness repairs share phase 17's replica v32 /
link v14 path; see the [current matrix](17-schema-changes-through-seals-qualification.md).
Earlier passes below qualify only their recorded tree. Carry records close
the carried-bundle provenance gap. See the
[brief](19-recovery-as-a-bookmark-move.md),
[guide](../runtime/stuck-recovery.md) and decision
[0075](../decisions/0075-generations-as-sealed-forks.md).

## Repairs implemented, pending final qualification

- Every recovery, including one received by an offline member, waits outside
  the graph for finality at the owner-signed line minimum of distinct trusted
  nodes. Acknowledgments bind the exact replaced running hash and terminal
  position. Verified proof admits without a coordinator lookup, even after
  retention. Candidates propagate separately from admitted statements.
- Own and received signed batches are durable before external commitment.
  Retries retain uncertainty and exact bytes. Admission replaces the unresolved
  remainder atomically, preserving journal ownership; an invalid batch does not
  block later batches. Graph, rollback, held changes and revision commit together.
- Recovery collects complementary local/peer holdings, including durable
  undecided storage, and verifies chains older than the real 1,024-entry hash
  window against the current anchor. Sparse retained indexes no longer hide
  earlier complete bundles. Malformed or nonadvancing pages fail closed.
- Rollback gathers all affected scopes before changing bundle statuses.
  Staged, assigned and carried own calculations on abandoned state are held
  during admission, without resolving the old log afterward. Retained work is
  excluded from this invalidation.
- Owner recovery allows retained equal to replaced, including zero, so full
  unreachable membership and pending work remain recoverable.
- Branch admission checks complete inherited ordinary state as well as
  coordinated entries. Ancestor serving is limited to the inherited frontier
  for every frame of a bundle.
- Pruned seal-less bases become unavailable when a graph invalidates them.
  Reads and authoring wait for verified seal rebootstrap. Checkpoint builds
  and imports begun before graph creation recheck before finishing.
- Automatic graph-backed seal acquisition repairs the issuer-switch
  prerequisite. `an_issuer_automatically_acquires_the_target_seal_when_main_changes_lines`
  exercises an issuer compacted past the target, automatic acquisition and a
  successful write after the switch.

The regression module is
`crates/rocky-secure/src/tests/coordinated/recovery/regressions.rs`.
It covers two-signer gating, wrong replaced-head binding, multi-table rollback
and transaction failure, lost replies, repeated and partially submitted batches,
atomic remainder replacement, immediate held mutation handles, complementary
holdings, full membership, zero/pending recovery, inherited ordinary rows,
frontier-limited serving, old pruned bases, in-flight checkpoint lifecycles,
coordinator retirement and the real hash-window boundary. Existing recovery
coverage retains branch row convergence, unrelated `main` isolation, ordinary
late frames and automatic bookmark advancement with explicit-move precedence.

## Carried-bundle coordinates: carry records

The accepted design keeps a carried write's frames unchanged and adds one
author-signed carry record to the tip graph
([carry records](../runtime/coordinated-writes.md#carry-records)). The
contract refinement: each frame signs its tip and position; a coordinated
write carried across `continue` seals keeps its frames and gains its
author's signed carry record naming the continuation position it committed
at. Frames are located by that record when present.

- `rocky-credentials::SignedCarry` (`rocky.entry-carry.v1`) is signed by the
  frames' author under its trusted node chain. The graph requires a
  structural continue path from the signed tip, refuses branch targets and
  conflicting records for one entry, and resolves a frame to its author's
  deepest record. No quorum changes; the coordinator never sees records.
- The retry signs the record in its staging transaction. In-sync nodes
  store a carried entry only with its author's record for exactly those
  frames, entry and signed coordinate. After commitment is known the record
  is admitted before the frames commit; `commit_entry` refuses carried
  frames without it. Catch-up pages carry it; served evidence attaches it.
- Entry indexes, receipt holding, rollback, own-work invalidation and member
  evidence use the resolved coordinate. A record admitted after its frames
  re-indexes them and rolls back abandoned effects in the same transaction.
  Graphless explicit generations carry no record; they have no recovery.

`crates/rocky-secure/src/tests/coordinated/recovery/carries.rs` covers record
after recovery with reopening, fresh-owner collection from a member
(`retained == replaced == 1`), a two-hop carry with an injected aborted
record, store refusals (missing, wrong position, other frames, other signer),
graph refusals (conflict, branch target), a fault between record admission
and entry commit with restart, trusted catch-up serving the record, sealed-log
catch-up skipping carried frames, and a two-table carried bundle. The original
regression asserts the record and `(B, 1)` index. Phase 14 carry/close gates
still pass.

## Formats

Only the current encodings are read. With phase 17's readiness repairs the
replica file is `rocky.encrypted-replica.v32`; the link handshake is
`rocky.link.v14`. Their requalification is recorded in the
[current matrix](17-schema-changes-through-seals-qualification.md).
`secure_entries` adds `carry`; stored and catch-up entries carry the record.
The carry-record implementation initially used replica v28 / link v11 and
checked refusal of replica v27 / link v10; those passes are historical.
The journal adds trusted terminal credentials; pending candidate messages are
separate from graph messages. Parent-format refusal fixtures cover the old seal
body, checkpoint binding, row cut fields, checkpoint offers and file header.
There is no migration or compatibility reader. Native recovery remains the JSON
`tips` action `recover`; Kotlin retains `Tips.recover` and `TipRecovery`.

## Verification boundary

On the final tree, after the carry-record repair (2026-10-17):

- `./scripts/lint` and `scripts/commitlint --jj`: passed.
- `./scripts/check`: Clippy, 164 unit tests (budget 10 s) and 1,010
  integration tests passed (wall 147.75 s, budget 300 s). Every test over
  2 s in the run was confirmed alone within budget. The SDK race
  `revoking_the_identity_whose_seal_won_makes_every_device_follow_the_next_winner`
  took 1.71 s / 1.73 s alone (budget 2 s), with no budget change.
  `an_issuer_automatically_acquires_the_target_seal_when_main_changes_lines`
  passed in both SDK feature builds. This verifies the phase 14 issuer-switch
  prerequisite on this tree.
- `./sdks/native/check`: passed (the native crate defines no test cases).
- `sdks/kotlin/gradlew -p sdks/kotlin check`: passed (202 tasks, 3 m 38 s).

Final qualification runs (2026-10-17):

- `publishToMavenLocal` passed. `./examples/shoppinglist-app/check`: shared
  JVM 57/58; the only failure is phase 17's `SchemaUpgradeTest` (expected
  `behind`, received `current`). The script stops there, so its later stages
  ran separately and passed: Android `shared` tests 25/25, `desktopApp` and
  `androidApp` builds.
- `./scripts/check --e2e`: 72 tests passed in both SDK builds, including the
  custody release gate. `a_migration_of_100000_rows_converges_on_every_member`
  timed out at the 600 s per-test limit in both builds, and the suite took
  1,282 s against its 900 s budget. The recorded custody flake
  (`devices_never_online_together_converge_via_keyless_custody`) did not
  fail in any run on this tree.
- A diagnostic, not a qualification: one direct run of the migration test
  binary passed functionally in 712 s. Its phases were `seed_open` 137 s,
  `fill_open` 329 s and `member_pull` 237 s, against phase 7's 83/120/276 s.
  The regression is in the issuer's migration opens. Samples show
  `Database::open` → `open_upgrading` → `audit::verify_open` →
  `replay_sealed`/`replay` decoding every frame of the unsealed tail. During
  the member pull, checkpoint maintenance (`checkpoint_step`,
  `seal_step` → `move_summary`) took about 40 % of CPU. Phase 19 does not
  change that code. The domain has no tip graph, so the seal-only
  checkpoint rule does not apply. Phase 15 already recorded 1,091 s for this
  binary under build load.
- Fixed here: recovery checks no longer decrypt each frame's payload, or
  decode its header, unless a recovery seal exists. The time effect of this
  fix on the migration case is not measured.

Phase 19 is not complete: the 100,000-row migration e2e gate misses its
budget. The path is explicit-generation migration and replay of unsealed
history; phase 17 replaces it with schema catalogs and migration parent
views. The fix is sequenced with phase 17 rather than optimizing the
transition path here. Budgets are unchanged and the test is not ignored.

Update 2026-10-18: on the phase 17 tree in the same change, the 100,000-row
test passes in both builds (367.2 s and 348.4 s), `./scripts/check --e2e`
passes all 73 tests in 796.96 s (budget 900 s), and the shopping check
passes all 58 JVM tests. See the
[phase 17 qualification](17-schema-changes-through-seals-evidence.md#qualification).
