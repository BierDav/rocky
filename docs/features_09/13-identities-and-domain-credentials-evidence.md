# Phase 13 evidence

Status: implemented, with the [limits](#limits) below. Three lanes built the
phase in parallel on one base and were merged into one line: B (slice 5,
author ranges and node signing), A (slices 2–4, identities, the owner's key
and revocation) and C (slice 6, credentials without a manifest and adoption
without renewal). Slice 1 is [decision 0075](../decisions/0075-generations-as-sealed-forks.md);
slice 7 is the guides listed below.

Guides: [credentials](../runtime/credentials.md),
[credential primitives](../runtime/credential-primitives.md),
[revocations](../runtime/revocations.md),
[issuer registry](../runtime/issuer-registry.md),
[existence logs](../runtime/existence-logs.md),
[SDK enrollment](../runtime/sdk-enrollment.md),
[adopting generations](../runtime/adopting-generations.md),
[public coordination](../runtime/public-coordination.md) and
[coordinated log](../runtime/coordinated-log.md). Ideas updated:
[one identity](../ideas/03_one_identity_and_passkeys.md),
[pseudonym rotation](../ideas/pseudonym_rotation.md) (obsolete) and
[partitioned existence logs](../ideas/partitioned_existence_logs.md).

## Contracts

### Identities and node certificates

- `IdentityProvider` (`public_key`, `sign(purpose, message)`,
  `reserve_author(range)`) supplies identities and never returns a private
  key. `FileIdentityProvider` keeps one in a directory; a `PrivateKey` signs
  as itself. SDK `Open.identity`, `Database::device_of` and
  `Database::enroll_identity`; native `NativeIdentityProvider`,
  `file_identity` and `NativeDatabase.enroll_identity`; Kotlin
  `IdentityProvider`, `FileIdentityProvider`, `RockyDatabase.enrollIdentity`
  and `RockySession.revokeIdentity`.
- A chain runs publisher → issuer identities → identity → node.
  `CredentialChain::certify`/`certify_with` sign the node certificate: the
  node is its subject and signing key, with exactly one author number of the
  identity's range, and it adds no rights. A stored identity's one node
  takes its range's first number; nodes of a supplied identity reserve from
  the bottom of its range (at most 256, half of it) through the provider.
- The node key signs every frame, existence record and checkpoint. Table
  author keys, pseudonym requests and assignments, sealed device bindings,
  their registry rows and revocation variants are gone. Grants and ordinary
  enrollment reserve up to 1,024 author numbers by default.

### The owner's key, issuers and revocation

- `Authority { identity, chain }`: `Authority::owner` for the owner, whose
  identity key is the publisher key, and `Authority::issuer` through issuer
  links (`issue_issuer`, extension field `issues`). No issuer, invitation or
  pair request carries the publisher key.
- `Endorsement` (the signer's issuer links and signature) signs
  `SignedManifest`, `SignedTransition`, `SignedHorizon` and
  `DiscoverySecret`. `verify` checks authenticity; `verify_current` also
  needs a signer no held statement ended, and adopting a generation met for
  the first time uses it.
- `Revocation` names `nodes`, `identities` and `capabilities`; revoking an
  identity ends every chain through it. SDK `revoke` takes `node` or
  `identity`.
- Coordinator: `DomainControl.trusted` lists identities; a `Request` carries
  the node certificate (`SignedCapability::certifies`); in-sync members
  record their identity and still count as nodes.

### Credentials without a manifest

- `Capability` names no manifest. `Credentials { publisher, revocations,
  chain }::verify(device, app, time)` refuses grants for another
  application; no chain verifier, cut, horizon acknowledgement or renewal
  request takes a manifest.
- `FieldSelection { All, Only }` (`rocky-core`): a grant without columns
  covers every field of its table in any generation, added ones included.
- The session `Hello` declares the sender's generation. The replica
  remembers it per peer (at most 1,024) and uses it for what the peer is
  sent and whether it is newer; a chain no longer decides it.
- Adoption verifies an offer from any device, issuers first, and settles it
  locally: `renew_into`, `renew_into_generation`, `issuer_routes` and
  `generation_pending` are removed. An issuer that adopts derives the new
  tables' first reader keys from the registry key and certifies its node
  again. A failed adoption fails that peer's sync. A checkpoint, envelope
  or frame of an unknown generation leads to adoption the same way.
- Ordinary renewal stays for expiry, key epochs and tables a generation
  grants like held ones; a member asks for it once per such generation, and
  the renewal attaches the new table: projection, partitions and
  subscriptions.
- A readable projection grows when grants widen over an unchanged schema:
  the replica records a projection plan of the installed schema, which
  rebuilds it with the added tables.

## Formats

Each changed format advanced once across the three lanes; every other
version is refused, with no migration or fallback.

| Format | Current |
| --- | --- |
| Capability signature and id labels | `rocky.capability.v2`, `rocky.capability.id.v2` |
| Capability extension (`authors`, `node`, `partitions`, `issues`) | 4 |
| Author binding (no manifest, `issues`) | 2 |
| Revocation ID and signature labels | 3 |
| Group secret envelope | 3 |
| Encrypted frame `ROW_FORMAT` | 9 |
| Existence record `EXISTENCE_FORMAT`; signature and hash labels | 3; 2 |
| Checkpoint `CHECKPOINT_FORMAT` | 12 |
| Encrypted replica file identity | 22 |
| SDK `identity.json` | 17 |
| Invitation URI | 5 |
| Pair request labels | 3 |
| Renewal answer | 4 |
| Identity request and answer | 1 |
| Signed manifest and transition labels | 2 |
| Horizon statement `HORIZON_FORMAT` | 6 |
| Discovery secret label | 2 |
| Live link handshake | `rocky.link.v6` |
| Coordinator control, request, Raft `FORMAT` | 3, 5, 7 |

## Completion gate

| Gate item | Tests |
| --- | --- |
| A second node of an enrolled identity joins without an issuer, writes every table it is granted under its own key and number, and its rows never collide with the first node's | `rocky-sdk` `identities::further_nodes_of_an_identity_join_without_an_issuer`; `rocky-credentials` `identities::issuers_grant_identities_whose_nodes_certify_themselves_without_an_issuer`; `rocky-secure` `tests::sql::two_nodes_of_one_principal_write_every_table_and_refuse_number_changes`; `rocky-sdk` `author_ranges::supplied_numbered_node_writes_offline_reopens_and_keeps_its_granted_range` |
| No pseudonym key, request, assignment or registry record remains in code or formats | Source audit below; `rocky-credentials` `signed::tests::a_capability_of_the_earlier_format_is_refused_and_names_no_generation` and `credentials::ranges_narrow_by_delegation_and_unknown_extension_versions_are_refused` |
| Revoking an identity stops all three of its nodes; revoking one node certificate stops only that node | `rocky-sdk` `identities::revoking_a_node_stops_it_alone_and_revoking_the_identity_stops_all_its_nodes`; `rocky-credentials` `identities::revoking_an_identity_stops_all_its_nodes_and_a_certificate_only_its_node` |
| A revoked issuer can no longer sign a manifest, seal or bookmark move, and no device it paired holds the publisher key | `rocky-credentials` `identities::a_revoked_issuer_signs_nothing_that_verifies_and_never_held_the_owners_key`; `rocky-sdk` `credentials::an_issuer_device_enrolls_and_renews_while_the_creator_is_closed` |
| A member adopts a newer generation while no issuer is reachable and keeps writing every table it is granted | `rocky-sdk` `generations::a_member_adopts_a_generation_from_another_member_with_no_issuer_reachable` |
| An invitation created before a schema change enrolls a device after it | `rocky-sdk` `generations::an_invitation_created_before_a_schema_change_enrolls_a_device_after_it` |

Related: `generations::a_table_granted_like_another_reaches_existing_members`
(a widening renewal grows the projection),
`migrations::an_issuer_without_a_partitions_key_keeps_the_households_generation`
(a checkpoint of a newer generation leads to adoption),
`shared_lists::a_cut_orders_items_and_links_categories_for_an_enrolled_writer`
(a table granted like a held one is held in its group partition) and
`rocky-coordinator` `controls_trust_identities_whose_certified_nodes_join_as_nodes`.

## Merged verification

On the merged tree, lane C rebased onto lane A and lane B below both:

- `./scripts/lint`: passed.
- `NEXTEST_TEST_THREADS=4 ./scripts/check`: passed clippy, 167 unit tests
  and 911 integration tests (670 across the workspace, 241 for `rocky-sdk`
  without default features). The integration tier took 147.3 s of its
  300 s budget. Two gate tests ran past 2 s under that load and pass alone in
  0.82 s (`a_member_adopts_a_generation_from_another_member_with_no_issuer_reachable`)
  and 0.93 s (`further_nodes_of_an_identity_join_without_an_issuer`).
- `cargo nextest run -p rocky-core -p rocky-credentials -p rocky-coordinator
  -p rocky-secure`, every tier: 386 passed.
- `cargo nextest run -p rocky-sdk -E 'not test(100000)'`, every tier: 290
  of 291 passed; the failure is the `service` test under [limits](#limits).
- `./sdks/native/check`: passed.
- `sdks/kotlin/gradlew -p sdks/kotlin check`: passed (3 m 50 s), including
  `IdentityTest` and the iOS simulator tests.
- `./examples/shoppinglist-app/check`: passed, required-schema parity, 58 JVM
  tests and both app builds, against this tree's SDK published to a
  workspace-local Maven repository (`-Dmaven.repo.local` through
  `KOTLIN_CLI_JAVA_OPTIONS`); the build's traces resolve Rocky from it.
- Source audit: `rg -i 'pseudonym|generation_pending|publisher_key|renew_into'`
  over `crates/`, `sdks/`, `examples/` and `docs/runtime/` finds nothing;
  neither do `SealedDevice` or `register_row_author`.

The merge removed the manifest from lane A's issuer links, authorities and
node certificates, kept its endorsement and revoked-signer checks on
adoption and its comparison of offers by manifest hash, and replaced its
placement of a chain's generation by the leaf's manifest with the
generation the session declares. It also fixed three regressions lane C
left: a widening renewal over an unchanged schema now grows the projection
through a plan and holds the new table in its group partitions, and a
checkpoint or envelope of a newer generation leads to adoption as a frame
does.

The 100,000-row migration
(`migrations::a_migration_of_100000_rows_converges_on_every_member`) times
out at 60 s under the default nextest profile, as before this phase, and
passes under `--profile e2e` in 454 s. Not run: `./scripts/check --e2e` as
a whole and the doc tests.

## Limits

- Seals and bookmark moves do not exist before
  [phase 14](14-seals-and-bookmarks.md). Today a revoked issuer can sign no
  manifest, transition, horizon statement or discovery value that verifies
  as current; seals and moves will use the same `Endorsement`.
- A table that a generation grants like a held one needs an issuer once:
  its grant and its own reader key come with a renewal, which only an issuer
  answers. Until then the member writes every table its chain names.
- Further nodes of an issuer identity sign and revoke but hand out no
  ranges; a non-owner issuer's own expiry grows only when an issuer above it
  renews it.
- `rocky-sdk` `service::a_registered_service_domain_commits_ordinary_rows_and_closes`
  fails its last assertion on every lane and on `p13-b`: a coordinated
  `BEGIN` after closing the domain still succeeds. It does not involve
  identities.
