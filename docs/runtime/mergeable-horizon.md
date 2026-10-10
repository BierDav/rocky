# Issuer-declared mergeable horizon

Ordinary [encrypted checkpoints](encrypted-checkpoints.md) keep tombstones,
hidden rows and creation stubs forever. A mergeable horizon finalizes a scope's
rows at an issuer-signed cut so that this evidence, and the
[existence log](existence-logs.md) records behind it, can be released. This guide
is for an author who schedules horizons, syncs across one, or changes the format.

## Declare, acknowledge, finalize

1. Sync the scope and its row/existence dependencies. The SDK captures a cut
   with participants from the issuer registry: unrevoked whole-scope readers
   and custodians, with their latest recorded chain expiry. An explicit list
   selects registry records; over 64 returns `horizon_participants_full`.
   The publisher signs outside the transaction. Any scope-proved reader or
   keyless custody peer stores and relays it and participant-signed
   acknowledgments; participants need no session with the issuer.
2. Participants sync missing cut evidence, then `acknowledge_horizon` and
   `receive_horizon_ack`. Exact row heads/digests and every existence cut must
   match. A holder already ahead of the cut refuses the proposal and imports the
   finalized checkpoint instead.
3. `horizon_progress` reports acknowledged, waiting and behind devices. A
   required device becomes behind at the minimum of its credential expiry and
   issue time plus 30 days (`HORIZON_WAIT_SECONDS`); restart preserves the
   deadline. A participant revoked in the locally held set stops being required
   immediately. A first acknowledgment after it is refused; rebootstrap is the only
   way back. The SDK's `horizon` status also reports the statement's `issued`
   time and `deadline`, the latest deadline of a waiting device: finalization
   waits no longer than that.
4. `finalize_horizon(scope)` reproduces every signed row and existence cut, then
   in one transaction drops tombstones, hidden primary-key and unique losers and
   cascade orphans, stores substituted SET NULL values, preserves register
   stamps and identity high-water marks per author/partition, installs the
   format-9 checkpoint and existence attachments, advances log floors and the
   revision. Notifications follow commit and flush. Retry is idempotent.
5. `prune(scope)` then releases evidence only when the ordinary copy gates and
   `HorizonCustody` permit: `max(1, copies)` other holders must acknowledge the
   cut or confirm a finalized rebootstrap. A behind participant is exempt from
   required-peer waits, never from the durable-copy count.

While a cut is pending, new writes in the declared scope and its cut
dependencies are refused and incoming operations beyond the cut wait; missing
cut operations can still arrive. This availability cost prevents finalization
against changing input.

## Dependencies and ordering

A statement embeds `related` cuts (transitive same-reader parents, stopping at
a logged parent) and `existence` cuts (referenced logged tables). Each existence
cut binds a base descriptor: live-state count/digest and sorted per-author
identity marks with their signing keys.

- A logged parent's own base authenticates only its resolved surviving real rows,
  excluding puppets and virtual rows. Finalize logged parents before declaring
  dependent child cuts; child-only holders import that base, without a parent
  row key, before acknowledging the child.
- For unlogged same-reader parents, finalize dependent children before the
  parent removes their captured input. Finalizing a parent refuses while a
  pending child declaration captured it, and automatic finalization during
  sync waits, so no participant changes a cut a child still needs.
- In-group dependencies name the parent's concrete scope in the same partition.
  Declaration and acknowledgment require that parent partition locally;
  required peers must be authorized for every related row scope. Devices holding
  only B never participate in A's horizon.
- Pending declarations index their affected scopes. Finalization and checkpoint
  adoption clear the index atomically; unrelated partitions remain writable.
  Rebootstrap rebuilds candidates and support within the affected group partition.
  An unpartitioned existence-base import rebuilds its connected constraint graph.
  See [decision 0043](../decisions/0043-partition-horizon-state.md).

## Existence bases

A base stores live IDs and creation commitments, unknown delete-before-create
witnesses, per-author sequence/signer marks and per-log heads. Absence below a
mark is definite nonexistence; unknown future IDs remain unknown. Pages hold at
most 256 entries. `existence_base_offer/page`, `begin_existence_base`,
`stage_existence_base` and `finish_existence_base` transfer them with durable
cursors, canonical-order and root checks, and an atomic base/suffix switch;
incomplete or forged pages leave installed state untouched. Only current
checkpoints, proposals and staging keep old base archives. A child base cannot
reverse a parent's retirement at the same cut.

## After the cut

A read-only participant acknowledges the cut and imports a writer's finalized
checkpoint. Automatic sync finalizes locally only with whole-scope read and
write authority; a reader must not publish a checkpoint that custody would
reject for lacking write authority.
Keyless custody retains the writer's finalized checkpoint as a sealed archive and
serves it to read-only participants; see
[encrypted checkpoints](encrypted-checkpoints.md).

- An authenticated edit/delete of a definitely absent incarnation is consumed
  without row effects, including one whose creation was pruned. Its author log
  continues; a local one keeps operation/outbox/handle semantics. The trusted
  profile classifies through `Application::definite_drop`.
- Finalized survivors protect primary and unique keys against stale inserts.
  Inserts sign the finalized sequence and commitment they were authored against:
  births from an older cut are consumed without rows (reserving their identity
  sequence), frames naming a future cut wait for that base. A dropped insert
  therefore stays dropped across successors.
- Inserts into keys vacant at the current cut keep the deterministic concurrent
  winner rules. Reusing a released baseline key needs a signed causal dependency
  on the release and the same finalized basis.
- Behind devices cannot sync rows or write until native `sync_scope` or SDK sync
  imports a format-9 checkpoint. Learning the remote cut installs a durable
  write/sync guard before pages download; only a covering finalized checkpoint
  clears it. Bootstrap fetches existence attachments, then row pages, then sends
  an authenticated finalized-bootstrap receipt.
- A long-offline holder may skip intermediate declarations through a newer
  finalized checkpoint covering its held cut; proposal replacement otherwise
  needs the immediate signed successor. Concurrent siblings choose the lower
  statement ID and discard the loser's acknowledgments. A holder that already
  finalized the higher ID uses the guarded rebootstrap path to the lower
  finalized winner. See [decision 0055](../decisions/0055-relayed-horizon.md).

## Formats

| Boundary | Current |
| --- | --- |
| Horizon statement | Format 5 with sorted policy IDs and the declaring issuer's [revocation](revocations.md) digest, signed `rocky.horizon-statement.v1`; acknowledgments `rocky.horizon-ack.v2` bind statement, node reference and credential alias |
| Checkpoint | Format 9 carries independent optional admitted and finalized declarations and bases and a required `moves` summary; an admitted cut is never treated as finalized |
| Row frame | 8, with an optional authenticated partition tuple; signs creation witnesses, finalized basis and reuse proofs |
| File identity | `rocky.encrypted-replica.v14` |
| Native session | `rocky.native-session.v14` |
| Recovery package | Cannot carry declarations/bases: declared-scope export/import is refused; file backups preserve them |

Horizon statements accept only format 5, with an explicit existence-base list.
Checkpoint 9, frame 8 and replica identity v14 are the only accepted versions. Unknown formats, noncanonical order, invalid issuer signatures,
mismatching dependency sets, backward cuts and declaration changes during
staging are refused at admission and again at the final switch. Schema
generations must satisfy the checkpoint's manifest/dependency bindings.

## SDK

The native JSON `horizon` operation and Kotlin `client.horizons` /
`session.horizons` expose declare, admit, status, finalize and maintain. Sync
the cut first; declaration defaults to all eligible registry devices. A caller
may name a subset explicitly; every device must have an eligible registry
record, including authority over related parent row cuts. Schedule issuer
`maintain(table, maxOperations = 1024)`: it declares after that many operations since the last
finalization, then finalizes/prunes when ready. There is no automatic issuer
timer. A non-issuer can admit, check status and finalize. Sync supplies relayed
acknowledgments and behind rebootstrap. A custody peer verifies evidence without decrypting or checking the cut. Signed finalized
checkpoints retire its pending statement and acknowledgments. One bounded
completed certificate preserves the original signed acknowledgments for a
holder that fetches them after another participant has already finalized.
A reader whose rows or related parent cut differ from that evidence enters
the existing rebootstrap guard and imports the finalized checkpoint.

## Evidence

`rocky-secure::tests::existence` (`horizon_*`) covers child-only orphans and
SET NULL, unique and parent losers, expiry and forced rebootstrap without parent
keys, definite drops on plain and logged tables, stale-insert and causal reuse,
base paging and tampering, and rollback of the final switch.
`tests::partitions::horizon_preserves_identity_sequences_independently_in_each_partition`
covers per-partition marks. `tests::partition_foreign_keys::horizon` covers
partition lifecycle finalization, six rollback boundaries with reopen, keyless
support/base import, custody-backed eviction, offline rebootstrap and unchanged
unrelated partition state. Kotlin/Room `kotlinHorizonsFinalize…` drives the
SDK path. The plateau probe:

```sh
cargo test -p rocky-secure --locked horizon_finalization_and_retention_plateau -- --nocapture
```

Two holders, 64 insert/delete cycles, one other copy, zero window, four handles:

| Cycles | Tombstones, stubs, existence records, retained operations | SQLite pages | Bytes |
| --- | --- | --- | --- |
| 16 | 0 | 170 | 696,320 |
| 64 | 0 | 170 | 696,320 |

This holds for fixed live data and authors, bounded window/handles and progressing
durable copies; growing data, new authors or unavailable copies retain more.
Without finalization, the same churn retains one tombstone and two existence
records per cycle.

Finalized checkpoints also travel to keyless custody as sealed page archives.
The writer deposits referenced existence bases, the signed checkpoint and each
bounded page before relaying finalization evidence. Page receipt and its resume
cursor commit atomically; incomplete archives are not offered. Read-only
participants import the writer's cut through the normal verified rebootstrap
path after custody restarts. They never sign a checkpoint with an expanded grant.
See [decision 0061](../decisions/0061-readable-projections-and-checkpoint-custody.md).

## Design rationale

- The issuer, not a device quorum, declares the cut: only it can authenticate a
  base that child-only holders accept without parent keys.
- Pausing writes at the cut is deliberate; finalizing against changing input
  would let holders compute different survivors.
- Inserts bind the finalized basis because otherwise a dropped insert could
  return once a successor horizon removed its conflict.
- Finalization never relaxes the durable-copy rule: the last copy is not released.

Participants and acknowledgments retain one-way node references. An acknowledgment
signs with a distinct credential alias bound to that reference. The issuer remains
the trust anchor; see [decision 0062](../decisions/0062-node-references-and-credential-aliases.md).
