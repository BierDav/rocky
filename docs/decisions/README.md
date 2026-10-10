# Decision records

Rationale lives next to the behavior it explains: each runtime guide ends with a
`## Design rationale` section naming the rejected alternatives and standing
constraints. Search there first (`rg -n "Design rationale" docs/runtime`). A
separate record exists only for a standing, non-obvious constraint that no guide
naturally holds.

## Records

| Record | Topic |
| --- | --- |
| [0018](0018-secure-transaction-bundles.md) | Secure transaction bundles, session authority, typed secure storage |
| [0028](0028-generation-lineage-and-signed-backups.md) | Generation lineage, projection plans, signed backups |
| [0037](0037-partition-groups.md) | Explicit partition groups, positional addresses and secret boundary |
| [0038](0038-row-moves.md) | Draft: creation proofs, atomic row moves and author-issued following |
| [0039](0039-single-current-formats.md) | One current persisted and wire format |
| [0040](0040-group-secrets.md) | Group secrets, derived table keys, group envelopes and rotation |
| [0041](0041-partition-constraint-state.md) | Partition constraint identities, current private state and catalog verification |
| [0042](0042-bounded-strong-cut.md) | A strong cut named by its checkpoint base |
| [0043](0043-partition-horizon-state.md) | Partition horizon state and pending dependency index |
| [0044](0044-group-frontier-exchange.md) | Bounded authenticated group frontier exchange |
| [0045](0045-custody-partition-offers.md) | Custody partition offers in native session v6 |
| [0046](0046-per-scope-frontier-refusal.md) | Per-scope frontier refusal in native session v7 |
| [0047](0047-verifiable-identity-retirement.md) | Retire only identities whose results stay verifiable |
| [0048](0048-replicated-issuer-registry.md) | Replicated issuer registry |
| [0049](0049-mergeable-revocation-set.md) | A mergeable revocation set replaces the policy epoch |
| [0050](0050-concurrent-key-epochs.md) | Sibling key epochs and exposure rotation converge |
| [0060](0060-test-tiers-and-budgets.md) | Three test tiers with enforced budgets |

Numbers 0001 to 0036 other than these were folded into the guides below and are
not reused. 0038 is reserved for row moves. 0048 to 0059 are reserved for
[features_07](../features_07/README.md#reserved-decision-records); the next
unreserved record is 0061.

## Rationale by topic

| Topic | Guide |
| --- | --- |
| Storage engine, durability classes, buffered commits, file layout | [write-concurrency](../runtime/write-concurrency.md#design-rationale) |
| Stored rows as state, authorized views | [local-replica](../runtime/local-replica.md#design-rationale) |
| Paged reads | [paged-reads](../runtime/paged-reads.md#design-rationale) |
| Wide schemas, recorded placement, additive indexes | [wide-schemas](../runtime/wide-schemas.md#design-rationale) |
| Replica roles, subsets, range reconciliation | [replica-roles](../runtime/replica-roles.md#design-rationale) |
| Checkpoints, pruning horizon, rebootstrap | [checkpoints](../runtime/checkpoints.md#design-rationale) |
| Signed encrypted checkpoints | [encrypted-checkpoints](../runtime/encrypted-checkpoints.md#design-rationale) |
| Issuer-declared mergeable horizon | [mergeable-horizon](../runtime/mergeable-horizon.md#design-rationale) |
| Existence logs, puppets, virtual rows | [existence-logs](../runtime/existence-logs.md#design-rationale) |
| Encrypted strong bases and retained suffixes | [strong-history](../runtime/strong-history.md#design-rationale) |
| Logical schema, typed projections, row identity | [logical-schema](../runtime/logical-schema.md#design-rationale) |
| Hidden rows, unique indexes, foreign keys | [mergeable-constraints](../runtime/mergeable-constraints.md#design-rationale) |
| SQL transactions, writer lease, row capture | [sql-sessions](../runtime/sql-sessions.md#design-rationale) |
| Schema definitions, resolved SQL, snapshot cursors, table events | [queries-and-observation](../runtime/queries-and-observation.md#design-rationale) |
| Database bindings, placement groups | [database-bindings](../runtime/database-bindings.md#design-rationale) |
| Signed capabilities, encrypted row protocol | [permissions-and-sync](../runtime/permissions-and-sync.md#design-rationale) |
| Credentials, grant profiles, issuer devices, key epochs | [credentials](../runtime/credentials.md#design-rationale) |
| Revocation statements, mergeable sets, the frame rule | [0049](0049-mergeable-revocation-set.md), [revocations](../runtime/revocations.md) |
| Author pseudonyms and author ranges | [credential-primitives](../runtime/credential-primitives.md#design-rationale) |
| Partitioned reader scopes, key envelopes | [partitioned-scopes](../runtime/partitioned-scopes.md#design-rationale) |
| Typed command descriptors, tickets, handler hosts | [typed-commands](../runtime/typed-commands.md#design-rationale) |
| Merged strong steps, local reconciliation read | [strong-publication](../runtime/strong-publication.md#design-rationale) |
| Portable result evidence, historical results | [encrypted-commands](../runtime/encrypted-commands.md#portable-result-evidence) |
| Recovery artifacts, effect observations | [recovery-and-effects](../runtime/recovery-and-effects.md#design-rationale) |
| Certified bootstrap, Raft coordination | [shared-coordination](../runtime/shared-coordination.md#design-rationale) |
| Bounded coordinator admission and retirement, composable P2P | [coordinator-retention](../runtime/coordinator-retention.md#design-rationale) |
| Public coordination service, tenant registration | [public-coordination](../runtime/public-coordination.md#design-rationale) |
| App-owned domain namespaces | [app-runtime](../runtime/app-runtime.md#design-rationale) |
| In-process UniFFI bindings | [sdk-packaging](../runtime/sdk-packaging.md#design-rationale) |
| Service boundaries, resource ownership | [sdk-composition](../runtime/sdk-composition.md#design-rationale) |
| Transport-independent identity, contact records, peer sync | [sdk-transports](../runtime/sdk-transports.md#design-rationale) |

## Writing a record

Prefer adding to a guide's rationale section. For a new record, create
`NNNN-short-topic.md` with `Status`, `Date`, then `## Context`, `## Decision` and
`## Consequences`. Link contracts rather than repeating them, follow the
[documentation conventions](../documentation.md) and add the record to the table
above. The architectural reference is
[proposal V4](../proposals/architecture-proposal-v4.md); the implementation
sequence is [the roadmap](../features_06/README.md).
