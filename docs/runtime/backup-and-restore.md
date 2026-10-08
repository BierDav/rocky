# Coherent backup and fresh-device restore

`Database::backup` (Kotlin `RockyDatabase.backup`) creates one signed archive.
`Database::restore` (Kotlin `RockyDatabase.restore`) enrolls a fresh signing identity
and restores it. See [decision 0028](../decisions/0028-generation-lineage-and-signed-backups.md).

## What is retained

The runtime flushes commits, serializes its file-copy boundary and uses SQLite
`VACUUM INTO` for a coherent full-file copy. The signed inventory binds exporter,
publisher, scope, manifest, revision and each file's length/hash. Files are streamed
with a 64 KiB buffer; inventory is capped at 256 KiB. The archive carries installed
credential/generation metadata and optional `coordinator.evidence` alongside SQLite.

SQLite retains typed projections and canonical signed operation bytes, business
keys/incarnations, tombstones, bundle dependencies, mutation handles, pending
invocations, command tickets/preparations/results, signed receipts and effect
observations. Coordinator evidence retains retirement/session facts and historical
receipts; missing public-window records still mean explicit historical unavailability.
A backup never makes the public coordinator an application payload store.

The archive contains no signing/reader keys and is not encrypted as a whole.
Private storage and public credential records remain sensitive backup material;
the application controls archive access and any external encryption. File-secret
storage and encrypted recovery-key packages remain separate mechanisms.

## Restore

1. Obtain a fresh invitation with the required grants from an available enrolled
   issuer. Delegated issuer recovery remains the primary authority recovery path.
2. Restore into an empty data directory, with a secret provider and a network able
   to reach that issuer. Verify archive signature, complete file hashes, installed
   publisher and invitation domain before replacing local files.
3. Enrollment makes fresh signing/transport keys and supplies currently authorized
   reader keys. The archive's old signing identity is refused. A durable marker
   records the archive/predecessor; repeating an interrupted restore continues that
   same archive. Another archive or an already finished installation refuses.
4. Rebind file ownership once, retaining original author logs and every durable
   handle. Reopen verifies signed history and reconstructs authorized projections.
   Imported grants do not renew old invocations or bypass current revocations.
5. Check private-scope/key completeness and coordinator readiness explicitly;
   obtain missing history/keys through authorized recovery. Fence an old executor
   before claiming execution readiness. New mutations use the fresh author's log.

Uncertain command/effect identities stay unchanged. Restore/rebuild/reopen never
calls an effect provider or runs a command handler merely to replay data. Scheduling
an unresolved ticket remains explicit, and uses its original request identity.
Retired/expired coordinator requests are never redispatched under a fresh ID.
Missing payloads/keys and acknowledged-loss replacement retain the explicit
[fenced recovery behavior](recovery-and-effects.md).

## Qualification

SDK backup tests cover altered archives, foreign-domain invitations, fresh identity,
unsent original-author operations, new-author writes, reopen and uncertain command
lookup after a lost coordinator response. The packaged Kotlin consumer restores
queries and an applied ticket without another handler call. Runtime recovery/effects
subprocess tests remain the evidence for interrupted preparation/confirmation and
external-effect uncertainty. See [phase-8 evidence](sdk-packaging.md#build-and-consume-a-package).

The SDK archive is one placement group's coherent local cut, not an atomic backup
of arbitrary application files, key escrow or a coordinator disaster-recovery backup.
