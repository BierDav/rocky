# 6. Migrations, checkpoints and recoverable transactions

> Superseded implementation brief, retained as an optional Room acceptance
> reference. Do not implement this sequence as the current Rocky roadmap.
> Generic work and deferred adapter work are assigned in
> [features_04](../../features_04/README.md#foundations-and-old-plan-disposition).
> [V4](../../proposals/architecture-proposal-v4.md) governs future design.

Status: superseded future brief; retained optional Room requirements. Owner: runtime schema/checkpoint/recovery, core generation
evidence and SDK upgrade/import tooling. Depends on phases 1–5.

## Separate schema changes

Room's local schema version/identity, Rocky's replicated schema, logical identity
mapping, projection generation, command handler generation and policy/key epochs
are separate facts. Changing `user_version` or Room's identity table cannot
authorize a replicated upgrade. Existing same-definition rebuild and additive
non-unique indexes do not implement general schema evolution.

Classify migrations in generated metadata:

- Local-only tables, indexes and derived read-model columns change local storage.
- Replicated field/table/key changes need a signed compatibility transform and
  an explicit deployment/peer generation policy.
- Permission-controlling fields and reader-set changes additionally need current
  command/security authority and key migration.

Room migration callbacks run through a privileged, restricted migration session.
Distinguish DDL/backfill from new user edits so rebuilding a derived column does
not publish thousands of accidental operations. A backfill that changes canonical
replicated values is a declared replicated transform, not suppressed capture.
Ordinary raw-query connections cannot acquire migration privileges.

Install generation plans before opening the new logical schema. Persist plan
identity, old/new commitments, bounded progress and completion evidence. Resume
an interrupted identical plan; refuse a different one. Atomically switch visible
schema/projection and prepared-plan generation for an atomic placement group.
Cross-file schema switches inherit phase 3's blocked atomicity boundary.

Define old-peer behavior, active credential/manifest renewal, pending transaction
and invocation migration, and late old-generation operation admission. Retain
original signed bytes and provenance; never edit or re-sign historical operations
as the recovering/migrating node. Renames need stable identity mappings from phase
1 rather than guesses from equal column shapes.

## Ordinary existing Room database import

Provide an SDK-owned, resumable import for an application-owned SQLite/Room file.
Validate its exported schema, keys, constraints and authority, then seed canonical
rows through the new transaction boundary. Preserve externally visible keys,
foreign-key relationships and declared local-only data. Room-owned files must
not be mistaken for installed Rocky files or opened under their identity.

Record source snapshot/import identity and destination checkpoints so a crash or
repeated import cannot duplicate rows. Keep the source intact until completion
is verified. Import authoring authority must be explicit; a read-only user cannot
turn a local cache into signed canonical data. Existing projection-adapter files
also contain pending actions: reconcile already-applied handles before import and
preserve uncertain work instead of discarding or repeating it.

## Checkpoints, pruning and rebuilds

Extend checkpoint/rebootstrap evidence to cover typed projections, business-key
incarnations, register stamps, tombstones, transaction outcomes and multi-resource
bundle dependencies. Define which subset evidence suffices for a filtered reader
and which outcomes remain pinned by unresolved foreign commits/strong requests.

Pruning may release payload bytes only when retry outcomes, causal dependencies,
reader-scoped visibility and required custody remain recoverable. Keep durable
outcome summaries long enough to deduplicate interrupted commits after compaction;
session handle expiry is not permission to reuse a durable operation identity.
Preserve existing retention holds and sole recovery copies.

Rebuild from verified canonical evidence into shadow typed projections, then
atomically switch the affected schema/revision. SQL triggers and capture must not
reinterpret replay as new edits. Observer invalidation follows the switch; active
old-generation statements fail or finish under an explicitly retained snapshot.
No foreign query sees halfway-rebuilt data.

## Backup, identity and loss

Backups include the placement catalog, all required groups and protocol evidence
at coherent cuts; copying only a Room-visible table is not a recoverable replica.
Preserve current signed encrypted recovery, missing-key status, incomplete-scope
reports and fresh-node enrollment. Recovering another device creates a fresh
signing identity; copying files/credentials must not fork an author log.

New transactions after recovery allocate fresh identities; imported operations,
committed outcomes and uncertain commands/effects keep their original identities.
An uncommitted SQL lease is rolled back after restart. An interrupted COMMIT is
looked up by its durable token before any new authoring. Buffered power-loss
semantics cannot be mistaken for a flushed acknowledged commit.

Domain replacement with acknowledged loss remains explicit and fenced. An ORM
must not recreate an incompatible database with destructive fallback and present
it as the old domain. WAL/disk durability and remote custody are separate evidence.

## Completion gate

- Local index/derived-column migrations and signed replicated migrations preserve
  identities, unrelated domains, pending work and exact historical evidence.
- Fault injection at every install/backfill/switch boundary is resumable or safely
  refused; Room version/hash alone never bypasses compatibility checks.
- Import a normal Room database and the current adapter database without changing
  entity keys, losing pending edits or modifying the original source on failure.
- Prune/rebootstrap/rebuild preserve transaction deduplication, tombstones and
  authorized bundle completeness, including subset replicas.
- Fresh-node encrypted recovery, key loss, acknowledged loss and interrupted
  commit recovery remain safe across migration and compaction.

Run migration/checkpoint/recovery and subprocess tests plus `./scripts/check`.
See [encrypted checkpoints](../../runtime/encrypted-checkpoints.md),
[scaling and rebuilds](../../runtime/scaling-and-rebuilds.md) and
[recovery/effects](../../runtime/recovery-and-effects.md) for retained guarantees.
