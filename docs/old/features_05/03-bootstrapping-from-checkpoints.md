# 3. Bootstrapping from checkpoints

Status: complete and verified on 2026-10-07. Owner: `rocky-sdk` sync, join and
readiness; `rocky-secure`
checkpoint transfer. Depends on nothing; wave 0. This is slice A of
[section 6](agreed-scope.md#6-bootstrapping-and-the-horizon), with no new
format. Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [slice A](agreed-scope.md#slice-a-use-it-no-new-formats),
[encrypted checkpoints](../runtime/encrypted-checkpoints.md),
[replica roles](../runtime/replica-roles.md) and
[decision 0011](../decisions/0011-encrypted-checkpoints.md). Inspect
`rebootstrap` and `rebootstrap_progress` in `rocky-secure`, the behind-horizon
signal and the SDK's `reconcile`, `join` and `readiness` paths. Baseline
inspection found `reconcile` already used `sync_scope` indirectly; join did not
start from checkpoints, readiness did not expose bootstrap, and no SDK retention
policy was configured.

Make the SDK use the machinery that exists: a joining device adopts a
verified checkpoint and catches up, a long-offline device rebootstraps instead
of replaying history, and a device enrolled after a revocation receives earlier
rows through a checkpoint verified against the current policy.

## Ordered slices

1. Default retention policies in the SDK, configurable, with the initial
   values recorded in the guide.
2. `reconcile` rebootstraps a scope whose frontier is behind the peer's
   horizon, through the existing behind signal.
3. `join` starts from a checkpoint and then catches up through ordinary sync.
4. A policy epoch change forces a fresh checkpoint, as a key epoch change
   already does.
5. `readiness()` reports a bootstrapping state distinct from syncing and ready.
6. **Signer right.** A bootstrap checkpoint is accepted only from a signer
   with a write grant on the scope, per the scope's
   [decision](agreed-scope.md#decided). No new right is introduced.

## Completion gate

- A device enrolled after a revocation reads rows sealed before its
  enrollment through a checkpoint; it still cannot decrypt rotated-out keys.
- A device offline beyond the retention window rejoins by rebootstrap and
  converges; its own unsent operations survive the rebootstrap.
- A checkpoint signed by a read-only device is refused.
- Deterministic tests with controllable clocks and fault hooks; no sleeps.
- [Encrypted checkpoints](../runtime/encrypted-checkpoints.md) and the SDK
  guides describe join, catch-up and the readiness states.

## Measured evidence

- Prerequisite baseline: `cargo test -p rocky-secure --locked checkpoint -- --test-threads=2`:
  16 passed, one process-child fixture ignored. Includes atomic rollback, process-exit recovery,
  interrupted rebootstrap, local edits and handle preservation.
- Slice 1: SDK defaults retain 256 covered operations and require one other copy; persisted
  overrides survive reopening. `cargo test -p rocky-sdk --locked --test bootstrap` checks
  sole-copy refusal, configuration persistence and invalid limits/scopes.
- Slice 2: `cargo test -p rocky-sdk --locked --test bootstrap`: 2 passed. SDK behind-horizon
  catch-up imports a pruned base, reapplies an unsent insert, pulls the tail, publishes local
  work, preserves retry identity and reopens with the same rows.
- Slice 3: SDK bootstrap suite: 3 passed; credentials suite: 6 passed. A new reader after
  revocation imports the earlier row with zero retained historical frames and only key epoch 2,
  retains that restriction after renewal, pulls the new tail and reopens successfully.
- Slice 4: `cargo test -p rocky-secure --locked policy_epoch_alone_refreshes`: passed. Bounded
  maintenance reseals after a policy-only change without a new operation or key rotation,
  preserves the rows/cut, links the predecessor, and audits successfully.
- Slice 5: SDK bootstrap suite: 4 passed; existing readiness suite: 2 passed. A controlled
  transport gates sync to observe bootstrapping, syncing and ready without sleeps; refused sync
  leaves durable intent, local reopen stays bootstrapping, and reattachment plus retry completes
  it.
- Slice 6: valid read-only checkpoint refusal test passed; SDK bootstrap suite: 4 passed with
  stricter admission. A correctly signed read-only checkpoint is refused at import start and
  offer without staged or projected changes; a writer checkpoint passes. Final switch rechecks
  the same authority.
- Retry hardening: SDK bootstrap suite: 5 passed. A reader missing epoch 2 renews directly from
  epochs `[1]` to `[1, 3]`, adopts a current checkpoint and converges without acquiring epoch 2.
  Bootstrap sessions continue the tail after a completed import and reserve room for the next
  offer.
- Integration hardening: full `cargo test -p rocky-sdk --locked` passed. Existing composition,
  fault and peer fixtures now complete the background join before measuring offline behavior.
  Current-only key delivery is separate from loading all locally retained epochs at reopen.
  Stable checkpoint offers preserve the existing exact-cut and generation-grace contracts.
- Full secure verification: `cargo test -p rocky-secure --locked`: 106 passed, five existing
  ignored fixtures; includes checkpoint process-exit recovery, session rollover, scope
  rebootstrap and local operation preservation.
- Controlled clock: `checkpoint_signer_expiry_is_checked_at_the_controlled_admission_clock`
  passes. The same writer signature verifies immediately before its credential expiry and is
  refused at expiry; no wall-clock waiting or staged/projection change is involved.
- Durable scope progress: SDK bootstrap suite: 6 passed. A transport refuses the second
  collection after the first is flushed; reopen resumes the single remaining collection (one
  bootstrap session plus ordinary reconciliation), preserves the first collection and clears
  intent only after completion. New authority intent is fenced against older completion writes.
- Authority race fault: SDK bootstrap suite: 7 passed. Replacing intent with a newer epoch
  during a paused second-scope import cannot be cleared by the old completion. The device
  remains bootstrapping until renewal supplies that policy; epoch-2 renewal and retry then
  complete normally.

## Required result and final verification

Required result **met**: SDK join and behind-horizon catch-up adopt existing
verified checkpoints and then sync their tails; retention policies and limits
are configurable through the native session contract and Kotlin. All six slices
and completion gates are covered. No phase-3 blocking contract remains.

- `./scripts/check`: passed workspace formatting, Clippy with warnings denied,
  workspace tests/doc tests, and the SDK without default features.
- SDK: 79 tests passed with defaults; 76 without defaults, including seven
  deterministic bootstrap cases. No new bootstrap test uses sleeps.
- Secure runtime: 107 passed, five existing ignored fixtures; controlled signer
  expiry and deterministic checkpoint/import/process-exit faults are covered.
- `./sdks/native/check`: passed formatting, Clippy/tests with `bindgen`, and the
  supported no-default-features build.
- `./examples/shoppinglist-app/check`: 20 Kotlin/Room tests passed and the JVM
  desktop application compiled against the native SDK.

Evidence is in-process/loopback and JVM qualification. Android deployment,
Internet qualification, strong-history pruning and horizon finalization belong
to other phases; this result does not claim them. Existing checkpoint row-copy,
log-count and import-transaction limits remain as documented in the owning guide.
