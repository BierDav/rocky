# 0039: One current persisted and wire format

Status: accepted and implemented for phase 1, 2026-10-09.

Rocky replaces formats in place and keeps no migrations, old readers, adoption
paths or historical encodings. Existing installations reinstall. The current
[roadmap contract](../features_06/README.md#no-backward-compatibility) supersedes
compatibility clauses in earlier decisions.

- Schema descriptors use format 5 and definitions format 6
  ([names are identities](0067-names-are-identities.md)). Canonical descriptors carry
  partition and liveness fields explicitly; replicated shapes carry their class
  and constraints explicitly. Key reuse is a row-incarnation feature, with no
  inert schema flag. Kotlin and native producers use the same format.
- Secure frames use row format 6 ([row moves](0038-row-moves.md)), with optional author and partition metadata.
  Every row payload has a creation-witness envelope (1) and bundle body (3);
  an optional membership distinguishes a single scope from a transaction bundle.
- Checkpoints use format 6, with independent optional partition, admitted-horizon
  and finalized-horizon fields and a required row-move summary. Admission never invents a finalized cut.
  Existence records use 2 with optional partition metadata.
- SQLite files use one application-id tag, `0x524b5901`, checked read-only before
  locks or initialization writes. Secure identity v8 ([row moves](0038-row-moves.md)), stored descriptors and
  checkpoint tags are likewise checked before an encrypted writer opens.
  Fresh checkpoint metadata belongs to resumable bootstrap, not open-time adoption.
- Credential extensions and horizons use 2, client evidence caches use 3,
  SDK identities use 9 ([group secrets](0040-group-secrets.md)), invitations
  use 4 and contacts use 2. Routing behavior
  is explicit data rather than an alternate version.

Application schema generations, placement relocation and key epochs remain
features. Current unknown-outcome command recovery and atomic projection,
operation, outbox and revision commits remain required.

Owning contracts: [schemas](../runtime/schema-definitions.md),
[frames](../runtime/secure-sessions.md),
[checkpoints](../runtime/encrypted-checkpoints.md),
[seal compaction](../runtime/lazy-compaction.md),
[SDK enrollment](../runtime/sdk-enrollment.md).
