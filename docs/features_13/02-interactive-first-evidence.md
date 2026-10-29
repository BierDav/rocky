# Interactive first: prerequisite audit

## 2026-10-29: blocked before slice 1

Audited change `lzoqqkwn`, starting commit `adb115fa`, parent `cb66b19f`
(`chore: merge phase 02 partial interactive-first work`). The working copy
was clean. No runtime changes or implementation slices were attempted.

The prerequisite is features_12 phase 2, bounded holds. Its roadmap was
removed by `0cc1eb01`; its last merged implementation and evidence remain
readable with:

```sh
jj file show -r 1dc6522c docs/features_12/02-bounded-holds.md
jj file show -r 1dc6522c docs/features_12/02-bounded-holds-evidence.md
```

That brief says phase 2 remains in progress. Its final evidence section,
"Sizing implementation verification and remaining scope", explicitly leaves
shared metadata scans, blockers, placements, tip prerequisites, publication
progress, restoration/custody frontier, rebootstrap discovery, move-following
and SQL connection planning unfinished. Source confirms surviving gaps:

- `replica/blockers.rs`: public `EncryptedReplica::blockers` acquires one
  background guard for `Inner::blockers`. The latter visits every domain;
  `derive_readiness` visits all domain scopes and rebootstrap needs under
  that guard. There is no per-acquisition page bound or release/revalidation.
- `replica/placements.rs`: public `wanted_placements` holds one background
  guard through discovery over the followed path and readiness scopes.
  The output request limit does not bound the seals/scopes examined when
  no requests are produced. There is no page release/revalidation.
- `replica/lock.rs` still delegates both acquisition classes directly to
  Tokio's mutex. It records classes and work, but has no waiter count,
  deferral or starvation guard; the interactive phase is not already present.

Completed foundation work is present: the group leader releases between
transactions, receive transactions admit up to four ordinary one-row frames,
settle uses one work unit per acquisition, checkpoint storage has row/metadata
steps, and passive WAL checkpointing runs outside the replica lock. Existing
regressions cover committed-prefix retry, receive step bounds, maintenance
resume, pruning and page reclamation. These do not establish bounded holds
for the remaining scans or qualify L0–L4 interactive latency.

The execution contract requires verification before editing and says to stop
and report a mismatch with the settled design. The prerequisite's settled
scan design requires bounded pages and validation before acting on a result.
Proceeding here would leave that foundation unmet. Resume this phase after
the surviving bounded-holds work is completed and verified against code;
start with slice 1, without treating this audit as implementation evidence.

No format changes, timing measurements, simulation changes or device runs
were made. Desktop/phone latency and paired sync gates remain unverified.

## Verification

- `./scripts/lint`: passed.
- `scripts/commitlint --jj`: passed.
- `./scripts/check`: passed formatting, workspace Clippy, 207 unit tests
  and 1,466 integration tests (1,142 workspace and 324 SDK without default
  features). Integration wall time was 246.94 s within its 300 s budget;
  the script's isolated checks also passed the per-test budgets. The host
  was heavily loaded during compilation and the workspace run.

The existing fixed-seed simulation tests are included in the integration
run. The e2e sweep, release measurements, 100k migration and device checks
were not run. No interactive probe was added, so these baseline results do
not satisfy slices 5–6 or any interactive completion gate.
