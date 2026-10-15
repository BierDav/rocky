# Recoverable application generations

Implemented for the encrypted runtime and SDK's single-file placement group.
[Decision 0028](../decisions/0028-generation-lineage-and-signed-backups.md) fixes the
formats. The trusted runtime's earlier same-definition APIs remain separate.

## Choose the change

- Local version, SQL spelling, local tables and indexes do not authorize a new
  replicated definition. A local plan keeps the manifest and row identities.
- Adding a mergeable table or nullable field requires a
  `GenerationTransition` the owner or an issuer endorsed
  ([identities](credentials.md#identities)). Rows from an older definition supply null for newly
  added nullable fields. Names are the identities of tables and fields
  ([decision 0067](../decisions/0067-names-are-identities.md)).
- Requiring a field of an existing table, newly added or once nullable, is
  additive too: the field needs a default, rows that lack it or hold NULL
  read the default, and the schema's migrations compute its values once
  ([required fields](#required-fields)).
- Key rotation/revocation uses the existing explicit credential policy mechanism.
  Generation plans refuse reader-scope remapping and key/type changes.
  Removing mergeable tables or fields needs [application confirmation](#confirmed-removals);
  renaming is removal and addition.

Partition group membership and names are immutable for existing tables, as are
partition columns. A new mergeable table may join an existing group with matching
slots. See [partition groups](partitioned-scopes.md#partition-groups).

`rocky_core::classify` rejects unsupported changes before
activation. A signed transition (`rocky.transition.v5`) binds application,
old/new manifest, schema generations, the field ids rows of the old
generation may lack, the table and field ids the step named and removed, and
when it was signed, in microseconds. A generation-number edit alone
cannot open a different definition.

## Plan, backfill and switch

`EncryptedReplica::install_tables` records a durable descriptor and progress,
creates shadow tables, fills 256 stored rows per transaction, verifies projections
against canonical stored rows and switches all affected tables in one transaction.
Concurrent committed updates also maintain the shadow. Readers see the old view
until switch, then the complete new view. Tables with unique indexes or foreign
keys, and every schema with a partitioned table, have no shadow: every candidate decides which rows show, so the switch
recreates them and resolves the stored rows in one transaction, proportional to
the domain's rows. Notifications follow commit; old secure
sessions and cursors become stale at the authority switch.

Reopen resumes the recorded plan. An incompatible pending plan is refused.
`open_upgrading` verifies the signed lineage before changing the installed manifest;
projection switch and generation activation retain a recoverable durable boundary.
Network/coordinator calls never occur inside these storage transactions.
`generations()` reports the current generation, the earlier ones whose
manifests the replica holds, the baseline and bounded progress.

The SDK issuer signs an additive upgrade when `Database::open` receives the new
schema. A member fetches that exact schema/lineage from a device that runs it,
issuers first, over its attached transport; while it reaches none, it opens at
its generation and reports its application ahead. Issuer state is saved before replica activation,
so reopening with the same schema continues after interruption. New collections
do not silently expand member grants. SDK lineage is capped at 32 steps; secure
runtime lineage at 64.

## Confirmed removals

An issuer opening a schema that removes tables or fields receives
`RolloutConfirmationRequired` before settings, secrets or the replica change,
and before its network starts. Its `report` lists removed tables and
`table.field` names with the live stored row count of each table on this copy,
including rows suppressed by constraints and every held partition. A removed
field counts every live row of its table, including NULL or missing values.
Counts are neither a device inventory nor a domain-wide estimate.

The application shows the report with its own wording and asks for confirmation.
Only after confirmation does it call `Database::open_confirmed(open, digest)`;
Kotlin calls `RockyDatabase.open(..., confirmation = report.digest)` or `resume`.
The digest binds the domain, installed manifest, complete target SDK schema
(including migrations) and counts. Open recomputes it under the directory lock.
A mismatch returns the current report with `mismatch = true` and changes nothing;
the application must show that report again. Counts changing invalidates approval;
a value change that leaves counts unchanged does not change this impact report.
Cancelling leaves the old schema usable. Pure additions need no confirmation.

Members never sign or authorize removal. Until an issuer supplies a signed
new generation, they open the installed one with `application = ahead` and
`waiting_for_issuer = true`. Adopting a generation already signed by an issuer
needs no further local confirmation. Confirmation governs SDK signing, not
verification of publisher signatures from peers.

## Peer discovery

Meeting a peer running another generation does not change the installed
workspace. Incompatible frames, checkpoints and horizon statements return an
admission error; sync never fetches or adopts another generation to retry them.
Explicit schema opens retain the existing signed generation verification and
recoverable switch. Replica-level [tips and bookmarks](tips-and-bookmarks.md) select the live
workspace; the SDK surface follows in phase 14 integration.

## Removing tables and fields

A signed generation can remove mergeable tables and fields, including their
constraints. Every adopter drops them and applies older frames field by
field under their original authority. Re-adding a name uses the earliest
signing baseline and replays retained frames after it. See
[removal](removal.md) for grants, checkpoint and recovery rules.

Never-offered local bundles that touch removed items are held whole and
voided; their effects on kept tables roll back during the switch. The app
lists, re-authors or drops them through the [held-change API](held-changes.md).

## Tables granted like another

Grant profiles name tables, so a table added later is in no existing grant.
The SDK schema's `granted_like` maps a new table to an existing one, as in
`{"item_categories": "items"}`; Kotlin declares `grantedLike = "items"` on
the table. Wherever the SDK turns a profile into grants or asks whether it
reads a table (enrollment, renewal, revocation rotation, group delivery and
horizons), a grant naming `items` in whole rows also names `item_categories`
with the same read and write rights. Partition selectors on
`items` select the same partitions of `item_categories`. A grant without
columns covers every field in every generation, so nothing widens fields.
A partial replica holds the new table wherever it holds its group partition.
A member that adopts the generation receives the new grants and keys with
its next renewal, which it asks an issuer for once per such generation; until
then it writes every table its chain names. The followed table must follow none, and the entry
is declared with the table: changing it for an existing table is refused, since
it would change what earlier grants mean.

## Required fields

A field rows of the installed schema lack, or hold NULL in, may become
required in an ordinary additive transition; it declares a default. Every
device reads the default wherever a row lacks the field or holds NULL in
it, including rows a device of an older generation writes later, and the
projection switch rewrites stored NULLs the same way. A device whose
application predates the field keeps syncing; its new rows read the
default.

The schema's `migrations` compute its values once, on one issuer
([migrations](#migrations)).

## Migrations

[Decision 0071](../decisions/0071-staged-migration-publication.md) records
the design. An issuer whose application declares a schema with migrations
above its installed version moves the domain to it:

1. It asks its known issuers for the generation; one already signed is
   adopted like a member's, and nothing runs.
2. It may migrate only if it holds the key of, and reads and writes, every
   partition the domain knows. Otherwise it opens at the domain's
   generation, and status reports `full_access_required`.
3. It records the version it migrates from in `migration.json` and signs
   the schema; the earliest signing time is the generation's baseline.
4. At open, before the network starts, it runs
   every migration above that version, in order, in one SQL transaction of
   any size, in the new generation. A step is SQL, or
   `{"insert": table, "select": sql}`, which inserts a row per selected row
   with an `id` Rocky issues in its partition. Migration SQL cannot call
   `rocky_next_id`.
5. The transaction commits as a staged publication: the net change as
   consecutive ordinary bundles within the [bundle bounds](secure-sessions.md#bundles),
   parents first, all stamped at that commit, and the record that marks the
   migration done. A crash before the commit repeats the migration; after
   it, nothing runs again.

The bundles leave by ordinary sync, a restart included, until peers hold
them. Status `migration` reports `{"state": "publishing", "bundles": n,
"published": k}`: `k` leading bundles some peer holds. Until its last bundle
arrives, a receiver reads the default in rows not reached yet; an edit a
member makes meanwhile is stamped later and wins. A publication's frames do
not count toward the [retained-history bound](encrypted-checkpoints.md#what-runs-on-its-own) until they
are pruned. Rows the migrating issuer did not hold, and rows devices on an
older application write later, keep their default; nobody's work is
dropped. [Discovering such rows](../ideas/discovering_unmigrated_rows.md) is
an idea.

Issuers that open the new schema without reaching each other each migrate;
the steps are one generation. Updates meet by latest wins, and rows an
`insert` step adds merge when the table is keyed by name. In a table keyed
by `id` each issuer's rows stay apart: an `insert` step's ids come from the
issuer's own creation log, an
[accepted limitation](../features_09/README.md#accepted-limitations).

## Manifests are a cache

Every device keeps the signed transitions of its lineage; they prove which
manifests belong to it and carry, for each step, the field ids rows of the
earlier generation may lack and the table and field ids it named and removed.
Removal and re-adding are read from them alone ([removal](removal.md)), and a
device checks a step against its manifests where it holds both. A manifest of an
earlier generation is kept only while a retained frame, a checkpoint, a
horizon statement or an existence base names it; credential chains name
none. After a horizon finalizes,
`EncryptedReplica::drop_unreferenced_generations` drops the others, and the
SDK removes them from the saved lineage. Retained history stays verifiable
by its transitions. Frames whose schemas the replica no longer holds are
refused; peer discovery does not fetch schemas or change the workspace.

## Old devices and pending work

Retained generations remain subject to the followed tip's admission rules.
New admission verifies the original authority plus current revocations, never
invented current authority.

Pending operations preserve exact original bytes and author logs. Retained
checkpoints keep typed keys/incarnations, tombstones, bundles and signed history;
rebootstrap accepts only supported generations and authorized dependency closure.
Existing [retention holds](retention-holds.md) protect unresolved outcomes.

## Evidence

SDK `tests/migrations.rs` runs a migration again after it failed before its
commit and never after it (`a_failed_migration_runs_again_and_a_committed_one_never`),
migrates on two issuers apart (`issuers_that_cannot_reach_each_other_both_migrate_and_converge`),
keeps an issuer without a partition's key at the domain's generation
(`an_issuer_without_a_partitions_key_keeps_the_households_generation`) and,
in the e2e tier, fills a required field of 100,000 rows
(`a_migration_of_100000_rows_converges_on_every_member`). `rocky-secure`
`tests::sql::publication` commits one transaction as consecutive bundles.
SDK `tests/generations.rs` also requires a field with a migration while an
older member keeps its unsynced work across an explicit target schema open
(`a_required_field_migrates_on_the_issuer_and_older_devices_keep_syncing`),
reads the default for a NULL an older generation writes
(`a_null_an_older_generation_writes_to_a_required_field_reads_its_default`),
keeps a dropped manifest absent when an old peer returns
(`a_dropped_manifest_stays_dropped_when_an_old_peer_returns`), and grants
a new table like an existing one. The shopping example's `SchemaUpgradeTest`
moves a version 6 household to version 7 while a version 6 member keeps syncing.
`rocky-secure`'s `tests::generations` interrupts shadow creation, backfill, switch
and definition activation; verifies atomic visibility, stale sessions, pending
work, old peers and typed checkpoint recovery, and drops and learns again an
unreferenced manifest (`an_unreferenced_manifest_is_dropped_and_learned_again_from_a_frame`).
Core/credentials tests
verify transition encoding, classification and signing; SDK `tests/generations.rs` exercises issuer/member
upgrade, an offline member opening at its generation, nullable fields, scope denial
and ordinary pending writes. Peer discovery tests keep both an idle member
and an older application on their installed schema.
See [phase-8 evidence](sdk-packaging.md#build-and-consume-a-package) for execution results.

This does not implement cross-file atomic switching,
Room file import or cross-coordinator migration. Migrations run only on the
migrating issuer; replicas never transform rows on their own.
