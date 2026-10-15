# Coherent backup and fresh-device restore

`Database::backup` (Kotlin `RockyDatabase.backup`) creates one signed archive.
`Database::restore` (Kotlin `RockyDatabase.restore`) enrolls a fresh signing identity
and restores it. See [decision 0028](../decisions/0028-generation-lineage-and-signed-backups.md).

## What is retained

The runtime flushes commits, serializes its file-copy boundary and uses SQLite
`VACUUM INTO` for a coherent full-file copy. The signed inventory binds exporter,
publisher, scope, manifest, revision and each file's length/hash. Files are streamed
with a 64 KiB buffer; inventory is capped at 256 KiB. The archive carries installed
credential/generation metadata in `identity.json` alongside SQLite.

SQLite retains typed projections and canonical signed operation bytes, business
keys/incarnations, tombstones, bundle dependencies, mutation handles, held changes
and coordinated-entry metadata. Application decisions are ordinary rows; no
command ticket, result, receipt cache or managed command effect is retained.
The coordinator remains a metadata service, never an application payload store.

The archive contains no signing/reader keys and is not encrypted as a whole.
Private storage and public credential records remain sensitive backup material;
the application controls archive access and any external encryption. File-secret
storage and encrypted recovery-key packages remain separate mechanisms.

## Restore

1. Obtain a fresh invitation with the required grants from an available enrolled
   issuer. Delegated issuer recovery remains the primary authority recovery path.
   Match the backup's readable tables: an issuer backup needs an issuer invitation
   to retain its registry projection. Changing access to an existing table requires
   [reinstalling the readable projection](../decisions/0061-readable-projections-and-checkpoint-custody.md#physical-projection).
2. Restore into an empty data directory, with a secret provider and a network able
   to reach that issuer. Verify archive signature, complete file hashes, installed
   publisher and invitation domain before replacing local files.
3. Enrollment makes fresh signing/transport keys and supplies currently authorized
   reader keys. The archive's old signing identity is refused. A durable marker
   records the archive/predecessor; repeating an interrupted restore continues that
   same archive. Another archive or an already finished installation refuses.
4. Rebind file ownership once, retaining original author logs and every durable
   handle. Reopen verifies signed history and reconstructs authorized projections.
   Imported grants do not bypass current revocations.
5. Check private-scope/key completeness and coordinated-log readiness explicitly.
   Obtain missing history/keys from authorized peers. New mutations use the
   fresh author's log; restore does not create a new identity for an uncertain
   coordinated append or synthesize its outcome.

A permanently lost in-sync set needs the separate
[Phase 10 recovery contract](../features_09/10-stuck-recovery.md). Database
backup does not replace coordinator durability or authorize a recovery cut.

## Qualification

SDK backup tests cover altered/retired archives, foreign-domain invitations,
fresh identity, unsent original-author operations, new-author writes and reopen.
The packaged Kotlin consumer restores ordinary queries and reopens generations.
See [Phase 11 evidence](../features_09/11-removing-strong-commands-evidence.md)
for current verification.

The SDK archive is one placement group's coherent local cut, not an atomic backup
of arbitrary application files, key escrow or a coordinator disaster-recovery backup.

Current archives use `rocky.backup.v2` and inventory format 2. They carry only
`identity.json` and `rocky.sqlite`; the removed coordinator receipt cache is
never archived. Earlier archives are refused, without an alternate reader.
