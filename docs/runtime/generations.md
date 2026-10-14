# Recoverable application generations

Implemented for the encrypted runtime and SDK's single-file placement group.
[Decision 0028](../decisions/0028-generation-lineage-and-signed-backups.md) fixes the
formats. The trusted runtime's earlier same-definition APIs remain separate.

## Choose the change

- Local version, SQL spelling, local tables and indexes do not authorize a new
  replicated definition. A local plan keeps the manifest and row identities.
- Adding a mergeable table or nullable field requires a publisher-signed
  `GenerationTransition`. Rows from an older definition supply null for newly
  added nullable fields. Names are the identities of tables and fields
  ([decision 0067](../decisions/0067-names-are-identities.md)).
- Requiring a field of an existing table, newly added or once nullable, is
  additive too: the field needs a default, rows that lack it or hold NULL
  read the default, and the schema's migrations compute its values once
  ([required fields](#required-fields)).
- Handler semantics require a higher handler generation; declared command
  versions within the support horizon keep their descriptors. Adding a version
  requires a new handler generation. Removing/redefining one is refused.
- Key rotation/revocation uses the existing explicit credential policy mechanism.
  Generation plans refuse reader-scope remapping. Key/type changes, removals
  and renames, which remove the old name, are unsupported.

Partition group membership and names are immutable for existing tables, as are
partition columns. A new mergeable table may join an existing group with matching
slots. See [partition groups](partitioned-scopes.md#partition-groups).

`rocky_core::classify` and `classify_commands` reject unsupported changes before
activation. A signed transition (`rocky.transition.v4`) binds application,
old/new manifest, schema/handler pairs, the field ids rows of the old
generation may lack, the table and field ids the step named and removed, and
when it was signed, in microseconds. A generation-number edit alone
cannot open a different definition.

## Plan, backfill and switch

`EncryptedReplica::install_tables` records a durable descriptor and progress,
creates shadow tables, fills 256 stored rows per transaction, verifies projections
against canonical stored rows and switches all affected tables in one transaction.
Concurrent committed updates also maintain the shadow. Readers see the old view
until switch, then the complete new view. Tables with unique indexes or foreign
keys have no shadow: every candidate decides which rows show, so the switch
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
schema. A member fetches that exact schema/lineage from an issuer over its
attached transport; while it reaches none, it opens at its generation and
reports its application ahead. Issuer state is saved before replica activation,
so reopening with the same schema continues after interruption. New collections
do not silently expand member grants. SDK lineage is capped at 32 steps; secure
runtime lineage at 64.

## Adopting newer generations

A device that meets a newer generation adopts it whatever its application
declares: it verifies the peer's lineage, is renewed into it and switches its
open replica before any frame of it applies. An application older than its
database opens it unchanged. See [adopting generations](adopting-generations.md).

## Removing tables and fields

A signed generation can remove mergeable tables and fields, including their
constraints. Every adopter drops them and applies older frames field by
field under their original authority. Re-adding a name uses the earliest
signing baseline and replays retained frames after it. See
[removal](removal.md) for grants, checkpoint and recovery rules.

## Tables granted like another

Grant profiles name tables, so a table added later is in no existing grant.
The SDK schema's `granted_like` maps a new table to an existing one, as in
`{"item_categories": "items"}`; Kotlin declares `grantedLike = "items"` on
the table. Wherever the SDK turns a profile into grants or asks whether it
reads a table (enrollment, renewal, revocation rotation, group delivery and
horizons), a grant naming `items` in whole rows also names `item_categories`
with the same rights, `Invoke` and `Execute` aside. Partition selectors on
`items` select the same partitions of `item_categories`, and renewal widens a
partition grant that covered a whole row to the fields later generations added.
A partial replica holds the new table wherever it holds its group partition. A member renewing into the
generation receives them. The followed table must follow none, and the entry
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

The first issuer to open the new schema migrates, after asking its known
issuers for the generation (one already signed is adopted like a member's):

1. It signs a step holding the required fields nullable, unless they already are.
2. At open, before the network starts, it renews its pseudonyms for that
   generation and runs `migrations` with versions above the installed one in
   one SQL transaction. A step is SQL, or `{"insert": table, "select": sql}`,
   which inserts a row per selected row with an `id` Rocky issues in its
   partition. It needs every partition the domain knows. `migration.json`
   records the version it migrates from and completion until the generation
   is signed, so a retry does not run statements twice.
3. It signs the schema and opens at it.

The migration's results are ordinary signed writes; nobody's work is
dropped. A member fetches the generation from an issuer that signed it.
Rows the migrating issuer did not hold keep their default. The migration is
one transaction, so it changes at most [2,048 rows](sql-driver.md) until
[phase 7](../features_09/07-migrations-at-any-size.md). Two issuers that open
the new schema without reaching each other each migrate; the steps are one
generation, and their writes merge.

## Manifests are a cache

Every device keeps the signed transitions of its lineage; they prove which
manifests belong to it and carry, for each step, the field ids rows of the
earlier generation may lack and the table and field ids it named and removed.
Removal and re-adding are read from them alone ([removal](removal.md)), and a
device checks a step against its manifests where it holds both. A manifest of an
earlier generation is kept only while a retained frame, a checkpoint, a
horizon statement, an existence base, a command or the credential chain of a
retained frame or registered author names it. After a horizon finalizes,
`EncryptedReplica::drop_unreferenced_generations` drops the others, and the
SDK removes them from the saved lineage. Retained history stays verifiable
by its transitions. A peer that still writes under a dropped generation
presents a chain the replica places by its transition; its first frame is
refused as unknown, and the device fetches the manifest from that peer
again before the frame applies, as for any
[older generation](adopting-generations.md#older-generations).

## Old devices and pending work

Every generation of the lineage stays admissible; a dropped manifest is
fetched again when a frame needs it. New admission verifies the
original generation's signed credentials plus current revocations, never invented
current authority. Current execution authorization remains required for commands.

Pending operations preserve exact original bytes and author logs. Pending command
tickets retain invocation/handle identity, accepted result, receipt and retry state.
No argument translation or automatic fresh invocation is performed. Retained
checkpoints keep typed keys/incarnations, tombstones, bundles and signed history;
rebootstrap accepts only supported generations and authorized dependency closure.
Existing [retention holds](retention-holds.md) protect unresolved outcomes.

## Activate strong execution

`command_activation` returns a proposal with transition, state commitment and
exact predecessor head. The owner's coordinator activation checks this head and
installed generation commitments; the new handler waits until that succeeds.
Native `DevelopmentCoordinator.activate` and Kotlin's equivalent accept the
proposal's `activation` object. Public coordination uses the certified owner
control lane described in [public coordination](public-coordination.md).
Schema distribution and payload verification stay on application nodes.

## Evidence

SDK `tests/generations.rs` also requires a field with a migration while an
older member keeps its unsynced work and a device whose application stays at
version 1 keeps syncing (`a_required_field_migrates_on_the_issuer_and_older_devices_keep_syncing`),
reads the default for a NULL an older generation writes
(`a_null_an_older_generation_writes_to_a_required_field_reads_its_default`),
drops a manifest nothing retained names and fetches it again
(`a_manifest_nothing_retained_names_is_dropped_and_fetched_again`), and grants
a new table like an existing one. The shopping example's `SchemaUpgradeTest`
moves a version 6 household to version 7 while a version 6 member keeps syncing.
`rocky-secure`'s `tests::generations` interrupts shadow creation, backfill, switch
and definition activation; verifies atomic visibility, stale sessions, pending
work, old peers and typed checkpoint recovery, and drops and learns again an
unreferenced manifest (`an_unreferenced_manifest_is_dropped_and_learned_again_from_a_frame`).
Core/credentials tests
verify transition encoding, classification and signing; coordinator tests verify
authorized exact-head activation. SDK `tests/generations.rs` exercises issuer/member
upgrade, an offline member opening at its generation, nullable fields, scope denial
and queued handler work; [adopting generations](adopting-generations.md#evidence)
lists its own evidence.
See [phase-8 evidence](sdk-packaging.md#build-and-consume-a-package) for execution results.

This does not implement cross-file atomic switching,
Room file import or cross-coordinator migration. Migrations run only on the
migrating issuer; replicas never transform rows on their own.
