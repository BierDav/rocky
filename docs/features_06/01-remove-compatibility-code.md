# 1. Remove compatibility code

Status: not started.
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
