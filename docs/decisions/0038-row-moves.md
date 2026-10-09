# 0038: Row moves with creation proofs and carried field stamps

Status: draft; implemented after phase 6 commit `9a5368df`.
Date: 2026-10-09.

## Context

A delete followed by an ordinary insert creates a new incarnation and loses
updates authored offline against the deleted row. The accepted
[row-move scope](../features_06/scope-moving-rows.md) requires an atomic move
with a kept application-visible `id`, original field stamps and author-issued
late updates. A destination-only reader cannot open the source partition.
This record describes the implementation of those settled requirements;
qualification evidence belongs in the phase brief.

## Decision

### Identity allocation and creation proof

Every partitioned table allocates creation sequences per author and table,
across all its partitions, including tables partitioned by their own `id`.
Unpartitioned tables retain their author/table allocation. Reservations are
transactional; rollback releases them. Committed births advance the permanent
mark, consumed births included; moves do not allocate another identity.
Pruning, eviction, reopen and checkpoints must not let an author reuse a birth
sequence in another partition. A node that cannot recover its allocation mark
must not resume creation under that author until it has recovered the mark.

A creation commitment is signed by the original table pseudonym. It binds a
domain-separated proof version, application and table identities, `id`, and
original creation stamp. Its author binding proves that the signing key owns
the number embedded in `id`. Carry a portable issuer-certified author binding
naming only the application, table, pseudonym key and author number; carrying
the original partition-grant chain could disclose A. Verification establishes
creation provenance,
not a current grant for the creator to write B. The proof contains neither a
source tuple nor a source operation hash that identifies its log. Proof
verification must not require the original author's source partition grants.
A birth proof keeps its originating manifest through supported application
generation transitions. That manifest must be current or in the publisher-signed
lineage; the carrying frame still passes current generation admission.

Ordinary inserts still require the frame signer to own the identity's author
number. A move insert relaxes only that equality: its independently verified
creation proof may name a different author. The mover still needs current
insert authority on B, including all inserted fields. The pseudonym does not
become a node credential, and no device signs an update for another author.

A proof is not a transferable insert grant. It is accepted only in a move
insert bound to a newly signed bundle, its destination tuple and its kept
identity. Replaying the same frame remains an operation duplicate. Replaying
a proof into an ordinary insert is refused; reusing it for a subsequent
properly authorized move is necessary for A → B → C. A proof cannot by itself
prove that no concurrent move has inserted the identity in another partition:
readers with disjoint keys cannot enforce global exclusion. Independently
valid concurrent moves to different destinations are distinct branches;
within a destination the existing identity and incarnation rules apply.
This limitation must be qualified explicitly, never represented as globally
unique custody of an identity.

### Bundle, admission and privacy

A move uses [0018's bundle](0018-secure-transaction-bundles.md): delete in A,
insert in B, and every collected child in the same transaction. Each insert
carries the creation proof and every field's original value and stamp. A
carried stamp is bounded by the carrying frame's stamp. Changed partition
columns use B's values; fields nulled by the move use the move's stamp.
Immutable identity and creation provenance remain the original birth's.

The destination insert contains an authenticated encrypted `moved from`
link, sealed with A's key and bound to the destination, kept identity and
bundle. The plaintext names A's tuple and source incarnation. It is data,
not a cross-scope dependency. B's insert depends only on B's log. A reader
holding only A sees a deletion; a reader holding only B sees the creation
proof and opaque link, without the source tuple. Bundle membership stays
sealed per member as in 0018; it exposes no additional destination to A.

The mover needs delete in A and insert in B on every moved table. Each
reader verifies the half it can open without the other key. Readers holding
both apply both halves in one savepoint only after all required members pass
admission. Transaction failure publishes neither projections nor outbox nor
observer notifications. A table partitioned by its own `id` refuses moves.
References composed entirely of partition columns resolve in B.

### Children and outgoing references

Collect the transitive in-group `CASCADE` closure, deduplicating rows before
writing. Parent-only calls include those children automatically. Rewrite
in-group references to the collected destination rows. Null nullable
`SET NULL` references to rows remaining in A; refuse a non-nullable outgoing
reference that cannot resolve in B. Destination uniqueness and foreign-key
resolution use normal insert policies.

Before recording a frame, check every child table's holdings and authority,
every rewritten row and the full bundle's existing limits. Refusal names the
cause and table, including a missing child holding, insufficient field right,
non-nullable reference or quota. No partial move is committed.

### Author following, chains and horizons

Only the authoring device follows its missed updates. It compares each own
update's field stamp with the destination's carried stamp and republishes
only newer fields under its own signature with the original stamp. It must
hold B and the required update rights. A device lacking them does not issue
a follow; other devices do not compensate by signing on its behalf.

Persist the original operation/field and destination follow identity with
the signed follow and outbox in one local transaction. Reopen, repeated sync
and retry must find that record rather than issue another follow. Subsequent
links extend the destination path, preserving the original stamp; they do
not make cross-scope causal dependencies. Later B edits win by ordinary stamp
ordering. Refuse cycles and malformed links rather than looping.

Follow only while the source move remains unfinalized. The destination's
horizon and admission also apply; a carried stamp is not an exemption from
B's finalized basis. Consumed operations need an explicit result describing
fields or births dropped behind a horizon. Rebootstrap must include each
rejected unsent insert's hash and reason in `RebootstrapReport.rejected`;
advancing its author log cannot be reported as successful materialization.

### Formats and surfaces

The row header format is 6 and the bundle body version is 3. The body adds
creation proofs, carried stamps and sealed move links. Encrypted frame bytes
and the witness envelope's body use canonical CBOR byte strings. The witness
version remains 1, with exactly that encoding. The encrypted replica definition
is `rocky.encrypted-replica.v8`; the Trusted reducer definition is
`rocky.sdk.typed.v2`. Other versions and noncanonical encodings are refused;
there is no old reader, migration or dual encoding.

Checkpoint changes land after phase 6's per-partition state. Keep checkpoint
format 6 and add the required `moves` count/digest of opaque row links; do
not encode presence with a new format number. The links themselves stay in
the authenticated materialized rows, keeping the signed statement bounded.
Carried field stamps and proofs remain authenticated with their rows. Checkpoint import/rebootstrap restores links only for the
checkpoint partition, reconstructs constraints there and preserves unrelated
partitions. A checkpoint of A must not disclose a destination tuple. Finalized
links can be released only once following is no longer admissible.

The Trusted runtime implements the same identity, field-ordering and author
following rules without requiring encrypted links, within its existing admitted
schema capabilities. Its schema restrictions remain unchanged; the secure
profile qualifies the in-group foreign-key closure. SDK, SQL, native ABI and
Kotlin expose one parent move with source and destination tuples. `SecureSql::move_row`, the native
`move_row` request, Kotlin `moveRow` and Room `moveRockyRow` each commit a
dedicated atomic mutation outside an open SQL/Room transaction. Applications
need neither Rust code nor manual child moves.

## Consequences

Moving preserves field conflict ordering and identity rather than creating a
replacement row. It adds durable proof, link and follow state and requires
qualification of replay, reopen, interruption, horizon and checkpoint paths.
The six accepted scenarios and additional completion gates remain in the
[phase brief](../features_06/08-row-moves.md#completion-gate).
The draft decision is distinct from the completion evidence in that brief.
