# Cheap domain forks

Status: accepted as [features_09 phases 13–20](../features_09/README.md#generations-as-sealed-forks), which supersede this draft.

A [split schema](../runtime/adopting-generations.md#limits) is undefined
today: issuers that sign different schemas on one generation fork the
lineage, devices follow whichever branch they meet first, and nothing says
what a device applies of the other branch. A fork that is a first-class,
cheap operation would make such a split explicit instead.

## Idea

A domain fork creates a new domain from an existing one at a point in its
history, without copying and re-signing every row. Domains should be so cheap that we can use them for every schema
migration. This makes the mental model easier. One domain = one schema installation. this solves on the one hand the
split-schema problem that can currently occur when two different issuers issue a new schema independently, now their
forced to create a new domain. if they successfully convinced some devices to follow their fork, these devices remain on
this fork, this makes it easier to spot and fix this case later on. it also makes it technically easy to allows to store
automatic rollback checkpoints at dedicated nodes when updates break something (this is not scope of the change and
should become its own idea docs). Furthermore, it allows us to make dev
lineages to test something (this is actually scope of the change). And it also makes handling of "old frames" easy,
instead of old apps breaking because they adopt the new domain, they just stay on the old one. and the developer can go three different paths for old frames:
1. ignore everything: all changes to the domain after the fork are lost
2. app reauthors: after the app migrated to the new domain, it can find its own changes that are after the fork and reauthor them
3. trusted nodes bridge: trusted nodes reauthor/bridge changes from the old domain to the new one



- **Shared history by reference.** The new domain starts from a signed
  statement naming its parent domain and the parent's checkpoint or
  frontier it forks at. Rows up to that point are read through the parent's
  retained checkpoint, not re-authored, so a fork costs a statement and new
  keys, not the size of the data.
- **Fresh authority below the fork.** The new domain gets its own reader keys,
  credentials and registry; writes after the fork belong to it alone.
- **A defined outcome for splits.** Two issuers that sign different schemas
  on one generation would each create a fork instead of a sibling
  generation. The household then holds two well-defined domains, and the
  application, or a rule such as the coordinated log's order, decides which
  one devices follow.
- **Incompatible changes.** Changes that a generation cannot express, such as
  a name changing type, a new primary key or regrouped partitions, could
  move to a fork with a migration that rewrites only what changed.

## Backups at every version

A node configured to retain every old domain keeps each fork's parent
read-only. Every schema version then stays a consistent, restorable state:
an automated backup per version, with no separate export.

## Open questions

- How a checkpoint of the parent stays verifiable and retained while forks
  depend on it, and when it may be pruned.
- How devices learn of a fork and which domain to follow, and how their
  unsynced writes to the parent are re-authored in the fork.
- What the fork statement binds: parent, point, schema and who may sign it.
