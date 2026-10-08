# 0028: Generation lineage, projection plans and signed backups

Status: accepted, 2026-10-07. APIs, limits and evidence are in
[generations](../runtime/generations.md) and
[backup/restore](../runtime/backup-and-restore.md); this record holds the
formats and invariants those guides rely on.

## Generation lineage

- Local logical-schema fingerprints stay distinct from replicated schema and
  handler numbers.
- A publisher-signed `rocky.transition.v1` commitment binds the old/new manifest
  hashes, application, schema/handler pairs and admission horizon. Envelopes use
  the credentials crate's canonical serialization and the
  `rocky.signed-transition.v1` signing label.
- Historical signed bytes are never rewritten. Replay keeps the original
  generation's authority, merged with current revocations. New admission behind
  the horizon refuses explicitly.
- Supported changes are nullable-field and mergeable-table additions. Key, type,
  removal and command-descriptor changes refuse before installation. There is no
  migration callback running business logic; renames, removals, key changes and
  reader-scope remapping need a future versioned transform.
- `secure_generations` holds previous signed manifests, policies and
  transitions; `secure_horizon` the admission horizon; `secure_projection_plan`
  a projection descriptor and bounded cursor.
- Local projections backfill shadows in batches of 256 stored rows and verify
  them before one atomic switch of the file's placement group. Authority changes
  invalidate old sessions and cursors.
- Strong generation activation requires owner authority and binds the exact
  coordinator head, the transition and a nonzero `state` commitment over every
  complete confirmed command resource in sorted table/row order. The SDK checks
  the local predecessor again after reading all resources. The coordinator
  trusts the owner's assertion and sees commitments only.
- A changed definition opens only with a verified lineage or as a classified
  local projection change. Lineage is capped: SDK 32 steps, secure runtime 64.

## Signed backups

- A `rocky.backup.v1` archive holds a bounded JSON inventory, the exporting
  node's signature and streamed files with lengths and SHA-256 hashes:
  `identity.json`, the coherent `rocky.sqlite` copy and optional coordinator
  evidence.
- An archive holds no signing or reader keys and is never key escrow. The fresh
  restore identity obtains grants and keys through enrollment; key gaps stay
  explicit.
- `restore.json` binds an interrupted restore to the same archive.
  `secure_predecessors` records identity rebinding without rewriting operations
  or tickets.
- Unknown archive formats, signature or hash failures, foreign domains and
  reused signing identities refuse.
- Backups preserve local uncertainty and signed history. They do not restore the
  public coordinator or dispatch effects. Archives contain public credential
  records and private local storage: store them under the application's own
  access and encryption policy.
