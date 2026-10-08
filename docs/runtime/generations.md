# Recoverable application generations

Implemented for the encrypted runtime and SDK's single-file placement group.
[Decision 0028](../decisions/0028-generation-lineage-and-signed-backups.md) fixes the
formats. The trusted runtime's earlier same-definition APIs remain separate.

## Choose the change

- Local version, SQL spelling, local tables and indexes do not authorize a new
  replicated definition. A local plan keeps the manifest and row identities.
- Adding a mergeable table or nullable field requires a publisher-signed
  `GenerationTransition`. Rows from an older definition supply null for newly
  added nullable fields. Logical names remain stable identities.
- Handler semantics require a higher handler generation; declared command
  versions within the support horizon keep their descriptors. Adding a version
  requires a new handler generation. Removing/redefining one is refused.
- Key rotation/revocation uses the existing explicit credential policy mechanism.
  Generation plans refuse reader-scope remapping. Key/type changes, removals,
  renames and nonnullable replicated field additions are unsupported.

`rocky_core::classify` and `classify_commands` reject unsupported changes before
activation. A signed transition binds application, old/new manifest, schema/handler
pairs and oldest admissible schema generation. A generation-number edit alone
cannot open a different definition.

## Plan, backfill and switch

`EncryptedReplica::install_tables` records a durable descriptor and progress,
creates shadow tables, fills 256 stored rows per transaction, verifies projections
against canonical stored rows and switches all affected tables in one transaction.
Concurrent committed updates also maintain the shadow. Readers see the old view
until switch, then the complete new view. Notifications follow commit; old secure
sessions and cursors become stale at the authority switch.

Reopen resumes the recorded plan. An incompatible pending plan is refused.
`open_upgrading` verifies the signed lineage before changing the installed manifest;
projection switch and generation activation retain a recoverable durable boundary.
Network/coordinator calls never occur inside these storage transactions.
`generations()` reports current/previous generations, horizon and bounded progress.

The SDK issuer signs an additive upgrade when `Database::open` receives the new
schema. A member fetches that exact schema/lineage over its attached enrollment
transport; an offline member needing it fails without changing its old installation.
Issuer state is saved before replica activation, so reopening with the same schema
continues after interruption. New collections do not silently expand member grants.
SDK lineage is capped at 32 steps; secure runtime lineage at 64.

## Old devices and pending work

SDK upgrades keep the immediately preceding schema generation admissible. Older
peers receive explicit upgrade/rebootstrap refusal after the horizon advances.
Retained history behind the horizon remains verifiable locally; the admission
horizon is not permission to delete recovery evidence. New admission verifies the
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

`rocky-secure`'s `tests::generations` interrupts shadow creation, backfill, switch
and definition activation; verifies atomic visibility, stale sessions, pending
work, old peers/horizon and typed checkpoint recovery. Core/credentials tests
verify transition encoding, classification and signing; coordinator tests verify
authorized exact-head activation. SDK `tests/generations.rs` exercises issuer/member
upgrade, an offline refusal, nullable fields, scope denial and queued handler work.
See [phase-8 evidence](sdk-packaging.md#build-and-consume-a-package) for execution results.

This does not implement arbitrary schema transforms, cross-file atomic switching,
Room file import or cross-coordinator migration.
