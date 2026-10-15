# Phase 5: confirmed rollout evidence

Status: implementation, confirmed rollout gates and full workspace verification
passed after isolating the Phase 9 invoice repair (2026-10-15).

## Split verification (2026-10-15)

The chain is Phase 9 `uywtwqpr`, invoice repair `ptxlnlqx`, Phase 5
`zlrwrvzz`, then historical handoff evidence `woxunxwp`. The repair owns
live-link delivery retries, void-aware author counters, their two deterministic
regressions and the Phase 9 guides. Phase 5 owns confirmation, row-count reports,
bindings, shopping UI and rollout documentation. The historical investigation
below records the prerequisite failure; it is not part of the rollout mechanism.

The [repair alone](09-coordinated-writes-evidence.md#invoice-liveness-and-void-counter-correction-2026-10-15)
passed 21/21 unchanged invoice runs and all eight secure coordinated regressions.
Verification on Phase 5 above that repair:

- `./scripts/lint`: passed, including six lint self-tests.
- `cargo test -p rocky-sdk --locked`: 310 passed, zero failed, eight ignored.
- One `./scripts/check`: formatting and strict workspace Clippy passed;
  174 unit tests and 1,031 integration tests passed, zero failures. Integration
  comprised 775 workspace and 256 SDK-without-default-feature tests. The unit
  tier took 93.1 ms (46.5 ms at load x2.0); integration took 182.93 s against
  its 300 s budget. All test timing gates passed. Nextest skipped 169 workspace
  and 47 SDK tests outside this integration selection.

Output: `/tmp/rocky02-phase5-lint.log`, `/tmp/rocky02-phase5-sdk-test.log`
and `/tmp/rocky02-phase5-full-check.log`. The older failures below remain
historical evidence. This split adds no production fix or test weakening;
Kotlin, native and shopping qualification results below were not rerun here.

## Prerequisites and implementation

Phase 3 is present on parent `uywtwqpr`: `SchemaChange::removed()` identifies
removals, `apply_names` strips stored state and the projection switch drops the
removed schema. Existing removal/adoption regressions remain in place.

The SDK checks removing opens using the identity settings already loaded under
its directory lock, before identity
evolution, migration intent, secrets, renewal, rotation or network startup.
A read-only snapshot counts all live stored rows by table, including
constraint-suppressed rows and every locally held partition. Field impacts
count every live row, including NULL/missing values. The report digest binds
domain, installed manifest, full target schema and these counts. A stale
approval returns the current report and signs nothing. The signing path and
its existing durability boundaries remain unchanged after successful approval.
No replica, handshake or transition format changed.

Rust returns `RolloutConfirmationRequired` through the typed error boundary;
`Database::open_confirmed` takes the digest. Native errors carry the report as
JSON with `confirmation_required` or `confirmation_mismatch`; Kotlin decodes
`RolloutImpact` and takes `confirmation` on open/resume. The shopping app owns
its wording and explicit confirm/cancel dialog. An ahead member opens the
installed generation and reports `waiting_for_issuer`, even when given a digest.

## Regression coverage

`rocky-sdk` `tests/removal.rs`:

- `removing_open_changes_nothing_and_only_the_current_report_can_sign` checks
  removed table/field counts, byte-identical identity and replica after refusal,
  unchanged key files, no network startup or migration intent, changed schema and changed counts invalidating a digest,
  cancellation followed by opening the old schema, and confirmed signing.
- `a_member_cannot_confirm_an_unsigned_removal_and_waits_for_an_issuer`
  checks both raw and typed waiting status, unchanged schema on an offline
  member, and fetching a confirmed signed generation without another prompt.
- Existing removal, held-change and re-addition tests explicitly confirm issuer
  removals and continue to exercise replicated activation.

`an_impact_report_counts_constraint_suppressed_rows_and_all_local_partitions`
checks remote uniqueness conflicts and two locally held partitions.

Kotlin `RoomGenerationsTest` checks typed report decoding, digest refusal when
schema changes, successful confirmed open and existing Room removal behavior.

## Verification

Passed on 2026-10-14–15 in workspace `rocky-02`:

- `./scripts/lint`, after `--fix`; final source formatting is clean.
- `./scripts/check --unit`: 157 workspace tests. The full check's unit stage
  also passed 174 tests including SDK without default features.
- `cargo test -p rocky-sdk --test removal --locked`: all six tests passed.
- `cargo clippy -p rocky-sdk --all-targets --locked -- -D warnings`.
- `./sdks/native/check`: Clippy, two native tests and the build without iroh;
  repeated successfully after settings reuse.
- `sdks/kotlin/gradlew -p sdks/kotlin check` with
  `-Dmaven.repo.local=/tmp/rocky-02-maven`: passed after decoder correction
  and warming the voter binary; the final run after settings reuse passed
  203 tasks in 5 min 13 s including rebuilds.
- Full SDK publication to that private Maven repository.
- `./examples/shoppinglist-app/check` with `ROCKY_SHOPPING_PROJECT` pointing
  to an ignored isolated source copy and
  `KOTLIN_CLI_JAVA_OPTIONS=-Dmaven.repo.local=/tmp/rocky-02-maven`: required
  schema qualification, 59 JVM tests, 25 Android tests and desktop/Android
  builds passed. The copy changes only SDK dependency coordinates to the
  private `0.1.0-phase5-zlrwrvzz-final` artifacts; application sources are identical.
- `scripts/commitlint --jj @`, including the requested co-author footer.

The first full check passed 1,028 integration tests and failed the existing
Phase 9 `trusted_devices_number_concurrent_invoices_without_gaps` with
`no invoice committed`. Its separate Cargo rerun passed. That check also
reported 2 s timing misses in concurrent rotation and removal tests while
another check and builds overlapped. A duplicate full run timed out
`a_cut_join_resumes_its_pages_and_keeps_edits_from_both_sides` and was stopped
to remove self-induced contention. The next serial check passed all 1,029 integration behaviors but missed the
existing re-addition test's budget at 2.03 s (2.00 s limit); its SDK-without-iroh
sample was 1.97 s. The final gate reuses identity settings already loaded under
the same directory lock, removing a redundant load. After that optimization, all Phase 5 tests and timing gates passed, including
re-addition in 1.91 s (default features) and 1.97 s (without iroh). The final
full check passed 174 unit tests and 1,028 integration tests; its only failure
was the existing Phase 9 invoice test without SDK default features:

```text
thread 'trusted_devices_number_concurrent_invoices_without_gaps' (69342169) panicked at crates/rocky-sdk/tests/coordinated.rs:95:5:
no invoice committed
test-tiers: failed: rocky-sdk::coordinated trusted_devices_number_concurrent_invoices_without_gaps
test-tiers: integration: 1028 passed, wall 185.42s, 185.42s at load x1.0, budget 300.00s
```

The inherited invoice investigation and repairs below have not closed this gate.
The [baseline comparison](05-invoice-baseline-evidence.md) reproduces the failure
on unchanged Phase 9; this handoff stops at that prerequisite blocker.
No test tier, retry count, assertion or deadline was weakened. The final Kotlin
check after settings reuse passed (203 tasks, 5 min 13 s).

The three new Phase 5 tests' later isolated measurements were 904.1 ms,
954.1 ms and 1.08 s, each below 2 s. Existing held-change coverage took 1.29 s;
the existing re-addition scenario took 2.62 s and missed that sample's budget.
No test tier or budget exception was changed.

Development failures were corrected:

- The schema-version fixture replaced only version 1; it now changes the
  version-2 target. Initial diagnostic: `confirmation required` assertion.
- Read-only SQLite probes created empty WAL/SHM cache files. File assertions
  now compare persistent files, keys and any nonempty WAL, excluding empty
  WAL and shared-memory metadata. Initial diagnostic: `left == right` failed.
- Local UNIQUE enforcement refused the second conflicting insert. The fixture
  now creates signed conflicts on two replicas. Diagnostic:
  `SQL session: UNIQUE constraint failed: items.category (code 2067)`.
- Partitioned `mutate` refused identity allocation without partition values.
  The fixture now uses the supported `next_id` and SQL path. Diagnostic:
  `partitioned identity allocation requires partition values`.
- Kotlin initially lacked the serialization compiler plugin; the final SDK
  decoder uses explicit JSON parsing. Diagnostic:
  `Serializer for class 'RolloutImpact' is not found.`
- The first Kotlin service test's nested Cargo build waited beyond its 180 s
  deadline: `java.lang.IllegalStateException: service command timed out`.
  No timeout changed; the corrected complete Kotlin run passed.
- Initial shopping resolution lacked Android runtime and iOS publications;
  complete private publication supplied them. Reusing version `0.1.0` then
  resolved an older Android SDK API (`No parameter with name 'confirmation'
  found`, `Unresolved reference 'RolloutConfirmationRequired'`). The isolated
  copy and unique private artifact version removed that cache collision.

## Blocking invoice investigation (2026-10-15)

The user authorized investigating and repairing the blocking Phase 9 test.
Direct reproduction showed both `no invoice committed` and
`SQL session: items is not available: Incomplete`. Delivery diagnostics showed
`Rejected("no live link to the trusted node")`. A proposal now retries storage
while its exact entry is pending instead of only polling the coordinator.

After that fix, repeated runs exposed a consumed-position bug: importing an
older peer checkpoint erased a local void's applied log tip, and authoring reused
the voided sequence. Receiver admission correctly excluded that fork, leaving
later frames pending on its backlink. Bundle planning and frame signing now use
one author-tip calculation that includes the highest locally stored void.
This preserves its sequence, hash and clock. Conflicting applied/voided tips
return a consistency error. No format changed.

Two deterministic `rocky-secure` regressions cover a first-delivery failure and
checkpoint import after a lost race. The original SDK gate is unchanged.
See [Phase 9 correction evidence](09-coordinated-writes-evidence.md#invoice-liveness-and-void-counter-correction-2026-10-15).

During development the frame-only fix exposed a planner mismatch:
`Storage(Corrupt("bundle sequence plan"))`; using the shared tip fixes both paths.
Two intermediate builds caught missing arguments while that helper was extracted:
`error[E0425]: cannot find value id in this scope` and
`error[E0061]: this method takes 3 arguments but 2 arguments were supplied`.
No final full-check success is recorded. The
[baseline comparison](05-invoice-baseline-evidence.md) records the handoff diagnosis.

## Implementation choices

- Reports are sorted by table/field name. The target commitment includes the
  entire parsed SDK schema, including local version, commands and migrations;
  even a different target with identical removals needs a fresh approval.
- A missing local scope contributes zero rows. Deleted rows and retained signed
  history do not contribute. NULL/missing removed fields still affect their row.
- The digest covers the data the report carries; it does not bind row values
  that were never shown. Changing values while preserving counts leaves the
  report unchanged.
- Confirmation is an SDK signing gate. Already publisher-signed peer generations
  continue through existing signature verification and adoption. Members cannot
  turn a digest into issuing authority. Pure additions take the existing path.
- The report crosses UniFFI as structured JSON in a non-retryable request error,
  then becomes a typed Kotlin exception. Kotlin uses explicit JSON parsing,
  matching the SDK's existing code without adding a serialization compiler plugin.
- The application owns the prompt, cancellation and retry. A stale confirmation
  causes another prompt; cancellation preserves the installed schema.

The first Kotlin run exposed `Serializer for class 'RolloutImpact' is not found.`
The final decoder uses explicit parsing. That run also encountered
`java.lang.IllegalStateException: service command timed out` in `ServiceOwnerTest`
while its nested Cargo build waited behind the workspace test build; the
verification rerun must avoid that build-lock contention.

## Limits

Counts describe this copy's active stored rows, not retained signed history or
other devices. Approval does not prevent concurrent issuers from signing other
schemas; existing lineage admission rules still govern them. The UI has not
been manually exercised, and no mobile or real-network rollout qualification
is claimed. The full shopping removal rollout across app versions is unqualified; phase 12 was skipped.
