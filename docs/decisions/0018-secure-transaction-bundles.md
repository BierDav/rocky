# 0018: Secure transaction bundles, session authority and typed secure storage

Status: accepted. Date: 2026-10-05. The API is in
[secure sessions](../runtime/secure-sessions.md) and
[secure SQL](../runtime/secure-sql.md); this record keeps the body format, the
visibility rule and the rejected alternatives.

## Decision

### Bundle wire format

A bundle is one local transaction of one author. Its writes are split per
scope in scope order, at most 32 rows per frame and 64 frames: 2,048 rows and
4 MiB of encoded writes. Each frame stays an ordinary signed frame of its
scope's log; partitions of one scope take consecutive sequences. The bundle is
recorded inside the encrypted body, never in the header:

| Body field | Meaning |
| --- | --- |
| `version` | 1 |
| `transaction` | Commitment to author and handle: the durable transaction identity |
| `index` | This frame's position |
| `members` | One sealed entry per frame, identical in every frame |
| `writes` | `RowWrite`s: the exact row change and its write mask |

Entry `i` encrypts member `i`'s scope and sequence under member `i`'s scope
key, with the transaction, index and count as associated data. A reader learns
which members it can open, and the member count. Relays see ordinary frames:
no header names the transaction, so nothing new links a public and a private
frame except timing and size.

### Write masks

`RowWrite` pairs a `RowChange` with `assigned`, the fields an update republishes
although unchanged. Admission requires a paired row/field grant for every
changed and assigned field, and a mask only on an update and only for fields
the after image holds. The merge stamps every published field. Under the
changed-values policy masks are empty, so read-only columns cost nothing.

### Visibility gate

A receiver stores members in custody as they arrive. When one is ready it
opens the entries it can and requires the members whose scope it holds, whose
key it has and which it may read. It applies all pending required members in
one savepoint, in scope and sequence order, after each passed admission, the
same membership check and dependency readiness, where a predecessor inside the
bundle counts as ready. A missing required member keeps the rest pending.
An invalid member, a disagreeing membership or a rejected partner rejects every
pending required member; a retained applied member must carry the same
membership. Members of other partitions never block: a public reader applies
the public partition alone, and a partition whose key arrives later applies
then. A pruned member counts as applied through its log tip or horizon; only
the signed checkpoint vouches for it.

Acknowledgments stay per frame, so each covers exactly its partition.

### Session authority

`EncryptedReplica` counts an authority generation, raised by `learn_policy`,
`renew_credentials`, `evict_key` and `logout`. A `SecureSession` binds the
login principal (the user the chain delegates from), the node signing key,
the manifest, the policy epoch and an explicit scope set at that generation.
Every call checks the generation and verifies the credentials at the current
time. Commit admission runs inside the replica's write transaction under the
replica lock, which is the linearization point: a policy learned before it
refuses the commit; one learned after it applies to later calls only.
Renewal evicts keys the new grants cannot read whole.

## Rejected alternatives

- **Bundle metadata in signed headers.** Relays and public readers would learn
  every member scope and link private frames to public ones.
- **One merged frame for all scopes.** It would widen readers to the union of
  reader sets, which phase 4 forbids.
- **Waiting for every member.** A public reader would wait forever for frames
  it can never hold.
- **Authority checked only at preparation.** Revocation would not end an open
  session; checks run at every call and at commit.

## Consequences

- A delete stays a permanent tombstone: SQL cannot insert a deleted key again.
  A delete and reinsert in one transaction republishes the whole row.
- Partial visibility is per reader: a reader that lacks a partition's key
  shows the others first. Views needing it stay unavailable, not empty.
- Field-limited reading still needs a separate reader scope: a key grants
  whole rows.
