# Partitioned existence logs

Status: idea, deferred on 2026-10-08 from
[the features_06 scope](../features_06/scope-partitioned-constraints.md) (its
option B); not scheduled.

## Background

[Design rationale](../runtime/existence-logs.md#design-rationale) lets a table
publish a keyless existence log so that another reader scope can reference it.
Logged parents must be unpartitioned. That scope accepts references inside one
partition group and from a partitioned child to an unpartitioned logged
parent, and refuses the rest.

## Problem

Some shapes need a reference into a different partitioning:

- a per-user `recipes` table referencing items of shared lists;
- an unpartitioned history table referencing partitioned rows;
- an owner-only `budget` per list. The group secret opens every table of a
  group, so such a table must stay outside the list's group with its own key,
  and then cannot reference `lists`;
- ERP order lines partitioned by customer referencing stock partitioned by
  warehouse.

## Idea

A partitioned table may declare `existence_log`; each partition has its own
log. The record already has an optional authenticated tuple
(`EXISTENCE_FORMAT` 3); puppets, support bookkeeping and resolution need the
partition-aware identities of that scope.

### Liveness grant

Today any authenticated device of the domain may fetch a log, so it learns
that each row exists, how many there are, when they changed and which
author wrote them. For partitions that is not acceptable: a device not
shared on "Birthday surprise" would see that list fill up.

A partitioned log is therefore served only to a device holding, for that
partition, a read or custody grant or a new keyless **liveness** right. The
liveness right uses the selectors of
[design rationale](../runtime/partitioned-scopes.md#design-rationale) ("stock, any
warehouse"), narrows by delegation like every right, and gives no key and no
ciphertext. A device without it learns nothing, including whether the
partition exists. A device with it still sees one record per row; that is
what a reference needs.

Unpartitioned logs keep the existing rule.

### Authors

Since [phase 13](../features_09/13-identities-and-domain-credentials.md),
each record names the node that wrote it, linking its
activity across partitions for readers of a log. Data that must not be
linked belongs in a separate domain.

## Open

- Whether a child device may hold a reference when it has the liveness right
  but the parent partition's log is not yet synchronized: pending, as today.
- Whether the tuple in a record should be replaced by the scope commitment,
  so that a relay does not read partition values.
- The cost of one log per partition against the
  200-partition budget.
