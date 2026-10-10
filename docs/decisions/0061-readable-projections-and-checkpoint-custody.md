# 0061. Readable projections and finalized checkpoint custody

Status: implemented. Node-key privacy is addressed by [decision 0062](0062-node-references-and-credential-aliases.md).

The shopping member gate requires physical table absence, rather than an empty
unauthorized table. Read-only horizon participants also need the writer's
finalized row cut even when their sole contact holds no decryption keys.

## Physical projection

The SDK uses `EncryptedReplica::install_readable_tables`. The complete logical
descriptor still must match the signed manifest. Its durable projection contains
only tables covered by local read grants or read selectors, local-only tables,
and referenced existence support. Custody grants alone create no application
table. Constraint candidates and projection shadows follow the same selection. Holding
proofs, existence routing and horizon validation use the full signed declarations,
including scopes with no physical table.
SQL against an omitted table reaches SQLite's ordinary `no such table` error.

The selected names are persisted independently of the signed descriptor. Reopen
uses that selection; additive schema plans may add newly declared readable tables.
Changing access to an existing table requires reinstalling this current-format
installation. The lower-level `install_tables` API still installs the complete
projection for explicitly composed runtimes.

## Checkpoint custody

Native session v13 adds authenticated checkpoint archive begin, progress, chain
and page messages. They require the same whole-scope holding proof as checkpoint
pulls. The sender transfers referenced existence bases first. Custody initiating a
session instead pulls the finalized pages after the evidence exchange. Chains travel in
separate frames; the stored first page retains the chains for later delivery.

A keyless holder verifies the signed finalized checkpoint's authority and stores
sealed pages without decrypting them. Each accepted page and the resumable cursor
commit together and flush before acknowledgment. Incomplete archives produce no
offer. A restarted session continues after the last accepted page. Retries never
replace an accepted page's ciphertext. The total stored sealed page bytes are
bounded by `MAX_SECURE_BYTES`; a full archive refuses further pages.

A reader imports through the existing checkpoint path, verifying decryption,
row digests, frontier, generation, write authority and finalized horizon. Custody
does not infer that ciphertext matches the signed row digest. It cannot create a
checkpoint or expand a reader's grant. An offer remains tied to its checkpoint
ID; replacement requires the reader to restart its import.

## Current formats and verification

The current format versions are listed in [decision 0062](0062-node-references-and-credential-aliases.md). There is no migration, legacy reader or session negotiation.

The Rust driver regression checks physical absence, SQL errors and reopen.
The horizon regression checks a read-only participant importing through restarted
keyless custody. Both belong to integration. The existing shopping release
regression remains end to end. See [Phase 20 evidence](../features_07/20-shopping-without-a-roster-evidence.md)
for measured checks and the remaining foreign-node-key contract.
