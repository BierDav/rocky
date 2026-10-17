# Authenticated encrypted row synchronization

`rocky_secure::EncryptedReplica` is the secure row profile: signed credentials,
per-scope encryption and authenticated sync over [live links](live-links.md). The trusted `Replica` and
`Runtime` files cannot be opened through it. Commands are in
[encrypted commands](encrypted-commands.md).

## Install and delegate

An installed `RowApplication` supplies its manifest, row validation and the
permission-controlling field IDs. `EncryptedReplica::open` requires the exact
compiled manifest body signed by its trusted publisher. Publisher-bound app,
entity and field IDs use the existing manifest format. Installation does not
load executable code from a manifest.

`SignedManifest`, `SignedCapability` and `SignedRevocation` live in
[`rocky-credentials`](credentials.md) and use domain-separated Ed25519 signatures. CBOR must round-trip to the pinned canonical encoder;
noncanonical/trailing input is rejected. Capabilities bind the
node public key, principal kind, validity interval and bounded
resource grants, but no manifest: grants name tables and fields by name, so
a chain holds in every generation. Delegation is publisher → user → optional deployment → node.
Users may delegate to users; deployments cannot turn back into users; nodes
cannot delegate. Every child narrows its parent's rows, fields, actions, validity
and delegation budget. Each child grant must fit one complete parent grant.

Publisher and issuer availability is unnecessary once signed evidence is
retained. Admission evaluates expiry with the receiver's local wall clock and
the [revocation statements](revocations.md) it holds. `learn_revocations`
merges a set by union, persists it and ends sessions and reads under credentials
it now ends. It cannot learn a revocation while offline. A statement ends every
chain through a key or capability it names; other chains stay current.
Previously applied history is retained, rather than retroactively erased, but
every frame of a revoked author is refused once its statement is learned
([limits](credentials.md#limits)).

`SecureConfig` supplies node and encryption keys from a
[key provider](credentials.md#key-providers). The runtime persists only
encryption-key fingerprints and generations, never secret keys. The runtime has
no key distribution or membership protocol; the SDK's
[enrollment and renewal](sdk-enrollment.md) distribute keys.

Frames use row format 11 and name a `rocky_core::TipId`; the local graph resolves
that tip to its schema manifest. The encrypted file identity is
`rocky.encrypted-replica.v32` and binds the followed tip and node key. Earlier
frame and file formats are refused.

## Row admission and keys

A `ResourceScope` identifies the mesh/app/domain, encryption reader set and
manifest table independently. Grants pair their row selection and field set.
Every changed field must have an applicable grant on that same row; combining
an unrelated row grant and field grant cannot authorize a patch. Insert and
delete have their own rights. Application validation sees both signed before/
after images and the actual candidate after merging. Every generated change
must be included in the exact batch; application code has no SQL-write channel.
Any denied member aborts a locally authored batch.

Mergeable changes to permission-controlling fields are denied. Such changes
use the [encrypted strong command path](encrypted-commands.md), which checks
caller and executor row/field authority over every generated effect.
`Invoke`, `Execute` and `PublishSchema` are distinct protocol rights, but this
row API never treats them as row-write authority. Schema publication follows the authority rules for
[seals and migration openings](tips-and-bookmarks.md).

Installing a key requires whole-scope, whole-row read authority. A partially
selected read grant cannot obtain a shared encryption key. Different scopes
in a configuration must have different key material. Applications must split
rows with different reader sets into different encryption scopes. Key holders
can decrypt ciphertext they have already obtained; revocation cannot remove
knowledge or old copies from a malicious former reader. A changed reader set
rotates the scope to a new [key epoch](credentials.md#key-epochs-and-rotation):
later writes are sealed under it, earlier epochs only open retained ciphertext,
and nothing is sealed again. A device that has not learned the rotation keeps
writing under the old epoch, which anyone who retained that key can read.

Each local batch covers one encryption scope, at most 32 rows and 64 causal
references. A [bundle](secure-sessions.md) commits several scopes and up to
2,048 rows as one transaction, and receivers apply its held partitions together. Signed operations retain their exact original encrypted body and
credentials. Logs are per node/resource scope, so public operations do not
require private predecessors. Creation dependencies are required for edits;
cross-scope dependencies are rejected. Inserts require fresh stable row IDs.
Field registers use logical-clock/author/sequence ordering; deletion is a
permanent tombstone. Application merge validation must remain compatible with
this deterministic merge model; stateful business transitions belong in strong
commands.

## Transactions and recovery

The private `p2panda-store::operations::LocalOperationStore` implementation
requires an active framework transaction and never starts or commits one.
It validates the operation/hash/log binding and rejects author-log forks. Its
delete methods return errors; operations are removed only by
[checkpoint pruning](encrypted-checkpoints.md). The upstream SQLite store is
not used.

Local authoring commits the precise projection, original signed ciphertext,
per-scope log sequence, immutable retry handle, key fingerprint/generation,
outbox and revision in one SQLite transaction. A busy file retries the whole
transaction up to ten times over 318 ms, then reports `RetryExhausted`.
Concurrent authoring and incoming requests share one transaction and flush:
whoever holds the replica lock takes up to 64 queued requests or 4 MiB, each in
its own savepoint, so a refused request rolls back alone. Stable handles return
the original bytes;
reusing a handle with different input conflicts. Counters derive from retained
signed logs, including after restart. XChaCha nonces use fresh 192-bit OS
randomness; failed attempts publish no bytes. Persisted sequences are never
reused. Revision watches and results publish only after commit.

Commits are buffered (`synchronous=NORMAL`) and counted against a durable
watermark; a flush syncs the WAL through a writable handle, using `F_FULLFSYNC`
on Apple platforms. Windows requires write access for `FlushFileBuffers`.
`author`, `receive` and
`receive_all` flush before answering, and so does every method that hands data
to a peer, the coordinator, an effect provider or a recovery export.
`author_buffered` and `receive_buffered` skip the flush; acknowledge or send
their results only after `flush` or `durable_commit` covers them. Power loss can
drop only unflushed local work and unacknowledged receipts
([design rationale](write-concurrency.md#durability-and-exposure)).
A group's flush runs after the replica lock is released, so readers and the
next group do not wait behind it ([write concurrency](write-concurrency.md#encrypted-path)).

Committed scope frontiers are cached under the replica lock. Ordinary write
attempts invalidate them before execution; transaction-local reads never enter
the cache. Graph, workspace and schema changes, and failed attempts, also clear
field-path caches. Only offer and peer-acknowledgment bookkeeping preserve a
committed frontier; authorization, staged privacy and durability are checked on
every transfer.

Incoming receipt first commits original ciphertext and forwarding obligations;
`network::pull` and recovery import receive a whole page per call.
Dependency-ready application then commits projection, key state, operation
status and revision together. Missing dependencies or keys remain pending.
Forwarding never signs a received operation again. Opaque relays validate signed
write authority without obtaining plaintext or becoming writers themselves;
plaintext row/field admission remains the authorized materializer's job.

Open verifies retained signatures, hashes, dependency chains, budgets and key
fingerprints. For available keys it replays signed ciphertext and compares the
projection. Key loss leaves ciphertext retained and reads unavailable;
`install_key` rejects a mismatch with committed crypto state. `evict_key` stops
materialization/read access without interpreting missing companions as deletion.

Authorized local projections are **plaintext at rest**. Disk encryption and
local process/OS isolation are deployment policies, separate from opaque relay
storage. Raw database access is outside the application API threat boundary.

## Native transport and completeness

`NativeSession` runs over any reliable ordered stream. `authenticate` pins the
peer's node key; `accept` takes whichever peer dialed. Mutual fresh challenges
bind both peers and their credential chains; every frame binds that session
and a monotonic sequence with an Ed25519 signature. The acceptor speaks only
once the initiator's chain verified for this domain, so an outsider sees no
chain of it. The handshake then exchanges the revocation statements either
side lacks and refuses a peer the merged set revokes
([decision 0051](../decisions/0051-roster-free-admission.md)); there is no
roster. The runtime still verifies capabilities against the held set at every
transfer and acknowledgment, and no session caches authority. Another session
version is refused as a failed authentication. The stream is not TLS: routing
identifiers, credentials, operation sizes and timing remain visible. Application data, old/new images and payloads remain encrypted.
No discovery, NAT traversal, BLE or browser transport is qualified.

Range sync by per-log frontiers, scope subscriptions, eviction and remote
reads are described in [full and partial replicas](replica-roles.md). The
older pull below remains. Every pull supplies explicit subscribed scopes, an
optional hash cursor and a byte quota. Restart a pass on entering/leaving a scope or after new records may
have appeared before a cursor. Retained operations include the same-scope causal
closure; reordered pages can remain pending until predecessor pages arrive.
Read/custody admission filters transfer scopes. Named custody acknowledgments
and application acknowledgments are stored separately by their `applied` flag;
custody does not imply successful materialization. A partial page's continuation
never acknowledges operations it did not include.

Frames are bounded to 256 KiB (`MAX_SECURE_FRAME`); native pages to 128 KiB;
sessions to a caller budget of at most 32 MiB with a 10-second per-I/O timeout.
Nothing about a peer is written before its chain verified, and a replica keeps
frontiers of at most 256 unconfigured peers
(`MAX_UNCONFIGURED_PEERS`), dropping the least recently served first; the peers
its owner configures and full nodes are kept.
Retained row operations follow `RetentionLimits`: 32 MiB starts checkpointing
and pruning, 64 MiB reports backpressure. Command, effect and recovery
artifacts are not pruned and stop at 4,096 records or 32 MiB. Limits fail
explicitly. One secure replica serializes its local work behind one lock and
groups concurrent writes; measured costs are in the
[encrypted baseline](scaling-limits.md#measured-limits).

`snapshot(scope)` returns a whole scope of at most 1,024 live rows and 1 MiB and
refuses larger ones with a quota error. `snapshot_page(scope, after, max_rows)`
reads any scope in row-key order; pass `next` as `after` until it is `None`.
Every page checks read authority, reports its own revision and cannot leave its
scope. `complete` means the local scope has a key and no retained
pending frames. It is not evidence of global network coverage. `companion` is a
point read and returns `Present`, `Deleted` (a retained tombstone), or `Unavailable`; absence,
key loss and partial replication never become deletion. Authorized callers can
join separately fetched companions by stable row ID.

## Results and evidence

`EncryptedPackage` separates row, command-result, job, blob and checkpoint
payloads by encryption scope; `PackageSet` commits to every package's bytes,
kind, scope and generation inside a `StrongPackage`. Subset verification,
`open_package` and the separate command projection are described in
[encrypted commands](encrypted-commands.md#receiving-subsets-and-completeness).
A receipt never grants read access.

## Qualification

```sh
cargo test -p rocky-secure --locked
cargo test -p todo-desktop secure_demo --locked
```

Tests cover an offline publisher's retained evidence, a keyless relay between
two TCP clients, original-byte forwarding, row/field pairing, mixed-batch,
insert and delete denial, tampering, delegation attenuation, scope/generation
mismatch, revocation, missing keys/dependencies, concurrent merges,
rollback/retry, restart and a subprocess exit after commit. This is
application-level evidence, not a cryptographic audit or power-loss
qualification.

## Design rationale

- Protocol bytes, projection, crypto progress and outbox share one SQLite
  transaction, so the p2panda signed-log types (`p2panda-core`/`p2panda-store`
  pinned at 0.5.2, upstream backends off) run over a framework-owned
  transaction. An upstream store or networking stack would add a second commit
  boundary and its own database ownership.
- Credentials and routing metadata stay public so relays can check write
  authority without keys; only payloads and row images are encrypted.
- Counters and nonces derive from retained signed logs, so a retry with a
  committed handle reuses its original bytes instead of signing again.
- The trusted `Replica` keeps its own fixture contract and is never relabelled
  secure by opening it through `EncryptedReplica`.
