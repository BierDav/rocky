# Phase 7 final legacy removal

2026-10-08, dedicated `rocky-db-p07-row-identity` jj workspace. This record
supersedes the unfinished legacy items in the phase brief's historical evidence.
The previous eleven format-2 changes are retained unchanged.

## Admission and old files

Only descriptor/definition format 2 is executable. `LogicalSchema::installable`
requires it, covering `TypedRows`, `TypedApp` and both profiles. Secure typed
installation and the shared SQL capture planner reject historical formats before
installation. Definition generation no longer emits format 1. The trusted codec
no longer admits operation-only update/delete tags 2/3, or their identity-free
branches. Historical operation-target API calls return explicit errors.

SDK collection/register declarations and register mutation requests are refused.
Their text-key descriptor conversion and register executor are removed. Rust
Room import emits format 2 and refuses historical application columns named
`id`/`row_kind`; historical generated exports were preserved as refusal inputs.
Natural-key copies and SQL/command fixtures use explicit format-2 IDs.

The shared database owner opens an existing file read-only and checks all typed
descriptors (`secure_typed`, `rocky_logical_schema`, and scoped counterparts)
before creating lock files or opening a writing connection, then repeats the
preflight while holding the lock. The SDK runs the same preflight before
identity/enrollment/settings writes. Format 1 and unknown versions return:

> row identity requires format 2; export with the original build and reinstall
> into a new database

There is no automatic upgrade, silent descriptor regeneration or invented author
assignment. Export application data through the build that wrote it, declare a
format-2 schema and reinstall into a new file with newly allocated identities.

Structural descriptor inspection remains available for historical data. Generic
compiled applications without a logical schema retain their separate protocol,
as specified by decision 0031; they do not admit format-1 typed installations.

## Completion gate

| Gate | Result and evidence |
| --- | --- |
| Unlink/relink; concurrent delete/reinsert; late old-ID edit | PASS: secure `natural_key_relink_converges_and_old_identity_edits_never_touch_replacement`, `multi_frame_relinks_verify_the_final_sql_projection`; trusted `trusted_id_reservations_roll_back_and_continue_after_tombstones_and_reopen` and `keys_survive_reordering_concurrent_insertion_deletion_and_replacement` |
| Forged author or reused sequence | PASS: secure `failed_identity_commit_releases_its_sequence_and_forged_frame_author_is_refused`; trusted identity reservation/forgery regression |
| Old databases upgrade or refuse clearly before writes | PASS: storage `old_and_unknown_typed_files_are_refused_without_writes` exercises three descriptor-table forms and versions 1/0/3; SDK `old_database_is_refused_before_enrollment_or_database_writes` checks open/create, unchanged database bytes and no lock/identity/bootstrap/secrets/WAL writes; core and SDK admission regressions refuse old/unknown descriptors |
| Only application row kind 0; kinds 1/2 reserved | PASS: secure `local_identity_columns_refuse_reserved_kinds_and_mutation`, trusted identity-column/refusal regressions and public consumer read/reopen |
| Owning guides describe columns and identities | PASS: `docs/runtime/secure-sql.md`, `logical-schema.md`, `schema-definitions.md` and `schema-builder.md` describe format 2 and refusal |

Fixture migration also qualifies early tombstones learning their immutable
numeric ID when an insert arrives later, deletion of observed incarnations,
nullable generation fields compared in typed stored form, and stale pseudonyms
retained for renewal without admitting their expired manifest/policy authority.
No new sleeps, unsafe code, successful placeholders or weakened assertions were
introduced. Existing ignored measurement/platform tests remain ignored.

## Previous verification before the latency fix

The removal is committed as `notxysxlzyus`; the eleven existing phase-7 change
IDs are preserved. The chain is rebased onto `rstmszowzqyn` (`c9e81d9c`), with
`main` unchanged and no default-checkout edits.

The rebase produced one textual conflict in `schema-builder.md`: retain the
landed P08 Room driver wording together with the required format-2 columns.
The first check also exposed P08's `Domain.schema` access after that legacy field
was removed. Room now reads the same application version from node settings.
P08's driver fixtures were migrated from text `id` to business `key`, registered
table pseudonyms and allocator-checked numeric identities. Their assertions
remain intact, including the five-second reader/writer bound. The trusted
shopping test retains the dedicated fixture hash instead of the new P08 export
hash; fixture Room bookkeeping still uses its own `id` column.

Required package runs exited 0:

- `cargo test -p rocky-core --locked`: 30 unit, 9 integration and 3 doc tests.
- `cargo test -p rocky-secure --locked`: 116 passed, 5 existing ignored; doc tests passed.
- `cargo test -p rocky-runtime --locked`: 156 unit tests; doc tests passed.
- Supplemental storage: 20 passed, including old/unknown-file refusal without
  lock files and preservation of application tables with metadata-like suffixes.
- Supplemental trusted typed family after the hash correction: 8 passed.
- Final formatting and workspace all-targets Clippy with warnings denied passed.
- Separate workspace doc tests passed.
- Supplemental workspace tests and SDK no-default-features tests/doc tests passed
  with the two expensive driver cases explicitly excluded. This is remainder
  coverage, not a green completion gate; both cases were also run unfiltered.

The final `./scripts/check` exited 101 in the workspace-test family. Its driver
family finished with 5 passed and 1 failed in 570.41 s; the separate unfiltered
driver rerun returned the same result in 570.72 s. The unfiltered SDK
no-default-features run also exited 101 with the same 5/1 driver result in
569.22 s.
Quota boundaries, atomic bundles/savepoints, the explicit lease timeout, Room
housekeeping and peer changed-column merging passed. The paging test failed
with `a reader blocked the writer`: its existing five-second bound expired
while deleting 500 of 600 rows with readers open. Inspection finds that numeric
incarnation winner lookup scans and decodes all stored rows of the scope; profiling
and correction remain necessary, without weakening this assertion.

At that checkpoint the legacy-removal bullet was closed, but the shared
integration gate and phase completion remained blocked. The final run below
supersedes those failing driver results.
Android emulator and `sdks/native/check` are out of scope. The prior Kotlin
schema-builder qualification remains valid; no Kotlin implementation changed.

## Driver latency correction and final qualification

Two isolated reproductions of
`cargo test -p rocky-sdk --test driver readers_page_snapshots_and_never_hold_the_writer --locked -- --nocapture`
failed with the unchanged five-second assertion, in 52.42 s and 52.28 s.
A three-second macOS `sample` of the first process placed 2,018 of 2,048
writer-task samples in `winner`'s `Materialized` decoding: 1,627 in CBOR decode
and 390 in canonical re-encoding. The stack was SQL `finish`/`seal`,
`verify_sql_projection`, `project_into`, then `winner`. Winner lookup paged
through all scope incarnations for every natural key. Verifying 500 deleted
rows therefore repeatedly decoded 600 records, including retained tombstones;
this was computation in the writer, not a reader holding its lock.

The correction is at the private secure storage/lookup layer:

- `replica/identity.rs` installs `secure_row_keys`, an index on `(scope,key,row)`
  and a deletion trigger, and narrows winner lookup to the key's incarnations.
  The existing latest-live-creation ordering and canonical validation remain.
- `replica/apply.rs` maintains the derived key index in the row transaction.
  `replica.rs` installs/backfills it for existing format-2 files;
  `replica/typed.rs` backfills after bulk restore/rebuild before reprojection.
- The deterministic secure SQL regression checks SQLite's indexed query plan,
  live/tombstone membership, reopen of a pre-index format-2 file, rebuild and
  deletion cleanup. Existing checkpoint/bootstrap and rollback tests also pass.
- The driver test only adds elapsed-time reporting. Its five-second timeout,
  500-row deletion, readers and all snapshot assertions remain unchanged.

The fixed isolated test passed in 3.82 s, measuring **1.854462334 s** for
begin/delete/commit of the 500 rows with both readers open. The full unfiltered
driver family passed all six tests in 7.91 s within workspace testing and
8.01 s without SDK default features. No test filters or assertion changes were
used in the final gate.

All five completion bullets were rerun green in the unfiltered gate: secure
and trusted relink/concurrent delete/reinsert/late-edit isolation, forged author
and reused-sequence refusal, old/unknown-file refusal before writes or lock
files, application row kind zero with reserved-kind refusal, and the owning
guides' format-2 columns/identity documentation. Storage also preserves
application tables whose names have metadata-like suffixes. The private index
changes neither application tables nor the format-2 admission requirement.

The full `./scripts/check` (invoked as `sh -x ./scripts/check` to timestamp its
unchanged commands) exited **0**, taking 409.71 s:

- Formatting: passed, 0.63 s.
- Workspace all-targets Clippy, warnings denied: passed, 7.02 s.
- Workspace unit/integration and doc tests: passed, 323.42 s including build
  and doc-test compilation; driver 6/6 (7.91 s), secure 117 passed with 5
  existing ignores (57.26 s), runtime 156 (16.47 s), storage 20 (0.18 s),
  trusted typed 8 (0.62 s). All workspace doc tests passed.
- SDK no-default-features tests and doc tests: passed unfiltered, 78.64 s;
  driver 6/6 (8.01 s), trusted typed 8 (0.63 s).

The three required separate package runs also exited 0 (wall times include
Cargo and doc-test work):

- `cargo test -p rocky-core --locked`: 30 unit, 9 integration and 3 doc tests;
  3.33 s.
- `cargo test -p rocky-secure --locked`: 117 passed, 5 existing ignored,
  doc tests passed; 57.77 s (unit family 56.09 s).
- `cargo test -p rocky-runtime --locked`: 156 unit, doc tests passed;
  17.43 s (unit family 16.62 s).

The latency correction and evidence use the existing working change
`poyzwovqmplp`, preserving all seventeen committed ancestors and their change
IDs. `main` remains `rstmszowzqyn` (`c9e81d9c`); the default checkout is untouched.
No pushes, bookmarks or PRs were created. Android emulator and `sdks/native`
surfaces remain out of scope. **PHASE 7 COMPLETE: yes.**
