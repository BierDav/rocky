# Secure sessions and transaction bundles

`EncryptedReplica` sessions and atomic multi-scope bundles (Rust API). SQL
over typed tables is in [secure SQL](secure-sql.md); grants and delivery are in
[permissions and sync](permissions-and-sync.md); the design is
[decision 0018](../decisions/0018-secure-transaction-bundles.md).

## Bundles

`author_bundle` commits writes of several scopes and more than 32 rows as one
local transaction. The frames are returned after a flush.

```rust
let frames = replica
    .author_bundle(handle, vec![
        RowWrite::changed(public_row),
        RowWrite::changed(private_row),
    ], vec![])
    .await?;
```

| Bound | Value |
| --- | --- |
| Rows per frame (`ROWS_PER_FRAME`) | 32, one scope per frame |
| Frames per bundle (`MAX_BUNDLE_FRAMES`) | 64 |
| Rows per bundle (`MAX_BUNDLE_ROWS`) | 2,048; each row once |
| Encoded writes (`MAX_BUNDLE_BYTES`) | 4 MiB |

A larger SQL transaction commits as consecutive bundles, a
[staged publication](secure-sql.md#staged-publications).

Every write is admitted against the node's grants and the application checks
the exact transaction across scopes. One refused write, an exceeded bound or
a failure before the commit commits nothing, frames already signed for other
scopes included, and leaves the sequences unused. A dependency must belong to
one of the bundle's scopes. The same handle and writes return the same
frames, also after a restart or recovery import; other writes conflict.

`RowWrite::assigned` lists fields an update republishes although unchanged.
Each needs a row/field grant like a changed one.

A [coordinated write](coordinated-writes.md) is a bundle committed with
`SecureSql::commit_coordinated`: its frames carry their log position and no
peer receives them before the domain's coordinated log commits the entry;
an abort voids them.

On adopting a removal, incompatible bundles never offered to any peer are
[held whole](held-changes.md), voided and rolled back locally. Offered bundles
keep their original signatures and apply kept fields. Applications inspect
the held list and current values, then re-author through a secure SQL
transaction or drop the change; neither bypasses ordinary write admission.

## What a receiver applies

Each frame carries the bundle's member list, every entry sealed under its own
member's scope key. A receiver applies together all members it can open, may
read and holds; it waits while one of them is missing. Members of other
partitions do not block it.

| Receiver | Result |
| --- | --- |
| Holds and reads every scope | Applies every member in one commit, after the last arrives |
| Reads the public scope only | Applies the public members; cannot open the others |
| May read a scope but lacks its key | Applies the rest; that partition applies once the key is installed |
| Finds members that disagree, or one rejected | Rejects every pending member it requires |

While a required member is missing, the received ones report
`PendingDependencies` and the scope's pages report `complete == false`.
Receipt and applied acknowledgments stay per frame, so each describes exactly
one partition. Members applied before a checkpoint count through the log;
the signed checkpoint vouches for them.

A reader of a member learns how many members the bundle has, not their
scopes. Relays see ordinary frames; timing and sizes stay visible.

## Sessions

`EncryptedReplica::session(scopes)` binds what authentication and enrollment
supplied at open. No call argument names a principal or a grant.

| `SessionStatus` field | Meaning |
| --- | --- |
| `principal` | The user the credential chain delegates from: the login identity |
| `node` | The device key that signs every operation |
| `manifest` | The definition the replica runs; credentials name none |
| `revocations`, `revoked` | The digest and count of the live [revocations](revocations.md) the credentials were verified under |
| `expires` | Earliest expiry in the chain |
| `scopes` | The resource set asked for; each must be granted |
| `readable`, `writable` | Scopes read with an installed key, scopes with a write right |

Every session call checks that the replica's authority has not changed since
the session opened and verifies the credentials at the current time. These
end every session: `learn_revocations` when the set changed, `renew_credentials`, `evict_key`,
`install_key_epoch`, `retire_key_epoch` and `logout`. `renew_credentials` evicts keys the new grants cannot read whole.
`logout` drops every key from memory; stored plaintext projections remain at
rest. An offline node enforces only the revocations it has learned;
`SessionStatus::revocations` shows which.

`SecureSession::transaction()` stages inserts, updates and deletes in memory
and reads its own writes. Each staged write is admitted at once and again at
commit; a refused edit leaves the staged row as it was.

```rust
let mut transaction = session.transaction();
transaction.update(scope, row, values, UpdateMode::Changed).await?;
let frames = transaction.commit(handle).await?;
```

`UpdateMode::Changed` publishes values that differ and needs rights to
those fields only. `Assigned` republishes every assigned value and needs a
right for each. Unchanged permission-controlling fields are never
republished; changing one needs a strong command. A row another writer
changed since the transaction read it fails the commit with `Conflict`.

The commit is admitted under the replica lock inside the write transaction.
A revocation learned before that point refuses it; one learned after it applies
to later calls. Bytes already returned cannot be taken back.

## Peer sessions on links

Peers authenticate with the [live link](live-links.md) handshake, signed under
`rocky.link.v9`; any other version is refused. Each hello declares the
generation its sender runs, since a credential chain names none: a replica
sends a peer only frames of generations it admits, and a sync that meets a
newer one [adopts it](adopting-generations.md). Every request runs as a
`NativeSession` on one of the link's request channels. Each session message is
signed under `rocky.native-frame.v2` over the digest of the session binding,
its sequence and the digest of its body, so signing hashes the same few bytes
however large the message is. A peer without a coordinator answers command
requests with a refusal and keeps the session. A frame's signed header names its author's credential chain by
digest. A channel carries each distinct chain once, in a message of its own
before the first page whose frames name it; later pages of the channel send
only frames. Each side keeps that table for the channel alone. Sharing the
bytes shares no authority: every received frame's chain is verified against
the current [revocations](revocations.md), so a chain revoked during a
session admits no later frame of it. A frame whose chain the session never
carried is refused as incomplete and not stored; `CatchUp::incomplete` and
`Pulled::incomplete` count such frames, and a pass on another session
receives them. See [decision 0053](../decisions/0053-chains-by-reference.md).

A partition's grants are checked
in addition to its [holding proof](replica-roles.md#proving-a-partition). Reader
proofs bind the key epoch, scope and mutual handshake; keyless custody uses the
tuple and custody authority. A link proves the holdings of both sides when
it opens; a channel may prove more. Frontier batches refuse unproved members alone.
Scoped requests fail with the same denial and session outcome as an unknown
scope, before row, checkpoint or command result state is inspected.
Scope enumeration includes only proved partitions and granted unpartitioned
scopes. Proof memory is local to that link; the application
`SecureSession` transaction and invalidation rules above are unchanged.

The link's bounded logical channels carry the request handlers for rows,
checkpoints, existence, results, envelopes, contacts and
[coordinated entries](coordinated-writes.md). Admission
and holding proofs bind the link; each channel has its own signed sequence and
chain table. Committed pushes carry self-contained chains and use the same
receive transactions as catch-up. Authority changes cancel channels and notify
the link owner without polling. A peer holding proof cannot register a new
reader partition; its envelope remains the installation path. One-shot Pair,
Renew and Command protocols remain separate.

## Qualification

`cargo test -p rocky-secure --lib tests::bundles`:

- A 41-row bundle over a public and a private scope: full readers apply it
  in one commit in any arrival order and across a restart; public readers
  apply their partition alone; a late key applies the private partition.
- An injected failure, a busy file, one denied write, an invalid row, a
  duplicated row and the bounds commit nothing. Handles return the same
  frames after a restart and without their member records.
- A forked log that makes members disagree is rejected in both arrival orders.
- Checkpoint, audit and pruning of bundle members; a later partition applies
  against pruned partners.
- Sessions end on renewal, attenuation, policy and logout. Read-only,
  custody-only and field-limited principals cannot widen their writes.

`cargo test -p rocky-secure --lib tests::chains` covers the chain table: one
chain across two pages of a 20-grant author, a withheld chain in a pull and a
push and a revocation inside a channel. `tests::admission` refuses another
link handshake version.

This is application-level evidence, not a cryptographic audit.
