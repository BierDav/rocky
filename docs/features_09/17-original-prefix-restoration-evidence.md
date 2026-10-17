# Round38 original-prefix restoration

Status: bounded implementation and focused verification complete, 2026-10-20.
Final qualification and fresh independent HIGH acceptance remain with the root
orchestrator. Phases 14/17/19 and the whole features_09 roadmap are not accepted;
phase 20 application work has not started in this repair.

## Accepted repair

The common rocky-secure restoration predicate preserves its installed/applied
prefix route and additionally verifies settled original-node dispositions.
It walks exact indexed stored records backward, iteratively, through the
recorded scope-log endpoint `(G,H)`, to genesis or an exact installed verified
checkpoint prefix. Every uncovered frame binds its complete signed envelope,
body hash/size, stored hash/scope/key/sequence/chain, historical source schema,
partition and portable author assignment. Settled applied frames retain their
applied-scope binding; held and recovery-abandoned originals remain dispositions.
Genuine signed void records supply authenticated cancellation, with exact
record digest, membership and cross-record links. Every edge checks the full
ResourceScope, backlink and strictly increasing clock, including the root edge.

A relevant higher log/base must bridge the exact H; a larger sequence alone
cannot discharge the obligation. Pending/rejected frames, header placements,
unfinished staging, raw candidates, NoFrames and R/P/O/S/L/claims are not prefix
proof. Historical verification does not re-grant expired authority. Existing
intake, current authoring grants, custody, dependency and target-readiness gates
remain unchanged. The exact source tip is resolved in its own domain graph,
structurally, without requiring it to be today's winning/live tip. A retained
registered concrete partition still requires its tuple when the current table
descriptor has been removed; descriptor absence cannot downgrade that binding.

Both existing clearing callers use the same read-only proof inside their
existing transaction. Capture still precedes destructive reset/replacement;
settlement can record a new obligation. Failure rolls back deletion and caller
effects. Reads remain presence checks, and publication remains after commit.
No public API, format, compatibility reader, persistent proof cache, synthesized
Applied/void/receipt/custody, or automatic re-authoring was added. Current formats
remain file32/link14/checkpoint16/row11/backup2.

## Focused evidence boundary

The small fixtures use genuine signed close/open/bookmark statements, capture
the source close before eviction, and register the selected line key normally.
Endpoint-first held delivery must wait for every missing predecessor through
actual restoration calls, read/write refusal and reopen. Completion retains the
original held bytes without applying them to the target projection. TEMP
triggers after deletion test full authoritative rollback and successful retry.

Checkpoint fixtures distinguish fully staged pages from installed verified
roots, cover a lower prefix with a held/void suffix, an exact installed endpoint
whose frames were evicted, and a higher installed prefix without the exact G
bridge. An independently missing sibling target scope is restored from its own
genuine checkpoint before strict domain authoring can succeed.

Incoming SignedVoid fixtures exercise real signed cancellation records, including
a gap between separate records. They do not claim the local never-offered
`drop_held` workflow: these source frames had previously reached a custodian.
Signed higher-log and retained SQL corruptions are privileged negative fixtures,
not admissible ingress examples. They test exact key/scope/source/clock/backlink,
original author assignment, full envelope/body and void record bindings. Existing
unknown-own admission limits remain independent controls.

The final secure selection passed **40/40** in **7.090 s** with one test thread,
including **15** focused round38 tests. It retains the Stage::Switch base/guard
atomicity control, paired guard merge, returning suffixes in both orders,
role eviction/resubscription, scoped partial reads, ordinary late/abandoned
bundle behavior, exact NoFrames after exclusion, journal tampering, finite fresh
restore cuts and exact kept bytes. Integration budgets and sizes are unchanged.
Scoped secure/SDK Clippy passed. Detailed commands and development failures
remain in the handoff; final qualification is still pending.

## Q2: coordinated private publication observation

The accepted observation uses read-only inspection of the actual canonical
committed coordinated stage, binding publication/source/target/exact close.
Networking-disabled sync publicly reports `skip:networking_disabled`. With T
closed, a real enabled sync leaves the identical committed stage and no remote
successor opening. Its empty peers report is scheduling evidence only.

After genuine T resumes on the source tip, automatic sync releases the stage
without reopening the publisher. Actual voter terminal digest and signed
terminal acknowledgments name the exact close; both peers verify finality,
T installs schema2 with the exact migration rows, and both late source writes
remain held. The internal `schema_publication_pending` string is not a public
sync result. Existing migration publishing progress is not an exact stage or
finality oracle. No consumer API or application flow was expanded.

## Final service prerequisite rerun

All **six** service prerequisites passed against the final production source,
with one test thread and their existing tiers. The three integration tests
passed in **3.025 s**, individually **0.740/0.976/1.308 s**. The existing service,
private migration and original rollback P5 passed in the e2e profile in
**51.023 s**, individually **0.529/4.574/45.919 s**. P5 preserves all 258 real
transactions, retention256, observation30s, automatic recovery before M reopen,
exact original-byte/disposition checks and its final ordinary target write.
Logs are `/tmp/rocky-round38-implementation-sdk-{integration,e2e}.log`.
These are real-voter/public-iroh prerequisites, not a no-iroh qualification or
whole-suite timing result.

## Qualification ownership

The final focused commands, counts, durations and failures/corrections are
recorded in `/tmp/rocky-phase17-round38-implementation-handoff.md` and its logs.
The [performance record](17-schema-changes-through-seals-performance.md) retains
prior timing failures and the earlier round37 holdings result. No new full
matrix, full holdings, 100,000-row, native/Kotlin/Maven/artifact or device gate
belongs to this repair. Root owns those final gates and fresh HIGH review.
