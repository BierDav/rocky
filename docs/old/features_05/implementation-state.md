# features_05: implementation state (orchestrator's record)

Live record of the parallel implementation of `docs/features_05`, maintained
by the orchestrator thread. It captures what exists today on `main`, which
phase chains are waiting to land, and the lane/thread state that drives the
next launches. The [roadmap README](README.md) is the dependency and gate
authority; each [phase brief](README.md#phases) holds its own measured status.
This file answers "where is the work right now, and what happens next".

Updated 2026-10-08 ~18:10 CEST.

## Current state (18:10 CEST)

- **`main` = `qqxlzono` (commit `3907d368`)**: **P15 landed** (Horizon). Chain on
  `xxtzvmos` (P14) -> `mryywpnt` (signed proposals + durable acknowledgments) ->
  `klkkxymu` (preparatory gates, incomplete verdict) -> `rplnlmwn` (format-5
  declaration preservation + writer fencing) -> `qqxlzono` (finalization, signed
  paged existence bases, definite drops, SDK integration, forced rebootstrap,
  completion evidence).
- **Wave 2 COMPLETE and verified (historical record).** Its example gates have since been removed. The reconcile agent
  `f05-wave2-rebase-reconcile-v1:0` resolved P13's four conflicts and P11's thirteen
  conflicts; the finisher `f05-wave2-rebase-finisher-v1:0` (claude-david) squashed a
  `cargo fmt` drift into `zrnxxxsynpuq` and certified **`WAVE 2 REBASE READY: yes`**:
  the removed market gate reported **42/42** (the two earlier step-[6] failures were
  load flake, no code change), `./scripts/check` **exit 0**, the **200-partition
  measurement within every budget**, and the P13 gate set green (secure
  `confirmed_tables` 3, core `command_controlled` 2, sdk `confirmed_tables` 1,
  room-qualification 11, schema-qualification 4, check-schema pass). `main` moved to
  `owossztsrwlv`; both wave-2 workspaces removed; both wave-2 threads settled.
- **Decision numbers:** `0032` = P09 encrypted strong history; `0033` = P14 existence
  log (draft record, refined by P14); `0034` = P11 partitioned scopes; `0035` = P12
  hidden rows/unique indexes/FKs. P13 records its format change as a dated section in
  decision `0025` (no new number).
- **P13 COMPLETE (`PHASE 13 COMPLETE: yes`)** - thread `f05-p13-finisher-1:0`
  (Magdalena) reported every gate green; its four P12<->P13 conflicts were resolved
  by the wave-2 rebase agent and the workspace was torn down after landing.
- **P11 COMPLETE (`PHASE 11 COMPLETE: yes`)** - thread `f05-p11-finisher-1:0`
  (David Codex/gpt-6.1-sol) reported full `./scripts/check` exit 0, Kotlin/schema/
  Room checks, native ABI, packaged consumer and driver tests, and the 200-partition
  measurement within the recorded budgets. Rebased onto P13 and landed.
- **Wave 3 COMPLETE and landed.** Thread `f05-p14-launch-v1:0` (claude-david)
  certified `PHASE 14 COMPLETE: yes`: `./scripts/check` exit 0 (secure suite 137
  passed + 6 ignored), `tests::existence` 5, `./sdks/kotlin/check`, `check-schema`,
  `./sdks/native/check`, schema-qualification 4, room-qualification 11 all green;
  every completion-gate bullet proven by a test. Landed at `xxtzvmos`; workspace
  `rocky-db-p14-existence-log` torn down; thread settled. Limitations recorded in
  decision `0033`: partitioned tables cannot yet have or be referenced through an
  existence log; existence-log tables must be keyed by `id`; logs are not pruned
  until P15.
- **Wave 4 COMPLETE and landed.** P15 needed three runs: `f05-p15-launch-v1:0`
  (`codex_1`) delivered the signed-cut/acknowledgment protocol; finishers
  `f05-p15-finisher-v1:0` and `f05-p15-finisher-v2:0` (`codex`/`codex_1`) added
  format-5 declaration preservation + fencing, then the finalization core.
  `f05-p15-finisher-v2:0` certified **`PHASE 15 COMPLETE: yes`**: `./scripts/check`
  exit 0 (606 workspace tests, 117 no-default SDK tests, secure suite 153 incl. the
  4,100-command soak), credentials 23, horizon 21, existence 20, native/Kotlin/
  schema checks, schema-qualification 4, room-qualification 12, SQLite 5. Storage
  **plateaus at 170 pages / 696,320 bytes at both 16 and 64 cycles, with zero
  retained tombstones or existence records**. Landed at `qqxlzono`; workspace
  `rocky-db-p15-horizon` torn down; all P15 threads settled. Single-phase wave, so
  the wave-level rebase/squash was a no-op (chain verified linear and conflict-free).
- **P16 (wave 5, final) LAUNCHED** on `codex`/gpt-6.1-sol in a fresh jj workspace
  `rocky-db-p16-shopping-qualification` (@ `qxlwxwzq`, parent `main`); thread
  `f05-p16-launch-v1:0` (MCP namespace `8f54b753-7531-4aba-a8e7-1a797a9ecef9`).
  Delivers the packaged Kotlin shopping-list app, all seven acceptance scenarios and
  end-to-end evidence.
- **Operational note:** a thread that launches a long background command stays
  `running` (blocked) and T3 wakes it when the command exits - do **not** settle or
  duplicate it. `t3_thread_organize settle` refuses a thread with blocked work
  ("active or blocked work and cannot be settled").
- **Lane policy (standing):** exactly four usable lanes, one agent per lane, no
  overall cap - `claudeAgent_david_claude`, `claudeAgent_claude_magdalena`,
  `codex_1`, `codex`. Claude was preferred during the wave-2 rebase; the user asked
  on 2026-10-08 to **use the Codex providers again**; wave 4 (P15) ran on `codex_1`
  and `codex`, and wave 5 (P16) runs on `codex`. `claudeAgent` (Alex) is forbidden;
  opencode/grok/pi/cursor/antigravity are not approved.
- Orchestrator thread `7816755a-bdd4-4358-a342-72cd2ecf4e9f`; threads launched via
  `create_threads` use `thread:mcp:<server-uuid>:<name>:N`. Waves 0-4 used
  `c2523c6d-80cf-4ca1-ab3e-5481ce7ed47a`; the P16 launch returned namespace
  `8f54b753-7531-4aba-a8e7-1a797a9ecef9` - always use the full returned thread ID.
- **Settled so far:** `f05-wave2-launch:0` (P11 original), `:1` (P12 usage-limit),
  `:2` (P13 original), `eeef63ee` (P14 draft), `f05-p12-restart-1:0` (opencode),
  `f05-p12-restart-codex1:0` (P12, landed), `f05-p13-finisher-1:0` (P13, complete),
  `f05-p11-finisher-1:0` (P11, complete), `f05-wave2-rebase-reconcile-v1:0`
  (reconcile), `f05-wave2-rebase-finisher-v1:0` (wave-2 certification),
  `f05-p14-launch-v1:0` (P14, complete and landed), `f05-p15-launch-v1:0` (P15
  first run, incomplete -> finisher), `f05-p15-finisher-v1:0` (P15, checkpoint
  preservation), `f05-p15-finisher-v2:0` (P15, finalization core, complete and
  landed).

## HISTORICAL (superseded): P09 was NOT actually landed; recovery in progress (05:55)

- **`uotrmlntlupv` — the "squashed P09 landing" from the previous tick — is an
  EMPTY commit** (`0 files changed` vs its parent `noprtkuwmzlr`). The `jj squash`
  invocation opened an interactive editor (`pico`), failed, and landed nothing.
  Main's tree therefore still lacks every P09 artifact:
  `docs/decisions/0032-encrypted-strong-history.md`,
  `crates/rocky-secure/src/tests/strong_history.rs`,
  `docs/runtime/strong-history.md`, etc.
- Cause: the earlier P09 chain was based on `poyzwovq` (P07) and my rebase
  attempts split it (pquznpum and srkotrpn were individually moved onto
  `noprtkuwmzlr`, nnouvrqm/zlozxtwv stayed on `poyzwovq`), leaving a divergent,
  tangled graph that the hash-set rebase could not move.
- **Recovery (in progress):** the four original commits still exist with their
  original trees (`56cb5bc63508` → `37ccc68c95c0` → `f2c0dd900ec4` →
  `99171354e826`, all on `poyzwovq`). Rebuilt the correct result as a jj 3-way
  merge `jj new noprtkuwmzlr 99171354e826` (merge base = `poyzwovq`), which
  combined P08 slice 6 + shopping + P09. Exactly one conflict
  (`docs/features_05/README.md`, the roadmap status paragraph) — resolved by
  combining "Phase 8 is complete …" and "Phase 9 is complete …". All other 31
  files merged cleanly. Restored that tree as a linear commit on `main`
  (`noprtkuwmzlr`) in workspace `rocky-db-p09-recovery`.
- **Gate:** `./scripts/check` re-launched **detached** in `rocky-db-p09-recovery`
  (writes `EXIT=` to `/tmp/p09-recovery-check2.exit`, log
  `/tmp/p09-recovery-check2.log`). The first run's wrapper was SIGTERM'd by the
  host mid-`cargo test` (only an orphaned `rocky_secure` test survived), so its
  result was discarded and the orphan killed. On exit 0 the linear commit
  `lsyzkyrv` (parent `noprtkuwmzlr`, resolved README, 32 files) becomes `main`
  and the wave-2/p14-draft chains rebase against it. If it fails, fix before
  landing.
- **Implication for wave 2:** P11/P12/P13 and the P14 draft were launched from
  the broken `uotrmlntlupv`; their chains must be rebased onto the corrected
  `main` on landing (rule 4). P09 touches `crates/rocky-secure*` + docs; P11/P12
  touch different areas, so rebases should be small.

## Wave-2 launched (05:45)

- Wave 1 complete (P07, P08+s6, P09, P10 + shopping green), main tip `uotrmlntlupv`.
- **Wave 2 launched** — three phases in fresh workspaces from `main`, three
  lanes (per user authorization of 4 parallel agents on different providers):
  - P11 Partitioned scopes → `codex_1`/gpt-6.1-sol, thread
    `f05-wave2-launch:0`, workspace `rocky-db-p11-partitioned-scopes` (@ zrnxxxsy)
  - P12 Hidden rows, unique indexes and foreign keys → `claudeAgent_david_claude`/opus, thread
    `f05-wave2-launch:1`, workspace `rocky-db-p12-hidden-rows-unique-indexes` (@ rvqlrylk)
  - P13 Confirmed rows in Room → `claudeAgent_claude_magdalena`/opus, thread
    `f05-wave2-launch:2`, workspace `rocky-db-p13-confirmed-rows-room` (@ lyymzknl)
- All three threads started; each brief includes workspace path, phase brief
  reference, gates (scripts/check green + COMPLETE verdict line) and jj-only rules.
- **P14 decision-record draft (docs-only) RUNNING on the `codex` lane** (user
  restart). Rules in force: the `claudeAgent` (Alex) provider must **not** be
  used — its `f05-wave2-p14-draft:0` thread is settled/cancelled. The P14
  *implementation* must not run in parallel with phase 13, but the docs-only
  draft may. The user restarted the draft as a new top-level thread
  `eeef63ee-6854-4cb5-8abb-7fce1a2e5cdd` on **`codex`** (display "david (Codex)",
  gpt-6.1-sol) — the "codex-david" lane the user asked to distribute work to.
  It works in `rocky-db-p14-existence-log-draft` (@ `ppxnqxus`: proposal
  `docs/decisions/0033-existence-log-and-puppets.md` + registry + brief link;
  0032 reserved). Rebase onto corrected `main` on landing (rule 4). **Allowed
  lanes now:** `codex_1`/gpt-6.1-sol, `codex` (David Codex)/gpt-6.1-sol,
  `claudeAgent_david_claude`/opus, `claudeAgent_claude_magdalena`/opus.
- **Status @05:47**: all three threads `running` (P11 already editing
  `crates/rocky-core/src/lib.rs`). A leftover duplicate workspace registration
  (`p12-hidden-rows-unique-indexes`, from the failed first setup attempt, same
  directory) was forgotten; the three active workspaces are the only extras on
  top of `default`. No 4th agent possible this wave: P14 needs P11+P12 landed,
  P15 needs P12+P14, P16 needs all — so parallelism is capped by dependencies,
  not lanes.

- **P08 slice 6 LANDED** at `xvzyoquqszny a3eb7a5c`, full gate green (scripts/check 411s), workspace torn down; thread settled.
- **Shopping repair LANDED** at `noprtkuwmzlr 7fd7434e` (format-2-only Kotlin SDK + shopping example migrated; all gates green, shopping 22/22); workspace torn down; thread settled.
- **P09 LANDED** at `uotrmlntlupv 29210b09` (strong history behind checkpoint; 986s scripts/check green; gates complete). Squashed landing of the 4-commit P09 chain onto main (after shopping repair); p09 workspace torn down; thread settled.

**Main tip:** `uotrmlntlupv`. All wave-1 work verified. All workspaces torn down except default. All lanes free.

- **P08 slice 6 LANDED**: all three entity rules implemented at import,
  housekeeping and driver layers with refusal + regression tests. `autoGenerate
  = true` refused at import and as `AUTOINCREMENT` in Room's CREATE TABLE;
  entities without `id`/`row_kind` refused; natural-key entities must declare
  the `<table>_identity` index; `SELECT rocky_next_id('t')` on a writer
  returns the table author's next id (no profile needed), refuses outside a
  write txn (1), on a reader (8), bad argument; rollback releases; later
  reservations continue the sequence, commit verifies every inserted id.
  Chain landed on `main` at `xvzyoquqszny a3eb7a5c` (5 commits: Cargo.lock
  refresh for the phase-7 rocky-storage dep + 4 slice-6 commits + doc-status
  commit). Full gate green: scripts/check exit 0 in 411 s (537 passed);
  sdks/native/check 0; rocky-sqlite 5/5; room-qualification 10/10 twice;
  schema-qualification 2/2; check-schema + sdks/kotlin/check pass; desktop
  build pass; driver tests 7 passed. Deviations recorded: fixture re-recorded
  (49 statement shapes, same pinned Room 2.8.4); upsert probe now compares
  against bundled engine codes (2067) instead of hardcoded 1555;
  oversized-transaction test on a 60 s lease (debug native ~2.7 ms/insert on
  JVM); new test counts commits via table events to avoid a SIGBUS from a
  second SQLite copy. p08slice6 workspace torn down; slice-6 thread settled.
- **Shopping surface on main: 8 passed / 14 failed — REAL wave-1 integration
  gap, now owned by the shopping-repair finisher** (correcting the 03:55
  record, which was measured against the stale default checkout):
  - The default checkout `~/RustroverProjects/rocky-db` (`@ slwwozlw`) hangs
    off the OLD P05-era chain (`ooxoluotlmpp`) and is NOT at `main`; my earlier
    "24/25 one-stale-assertion" reading ran against that stale tree. Also
    `schema.rs` there predates the refusal.
  - Measuring in the clean workspace `rocky-db-shopping-fix` (true main copy):
    `./examples/shoppinglist-app/check` → shared JVM **8 passed / 14 failed**,
    all 14 with `legacy register schemas are unsupported; use format-2
    definition tables; export with the original build and reinstall into a new
    database`. Same failure set the slice-6 agent reported (it was right; its
    count was measured in the p08slice6 workspace, not the stale default).
  - Root cause: P07's `notxysxlzyus a97fada8` made crate-side
    `Schema::validate` refuse every schema without definition tables
    (`crates/rocky-sdk/src/schema.rs:96`), but the Kotlin SDK's `Schema`
    (`sdks/kotlin/rocky-core/src/Schema.kt`) still defaults `definition =
    false` and emits the old `collections` format in `json()`. Every
    legacy-style construction in the shopping example (ShoppingSchema in
    `ShoppingRepository.kt`, `authors` in CredentialPrimitivesTest, `stock` in
    ShoppingIntegrationTest, ReservationSchema in Reservation.kt) is refused;
    the Room-based tests using the definition builder pass. P07's Rust gate
    (`./scripts/check`) does not cover this Kotlin surface — genuine gap.
  - The 03:55 "shopping-fix" thread (Magdalena) correctly declined to do it as
    a one-line version assertion and produced the real finalizer. **Settled**.
- **Shopping repair LANDED**: format-2-only Kotlin SDK, SQL writes replace legacy actions, shopping example migrated to keyed tables + SQL writes; engine rules unchanged. Gates all green (shopping 22/22, desktop build, sdks/kotlin/check, sdks/native/check, scripts/check exit 0). Chain landed at `noprtkuwmzlr 7fd7434e` on `main` (on top of P08 slice 6); shopping workspace torn down; repair thread settled. Android/market-app remain documented as out of scope.
- **P09 LANDED**: strong history behind the checkpoint (squashed landing of the 4-commit P09 chain) landed on `main` at `uotrmlntlupv 29210b09` on top of `noprtkuwmzlr`. The agent's verdict was `PHASE 9 COMPLETE: yes` (986s `./scripts/check` green, 4,100-command gates, bootstrap/recovery/prune semantics, reopen latency measured). p09 workspace torn down; P09 thread settled. **Wave 1 complete** (P07, P08+s6, P09, P10, shopping surface green). Main tip: `uotrmlntlupv`.
- **P09** (Sol/codex_1, running): chain in `rocky-db-p09-strong-history` is based on `poyzwovq` (P07), so it MUST be rebased onto `main` (`noprtkuwmzlr`, which includes P08 slice 6 + shopping repair) before landing. The agent reported COMPLETE: yes with 986s full check, strong base/horizon/pruning implemented, 4,100-command gates, reopen latency measured, committed 4 commits. Will rebase to main and land next tick.

## Wave-1 scoreboard (updated 03:15)

- **P07 FULLY LANDED**: perf finisher fixed the one red driver paging test with
  a private `(scope,key,row)` lookup index in rocky-secure (`replica/identity.rs`
  + `apply.rs` + `replica.rs` + `replica/typed.rs` + tests): winner lookup was
  decoding every stored incarnation per key and SQL commit verification
  repeated that scan for all 500 deleted rows (~52 s). Fixed commit
  `poyzwovqmplp 5038042b` ("index secure incarnation keys and close the
  driver latency gate"): the unchanged 5-second assertion passes, the 500-row
  delete is now 1.85 s; driver families 6/6 in ~8 s (with and without default
  features); full unfiltered `./scripts/check` exit 0 in 409.71 s; locked
  suites green (core 30u/9i/3d, secure 117+5, runtime 156). Every
  completion-gate bullet passes. **Landed on `main`** by the orchestrator
  (bookmark move; parent was exactly main → zero conflict), p07 workspace
  torn down, v3 and perf-finisher threads settled. 17-commit P07 chain (11
  original + 6 follow-ups incl. the fix) is on `main`.
- **P09 + P08 slice 6 launched** (rule 4, unblocked by P07's gate):
  - P09 strong history behind the checkpoint → `codex_1`/Sol, workspace
    `rocky-db-p09-strong-history` from `main` (poyzwovq) — running.
  - P08 slice 6 (refuse autoGenerate, require id/row_kind, id-before-insert)
    → `claudeAgent_david_claude`/opus, workspace `rocky-db-p08-slice6` from
    `main` (poyzwovq) — running.

## Earlier tick notes (02:15)

- The first P07 completion attempt died before committing (work uncommitted in
  the working copy); two relaunch attempts on the `codex` instance failed at
  launch in ~2s (provider issue — rotated to `codex_1`). The rebase onto the
  new main at that time produced one doc conflict (schema-builder.md) which
  the completion run resolved by retaining P08's Room wording and making the
  format-2 requirement explicit. Full detail of the P07 removal work it
  verified: format-1 admission removed everywhere, read-only refusal before
  lock files with race-check repeat, scanner namespace edges (application
  tables ending `_rocky_logical_schema` untouched), fixture migration to
  format-2 IDs, awaited-close fix, docs + evidence.
  `codex` lane presumed broken at launch until evidence otherwise.

Deviation notes from P10's report: allowing issuer revocation is an amendment
to decision 0023 (required by slice 3); pairing request gains an optional
address field and the sealed renewal payload changes to `{keys, issuers}`
(new builds read old payloads, older builds refuse new — matches the
"upgrade every device of a domain" rule); one commit for all three slices
(they touch the same functions); `sdk-enrollment.md` is 334 lines (gate names
that file).

## P10 — landed (2026-10-08)

P10 reporter verdict was **PHASE 10 COMPLETE: yes** (all 3 slices, all gate
bullets, checks green on its original base). The chain was rebased onto the
new `main` (after P08), and rebased-tip re-verification found 2 failures
(both traced to the rebase over P08's landing, not P10 defects):

1. **Shopping gate tests**: P10 wrote `IssuerRecoveryTest` against old main
   where the shopping table was `items`; P08's landing renamed it to
   `shopping_items`, and additionally turned it into a definition table
   written through Room. The landing finisher updated the test to the landed
   schema (open-Room path via `ShoppingRepository`, profile assertion
   `Writer(setOf("shopping_items"))`).
2. **`sdks/native/check` formatting only** (pre-existing on main from P08's
   driver landing — `sql.rs:77` line split and `lib.rs` mod order; that
   surface is outside `./scripts/check`). Committed as `rstmszowzqyn` by the
   landing finisher.

The landing finisher (`f05-wave1-p10-landing-finisher:0`, Magdalena) fixed
both, ran all five check surfaces green (scripts, native, Kotlin, packaged,
shopping 22/22) and certified **P10 LANDING READY: yes**. Landing, p10
teardown, and thread settle were completed by the orchestrator. The agent's
deviation-4 fix (`vsqtvwlw`, identity v7→8 test assertion) became redundant
after the rebase and was abandoned (P08 already carried it; recoverable in
jj).

Deviation notes from P10's report: allowing issuer revocation is an amendment
to decision 0023 (required by slice 3); pairing request gains an optional
address field and the sealed renewal payload changes to `{keys, issuers}`
(new builds read old payloads, older builds refuse new — matches the
"upgrade every device of a domain" rule); one commit for all three slices
(they touch the same functions); `sdk-enrollment.md` is 334 lines (gate names
that file).

## Current `main` — wave 0 + P08 + P10 + P07 landed

`main` = change `poyzwovq` (commit `5038042bad7b`), the P07 chain tip ("index
secure incarnation keys and close the driver latency gate"). Wave 0 (phases
1–6) plus its flake fix, the whole P08 sqlite-driver/Room chain (slices 1–5 +
7; slice 6 running), the whole P10 issuer-recovery chain, and the whole P07
row-identity chain are on `main`.

| Change | Commit | Phase slice |
| --- | --- | --- |
| `poyzwovq` | `5038042bad7b` | 07 index secure incarnation keys + driver latency gate (P07 landing tip) |
| `xoxkttrm` | `53197fd0165e` | 07 correct the core doc-test total in final evidence |
| `uumnmxpm` | `50fb177e9c40` | 07 record legacy gate evidence and unresolved driver latency |
| `ptossmpo` | `84aaa922c601` | 07 retain identity fixture hashes across the Room rebase |
| `kptzwwpu` | `81c0c0c657f5` | 07 migrate landed SQLite driver fixtures to numeric identities |
| `lozukqtm` | `fe495ba0c853` | 07 preserve Room schema version after legacy domain removal |
| `notxysxl` | `a97fada86c85` | 07 remove legacy typed admission and refuse old files before writes |
| `olylqkxz` | `b787000428a3` | 07 qualify populated consumer reopen and Kotlin identity schemas |
| `smuupmzy` | `0211781fd2b1` | 07 preserve shadow writes and enforce identity columns on local tables |
| `vzxtwxwp` | `349908416051` | 07 slice 6: transport decimal IDs through consumer SQL and checkpoint rows |
| `uumptmvw` | — | 07 slices 3–5: allocate trusted IDs and retain numeric targets and sequence marks |
| `rwrtowok` | — | 07 slices 4–5: retain receive marks and merge secure numeric incarnations |
| `wnlnmkkn` | — | 07 slice 3: register consumer table pseudonyms and guard trusted identity fields |
| `lvxwukkz` | — | 07 slice 3: qualify failure rollback and author-number refusal |
| `smtlnnpm` | — | 07 slice 3: reserve secure row identities and sign per-table bundles |
| `sxnpspnk` | — | 07 slice 2: qualify shared columns on secure and trusted nodes |
| `nswxxppy` | — | 07 slice 2: generate explicit versioned identity descriptors |
| `onrwzkyw` | — | 07 slice 1: record incarnation identity format and refusal path |
| `rstmszowzqyn` | `c9e81d9c6380` | sdks/native rustfmt after the sqlite driver landing (P10 landing cleanup) |
| `zypwyqmstpyt` | `c9b4b39d9106` | 10 shopping recovery test on the landed `shopping_items` Room schema |
| `quqspluysono` | `5dbacd8d5547` | 10 issuer recovery: second issuer by default, issuer contacts, revoking a lost creator |
| `tqtsvpusmpqw` | `e7de2c2a9527` | 08 remove the Room projection adapter (P08 landing tip) |
| `nyuwyymrlvmr` | `e293268b8878` | 08 completion gate through Room |
| `yzqyzmltqwzq` | `2a0c0b03` | 08 shopping example on plain Room DAOs (renamed table to `shopping_items`) |
| `ynqmlwnopktu` | — | 08 invalidation from table events |
| `tqtmyymrtkzu` | — | 08 Room 2.8.4 housekeeping from the session catalog |
| `xyupqxoxssmz` | — | 08 rocky-sqlite androidx.sqlite driver |
| `wtyymlszslkn` | — | 08 typed statement, cursor and table-event handles |
| `rwzmsvtp` | `ed70f85219d9` | wave-0 flake fix (stable signing time for service control retries) |
| `ooxoluot` | `c2513104` | 05 qualification on an emulator (wave-0 chain base) |

Below `c2513104` sit the rest of wave 0: P05 (keystore/lifecycle/packaging),
P01 (primitives/pseudonyms/Kotlin qualification), P06 (keyless custody +
enrollment forwarding), P04 (keyed handler reads), P02 (schema builder),
P03 (bootstrapping). `main@origin` stays behind (remotes are never pushed in
this workflow; only local `main` advances).

Landing history recap:
- Wave 0 fully landed and certified green (597 passed, 0 failed, 12 ignored)
  at `rwzmsvtp ed70f85219d9` — see earlier ticks for the P06 rebase handover,
  the custody↔bootstrap regression fix (folded into the P06 tip), the
  P01/P05 landing by `claudeAgent_david_claude`, and the `RequestConflict`
  flake fix by the wave-0 flake fixer (SDK reused a request ID across a
  whole-second retry boundary; fix = time-free input fingerprint +
  first-attempt time kept under the service lock; deterministic regression
  tests force the next-second boundary).
- **P08 landed 2026-10-08** by the orchestrator: chain parent was exactly
  `main` (zero-conflict bookmark move to `tqtsvpusmpqw`) after the P08
  finisher certified the phase complete (513 passed, exit 0). A later check
  of the rebased P10 tip surfaced a `sdks/native` rustfmt drift on P08's
  landed driver code (that surface is outside `./scripts/check`); it was
  fixed by the P10 landing finisher as `rstmszowzqyn`.
- **P10 landed 2026-10-08**: rebased by the orchestrator onto the new main
  (after P08), then handed to a landing finisher (Magdalena) after the
  rebased-tip verification found two failures: `IssuerRecoveryTest`
  referenced the pre-P08 table name `items` (P08 renamed it to
  `shopping_items`; plus the schema became a definition table written through
  Room, so the test now opens Room via `ShoppingRepository`), and the
  `sdks/native` fmt drift. The finisher fixed both, ran all five check
  surfaces green, and certified `P10 LANDING READY: yes`.
- **P07 landed 2026-10-08** (final): after the v3 completion finisher held a
  red full gate on only the driver paging test (5 s bound exceeded by the
  500-row delete under format-2 winner lookup), the perf finisher added a
  private `(scope,key,row)` lookup index in rocky-secure, cutting the delete
  to 1.85 s with the assertion unchanged, and certified the full unfiltered
  gate green (exit 0, 409.71 s). Landed by the orchestrator: parent was
  exactly `main` (zero-conflict bookmark move to `poyzwovq`).

## Wave status

| Wave | Phases | Status |
| --- | --- | --- |
| 0 | 1, 2, 3, 4, 5, 6 | **Certified green on `main`** |
| 1 | 7, 8, 9, 10 | **Certified green on `main`** |
| 2 | 11, 12, 13 | **Complete and verified; landed on `main` (`owossztsrwlv`)** |
| 3 | 14 | **Complete and verified; landed at `xxtzvmos`** |
| 4 | 15 | **Complete and verified; landed at `qqxlzono`** |
| 5 | 16 | **running** (codex, `f05-p16-launch-v1:0`) |

Critical path: 1 → 7 → 11 → 14 → 15 → 16. P1 landed; P7 in its final
(legacy-removal) finisher; P11 may start its credential slice (needs 1) beside
7 in wave 2, with the frame-format slice waiting for 7's gate.

## Lanes and active threads

| Lane (provider instance) | Model | Thread | Task | Status |
| --- | --- | --- | --- | --- |
| `codex_1` | gpt-6.1-sol | `f05-wave1-launch-p09-and-p08s6:0` | P09 strong history behind the checkpoint | **completed — awaiting rebase+land onto main (noprtkuwmzlr)** |
| `claudeAgent_claude_magdalena` | claude-opus-5-5 | (free) | — | free after shopping repair (landed) |
| — (settled) | — | `f05-wave1-shopping-repair-format2:0` (Magdalena) | Kotlin SDK format-2 + shopping repair | completed — settled; landed |
| `claudeAgent_david_claude` | claude-opus-5-5 | (free) | — | free after P08 slice 6 |
| — (settled) | — | `f05-wave1-launch-p09-and-p08s6:1` (David) | P08 slice 6 entity rules | completed — settled; landed |
| — (settled) | — | `f05-wave1-shopping-version-fix:0` (Magdalena) | Shopping version-assertion probe | completed — settled; verdict `no` with real root cause → spawned repair finisher |
| — (settled) | — | `f05-wave1-launch-p09-and-p08s6:1` (David) | P08 slice 6 entity rules | completed — settled; landed |
| — (free after use) | — | `f05-wave1-launch-07legacy-10:0` (codex) | P07 legacy removal (first finalizer) | failed mid-check — settled; uncommitted work handed to completion finisher |
| — (free after use) | — | `f05-wave1-launch-07legacy-10:1` (Magdalena) | P10 issuer recovery (report run) | completed — settled; chain rebased, handed to landing finisher |
| — (free after use) | — | `f05-wave1-p07-finisher:0` (codex_1) | P07 first finalizer | completed — settled; handed to legacy-removal finisher |
| `claudeAgent_david_claude` | claude-opus-5-5 | (idle) | — | reset ~01:30 UTC; free for a next task |
| — (free after use) | — | `f05-wave1-p08-finisher:0` (Magdalena) | P08 finalizer | completed — settled; phase landed |
| `claudeAgent_david_claude` | claude-opus-5-5 | (idle) | — | reset ~01:30 UTC; free for a next task |

Settled: both wave-1 finalizers (P07, P08), wave-0 flake fixer, wave-0
verification finisher, merge-recovery-opus, P06 rebase, P07/P08 first runs,
and earlier P01–P05 threads.

## Workspaces

| Workspace | Path | @ |
| --- | --- | --- |
| default | `~/RustroverProjects/rocky-db` | `slwwozlw` (wc: skill files + `docs/features_05` records) |
| p09 | `~/AgentWorkspaces/rocky-db-p09-strong-history` | `xsrvmxss` (chain based on poyzwovq; needs rebase onto main noprtkuwmzlr) |
| shopping-fix | `~/AgentWorkspaces/rocky-db-shopping-fix` | (torn down) |

Torn down 2026-10-08: p07, p08 (slice6), p08, p10 (`jj workspace forget` +
directory removal after landing), plus earlier phase workspaces p01, p05,
p06, wave0-flake-fix. Nothing uncommitted was lost.

## Next moves

1. **Wave 1 is fully verified** (P07, P08+s6, P09, P10, and shopping surface green) — all wave-1 phases landed and settled. Main tip: `uotrmlntlupv`.
2. P09 land-then-dependency: phase 15 is independent; P14 (wave 3) and required peers work are within P09's scope.
3. Rebase each phase's chain onto `main` as it completes (rule 4). All wave 1 complete — **launch wave 2 (11, 12, 13) in fresh workspaces from `main`**.
4. Keep the scheduled monitor tick running until all waves 0–5 are complete and verified; only then disable it and post the final summary.
2. P09 land-then-dependency: P09 lands after 7 (satisfied); phase 15 is
   independent. P14 (wave 3) and the required peers work in P09's gate are
   within P09's own scope.
3. Rebase each phase's chain onto `main` as it completes (rule 4). Wave 1 complete — launch wave 2 (11, 12, 13) in fresh workspaces from `main` (uotrmlntlupv).
4. Keep the scheduled monitor tick running until all waves 0–5 are complete and verified; only then disable it and post the final summary.
