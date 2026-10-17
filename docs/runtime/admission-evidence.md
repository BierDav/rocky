# Admission evidence

Status: specified in phase 17; implementation in progress. This guide states
the contract the implementation must meet; no test evidence is claimed here.

A row identity is never reused by its author in a table, across partitions,
pruning, rebuilds, checkpoints, eviction and reopen
([row identities](logical-schema.md#row-identity)): honest allocation always
continues above the table-wide mark, and a receiver refuses what its own exact
evidence proves (see Limits). A permanent maximum alone
cannot tell a frame this replica once admitted and later pruned from a forged
birth reusing the same identity: both sit at or below the mark. The replica
therefore keeps exact, local evidence of its own admission decisions.

## Five records

| Record | What it holds | Raised by | Lowered or removed |
| --- | --- | --- | --- |
| P, permanent marks | Per `(scope, author)` maximum and signer, at the concrete and the table allocation scope | Every admitted birth and every installed checkpoint statement | Never |
| O, own-birth floor | Per concrete `(scope, author)` maximum of the author's own births; carries excluded | Own births and installed statements' `own` | Never |
| S, summary floor | Per concrete `(scope, author)` maximum of installed statements' `own` | Installed statements only | Never |
| L, replay ledger | The installed base's `own` folded with own births of replay-eligible frames after it | Replay and admission | Recomputed whenever eligibility changes; reset on eviction |
| R, admission journal | Signed receipts of birth-bearing admissions and the claims derived from them | Admission | Never, until the file's domain identity is retired |

P keeps today's role, including the allocators' table-wide reads. S records
identities retired by summarized history this file holds no individual
evidence for. L is a cache of the [replay ledger](lazy-compaction.md) for the
installed workspace state and must equal its recomputation at open and audit.
Carries raise P but never O, S or L. The invariant is S ≤ O ≤ P and L ≤ O.
L folds only own births of frames replay itself would apply at the workspace tip, never
abandoned frames, held frames the workspace does not see, or frames a tip
rebuild hides. Every transaction that changes a scope's base, the workspace
tip, the graph or a frame's eligibility recomputes it before committing. A
scope waiting for its seal has no L, and a decision that needs it waits rather
than reading it as empty. A checkpoint switch no
longer deletes and restores P: it raises P from `sequence`, and O and S from `own`, replaces
L, and replays retained frames through the same admission rule as a frame
that returns later.

## Receipts

A frame gets a receipt only when one of its writes is a birth. Frames that
only update or delete get none, so R is not a second operation log. A receipt
holds:

- the exact frame hash, signer, credential author number, concrete scope and
  table allocation scope;
- the scope-log sequence and domain author position, kept as distinct
  coordinates;
- commitments to the source tip and its schema; when path filtering withheld
  the frame's births, the admission target and the floor, the frame author's
  concrete own-birth mark O at the frame's own first admission;
- the frame's original write count and, per birth by original ordinal, one
  disposition.

| Disposition | Meaning |
| --- | --- |
| Created | This exact frame and ordinal was admitted as an original creation |
| Consumed | Admitted without creating; it never becomes a creation |
| Filtered | Not admitted at the recorded target; it grants nothing |
| Carried | A moved row's original creation observed; no grant to the carrier |

A Created birth is the only exception to the fresh O check, and only for the same
frame and ordinal returning with the same provenance. It never authorizes
another frame, ordinal or scope, and it does not override replay retirement,
path filtering, authority, revocation, the creation proof, dependencies,
bundles, clocks, partitions, [no-frames](encrypted-checkpoints.md#no-frames-checkpoints)
refusals or the own-log restoration guard. An ordinal without a disposition is
never a grant. A missing journal for an applied birth is corruption; there is
no empty-journal reading and no backfill from rows, maxima or applied markers.

Provenance is a domain-separated commitment to the original creation
statement (manifest, table, identity, stamp and binding digest), shared by
live and stored creation proofs. It is never computed from a transport frame.
Per normalized table identity, the claims hold at most one original
assignment and any carried observations.

A filter withholds all of a frame's births or none, since a frame writes one
table. A Filtered birth is neither a veto nor a grant: when another target's
path makes it eligible and no original assignment names its frame and
ordinal, it is restored only if it passes, in order, the signer and author
binding checks, the floor (the first-admission identity check against the mark
snapshot of its own admission), the known claims, S, L, and the creation checks
a current birth gets, including its creation proof. A restoration mints a new
record and claim at a write transaction; the original receipt never changes.
An audit that finds such a birth materialized without that record reports
corruption. Consumed births are never promoted.

When an absent table has no known allocation binding, an all-Filtered receipt
records an explicit unresolved allocation. It invents no template scope or
marks. A later qualified descriptor supplies the binding; the signed
Restoration records that full allocation without changing the receipt.
The floor is the concrete own-birth mark O, and S rises only from an installed
statement's `own` in its own partition. A first admission is refused at or below
O, on an existing original assignment or contradictory provenance in any
partition, or below concrete S; another partition's maximum or summary never
refuses a birth, since it proves no particular origin and an honest author's sequences arrive
across partitions in any order. L stays concrete.

A filtered carry instead passes the ordinary bundle, move, proof, partition
and signer checks. Sequence retirement does not suppress a legitimate carry.
Its first eligible materialization signs a separate CarryObservation and a
kind-1 provenance claim, never an original assignment. A derived lookup binds
each observation to its parent receipt and ordinal. Audit checks both lookup
and claim indexes against the signed entries; equal carried observations share
the first claim. Check mode requires this evidence and never creates it.

## Kept views

Kept seal views, reads at a seal, held-seal reports and signed kept states
read history rows and covered marks only. They never consult P, O, S, L, R or the
claims, never classify a birth and never mint. A restoration, a lowered base,
new claims or eviction change only live rows, whose replaced versions history
keeps, so a kept seal reads the same before and after.

## Checkpoint comparisons

Before any destructive step of an import, and for every staged page, an
incoming row whose original creation provenance differs from a known claim
for its identity refuses the import with nothing changed. The same original
proof, carried legitimately, is accepted. A row's current partition is never
equated with its original one. Checkpoint provenance never becomes a claim or
receipt, and an inflated maximum is never a permit.

## Integrity

Each receipt is signed by the admitting node and chained to the previous one.
A signed head binds the count, chain and committed revision, inside the same
transaction as the rows, projection, status, outbox and revision. Open checks
the head and restore cuts; a receipt is verified in full before any decision
uses it; the full audit verifies every signature, the chain and the claims.
Signed receipts are never served or transported.

Within one frame and transaction attempt, classification may reuse the owned
derived tuples of an already verified immutable entry. Every lookup still reads
the current SQL claims and compares their complete tuples. This memo is discarded
at frame end or rollback; it carries no current authority across frames or commits.
It assumes no privileged replacement of the verified source within that attempt,
which no supported runtime transition permits. Full audit independently derives
and compares the complete claim index.

## Lifetime and non-powers

P, O, S, R, the claims and the table allocation bindings stay until the file's
domain identity is irreversibly retired. Compaction, a covering checkpoint, a
losing seal, acknowledgments, releasing a kept seal, table removal and scope
eviction do not retire them; eviction and a reset replay clear L only.

R authenticates a past local decision and nothing else. It is never consulted
for dependencies, stubs, log tips or pruned prefixes, existence or liveness,
support rows, the `Applied` outcome, the restoration guard, checkpoint
creation sets, frontiers, holdings, custody or acknowledgments. It retains no
row key, values, deletion event, creation clock, raw stamp, bindings, remote
signature or schema document, and it keeps no schema required. Without the
returning frame it cannot recreate a row. The phase 15 gate therefore still
holds as written: no tombstones, stubs or existence records for deleted rows.
A receipt does count as scope history: any receipt of a scope forbids
signing a no-frames base for it, and a receipt the exact recorded seal of a
no-frames fact covers is a contradiction, judged by the same
[exact coverage](encrypted-checkpoints.md#exact-coverage) as frames, voids and
placements.

## Backup and restore

A backup copies R with the rows and status in one snapshot; the SDK inventory
already binds the file. Adopting a restored file under its fresh node verifies
the predecessor's journal in full once and records a finite cut: the
predecessor's lineage, node, count, chain and revision. Receipts up to a cut
verify only under that cut; a predecessor's receipt beyond it, or a sibling
backup's, is refused. New receipts belong to the new node. A raw copy's cut
rests on the caller's assertion that the copy is genuine.
See [backup and restore](backup-and-restore.md).

## Growth and accounting

R grows with every birth-bearing admission and is never evicted at a quota or
counted inside the 64 MiB history budget. It is reported beside the phase 15
row-state plateau, never folded into it:

1. logical encoded bytes of receipts, signatures, chain values, claims,
   ledger, summary, bindings and context;
2. allocated pages per table and index (`dbstat` where available, else total
   main-file pages);
3. main-file bytes and WAL bytes both before and after a truncating WAL checkpoint.

Backups add one full copy; each restore cut adds a constant-size context
entry. Graph history is reported separately. In the churn fixture, cycles 16
to 64 add 48 receipts, 48 original assignments and 48 chain values, and leave
P, O, S, L and the bindings unchanged. Fixed-width fields of a one-birth receipt
come to 288 bytes before the scope, integers, encoding and SQLite overhead: a
component floor, not a size prediction; a filtered receipt adds its floor.
Total replica storage is not constant.

## Limits

- Restoring an earlier, genuinely signed head with its prefix is not detected;
  there is no external monotonic anchor. Deletion of entries, foreign-lineage
  splices and predecessor entries beyond a cut are detected.
- After eviction or a lowered base, an honest birth this file never admitted
  individually at or below S, or a return with only summary evidence, is
  refused with its own reason. The repair is a listed checkpoint covering it
  or owner recovery.
- Two filtered births of one identity, signed by the identity author's key
  while its table was not installed here, resolve per device: the first in
  replay order wins and the other never mints.
- A birth is refused when this file's own evidence proves reuse: a known
  original or carried claim for the identity in any partition, or the
  author's own-birth mark in the birth's partition. An identity author's key that
  signs a second origin is otherwise detected per device, like two filtered
  births of one identity. This applies when the second origin is in a
  partition where this file knows the earlier use only as another
  partition's maximum or summary, or a carried origin's summary maximum, and on any reader that does not hold the
  earlier partition. Honest authors never do this, because allocation always
  continues above the table-wide mark. A maximum never refuses an honest
  birth that arrives late from another partition.
- Open costs constant time plus one step per restore cut; a decision verifies
  one receipt; the full audit and restore adoption are linear in R.
- R is a permanent cost proportional to births.

The file format stays `rocky.encrypted-replica.v32` with these records
mandatory; link, checkpoint, row-frame and backup formats do not change.
