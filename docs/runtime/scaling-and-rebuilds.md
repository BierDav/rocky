# Bounded history access and projection rebuilds

`EncryptedReplica` history paging, recovery-archive pruning and same-definition
rebuilds. Checkpoints and pruning are in
[encrypted checkpoints](encrypted-checkpoints.md), application migration in
[generations](generations.md), measurements in
[scaling limits](scaling-limits.md#measured-limits).

## Encrypted retained history

`EncryptedReplica::transfer_page` filters authorized resource scopes in SQL,
seeks after the retained hash cursor and fetches at most 33 records: 32 deliverable
records and one lookahead. A page respects both the record and encoded-byte limits.
Original signed/encrypted bytes are forwarded without reauthoring.

Pending processing examines only the first pending frame of each log, since a
later frame needs its backlink applied, and follows a log while its frames apply;
it starts another round when any operation advanced. A waiting log costs one
examination per round, so a long wait cannot starve the others. It applies at most
64 operations per transaction and releases the replica mutex between
transactions ([write concurrency](write-concurrency.md#encrypted-path)). Reopen
history verification fetches 32 records per batch.

Hash cursors describe a retained range, not a snapshot or commit sequence. Restart
a pass after new receipts or subscription changes, including insertions before the
cursor. Custody and application remain distinct. Operation headers and bodies
behind a verified checkpoint are pruned under a retention policy; tombstones,
strong requests and effect identities are not. Each page names the scopes with
a pruned prefix, and a replica behind it rebootstraps from a signed checkpoint; see
[encrypted checkpoints](encrypted-checkpoints.md).

## Redundant recovery custody

`prune_redundant_recovery(obsolete, successor, key)` removes a redundant encrypted
recovery archive only when its successor is already stored locally. Both packages
must verify against the installed publisher/policy, have the same exporter and
compatible definition/schema/handler/key epochs, and target this custodian.
The successor must cover every frame, result, preparation, command, mutation
handle, effect observation and durability record byte-for-byte. Its revision
cannot precede the obsolete archive's revision.

The successor's authenticated dependency closure is checked by opening the
package. Authorization covers every archive scope. A missing successor, missing
key, incompatible archive or omitted pinned record prevents deletion. The only
deletion is the obsolete archive; the successor remains durable. Repeating a
successful prune returns `false`. This API cannot release the last recovery copy
or infer remote durability from receipt/application acknowledgments.

Coverage is deliberately conservative: a confirmed result does not automatically
replace a different prepared-result encoding. Scope eviction, tombstone deletion,
key retirement and remote custody release are separate, unsupported operations.

## Local projection rebuild

`rebuild_projection(scope)` requires current whole-scope read authority, the
correct data key and no pending inputs in that scope. It starts from the scope's
verified checkpoint rows and replays the retained signed frames after it in
application order, retaining creation identities and permanent deletion
evidence. One batch of 64 frames is held in memory. The rebuild then replaces
the projection and its row digest and advances its revision, all in one
database transaction. Other scopes and strong projections are untouched.

The replica mutex serializes admission during replay/switch, so no concurrent
local receipt can escape catch-up. Observers are notified only after commit.
An injected failure after replacement but before commit restores the old rows
and revision and emits no notification. Successful rebuilds survive reopen and
preserve duplicate-operation detection and tombstones.

The installed application definition remains immutable. This rebuild changes no
schema/handler generation or strong head. It is a same-definition projection
refresh, not a signed compatibility transform, online schema upgrade or application
migration. Opening an incompatible compiled definition continues to fail closed.

## Budgets

| Boundary | Enforced budget |
| --- | --- |
| Secure encoded frame | 256 KiB |
| Retained row operations | `RetentionLimits`: checkpoint at 1,024 operations or 32 MiB, backpressure at 64 MiB header/body bytes |
| Command, effect and recovery records | 4,096 records |
| Transfer query batch | 33 records |
| Transfer response | 32 records and caller quota, at most 256 KiB |
| Pending operations per apply transaction | 64 |
| History query batch | 32 records |
| Recovery custody archives | 32 MiB aggregate stored package bytes |
| Recovery package | 32 MiB encoded bytes |
| Scope read (`snapshot`, one `snapshot_page`) | 1,024 live rows and 1 MiB stored records |
| Rebuild | The retained operations of the scope; no row limit |
| SDK session | [SDK budgets](sdk-contract.md#budgets) |

A rebuild is one write transaction over the scope's retained operations. Open
replays only the operations after each scope's verified checkpoint. Coordinator
log reclamation, fairness between remote principals and independently
credentialed multi-node deployment are unmeasured.
[Scaling limits](scaling-limits.md#known-costs) lists what these budgets cost.

## Design rationale

Encrypted command row state stays one record per scope because command results
carry complete before and after row maps in their encrypted packages; changing
that is a wire format change. Scope reads page by row key, so the cursor needs
no SQL parsing and a page costs the same at any depth.
