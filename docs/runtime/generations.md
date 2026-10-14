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
- Requiring a field of an existing table, newly added or once nullable, is a
  [breaking generation](#breaking-generations): the field needs a default,
  and the schema's migrations compute its values once.
- Handler semantics require a higher handler generation; declared command
  versions within the support horizon keep their descriptors. Adding a version
  requires a new handler generation. Removing/redefining one is refused.
- Key rotation/revocation uses the existing explicit credential policy mechanism.
  Generation plans refuse reader-scope remapping. Key/type changes, removals
  and renames, which remove the old name, are unsupported, cuts included.

Partition group membership and names are immutable for existing tables, as are
partition columns. A new mergeable table may join an existing group with matching
slots. See [partition groups](partitioned-scopes.md#partition-groups).

`rocky_core::classify` and `classify_commands` reject unsupported changes before
activation. A signed transition binds application, old/new manifest, schema/handler
pairs, whether it is a [cut](#breaking-generations) and when it was signed. A
generation-number edit alone cannot open a different definition.

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
`generations()` reports current/previous generations, the cut horizon, the
baseline and bounded progress.

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

## Breaking generations

A field rows of the installed schema lack becomes required in a *cut*, a
transition signed with `cut` set: no earlier generation is admitted. Every
required field declares a default, which rows the cut never reached read,
such as hidden unique losers or children of a deleted parent.

The first issuer to open the new schema cuts, after asking its known issuers
for the generation (a cut one already signed is adopted like a member's):

1. It signs a step holding the required fields nullable, unless they already are.
2. At open, before the network starts, it assigns its own pseudonyms for that
   generation and runs `migrations` with versions above the installed one in one
   SQL transaction, then refuses the cut while a live row still holds NULL. A
   step is SQL, or `{"insert": table, "select": sql}`, which inserts a row per
   selected row with an `id` Rocky issues in its partition. It
   needs every partition the domain knows. `migration.json` records completion
   until the cut is signed, so a retry does not run statements twice.
3. It signs the schema as the cut and opens at it.

A member, or an issuer that finds the cut signed, fetches the lineage, saves
checkpoint-first [bootstrap intent](sdk-bootstrapping.md#join) before adopting
it and imports a checkpoint of the cut. Rows the cutting issuer never received
are gone, and the device's own unsynced work of earlier generations is dropped:
it offers no checkpoint and serves no frame of a generation behind its horizon,
and each cut derives new pseudonym keys, so its new writes start new author
logs. A device that has not upgraded is refused at admission.

Limits: two issuers
that open the new schema while unable to reach each other each cut, and their
households diverge. A peer behind a cut is refused at admission until its
application declares the cut; discovery never adopts one. The migration is one transaction, so it changes at most
[2,048 rows](sql-driver.md); see [decision 0066](../decisions/0066-breaking-generations.md).

## Old devices and pending work

Every generation of the lineage stays admissible, except behind a cut, whose
older peers are refused until their application declares it. Retained history behind a cut
remains verifiable locally; the cut is not permission to delete recovery
evidence. New admission verifies the
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

SDK `tests/generations.rs` also cuts with a migration, refuses a migration
that leaves NULL, drops a member's unsynced work and refuses a device left at
the old generation, and grants a new table like an existing one. The shopping
example's `SchemaUpgradeTest` moves a version 6 household to version 7.
`rocky-secure`'s `tests::generations` interrupts shadow creation, backfill, switch
and definition activation; verifies atomic visibility, stale sessions, pending
work, old peers/horizon and typed checkpoint recovery. Core/credentials tests
verify transition encoding, classification and signing; coordinator tests verify
authorized exact-head activation. SDK `tests/generations.rs` exercises issuer/member
upgrade, an offline member opening at its generation, nullable fields, scope denial
and queued handler work; [adopting generations](adopting-generations.md#evidence)
lists its own evidence.
See [phase-8 evidence](sdk-packaging.md#build-and-consume-a-package) for execution results.

This does not implement cross-file atomic switching,
Room file import or cross-coordinator migration. Migrations run only on the
cutting issuer; replicas never transform rows on their own.
