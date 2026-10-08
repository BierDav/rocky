# 0037: Explicit partition groups

Status: accepted; schema, address and [group secrets](0040-group-secrets.md) implemented.
Date: 2026-10-08

## Context

A table's partition tuple contains its own column identities. Equal values in
`lists.id` and `items.list` therefore have different table scopes. Applications
need an explicit shared boundary without deriving confidentiality from foreign
keys. See the [settled scope](../features_06/scope-partitioned-constraints.md#1-partition-groups).

## Decision

A partitioned table declares optional `partition_group` beside `partition_by`.
The Kotlin builder and Room overlay use `partitionBy("list", group = "list")`.
Group names are case-sensitive schema identities. A group is declared by its
members, once in the schema namespace; repeated membership names join that
same group. Every member maps its partition columns by declaration position to
slots with identical affinity and collation. Validation names both incompatible
tables. A table omitting the name owns an implicit group named by its logical
table identity; no other table may name it. Unpartitioned tables cannot join.

`LogicalSchema::partition_groups` exposes names, slots and member tables.
`group_address` takes a domain, table and positional SQL values. It normalizes
values using the business-key codec and commits under `rocky.partition-group.v1`
to the domain's mesh/app/domain IDs, the UTF-8 group name prefixed by its u32
big-endian byte length, the u8 slot count, and each canonical slot value prefixed
by its u32 big-endian byte length. Slots retain declaration order. Addresses are
bounded to 1 KiB. Table and column identities are absent from the group address;
table scopes, grants, pseudonyms, operation logs and checkpoints remain distinct.

The sole current definition/descriptor is format 4, replacing format 3. Format 3
is refused before installation or stored-descriptor writes. There is no migration,
legacy reader or alternate group encoding. Existing installations reinstall, as required by phase 1. All other versions
are refused for both partitioned and unpartitioned schemas.

Phase 3 supplies one random secret per group address and key epoch. Table keys
are derived by a domain-separated cryptographic KDF binding the group address,
epoch and stable table identity, never by treating one table key as the secret.
[Decision 0040](0040-group-secrets.md) pins the KDF and envelope format.
Adding a table lets existing secret holders derive its key without another
envelope. Generation admission refuses changing an existing table's group,
removing membership or renaming a group; new tables may join an existing group.

Holding, subscription, eviction and bootstrap default to the whole group
partition, with explicit table omissions allowed. Constraint resolution cannot
resolve a child without its parent partition. Phase 3 implements holding; phases
4 and 5 implement partition-aware constraints. Phase 2 keeps their refusals.

## Consequences

Read authority is all-or-nothing for the group partition: its secret reveals
every member's key. An envelope requires whole-partition read on every member;
write authority stays per table. A selectively readable table, such as an
owner-only budget, must stay outside the group and cannot reference it until
partitioned existence logs are implemented.

One secret means one envelope and rotation per group partition, independent of
table depth. Sync, logs and checkpoints still cost one scope per table and
partition; phase 9 measures the four-table ERP fixture. A schema group alone
does not implement shared secrets, group lifecycle or partitioned constraints.
