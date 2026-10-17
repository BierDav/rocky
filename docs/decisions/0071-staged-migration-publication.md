# 0071: Staged migration publication

Status: accepted; phase 17 implementation under qualification.
Date: 2026-10-14; refined 2026-10-18.

## Decision

A migration runs once in one local SQL transaction on a fresh migration tip.
The issuer first signs a close of its followed source tip, an opening with the
new manifest, and a forward bookmark move. The private stage freezes only the
parent tables the SQL names. Its publication identity is
`rocky.sdk.migration.v3(target_tip)`; bundles retain ordinary row-frame encoding
and application semantics. See [0075](0075-generations-as-sealed-forks.md).

Staging rechecks the followed open source, bookmark, absence of another private
publication and exact signed frontier under the same replica guard that admits
the statements and freezes the parents. An intervening source write refuses the
unchanged candidate before mutation; preparing and signing again includes the
new frontier. Withdrawing an interrupted private switch validates its intent
and installed source application before changing the live graph.

The SQL commit journals projection changes, operations, existence records,
outbox, handles, revision, publication completion and an exact unsigned
migration descriptor. A paged disk-backed Merkle-sum tree binds its frames;
construction keeps one page and a logarithmic stack in memory rather than
another publication-sized array.

After commit, the opening signer's `IdentityProvider` endorses those exact
journaled bytes outside every database transaction. An unavailable signer
leaves the committed candidate private. Reopen retries the same intent without
running SQL again. An uncoordinated publication releases after endorsement;
a coordinated publication additionally requires the existing terminal-finality
proof for its exact closing seal. Descriptor endorsement supplies no terminal
acknowledgment and lowers no line minimum.

## Authenticated storage account

`MigrationPublicationStatement` format 1 binds the domain, target tip, exact
opening and parent close, target schema, publication identity, designated node
and credential-bound author number, frame count, retained frame bytes and one
ordered Merkle-sum root. Only the exact opening identity can endorse it. The
opening must be a migration successor, with a different schema on a close and
the inherited signed minimum. The designated node's initial domain-wide
sequence is `0..N`; later sequences and other authors receive ordinary capacity.

Each ciphertext carries a bounded inclusion proof committing to its index,
exact frame hash and stored header-plus-ciphertext length. Internal nodes bind
child hashes, counts and checked byte sums. An empty migration has zero count,
zero bytes and the domain-separated empty root. Descriptor/proof verification
allocates no storage proportional to an advertised extent.

One immutable statement is allowed per publication. Equivalent endorsements
are idempotent; different valid statements create a bounded equivocation record
and stop further acquisition, without another account. Graph authority and
digest ordering alone select the migration winner.

For retained operations, `T` is their header-plus-ciphertext bytes and `M` is
the bytes with verified migration membership. Ordinary admission requires
`T - M + incoming <= 67,108,864`, using checked arithmetic. Migration admission
requires the descriptor and exact proof; a generic `commit_publication` record
confers no exemption. Transfer framing and proof bytes also consume existing
page, frame and session budgets.

Insertion, classification, counters and outbox commit together. Every operation
deletion debits its account in the same transaction, including prune, checkpoint
import, eviction and branch disposal. Retained frames keep self-contained proofs
when neighboring leaves disappear. A compact publication identity remains after
pruning. Open-time and explicit audits verify provenance and reconcile counters.
Descriptor, proof, index and support history are migration storage, rather than
free disk. Disk-full and durability failures remain normal failures.

## Acquisition and privacy

Only required followed-path history or explicit local retained evidence permits
an exact descriptor lookup. No discovery message triggers unrelated branch
acquisition. A missing descriptor leaves acquisition pending. Keyless custodians
verify descriptor, proof, frame signature, scope and custody rights without
opening ciphertext, and never report applied full-node custody. Descriptors
contain aggregate volume, never public bundle-member lists or partition maps.

Bundle dependency, field, existence and path-filter checks remain unchanged.
Progressive bundle application is permitted; global completion requires all
indices and authenticated totals. Partial readers use their authorized scope
frontiers and do not claim global completion.

Losing or revoked retained history keeps its provenance account until legitimate
pruning or disposal. It never projects into a winner or becomes automatic
re-authoring work. Source writes beyond the signed close remain held and gain
no migration exemption. Checkpoints remain exclusively at seals.

## Formats and qualification

The replica file is `rocky.encrypted-replica.v32`, live link `rocky.link.v14`.
Previous versions are refused. Row frame 11, bundle payload 5 and opening 2 are
unchanged. No coordinator payload or storage format changes.

The issuer endorsement proves authorization of exact initial frames; it cannot
prove that arbitrary SQL produced them. A malicious authorized issuer can endorse
a harmful or large migration. An ordinary node cannot manufacture that authority.

See [phase 17 evidence](../features_09/17-schema-changes-through-seals-evidence.md)
for current qualification results and the unchanged 100,000-row gate.
