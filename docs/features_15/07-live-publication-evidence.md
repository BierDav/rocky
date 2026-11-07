# Live publication investigation evidence

Date: 2026-11-07. Status: source investigation and design recommendation;
no runtime fix or new end-to-end performance result is claimed.

## External reproduction

The local Paperless report at
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

## Confirmed in current source

The phase 3 checkout has integrated membership changes beyond alpha.4.
The following mechanisms still exist in the inspected source; the external
timings have not been reproduced on it.

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

## Validation

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
this evidence-only update. The full suite was not repeated for this
documentation change.

Baseline checks do not implement or validate the proposed publication
protocol, remeasure Paperless, or qualify a desktop application. The
proposal's acceptance cases remain unimplemented and unverified.
