# 0071: Staged migration publication

Status: accepted; implemented except issuer-independent `insert` ids
([phase 7](../features_09/07-migrations-at-any-size.md)).
Date: 2026-10-14.

## Context

A schema's `migrations` ran in one SQL transaction that committed as one
bundle, so a migration changed at most 2,048 rows. The issuer also signed a
step holding new required fields nullable, migrated and only then signed the
generation that requires them
([decision 0070](0070-confirmed-rollouts-and-the-end-of-cuts.md)). A schema
that evolves over decades must fill a new field of any table, however large
([features_09](../features_09/README.md)).

## Decision

- **Sign, then migrate.** An issuer whose application declares a schema
  with migrations above its installed version first asks its issuers for
  the generation; one already signed is adopted and nothing runs. Otherwise
  it writes `migration.json` (the version it migrates from), signs the
  schema, and at open, before the network starts, runs the migrations in one
  local SQL transaction in that generation. Required fields need no
  intermediate nullable step: rows read their default until migrated.
- **Who migrates.** Only an issuer that holds the key of, and reads and
  writes, every partition the domain knows signs such a generation. Another
  device keeps the domain's generation; its status reports
  `full_access_required`.
- **A staged publication.** `SecureSql::commit_publication` commits the
  transaction's net change as consecutive ordinary bundles within the bundle
  bounds, parents before children, all stamped at that commit. Bundle `n`'s
  handle is `rocky.secure-publication.v1` over the publication and `n`; the
  SDK's publication is `rocky.sdk.migration.v2` over the generation's
  manifest. The record of the publication commits in the same transaction
  and is the done mark: a crash before it repeats the migration, after it
  repeats nothing, and a repeated commit returns the recorded frames.
- **Publication** is ordinary sync: the frames are retained and pushed or
  pulled until peers hold them. Progress counts the leading bundles some
  peer's frontier holds. A publication's frames do not count toward the
  retained-history bound until they are pruned; otherwise a migration larger
  than the bound would refuse the writes that deliver it, such as renewing
  members into its generation.
- **Concurrent issuers converge without coordination.** Updates meet by
  latest wins; rows an `insert` step adds merge when the table is keyed by
  name, since equal keys are one row.

## Consequences

The encrypted replica records publications and their frames, so its file
identity moves to `rocky.encrypted-replica.v17`; v16 files are refused.
`migration.json` holds only `from`. A migration of `n` rows retains roughly
3 KB per row of history until it is pruned, outside the retained bound. A
projection switch recreates partitioned tables from their stored rows, since
a shadow fills from one scope.

Not implemented: an `insert` step's ids derive from the issuer's own
creation log. A row's id is an author number and a sequence, bound by the
publisher's author binding and creation proof, and each author's births must
continue its log; ids derived from the generation, step and selected key
need a new identity class. Until then two issuers that insert into a table
keyed by `id` add each row twice. Migration SQL cannot call
`rocky_next_id`, which the SQL authorizer refuses.
See [generations](../runtime/generations.md#migrations) and
[secure SQL](../runtime/secure-sql.md#staged-publications).
