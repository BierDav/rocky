# 2. Deterministic simulation: evidence

Status: implemented 2026-10-25 on top of `3ed4ad8f`. Liveness is reported,
not gated, until [phase 4](04-recovery-that-drives-itself.md); see
[Known liveness seeds](#known-liveness-seeds) and [Not verified](#not-verified).

## What landed

- **Seeded hub faults.** `Hub::seeded(seed)` and `set_faults(a, b,
  LinkFaults)`: per-link latency, jitter, bandwidth, stalls of a given
  length, cuts after a drawn byte count (both directions together) and
  refused dials. `set_down` takes a link down (dials refused, open streams
  fail) and back up; `network_changed` already signalled network changes.
  Every draw comes from SplitMix64 (`Draws`) keyed by the seed and the
  stream's identity (direction, protocol, ordinal), never the wall clock.
  `events()` lists every stream opened, refused, stalled and cut.
- **Crash points.** `EncryptedReplica::arm_crash(CrashPoint)` in
  `qualification` builds (rocky-sdk's tests enable it): import, switch,
  prune, compact, settle, evict, and a group commit before or after it
  commits. The step fails inside its transaction, or right after the commit
  before anyone hears of it, and the simulation drops the node.
- **Hosted databases.** `Database::host` (test only) runs the real
  `Database` open, join and serve loop as a task of the caller's thread, so
  every node shares one current-thread runtime and paused clock;
  `Database::crash` drops its tasks mid-step as a process death would.
- **The simulation** (`rocky-sdk/src/simulation`). A seed draws the whole
  step list (`schedule::plan`), independent of outcomes, so a seed replays
  the same steps. Actions: write (two rows in one transaction), write in a
  partition, sync and pairwise sync, link faults, sever and heal, network
  change, advance the clock, restart, crash at a point (arming it and
  running the step that reaches it), seal, compact, open a branch at the
  newest seal, follow or leave a bookmark, create and share a partition,
  invite and join, revoke. Follow-ups favor the device paths: a member
  joining after a seal, a crash during a joiner's bootstrap, another node
  following a fresh branch, a seal while a branch is followed.
- **Safety after every step** on every open node: `audit()` of every scope
  (a refusal naming blockers is not a violation), no pending frame without
  a wait reason after the node's sync, both rows of every write or neither.
  `clears_by` is required by the `Blocker` type.
- **Liveness after healing:** all links up and fault free, every member
  open, introduced and following `main`, then K sync rounds (4 in the
  integration tier, 5 in the sweep). Converged means no blockers, no
  bootstrap marker and identical tables; a round with more blockers than
  the last fails at once ("blockers grew"). What blocks is reported by
  class (blocker kind and who clears it), without hashes.
- **Tooling.** `ROCKY_SIM_SEED=<seed> … simulation::tests::replay -- --ignored
  --nocapture` replays a seed with its steps; `simulation::tests::shrink`
  drops steps while the same violation remains; `ROCKY_SIM_TRACE=1` prints
  each step with its wall time.
- **Chaos.** The fuzzer seals, compacts, opens branches and follows
  bookmarks on a `main` bookmark the owner creates; healing follows `main`.
  `benchmarks/chaos/nightly` runs the range in `nightly-seeds` (seeds 1–8,
  120 steps) with schedule logs, failing on any finding or when no
  schedule reached a seal, a follow and a compaction;
  `.github/workflows/chaos.yml` runs it nightly.

## Defects the simulation found

Each is fixed and kept as a regression (`simulation::regressions`, plus
one node test); the shrunk schedules are in the test bodies.

| Seed | What broke | Fix |
| --- | --- | --- |
| every seed | An owner syncing with a writer kept a `SyncRequest` blocker for the issuer registry, which the writer never holds, so it never cleared | A scope the peer does not share (`Transfer.refused`) is no blocker; only failed and denied requests are (`node::sync_tests::status::a_scope_the_peer_does_not_share_is_not_a_blocker`) |
| 10187, 10096 | After branching from a compacted seal and sealing the branch, finalization folded a write made past an ancestor tip's seal into the identity ledger; the audit's recomputation did not | `row_replay_ledger` and checkpoint replay admit a frame by its seal frontier (`visible_frame`), not only by its tip being on the path |
| 10041 | Leaving a compacted branch for a line with an unsent write lost the compaction, and every scope kept the branch's rows, also those whose base stays on the line | `rebuild_tip_workspace` rebuilds the scopes whose base is on the path; only off-path bases wait for the rebootstrap |
| 10095 | A member that bootstrapped while the owner followed a branch audited a base signed off its followed path as divergence | The audit refuses with the scope's blockers when its base is off the path |
| all (virtual clock) | The SQL transaction watcher converted a wall-clock deadline to the runtime clock, so under a paused clock ahead of real time every transaction timed out at once | The watcher sleeps the transaction timeout on the runtime clock |

## Measurements (2026-10-25, macOS, 18 cores, debug build)

| Gate | Result |
| --- | --- |
| Same seed, byte-identical hub transcript and events | `the_same_seed_yields_byte_identical_hub_transcripts`, every fault kind present |
| Integration seed set | 8 seeds of 3 nodes and 24 steps, each 0.55–1.32 s alone; 5 regressions 0.35–0.73 s; budget 2 s per test |
| Same seed, same step outcomes | seeds 0–7 run twice: identical step lists and outcomes, scope hashes aside |
| e2e sweep | 1,000 seeds (10000–10999, 3 or 4 nodes, 32 steps) in 627.9 s wall on 16 shards; recorded suite-budget exemption in `.config/test-tiers.toml` |
| Safety failures in the sweep, final revision | 0 of 1,000 |
| Liveness, final revision | 955 converge; 45 known, below |
| Chaos fuzz, seed 1, 120 steps | 3 seals, 2 follows, 1 compaction in `schedule.log`; 12 min |

The sweep is I/O bound (every commit fsyncs a real SQLite file): the
320-seed baseline sweep used 587% CPU on 18 cores.

## Mutation check

Each fix reverted in a scratch build, swept over seeds 10000–10319, then
restored:

| Reverted fix | Found by the sweep |
| --- | --- |
| Reset replay applying pending frames (`ledger_waits` returns early without a pruned prefix) | All 320 seeds fail safety, from seed 10000 step 0: the identity ledger differs from its history |
| Recovery skipping an unverifiable proof (`try_scoped_holding` returns `Ok(false)` for an incomplete batch) | No safety failure; 174 seeds no longer converge behind `SyncRequest` blockers (16 with blockers growing), against none with that class in the baseline |

## Known liveness seeds

Until phase 4 these are reported, not failed. Seed lists are of the final
sweep (seeds 10000–10999):

| Seeds | Classes |
| ---: | --- |
| 19 | tables differ: 10020, 10023, 10050, 10134, 10145, 10164, 10174, 10230, 10329, 10335, 10397, 10504, 10555, 10614, 10702, 10743, 10917, 10953, 10958 |
| 11 | finalization waiting, pending frames, unreadable: 10123, 10153, 10237, 10373, 10381, 10391, 10522, 10643, 10693, 10717, 10841 |
| 5 | finalization waiting, tables differ: 10248, 10437, 10604, 10721, 10832 |
| 4 | pending frames, unreadable: 10151, 10354, 10795, 10815 |
| 2 | bootstrap marker, tables differ: 10236, 10777 |
| 1 | bootstrap marker, finalization waiting, importing, sync request, blockers grew, tables differ: 10408 |
| 1 | finalization waiting, pending frames, tables differ, unreadable: 10961 |
| 1 | finalization waiting, seal rebootstrap: 10114 |
| 1 | seal rebootstrap, unreadable: 10095 |

Every blocker named clears by any permitted peer, yet K rounds with every
peer reachable did not clear it: these are phase 4's inputs. The
integration seeds all converge (`KNOWN_LIVENESS` is empty); a change in
their liveness fails the tier, so the list stays exact.

The first chaos fuzz with seals and compaction reported a full node still
waiting for a seal rebootstrap after healing (four `FINDING:` lines), which
the nightly job fails on until phase 4.

## Checks

`./scripts/lint` passes. `./scripts/check` passes formatting, clippy, the
unit tier and the integration tier's suite budget (1,352 passed, 217.4 s of
300 s), with two failures that `main` (`3ed4ad8f`) has too:
`rocky-secure tests::tips::frontiers::headers::excluded_and_other_domain_headers_still_decode_after_warming`
fails there as well, and `rocky-sdk::tips
races::revoking_the_identity_whose_seal_won_makes_every_device_follow_the_next_winner`
takes 2.11–2.15 s alone there and 1.99–2.02 s here, against 2 s. The full
`rocky-secure` suite otherwise passes, its two e2e measurements alone.

## Not verified

- The nightly chaos workflow has not run on CI; its script ran locally.
- Byte-identical transcripts are proven for the hub with deterministic
  endpoints. Whole simulated nodes use real keys, nonces and Tokio's
  randomized `select!`, so their bytes differ between runs; step outcomes
  were identical in every replay tried.
- In-process crashes drop tasks and connections; they do not lose writes
  the operating system already took, as a power cut would.
- Kotlin and the shopping app are unchanged, so their checks were not run.
