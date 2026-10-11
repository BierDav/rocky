# 1. Remove compatibility code

Status: complete; all completion gates passed on 2026-10-09.
Owner: every crate with a persisted or wire format; the Kotlin SDK and its
qualification fixtures. Depends on nothing. Wave 0.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [no backward compatibility](README.md#no-backward-compatibility). Earlier
phases kept earlier encodings readable, migrated identity files and refused
historical layouts with dedicated paths. None of it is wanted. After this
phase each format has one version, and the later phases of this roadmap
change formats in place.

Known starting points, found by searching for `legacy`, `migrate_v` and
serde defaults that preserve earlier bytes; the search is not complete:

- `rocky-secure`: the frame's `legacy_row_format` default, legacy horizon
  existence and `secure_legacy_creations`, legacy checkpoint upgrade and
  metadata adoption at open.
- `rocky-sdk`: identity file migrations (`migrate_v3`, `migrate_v4`, versions
  6 and 7), legacy collection and register schema paths.
- `rocky-core`: descriptor and definition formats 2 and 3 side by side;
  entries omitted when false to keep earlier fingerprints.
- `rocky-credentials`: capability extension versions 1 and 2; envelope
  version handling.
- `rocky-coordinator`: `LegacyCache`.
- `rocky-core`: descriptor format 1 still accepted by `LogicalSchema::validate`.
- `rocky-secure`: adoption of replica identity labels `encrypted-replica.v1`
  to `v5`; legacy checkpoint witness transfer.
- `rocky-credentials`: the optional extension that preserves the legacy
  encoding and signatures.
- `rocky-runtime`: opening a legacy file as one unscoped domain.
- `rocky-secure`: the format-1 `legacy_horizon_existence` path and checkpoint
  formats 3 to 6 accepted side by side.
- [Mergeable horizon](../runtime/mergeable-horizon.md) and [encrypted checkpoints](../runtime/encrypted-checkpoints.md) note the formats still accepted until this phase.
- Kotlin: `fixtures/legacy` and the checks that load them.

Further sites found on 2026-10-08:

- `rocky-coordinator/src/client.rs`: format-2 upgrade and `serde(default)` on
  `settled`, beside `LegacyCache`.
- `rocky-sdk/src/enrollment.rs`: identity versions 3 to 7; invitation `v` 3
  or 4 and contact `v` 1 or 2 used as flags for custody and enrollment-only.
  One version each, with an explicit field.
- `rocky-sdk/src/node.rs`: peer and network JSON emit `creator` beside
  `issuer` for older callers.
- `rocky-secure`: `row_format` 1 to 5 (`wire.rs`, `replica/apply.rs`);
  replica identity readers in `replica.rs`, `replica/generation.rs` and
  `replica/backup.rs`; existence record versions 1 and 2
  (`replica/existence.rs`); `Message::Result.evidence` with `serde(default)`
  (`network.rs`).
- `rocky-credentials/src/horizon.rs`: `HORIZON_FORMAT` 1 and 2.
- `rocky-runtime`: `refuse_foreign_engine` and the Turso sidecar check
  (`rocky-storage/src/database.rs`), `refuse_old_row_identity`, the legacy
  single-domain refusal (`layout.rs`, `lib.rs`, `runtime.rs`), files without
  `rocky_checkpoint` and the request-hold backfill (`worker.rs`),
  `CommandState::Prepared` (`worker/strong.rs`).
- `rocky-core/src/logical.rs`: `Incompatibility::KeyReuse` is never
  constructed and `reuses_keys` is serialized and fingerprinted but read
  nowhere. Remove both.
- `rocky-sdk/src/room.rs`: `generate_room` is used by tests only, refuses the
  identity columns the Kotlin importer requires and can emit invalid SQL.
  Delete it.

## Ordered slices

1. Inventory: list every format with more than one accepted version, every
   migration and every legacy refusal path, per crate, in this brief. Separate
   them from application schema generations, placement relocation and key
   epochs, which are features and stay.
2. Collapse each format to its current version. Keep the version tag and one
   refusal for any other value.
3. Delete migrations, legacy readers, legacy tables and the tests and
   fixtures that only cover them.
4. Update decision records' status lines where a compatibility clause no
   longer holds, and the guides that describe upgrade or reinstall paths.

## Completion gate

- No format accepts two versions; a search for the starting points above
  finds no production code.
- A file or frame with another version is refused before any write, with one
  error per format.
- Application schema generation transitions still pass their tests.
- `./scripts/check` and the Kotlin checks pass; the removed lines and tests
  are counted in this brief.

## Phase 1 inventory (2026-10-08)

Verified against source at parent `357dbbb4`; this inventory distinguishes
format compatibility from product history. Every listed action is implemented.

| Owner | Multiple readers, migrations or historical refusals | Current format / required action |
| --- | --- | --- |
| `rocky-core` | `LogicalSchema` accepts 1/2/3; `SchemaDefinition` accepts 2/3; omitted descriptor fields preserve fingerprints; unused `reuses_keys` and `KeyReuse` | Descriptor and definition 3; require identity columns, delete old tags and preservation-only fields/branches |
| `rocky-coordinator` | Client evidence cache: untagged `LegacyCache`, upgrade from 2, optional `settled`; old `raft.cbor` refusal | Client cache 3 with required floors; audit raft store's single format refusal |
| `rocky-credentials` | Capability extension 1/2 and optional extension preserving signatures; horizon 1/2 and optional bases; key envelopes' version checks | Extension 2, horizon 2; one current capability encoding; retain only current envelope versions |
| `rocky-sdk` | Identity 3–7 migrations to 8 and transport-key creation; invitation 3/4 and contact 1/2 use versions as routing flags; renewal `Sealed` fallback to bare keys; `creator` JSON alias | Identity 8, invitation 4 and contact 2 with explicit routing flags; one sealed-renewal shape; remove alias |
| `rocky-sdk` | Legacy collection/register schemas, Room/policy generator `generate_room`, descriptor default 2 | Definition/descriptor 3; delete generator and fixtures; update Kotlin producers |
| `rocky-secure` | Frame default 1, row readers 1–5, command body 1/2, optional result evidence; replica identity labels v1–v5 accepted beside current identity | One current frame/body encoding; one identity label; required result evidence |
| `rocky-secure` | Checkpoint 2 upgrade, accepted checkpoints 3–6, optional witness/partition/finalized fields, `secure_legacy_creations`; existence records 1/2, format-1 horizon existence | Checkpoint 6, existence 2, horizon 2; delete upgrades/witness transfers and legacy table |
| `rocky-runtime` / `rocky-storage` | Turso sidecar/foreign-engine refusal, old descriptor refusal, unscoped single-domain file open/refusal, missing-checkpoint adoption, request-hold backfill, `CommandState::Prepared` | One current file identity checked before initialization; delete historical adoption/backfill and specific refusal paths |
| Kotlin | `schema-qualification/fixtures/legacy` and legacy-loading checks; schema format producers | Current schema fixtures only; version refusal regression remains |

Application schema generations and signed lineage remain authenticated history
within the current format. Placement relocation (`rocky_migrations`) remains a
resumable product operation. Reader-key epochs and rotation remain features;
versioned cryptographic domain-separation labels are not themselves old readers.
The phase has no dependency prerequisite. Existing partition/constraint refusals
remain in force; this phase does not implement later roadmap phases.

## Implemented slices and evidence

Core descriptors and definitions accept only 3. Kotlin emits 3; the SDK no
longer has collection/register decoding or the unused Rust Room generator.
Meaningful SQL, typed-row and domain tests use current descriptors. Inert key
reuse flags and fingerprint-preservation omissions are gone.

Secure frames accept only 5, always wrapping bundle body 2 in witness envelope
1. Author and partition metadata are independent optional features. Checkpoint
6 likewise represents partition, admitted horizon and finalized horizon
independently; an admitted cut is never silently finalized. Existence records
accept only 2. Replica identity v6 is checked before writer initialization.
Legacy proof transfer, tables, checkpoint upgrades, identity adoption and
open-time command/preparation/retention/row-index backfills are deleted.
Current creation clocks, command recovery, replay and bounded retention remain.

SQLite uses one current application-id tag, checked read-only before lock or
writer initialization. Stored descriptors are checked in both unscoped and
scoped files. Fresh runtime retention metadata is installed atomically by
resumable bootstrap. Historical engine/sidecar refusals, request-hold backfill
and `CommandState::Prepared` are gone. Unknown-outcome recovery stays.

Coordinator caches accept only 3 with required floors. Raft state, logs and
snapshots are checked before lock writes; restore checks before copying files.
Current interrupted-write cleanup and corruption recovery remain.
Credentials require extension 2 and horizon 2. SDK identities accept only 8,
invitations 4 and contacts 2; routing is explicit data. Renewal has one sealed
shape; JSON has no `creator` alias. Owning guides and decision 0039 document
these contracts; decision 0018 links the replacement bundle encoding.

Twelve legacy-only Rust test functions were deleted. One Rust and one Kotlin
migration test were replaced with refusal regressions; mixed behavior tests
keep their admission, retention, routing and recovery checks. Eight new Rust
test functions in the continuation cover schema tags, file tags and fresh
reopen, stored descriptors, encrypted identities/checkpoints/frames/payloads,
and coordinator open/restore before writes. The preceding change added four
other format/authority/routing regressions. Six obsolete JSON fixtures were
deleted, with two current descriptor fixtures replacing meaningful SDK uses.
Two legacy production modules were deleted (Room generator and witness transfer).

Measured aggregate diff from main: 140 files, 1,767 added and 3,567 removed lines.
These counts include source, tests, fixtures and guides.

`./scripts/check` completed with exit 0: 719 passed, 0 failed, 15 ignored across
workspace/doc tests and the repeated SDK gate without default features. Secure
passed 153, runtime 154, and storage 26. Generation transitions and current
crash recovery passed. The four ignored crash-child helpers execute through
their parent tests; measurements and Docker qualification were not run.

The final native gate passed format, Clippy, 2 ABI tests and the build without
default features. Full Kotlin passed 50 JVM tests. Packaged consumption passed;
schema qualification passed 6 descriptor/fingerprint pairs and native reopen;
schema tests passed 4, Room 12, shopping 25 plus desktop compilation, and SQLite
5. Inventory qualification passed, including 3 intentionally rejected type
mismatches. Android arm64-v8a and x86_64 release artifacts rebuilt and published
locally. Both conventional commit descriptions passed `commitlint --jj`.

Exact commands, corrected failures, per-crate counts and logs are in the
workspace evidence report at `local/p01-evidence/report.md`. The inherited
change remains intact; the continuation is directly above it, with main unchanged.

No device UI, Android device execution, remote CI, real-network deployment or
performance qualification is established by these checks. Ignored probes are
reported separately from executed tests.
