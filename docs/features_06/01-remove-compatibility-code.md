# 1. Remove compatibility code

Status: incomplete; credential, horizon, SDK identity/routing and client-cache
slices implemented. The completion gate remains blocked by the formats listed
under remaining contracts below.
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
format compatibility from product history. Completion is not yet claimed.

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

The coordinator now reads only cache 3 with explicit acknowledged floors and
checks it before lock-file writes. Credentials require extension 2, including
empty authority, and horizon 2 with explicit existence bases. Horizon version
refusal precedes decoding the rest of the signed body; the format-1 existence
calculation is deleted. SDK identities read only 8 without migration or missing
transport-key creation. Invitations use 4 and contacts 2 with independent,
explicit routing flags. Renewal no longer falls back to bare reader keys;
JSON replies carry `issuer` only. Current credential constructors, including
the desktop composition and runtime-scaling probes, use explicit extensions.

Three compatibility-only Rust tests were deleted; one migration Rust test and
one migration Kotlin test were replaced with refusal regressions. Three mixed
retention/authority tests keep their behavior checks without legacy clauses.
Four additional Rust regressions cover cache versions/missing floors, horizon
versions, extension versions/empty authority and contact routing flags. Two
legacy Kotlin fixture files and their two qualification assertions were removed.
SDK refusal tests compare identity/database/secret-file bytes and prove that
unsupported invitations create no installation directory.

Measured diff: 49 files, 624 added lines and 993 removed lines.
Counts include source, tests, fixtures and guides; they are diff counts, not net
production-code removal.

Final diff counts and command results are recorded in the local evidence report
at `local/p01-evidence/report.md`. The evidence is for this partial change,
not the whole phase. Full Kotlin checking passed with 50 JVM tests after
rebuilding the Android runtime. Native checks passed 2 tests; packaged JVM
consumption passed; schema qualification passed 6 descriptor/fingerprint pairs;
schema tests passed 4; Room passed 12; shopping checks passed 25 JVM tests and
desktop compilation; SQLite passed 5; inventory qualification passed, including
3 intentionally rejected Kotlin type-mismatch fixtures. Credentials passed 24.
Rust baseline and secure-suite completion must be read from the final report.

## Remaining blocking contracts

The first two completion bullets are not satisfied. There is no external
implementation dependency blocking this phase; the following required work
remains in this change's continuation:

- Core descriptor 1/2/3 and definition 2/3 must become current 3 only. Remove
  unused `reuses_keys`/`KeyReuse` and fingerprint-preservation omissions; update
  Kotlin producers and supported generation fixtures, preserving generation
  transitions.
- SDK collection/register schema paths and `generate_room` remain. Delete the
  generator and its fixtures/tests, replacing meaningful storage/session tests
  with current definitions rather than dropping their invariants.
- Secure row formats 1–5, bundle bodies 1/2, checkpoint upgrade and readers
  3–6, old replica identity adoption, existence records 1/2, witness transfer,
  `secure_legacy_creations` and defaulted result evidence remain. One current
  checkpoint must still represent optional partition, horizon and finalization
  state without falsely finalizing an admitted cut.
- Runtime/storage historical engine/sidecar and row-identity refusal paths,
  unscoped file opening/refusal, checkpoint adoption, request-hold backfill and
  `CommandState::Prepared` remain. Replace them with one current file-format
  admission before initialization writes; do not remove durability or recovery
  state needed by current files.
- Finish owning guides and remaining decision compatibility clauses, then
  recount removals and rerun every completion gate after those changes.

No device UI, Android device execution, real-network distributed deployment or
performance qualification is established by this partial change's checks.
