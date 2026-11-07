# Live publication implementation and investigation evidence

Updated: 2026-11-08. Status: implemented; native/wasm and browser/native relay
correctness gates passed; consumer performance unqualified.
The investigation results below describe the earlier baseline. Current work
and verification are recorded separately at the end.

## External reproduction

The previously supplied Paperless report at
`/Users/david-bieregger/AgentWorkspaces/paperless/docs/database-refresh-latency.md`
uses Rocky `0.1.0-alpha.4`. Its disposable real-schema diagnostic reports:

- 32 held scopes; signed two-frame direct pushes of 71,692–71,853 bytes
  against a 65,536-byte cap. A three-frame push was 107,728 bytes; individual
  frames fit at approximately 36.5 KiB per signed message.
- Three unmodified contact edits exchanged 822, 825 and 825 protocol messages.
  Native development-build notifications arrived about 4.6 seconds after
  starting the commands.
- An isolated splitting experiment emitted a 71,882-byte page as 36,583-byte
  and 35,728-byte messages. Notification followed source command return by
  225 ms; the snapshot took a further 453 ms and contained the receipt.
  That run still exchanged 387 messages. It logged an incomplete push before
  five complete pushes: the first had already scheduled reconciliation.

These are supplied measurements from different runs, not a paired benchmark
of the proposed design. The probe patches are not installed dependencies.
The preview's separate 10,168 ms interval begins at reconciliation start,
not sender commit; the report does not measure commit-to-render time.

## Confirmed in the investigation baseline

The phase 3 checkout has integrated membership changes beyond alpha.4.
The following mechanisms existed in the investigation baseline and are replaced
by the implementation below; the external timings have not been reproduced here.

- `EncryptedFrame::weight` excludes the credential chain, while `Chain`'s
  serializer emits the full credentials. `Handle::push_committed` reads
  `FRAME_BYTES / 8` storage bytes and wraps the resulting frames with their
  chains in a single signed push. Its final size check catches overflow but
  selects fallback instead of smaller transport messages.
- `Node::signal_links` publishes scope by scope. `run_link` launches intake
  futures and calls `link_pending(peer)` as soon as any `receive_push` returns
  incomplete. Its boolean combines pending dependencies and missing keys;
  neither publication completion nor outstanding sibling scopes are represented.
- Changed hints already carry proved scopes. `link_pending` retains only a
  peer-level boolean, and `exchange_inner` selects all eligible held scopes.
- `sync::rows` calls `rows_scope` per scope, four at a time. Each calls
  `sync::session`, including revocation refresh and `mesh::exchange`, before
  checking its frontier. The lower-level frontier planner already supports
  multi-scope batches of up to 64 with partition-group expansion.
- Native session transfer already has `carry_chains`/`complete_frames`,
  `carry_escrow`/`keep_escrow`, 8 KiB channel fragmentation, bounded reassembly
  and inherited link admission. Native row push replies per page, so using it
  unchanged would introduce stop-and-wait round trips.
- Direct `receive_push` does not call the native escrow carry/keep path.
  This is an audit and regression requirement for the replacement; this
  investigation does not establish data loss or a failed root recovery.
- The limits differ: signed link messages 64 KiB, native logical messages
  256 KiB, native range requests at most 128 KiB, and one session 32 MiB.
  A larger direct envelope alone does not establish a complete large-frame path.

## Investigation baseline validation

Workspace mapping used `cargo metadata --no-deps --format-version 1`.
The root README, contribution instructions, phase prerequisites, owning
modules and existing live-link tests were read before adding this proposal.
Primary research sources are linked in the phase brief; no paper's reported
speedup is used as an estimate for Rocky.

Checks completed during the investigation:

- `./scripts/check` passed workspace formatting/clippy, 261 unit tests,
  1,172 integration tests and the isolated replica-lock regression. Its wasm
  checks passed 903 tests in 71 browser runs, plus browser/native relay sync
  with a browser write and coordinator claims.
- The existing SDK single-message committed-push and bounded-burst recovery
  regressions also passed individually. These establish the existing small
  fixture's behavior, not the proposed real-schema improvement.
- `./scripts/commitlint --jj` passed; the proposal's relative links resolve.
- `./scripts/lint` returned zero, but actionlint reported the existing
  unsupported arithmetic expression in `.github/workflows/chaos.yml:86`:
  `100000 + github.run_number * 1000`. Workflow lint remains unresolved
  despite the wrapper's successful exit; that workflow was not changed here.

After workspace restoration, phase 3 commit `0d031c15` had the same tree as
the investigation snapshot `73a7daf856d6`, verified with `jj diff` before
that investigation evidence-only update. The full suite was not repeated for
that documentation change.

Baseline checks do not implement or validate the proposed publication
protocol, remeasure Paperless, or qualify a desktop application. The
proposal's acceptance cases were unimplemented at that point.

## Completed implementation on david-mm-linz

The registered checkout is `/Users/david-bieregger/AgentWorkspaces/rocky` on
`david-mm-linz.local`. The installed `git.abandon-unreachable-commits` key was
verified, and the final fetch used
`jj --config git.abandon-unreachable-commits=false git fetch` (no new changes).
Earlier ordinary fetch's automatic abandonment had been undone. Existing change
`xknrpntn`, commit `115a018128ce590f12ae1c0f102f3c0c2037a902`, remains preserved;
its identity work was independently integrated upstream. No worktree, bookmark,
push or PR was created.

Fetched `origin/main` and implementation parent are exactly
`5b937088e6a743deb688fde6c25de208726eb8ac`. Its ancestry includes features_14
`4456c62ccb41e1201742582312d5554809f4cb37` and prerequisite phase 3
`0d031c15c4db5854c76bcb779d24dccc4614d709`. Retired roadmaps were not recreated;
the root README now links to features_15. Implementation remains in current
jj change `kllqtqssutlxqstloozzpplyyrrlxxlz`, described as
`feat(sync)!: publish bounded committed changes over native channels`.

The final full check qualified source revision
`77cf0e057d2f5dee95f97ed96e5a6a0cf63e63c4`. The completion audit and fresh
fixture diagnostics used that same runtime; subsequent edits update only README
and current roadmap/evidence documentation. No additional runtime change was
needed after recovering the completed check artifacts.

The implementation replaces direct push with one native publication channel,
shared credential chains/escrow/migration metadata, ordered mixed-scope pages,
Finish and one held/applied receipt. Wire version 28 rejects other authenticated
versions; persisted signed escrow and migration forms remain unchanged.
Preflight measures the complete signed encoding at maximum sequence width,
reserves the reverse receipt including repair scopes outside the export, and
rejects incomplete or oversized plans before opening a data channel.
`EncryptedFrame::weight()` retains its storage semantics.

Export fences, bundle membership and escrow are captured under replica
ownership. Racing commits stay dirty; retirement is rechecked before marking
frames offered. Causal bundle ordering keeps outcomes after their inputs.
Finish settles durable intake and inspects current and previously unresolved
records; held receipt never implies applied state or custody. Ready transactions
notify observers after local commit, before Finish/receipt can complete.
Authorization, membership and revocation are rechecked through the existing
verified receive path, including between pages and before receipt.

The audit confirmed byte/fragment quotas apply before reassembly retention and
CBOR/embedded metadata charges apply before typed expansion, chain learning and
escrow reconstruction. Initial limits remain 64 scopes, 256 KiB forward signed
native messages, a 32 KiB reverse receipt, 32 items/128 KiB per page and 8 KiB
fragments within 64 KiB outer signed messages. The channel has an absolute
120-second lifetime starting at open, including handler queue residence;
progress and trickled input cannot renew it. Cancellation/backpressure retain
retry work. Bounded scope/full repair generations, receive ownership and link
identity guards protect newer hints, commits and replacement streams. Recovery
batches frontiers and opens row channels only for changed or unfinished scopes.

The related unlink correction omits an explicit cut only when its statements
are empty and acknowledgments absent. The existing identity-owned unlink verifier
already reconstructs that exact empty lane. Nonempty cuts, signatures, the 8 KiB
signed revocation bound and persisted format are unchanged. This resolves the
reproduced oversized unlink without weakening revocation semantics.

## Final correctness validation (2026-11-08)

The prior delegate stopped reporting while browser binaries compiled, but its
persistent runner completed successfully. No build was running at handoff.
`/tmp/rocky-publication-final-required.status` records
`{"lint": 0, "commitlint": 0, "check": 0}`. Its check log confirms:

- `./scripts/check`: formatting, workspace clippy, SDK all-target compile without
  default networking features, 282 native unit tests, 1,190 native integration
  tests and the isolated replica-lock regression passed.
- The same check passed wasm clippy and 927 tests across 68 binaries in 72
  headless Firefox runs. Browser/native synchronization over a local relay
  passed, including the browser's write and coordinator claims arriving natively.
- All 35 live-link tests passed together with eight test threads, including
  simultaneous writers, the deterministically gated receipt/frontier race,
  cancellation, backpressure, reconnect and revocation. The 65-held-scope case
  uses 62 application tables, one partition selector and three shared partitions,
  respecting both the 64-table and 64 concrete/partition credential grant bounds.
- Focused cases cover early metadata/framing quotas, maximum-width signed sizing,
  absolute lifetime, missing keys/chains/history/siblings, malformed or missing
  terminals, cross-page bundles, restricted peers, mid-publication revocation,
  live-only escrow backup/key recovery and verified migration provenance. A
  36 KiB SQL value produces a supported operation above 64 KiB; a 70 KiB value
  rejects the authored 128 KiB intent bound atomically. Oversized publications
  send no data and recover through actual native repair.
- Empty-cut semantic equivalence and the original partition/link/unlink
  integration case passed; that integration case also passed 12 automatic-sync
  repeats. Supported default and no-default SDK builds passed; no indiscriminate
  `--all-features` run was used.
- `./scripts/commitlint --jj` passed. `./scripts/lint` returned zero, but actionlint
  still reports unsupported arithmetic at `.github/workflows/chaos.yml:86`
  (`100000 + github.run_number * 1000`). Workflow lint is unresolved despite the
  successful wrapper status; the unrelated workflow was not changed. The native
  build also reports the existing future-incompatibility warning for `block 0.1.6`.

Earlier development failures are superseded by this final source qualification:
notification-time `Incomplete` snapshots required causal mixed-scope pages;
three lifecycle timeouts required capturing repair work after link admission
and separating work generations from retry resets; the unlink size failure
required the implicit empty cut. Invalid 65-table and over-granted boundary
fixtures were replaced by the supported partition fixture above. An earlier
browser/native run expired its invitation during a long build; the final
cached-build relay run passed. Earlier 926/927-test wasm runs do not substitute
for the final run listed here.

Raw local artifacts (not checked in):

- `/tmp/rocky-publication-final-required-check.log`,
  `/tmp/rocky-publication-final-required-lint.log` and
  `/tmp/rocky-publication-final-required-commitlint.log`;
- `/tmp/rocky-publication-final-live-links.log`;
- `/tmp/rocky-publication-empty-cut-test.log` and
  `/tmp/rocky-publication-unlink-qualification.{log,status}`;
- `/tmp/rocky-publication-finish-diagnostics.{log,status}` for the fresh sample
  below, run directly from the current workspace test binary located with
  `cargo nextest list --workspace --locked --message-format json`.

The final check used `CARGO_BUILD_JOBS=1 ROCKY_WASM_JOBS=2 ./scripts/check`.
After the documentation update, `./scripts/lint` again returned zero with the
same actionlint finding (`/tmp/rocky-publication-finish-lint.log`), commitlint
passed and all current roadmap/README relative links resolved. A `jj diff`
against the qualified source revision confirmed only the four documentation
files changed, so the full native/wasm build was not repeated.
These correctness gates establish the exercised contracts, not real consumer
performance or general distributed-protocol qualification.

## Fresh native diagnostics (2026-11-08)

After confirming final correctness, both disposable SDK fixtures were rerun
once, sequentially on the cached final binary. The 32-scope fixture has eight
text columns plus stable identity per table, production root escrow sealing,
and three atomic contact/receipt/version edits. Every Sync table notification
was followed immediately by a snapshot containing the complete associated
transaction. The tiny fixture likewise observed its complete write.

Each edit used one publication channel (two local session endpoints), one
intake and one receipt. Native message families were exactly Chains ×1,
Escrow ×1 and Publication ×4 (Start, Page, Finish, Receipt). There were no new
dials, discovery/control/row-transfer requests, repair scopes, full-repair flags
or repair reasons. Each chain is carried once within the publication; a Chains
message can contain several distinct chains. These counters belong to the link,
so parallel-test tracing subscribers cannot affect the assertions.

| Fixture/edit | Signed native bytes | Framed link bytes | Link messages out/back | Commit return to Sync event (µs) | Event to snapshot (µs) | Receiver queue (µs) |
| --- | ---: | ---: | --- | ---: | ---: | ---: |
| Tiny | 9,977 | 11,580 | 7 / 2 | 16,316 | 1,468 | 421 |
| 32 scopes / 1 | 32,092 | 34,059 | 9 / 2 | 22,596 | 4,355 | 595 |
| 32 scopes / 2 | 32,403 | 34,370 | 9 / 2 | 29,958 | 879 | 581 |
| 32 scopes / 3 | 32,383 | 34,350 | 9 / 2 | 30,294 | 866 | 583 |

Native byte totals include the reverse receipt; framed totals include outer
signatures and length prefixes in both directions. Queue residence and elapsed
times use the wall monotonic clock while the fault-test Tokio clock is paused.
The fixtures use a development test build, disposable SQLite files and an
in-process memory transport on the shared Linz host. Generated credentials and
operations make byte counts vary between runs. Durations have no assertions;
there is no calibrated RTT, paired baseline, latency budget or speedup claim.
The tiny case's fixed channel/receipt framing cost is explicit; fewer messages
are not claimed for every workload.

## Remaining consumer qualification

The supplied Paperless report and `.local/sync-latency-probe` artifacts remain
absent at the registered Paperless path on Linz, confirmed again on 2026-11-08.
No real application data was changed. The supplied alpha.4 measurements above,
new Rocky fixture diagnostics and unmeasured consumer results are separate
evidence categories and cannot be used to calculate an end-to-end speedup.

Still required when the disposable consumer probe is available: measure the
actual application schema after commit return, notification and snapshot,
including signed bytes, message families, session/receipt/repair counts and
queue residence over a real relay on native and browser targets. The local
browser/native relay test above proves correctness only. Consumer/real-relay/
browser performance, desktop commit-to-render behavior and optimal limits
remain unmeasured. Compression, cross-publication caches and set reconciliation
remain follow-up research.
