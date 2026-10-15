# Existence logs and puppets

A device holding a child but not its parent's reader scope still needs
authenticated parent liveness and a local parent row for its real SQLite foreign
key. This guide is for an application author who references a table from
another reader scope, and for anyone changing the record or support rows. The
exposure and when to avoid it are in
[mergeable constraints](mergeable-constraints.md#references-across-reader-scopes).

## Declare it

```kotlin
val erp = schema("erp") {
    val customers = table("customers") { text("name"); primaryKey("id"); existenceLog = true }
    table("orders") {
        text("label"); primaryKey("id")
        reference("customer", customers, onDelete = Delete.Cascade)
    }
    view("partial_customers", "SELECT * FROM customers WHERE row_kind = 0")
}
```

From a Room export, set the flag in the overlay; Room's foreign key supplies
the reference: `schemaFromRoom("erp", json) { table("customers") { existenceLog = true } }`.
Definition data uses `"existence_log": true`; the replicated table shape binds
it into the signed manifest. `LogicalSchema::existence_tables` lists logged
tables. Enabling or removing a log on a populated table means export and
reinstall into fresh history, never invented creation evidence.

A foreign key may cross unrelated reader scopes only to an unpartitioned logged
table; other parents share the child's unpartitioned scope or its explicit
partition group with every partition column included in the reference. Support tables are the logged tables plus,
transitively, the parents of their `NOT NULL` references. Each must be
mergeable, unpartitioned and keyed by `id` alone; installation otherwise refuses
with `Incompatibility::SupportRow` naming the conflicting key. The trusted
profile refuses existence logs. The SDK assigns the child and the logged parent
separate reader scopes and does not merge them.

## The record

Each table scope with a log has one log per signing node. The node
signs the CBOR statement under `rocky.existence-record.v2`; the commitment is
`rocky.existence-record.hash.v2` over the signed record.

| Field | Contract |
| --- | --- |
| `version` | 4, the sole current format; any other value is refused |
| `tip` | The followed tip the record was written under; its manifest resolves through the local graph |
| `scope` | Domain, reader commitment and table |
| `partition` | None for installed unpartitioned logs; partitioned logs remain deferred |
| `id` | The incarnation identity (`RowIdentity`), never 0 |
| `event` | `Created` or `Deleted` |
| `author`, `credentials` | The node's author number and credential chain |
| `sequence`, `previous` | Position from 1 in the signer's log of this scope; predecessor commitment |

No column value, business key, reference, payload or application timestamp is
included. Verification needs no scope key: format, admitted tip, installed
log, signature, `verify_author` for the table, an `Insert` grant for a creation
or `Delete` for a deletion, and a creation `id` belonging to the author number.
A deleting author need not be the creator.

Storage is idempotent by commitment. A different record at a held position, a
wrong predecessor or a second creation of one `id` is refused; a gap returns
`PendingDependencies` and stores nothing. Rejected records change no state.
Existence admission is separate from sealed-frame identity marks, so receiving
both representations of one creation is not identity reuse.

## What devices do

- **Authoring.** A commit that creates or deletes a row of the table signs one
  record per row with the node key, in the same transaction as the
  sealed frames, projection and outbox. Updates write no record.
- **Liveness.** A row is alive with a verified creation and no verified
  deletion; deletion wins in any order. Missing evidence keeps dependants
  hidden as pending. Holders follow the same rule, so a sealed row without its
  creation record stays hidden.
- **Constraint resolution.** A candidate of a logged table is anchored only
  while alive; a `CASCADE` or `SET NULL` reference to a logged parent depends on
  the parent's liveness alone. Devices with different held scopes therefore
  derive the same orphans.
- **Support rows.** A typed row referencing an alive row that has no visible
  real row here gets a puppet. Puppets go with the last referencing row and are
  replaced in place when the real row appears. A holder also uses one while its
  real parent is hidden by a unique-index loss or evicted.

  | `row_kind` | Meaning | `id` |
  | --- | --- | --- |
  | 0 | Real application row | Its incarnation |
  | 1 | Puppet | The referenced `id` |
  | 2 | Virtual row, one per table | 0 |

  A support row holds NULL in nullable columns and the zero of the declared
  type in `NOT NULL` ones; `NOT NULL` references name the target's virtual row
  (a self-reference names itself), nullable ones are NULL. Defaults are not used.
- **Never replicated.** Support rows live in private `secure_support`
  bookkeeping and are not captured, sealed, digested or counted. Rebuild,
  eviction, bootstrap and reopen derive them again. Unique business indexes are
  partial on `row_kind = 0`. Generated
  `secure_support_<table>_{insert,update,delete}` triggers reject any change
  made under a session gate; runtime statements pass through
  `rocky_support_maintenance()`. Capture refuses an image with a reserved
  `row_kind`.
- **Reads.** Every table that may hold support rows is filtered to
  `row_kind = 0`. A session may name an unheld logged parent of a table it
  reads; such a reference needs `row_kind = 0` (for example through
  `partial_customers`) and returns no real rows; `row_kind = 1` or `2` is never
  accepted.
- **Local references.** A child may name a held real row or any alive row of
  the log; a dead, unknown or virtual target fails at commit with SQLite's
  foreign-key code.

## Sync

`EncryptedReplica::existence_scopes` lists the logs a device follows: those of
tables it has a read or custody grant on, and of the tables they reference.
`NativeSession::sync_existence(replica, scope)` pulls missing records by the
per-signer frontier and pushes those the peer lacks; the SDK runs it after row
sync. Any device of the domain may serve and receive, keyless. Records leave
only after the commit that stored them is durable. Direct callers use
`existence_page`, `existence_frontier` and `receive_existence`.

A record is refused before any state change when its format, tip, domain,
table, signature, node chain, insert/delete right, creation author or log
position is wrong. A gap returns `PendingDependencies` and is not stored.

## Limits

- Logs keep every record until a [mergeable horizon](mergeable-horizon.md)
  seals them into an existence base.
- Partitioned children may reference an unpartitioned logged parent. Partitioned
  existence logs and references between different partition groups remain deferred.
- Across a reference to a logged table, a child depends on the parent's log
  only. A parent a holder hides for its own reasons (a unique-index loss, an
  orphaned `CASCADE` parent of its own) does not hide the child; the child's
  foreign key names a puppet there, as on devices without the parent.
- The schema exposes, to every device of the domain, row counts, creation and
  deletion times, row `id`s and creating/deleting nodes of each logged
  table.

## Design rationale

- Liveness comes from the log alone, also for holders, so every device derives
  the same orphans regardless of which scopes it holds. A holder's own hidden
  parent therefore does not hide a child.
- Records carry no values or keys, so the log can be served keyless to any
  device of the domain; an unpartitioned scope adds no routing information.
- Support bookkeeping uses `(table, scope, id)`. A logged parent remains
  unpartitioned and its puppet is determined by the child's identity reference.
  In-group partitioned parents use real candidates reached through the complete
  partition-and-id reference; they do not require a keyless log or puppet.
