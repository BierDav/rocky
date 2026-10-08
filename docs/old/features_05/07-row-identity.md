# 7. Row identity

Status: complete and qualified after the P08/P10 rebase. All row-identity
completion bullets and the full unfiltered shared gate pass after the driver
latency correction; the five-second paging assertion is unchanged. Owner: `rocky-core` logical schema and frames,
`rocky-secure` apply and checkpoints, `rocky-runtime` for the shared columns.
Depends on [phase 1](01-credential-primitives.md) for author numbers. Wave 1.
On the critical path. Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [section 3](agreed-scope.md#3-row-identity) of the scope,
[decision 0015](../decisions/0015-logical-schema-and-row-identity.md),
[secure SQL](../runtime/secure-sql.md) and
[encrypted checkpoints](../runtime/encrypted-checkpoints.md). Inspect how the
secure profile identifies a row by table and key, and how the trusted profile's
incarnations work.

Give every table the `id` and `row_kind` columns on every node, make the `id`
the incarnation a frame and a checkpoint name, and let a deleted key be
inserted again. This is a new frame and checkpoint format; write the decision
record first. Its format lands before
[phase 9](09-strong-history-behind-the-checkpoint.md) and
[phase 11](11-partitioned-scopes.md) touch the same statements
([hazards](README.md#shared-format-hazards)).

## Ordered slices

1. Decision record: the two columns, the snowflake layout (zero bit, 31-bit
   author number, 32-bit sequence, no timestamp), the identity index, author 0
   and `id = 0` for the virtual row, the frame and checkpoint changes and the
   upgrade or refusal path.
2. Schema descriptor: `id` and `row_kind` on every table, undeclared; the
   identity index of a partitioned table is validated once phase 11 adds
   partition columns. `real` and `blob` from phase 2 are untouched.
3. Issuance: the next sequence is the highest in the device's own log for that
   partition plus one, with no extra state; an `id` can be obtained inside a
   transaction before the insert. A table partitioned by its own `id` counts
   per author and table.
4. Verification on receive: the author number belongs to the signing key; the
   sequence exceeds the highest seen from that author in that partition. The
   highest sequence per author and partition is kept permanently.
5. Key reuse: inserts create a new `id`; updates and deletes name an `id`; the
   visible row for a natural key is the live `id` with the latest creation
   stamp. Grants stay key-based.
6. Transport: an `id` crosses JSON as a string.

## Completion gate

- Unlink and relink the same N:M pair converge to one new row on every
  replica; a concurrent delete and re-insert keep the inserted row; a late
  edit against the deleted `id` has no effect.
- A frame with a forged author number or a reused sequence is refused.
- Old databases upgrade or refuse with a clear error; no silent reinterpretation.
- `row_kind = 0` is all an application reads today; the other kinds are
  reserved for phase 14.
- [Secure SQL](../runtime/secure-sql.md) and
  [logical schemas](../runtime/logical-schema.md) describe the columns and
  identity.

## Final qualification

The [legacy-removal evidence](07-row-identity-legacy-evidence.md) is the current
completion-gate record. Earlier measurements below describe intermediate
states; their open legacy items are closed by that record.

## Historical measured evidence

2026-10-08, dedicated `rocky-db-p07-row-identity` workspace:

- Slice 1: [decision 0031](../decisions/0031-row-identity.md) records the
  columns, layout, identity index, author-zero reservation, versioned frame and
  checkpoint boundaries, and explicit export/reinstall refusal path.
- Prerequisite: phase 1's `CredentialChain::verify_author` and
  `verify_pseudonym` are present in `rocky-credentials`; the phase-1 brief
  records their wrong-number/table/key and attenuation qualification.
- Inspection confirms secure `TypedRows::row_id` currently hashes the business
  key and `merge_row` refuses a second creation. Trusted incarnations use a
  creation operation, as decision 0015 describes.
- No implementation or completion-gate qualification is claimed yet.

- Slice 2 descriptor evidence: `cargo test -p rocky-core --locked` passed
  27 unit tests, 9 integration tests and 3 doc tests. The format-2 regression
  checks implicit integer columns on mergeable/local tables, the identity index,
  primary keys naming the implicit `id`, reserved declarations, missing identity
  indexes, fingerprints and both profile diagnostics. General unique-index
  refusal remains unchanged for format 1.
- The legacy SDK register-schema conversion explicitly emits definition format 1
  rather than changing existing stored descriptors when the current constant
  advances. Runtime issuance and format refusal remain later slices; the phase
  completion gate has not been met.

- Slice 2 installation evidence: secure
  `cargo test -p rocky-secure --lib identity_descriptor --locked` and trusted
  `cargo test -p rocky-sdk --test typed identity_descriptor --locked` each
  passed one regression. Both install and reopen format-2 tables, including
  a natural/compound primary key, an implicit-id primary key and a local table;
  SQL reads can select `id,row_kind` and filter `row_kind=0`.

- Secure slice 3: checked `RowIdentity` layout, externally registered table
  pseudonyms, transaction-local `next_id`, contiguous local insert sequences,
  immutable IDs/kinds, and version-2 frame/bundle signing. Creation metadata
  survives delete, checkpoint replay and reopen without a separate allocator.
- `cargo test -p rocky-core --locked`: 28 unit, 9 integration and 3 doc tests
  passed. `cargo test -p rocky-secure --lib --locked`: 111 passed, 5 existing
  ignored; a later targeted two-table/savepoint test also passed. It verifies
  reverse-delivery atomic projection with distinct signing keys and refuses
  application updates to `id` and reserved `row_kind`.
- Receive high-water marks, checkpoint statement version 2, key reuse and JSON
  transport are not implemented by these issuance changes. Trusted issuance
  and default consumer-builder alignment remain open; no completion claimed.

- Additional slice-3 evidence: deterministic pre-commit failure rolls back
  the projection and releases its sequence; the next transaction uses the same
  ID. A frame re-signed with a forged author number is refused before storage;
  the authentic frame then applies.
  `cargo test -p rocky-secure --lib failed_identity --locked`: 1 passed.
- `cargo clippy -p rocky-secure --all-targets --locked -- -D warnings` and
  `cargo fmt --all -- --check`: passed after the final code changes.

### Remaining work and next slice

This is an incomplete implementation chain, not a phase-7 completion claim.
Continue slice 3's remaining integration first: align the trusted identity
contract and consumer entry points with the numeric incarnation contract.
Definition format 2 is currently explicit input; the existing Kotlin builder
continues to emit format 1. The legacy SDK register conversion is intentionally
unchanged until the new consumer/refusal path is ready.

Then finish the ordered slices:

- Slice 4: require an inserted ID's author to match the frame signer; refuse
  reused sequences; retain one permanent high-water mark per author/scope;
  version the checkpoint statement, carry/verify the marks and preserve them
  through pruning, eviction, rebuild and bootstrap with retained local suffixes.
- Slice 5: use the numeric incarnation in stored rows, frames and checkpoints;
  keep grants key-based; select the latest live creation at a natural key;
  capture delete/reinsert as distinct incarnations rather than an update.
- Slice 6: carry IDs as decimal strings over JSON, including allocation and
  query/cursor paths; align the builder/importer's implicit fields and the
  explicit incompatible-open/export-reinstall path.
- Qualify every completion gate on multiple replicas, including reversed
  delivery, checkpoint bootstrap and late edits of a deleted identity.

Current gate results: unlink/relink, concurrent delete/reinsert and late-edit
replacement isolation are unqualified; reused-sequence refusal is unimplemented;
old typed databases remain legacy-compatible and do not yet meet the required
upgrade/refusal gate. The shared-column reads and secure `row_kind = 0` write
restriction are qualified. Both owning guides describe the implemented subset
and link the required final format without claiming it is complete.

### Shared check at this checkpoint

`./scripts/check` exited 0 on 2026-10-08 in this workspace:

- Formatting: passed.
- Workspace Clippy, all targets, warnings denied: passed.
- Workspace unit/integration tests: passed (existing ignored probes remain ignored).
- Workspace doc tests: passed.
- `rocky-sdk` without default features/iroh, including its doc tests: passed.

The subsequent fault-injection regression passed its targeted test and
all-targets secure Clippy; production changes were included in the shared run.
Android emulator qualification and `sdks/native/check` were not run, as
requested. A green baseline does not close the outstanding phase-7 gate.

### Continuation: consumer pseudonym registration

2026-10-08: consumer open restores persisted table pseudonyms into the secure
replica; `pseudonym_install` immediately registers the installed key and chain.
Trusted format-2 inserts validate the positive identity and `row_kind = 0`,
and updates refuse the two immutable fields. `cargo test -p rocky-sdk --lib
--locked` passed 9 tests; the identity installation regression is rerun separately.
Trusted sequence allocation and numeric operation targets remain unfinished;
this evidence does not complete slice 3.

### Continuation: secure slices 4 and 5

Permanent receive marks bind author numbers to signer keys and the highest
insert sequence per table/scope. Format-2 checkpoint statements sign the marks;
bootstrap validates row sequences against them and preserves permanent local
marks after replaying a retained suffix. Pruning, eviction and projection rebuild
do not delete the mark table. Secure records now address numeric incarnations
separately from key-based grants and retain creation stamps. Typed and shadow
projections select the greatest live creation at each natural key.

`cargo test -p rocky-secure --lib natural_key_relink --locked`: 1 passed.
Three replicas converge after an N:M pair is deleted/reinserted in one SQL
transaction, a concurrent old-ID deletion and a late old-ID edit, with reversed
frame delivery. A fresh replica bootstraps the same replacement from a checkpoint.
`cargo test -p rocky-secure --lib failed_identity --locked`: 1 passed, now also
refusing an authenticated new log operation reusing an insert sequence at another
key and checking the checkpoint mark. Existing issuance/reopen/savepoint tests
passed in the 16-test secure SQL family before adding this convergence regression.

Deviation in execution order: secure receive and incarnation changes were made
together because both use the private row address and checkpoint representation.
The secure result closes those gates for format-2 Rust callers; it does not
complete trusted issuance, consumer defaults, JSON transport or legacy refusal.

### Continuation: trusted slices 3–5

Trusted `SqlSession::next_id` reserves from the own creation-log summary inside
the writer lease, with transaction/savepoint rollback. `TypedApp::with_row_author`
binds a table author to an admitted node. Inserts verify author/sequence; the
permanent summary is committed atomically and included in checkpoint tables.
Updates/deletes carry the numeric incarnation in new action tags; creation
operations remain their dependency references. Tombstones retain the numeric ID.
`update_id`/`delete_id` expose numeric targets to Rust callers.

`cargo test -p rocky-sdk --test typed trusted_id --locked`: 1 passed, covering
rollback, savepoint rollback, relink, concurrent old-ID edit/delete, two replicas,
forged/reused identities, checkpoint import and own sequence continuation after
delete/reopen. The full typed family passed 8 tests before the last expanded
gate regression. SDK/runtime all-targets Clippy passed.

Trusted issuance and numeric targets are now qualified for explicit format-2
Rust applications. The phase remains incomplete until consumer/default legacy
paths and the global incompatible-open gate are resolved.

### Continuation: slice 6 and consumer definition input

`RowIdentity` now serializes/deserializes canonical decimal JSON strings. A max-ID
regression covers `9223372036854775807` and refuses numbers, leading zeros, signs,
zero and exponential forms. Secure checkpoint row identities use this encoding.
SDK definition tables default to format 2; Kotlin builder output and Room implicit
column handling follow it. SDK `sql`/`next_id` requests expose transaction-local
allocation and tagged integer-string result cells. Kotlin client wrappers expose
them. `cargo test -p rocky-sdk --lib consumer_sql --locked`: 1 passed through
pseudonym install, allocation, SQL `RETURNING`, commit and the read API.

An additional secure boundary regression relinks 20 keys across multiple frames,
checks the final SQL projection only after all members, and reproduces it through
checkpoint/rebuild. `cargo test -p rocky-secure --lib multi_frame_relinks
--locked`: 1 passed. The earlier 10-key attempt fit one frame; it was corrected
to cross the actual frame bound.

### Historical remaining work after the earlier continuation

The earlier run left legacy admission/removal open. The final
[legacy-removal evidence](07-row-identity-legacy-evidence.md) closes it, migrates
historical fixtures and records refusal before writes. Generic compiled
applications without a logical schema keep their separate protocol, as required
by decision 0031. Android emulator and `sdks/native/check` remain out of scope.

### Continuation: projection and local-table corrections

The full secure run exposed an interrupted-shadow-plan regression in deferred
SQL projection verification. Shadow writes now continue during authoring while
the installed SQL projection is verified after all bundle frames. The targeted
interruption test passed; the subsequent full secure package passed 115 tests,
5 existing ignored, plus doc tests. The relink regression now uses a compound
N:M key (`left`,`right`) across three replicas and passed with checkpoint bootstrap.

Shared SQL capture also validates format-2 identity columns on local-only tables,
refusing zero/invalid IDs, ID updates and reserved row kinds. The local-only secure
regression passed and leaves its original row unchanged after every refusal.
A new full verification run includes these final changes.

### Final verification for the continuation chain

2026-10-08, dedicated workspace; all commands exited 0 after the corrections:

- `cargo test -p rocky-core --locked`: 29 unit, 9 integration and 3 doc tests passed.
- `cargo test -p rocky-secure --locked`: 116 passed, 5 existing ignored; doc tests passed.
- `cargo test -p rocky-runtime --locked`: 156 unit tests and doc tests passed.
- Final `./scripts/check`: formatting, workspace all-targets Clippy with warnings
  denied, workspace unit/integration tests, workspace doc tests and SDK
  no-default-features tests/doc tests all passed.
- `cargo test -p rocky-sdk --test typed --locked`: 8 passed after the numeric
  target, permanent mark and immutable-upsert changes.

The first shared/secure runs caught the shadow-plan regression described above;
the final full script includes its correction and the local-only identity test.
Kotlin source compilation/packaging, Android emulator and `sdks/native/check`
were not run. This green continuation is not phase completion: the explicit
legacy refusal/removal gate in the authoritative remaining-work section is open.

### Additional consumer qualification

`cargo test -p rocky-sdk --lib populated_numeric_consumer --locked`: 1 passed.
The public `Database` API creates a format-2 file, installs its table pseudonym,
commits a row, closes and reopens, reads the original decimal-string ID and
row kind zero, and allocates/commits the next sequence without reinstalling keys.
This exercises the actual consumer open restoration path.

`./sdks/kotlin/check-schema`: exited 0. Kotlin core sources compile with warnings
denied. Builder output is format 2 and refuses declared `id`/`row_kind`. Room
imports reject the historical text-ID exports. Separate natural-key qualification
fixtures use `key` and compare hand/import descriptors and fingerprints across
shopping, market and scalar schemas; Rust consumers install and reopen all three.
Historical generated Room exports remain refusal inputs, not edited exports.
This supersedes the earlier note that Kotlin compilation was unrun; full Kotlin
consumer/packaging, Android and native checks remain unrun.

Legacy refusal/removal in production is still open. These additional tests close
the consumer restoration and schema-builder qualification items, not that gate.

After the consumer-reopen/Kotlin qualification changes, the required package
runs again exited 0: core (29 unit, 9 integration, 3 doc), secure (116 passed,
5 existing ignored, doc tests passed), runtime (156 unit, doc tests passed).
The full `./scripts/check` again exited 0: formatting, workspace all-targets
Clippy with warnings denied, workspace unit/integration tests, workspace doc
tests, and SDK no-default-features tests/doc tests. No legacy gate closure is
claimed by these green checks.
