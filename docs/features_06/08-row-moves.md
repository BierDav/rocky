# 8. Row moves

Status: not started.
Owner: `rocky-secure` frames, apply and constraint resolution; `rocky-sdk`
and the Kotlin write surface. Depends on phases
[4](04-partition-aware-constraints.md) and [5](05-partitioned-foreign-keys.md);
lands after [phase 1](01-remove-compatibility-code.md) on the frame and after
[phase 6](06-horizon-checkpoints-bootstrap.md) on the checkpoint. Wave 3.
On the critical path; the decision record may be drafted during phase 4.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [moving rows between partitions](scope-moving-rows.md),
[design rationale](../runtime/logical-schema.md#row-identity) and
[decision 0018](../decisions/0018-secure-transaction-bundles.md). A move is a
delete and an insert today, and the insert is a new incarnation, so an update
made offline to the old row is lost. Per-field stamps already exist in the
row store; a move must carry them.

## Ordered slices

1. Decision record 0038, from the scope's
   [decisions](scope-moving-rows.md#decided-2026-10-08). It includes `id`
   allocation per author and table for every partitioned table, the creation
   proof that admits a kept foreign-author `id` in B, the pseudonym rule it
   relaxes and replay of one proof into a second partition.
2. The move in one bundle: a deletion in A and an insert in B under the
   kept `id` and its creation proof, carrying every field's value with its original stamp and the `moved from` link, sealed
   under A's key. Both or neither apply; 0018's visibility gate already
   holds a bundle's members until all held scopes arrive. Today a stamp is
   the frame header's `{clock, author, sequence}`, with no per-field stamps,
   so carrying stamps is a frame body change. A carried stamp never exceeds
   the carrying frame's stamp. The link is data, not a causal dependency;
   dependencies stay in one scope.
3. Admission: the mover needs delete in A and insert in B on every moved
   table; a device holding one side verifies its half without the other key,
   so admission of B's insert never depends on opening the link. A table
   partitioned by its own `id` cannot move. A reference made only of
   partition columns follows the move and resolves in B.
4. Following: only the author's device issues its late update again in B,
   with the original stamp, when it holds B with the needed right. Otherwise the update
   is dropped identically on every device.
5. Children: rows reached through in-group `CASCADE` references move in the
   same bundle, without a separate call. `SET NULL` references to rows that
   stay are nulled; a `NOT NULL` one, a bundle over its limits or a child
   table the mover does not hold refuses the whole move, naming the cause.
6. Chains, horizon and checkpoints: a row moved twice is followed; links not
   yet finalized and the carried field stamps of moved rows are carried by
   checkpoints. B's horizon also applies: a followed update must still be
   admissible in B.
7. SDK, SQL and Kotlin surface for a move, including Room.

## Completion gate

The six scenarios of [the scope's acceptance](scope-moving-rows.md#acceptance),
deterministic and multi-process, and:

- An interruption between the two halves never shows the row in both
  partitions or in neither.
- A followed update is issued once, also after reopen and repeated sync.
- A device holding only A learns nothing about B from the move.
- A row moved A → B → C receives an update authored against A.
