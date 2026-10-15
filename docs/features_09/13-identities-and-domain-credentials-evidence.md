# Phase 13 evidence

## Lane B — author ranges and node signing

Scope: brief slice 5. This lane removes table author keys, request/assignment
messages and APIs, sealed device bindings, issuer-registry mapping rows and the
principal/revocation variants that represented them. It retains principal author
ranges and partition capability extensions.

### Contracts for integration

- `AuthorRange`, `MAX_AUTHOR`, `Extended` and `Extension` now live in
  `rocky-credentials/src/extension.rs` and remain re-exported at the crate root.
- `Authority::issue` and `issue_delegable` take an explicit `AuthorRange`
  before the validity argument; raw issuers must allocate disjoint ranges.
- `CredentialChain::complete(holder, node, author)`,
  `complete_with_alias(holder, node, node, author)` and
  `device_chain(root, holder, node, author)` require an explicit number.
  A node leaf's `Extended.authors` is exactly `[author, author]`; the granting
  principal's range must contain it. Its subject and signing key are the node.
  Lane A's node certificate should take over this field and allocate distinct
  numbers within the identity's granted range.
- `author_number()` returns the leaf's one number. `verify_author` and
  `verify_partition_author` now verify node leaves. A node can sign every table
  in its grants with the same key and number. The secure runtime persists its
  node-to-number binding atomically with accepted chains/existence/checkpoints,
  refuses conflicting leaves and preserves the binding across reopen.
- SDK grants and ordinary enrollment reserve up to 1,024 available numbers by default;
  explicit `authors` chooses a count. The creator uses 1 and starts allocation
  at 2; issuer invitations split the unallocated range. Registry capability
  entries include the principal range; re-granting a recorded principal reuses
  it. Non-issuer delegators persist outgoing principal ranges locally and reuse
  them after reopen. Default grants retain half of a small free range for later
  recipients. Allocation is saved before returning credentials. Self-enrollment and
  provider-owned certificate allocation remain lane A's contract.
- Kotlin and native JSON no longer expose table-author setup or lookup calls.
  Row allocation needs an open SQL transaction and the numbered node credential.
- Generation manifest handling, adoption renewal and publisher-key distribution
  are retained for lanes C and A respectively.

### Current formats

Every changed format advances once; integration must resolve overlapping bumps
to these values rather than incrementing them again.

| Format | Before | Current |
| --- | --- | --- |
| Capability extension | 3 | 4 |
| Portable author binding and label | 1 | 2 |
| Revocation signature/ID labels | 2 | 3 |
| Encrypted frame `ROW_FORMAT` | 8 | 9 |
| Existence record `EXISTENCE_FORMAT` | 2 | 3 |
| Existence signature/hash labels | 1 | 2 |
| Checkpoint `CHECKPOINT_FORMAT` | 11 | 12 |
| Encrypted replica file identity | 21 | 22 |
| SDK `identity.json` | 16 | 17 |

Other versions are refused. There is no migration, fallback or old decoder.

### Verification

- `cargo test -p rocky-credentials --locked`: passed 35 unit and 13 integration
  tests in the initial credential run. Coverage includes two node leaves of one
  principal, table grants, range attenuation, forged out-of-range leaves,
  partition selectors and extension-version refusal.
- `cargo test -p rocky-sdk --no-default-features --test author_ranges --test
  credentials --test partitions --locked`: passed 1 new range/offline/reopen test,
  6 credential tests and 3 partition tests. The new test took 0.25 s in that run.
- `cargo test -p rocky-secure --lib --locked`: passed 235 tests, with 9 ignored
  measurements/helpers, in 34.00 s. The two-node regression checks both tables,
  signatures and non-colliding IDs, and rejects a changed number through frame
  receipt, renewal and checkpoint import after reopen. Existing existence-log,
  partition, checkpoint, held-change and generation tests use node signatures.
- The mixed-grant checkpoint regression passes: an ordinary table checkpoint
  preserves selector grants for other tables. The delivery regression that uses
  that checkpoint also passes.
- The Rust SQL driver suite passes 11 tests, with one deliberately ignored tier
  case. Its fixtures now allocate IDs instead of assuming per-table numbers.
- The final targeted SDK regression run passes 44 tests across composition,
  grants, group secrets, peers, registry, registry revocation and shared lists.
  Delegation coverage reopens a non-issuer delegator before narrowing a grant
  and verifies that the recipient retains its node number.
- `sdks/kotlin/gradlew -p sdks/kotlin check`: passed again with the final native
  libraries, including JVM, Room, schema qualification, packaged consumers and
  iOS simulator checks (2m 18s).
- `./examples/shoppinglist-app/check`: passed with this lane's staged Maven
  artifacts: hand/Room required-schema parity, all 58 JVM tests, Android test
  compilation, desktop build and Android build. A generated copy under `target/`
  and `-Dmaven.repo.local=<lane staging repository>` isolated this consumer from
  concurrent lanes' Maven-local publications; tracked app configuration is unchanged.
- `./scripts/lint`: passed.
- `NEXTEST_TEST_THREADS=4 ./scripts/check`: passed clippy, 165 unit tests and
  897 integration tests across the workspace and SDK without default features.
  The integration tier took 154.93 s (90.18 s after load normalization), within
  its 300 s budget. An earlier unrestricted run during native builds hit three
  runtime retention timeouts; all three pass in 0.89–1.43 s with four concurrent
  tests, with unchanged limits. No assertion failures remain.
- A source audit over `crates/`, `sdks/` and `examples/`, including their
  qualification trees and excluding generated/build output, finds no removed
  key/type/RPC names (`pseudonym`, `SealedDevice`, `register_row_author`).

Limits: the separate e2e tier and doc tests were not run. These checks do not
qualify lane A's identity-provider allocation or the final merged phase.

## Lane A — identities, node certificates, the owner's key and revocation

Scope: brief slices 2, 3 and 4, on top of lane B. Guides:
[credentials](../runtime/credentials.md#identities),
[credential primitives](../runtime/credential-primitives.md#principal-ranges-and-node-authors),
[revocations](../runtime/revocations.md),
[SDK enrollment](../runtime/sdk-enrollment.md#identities),
[issuer registry](../runtime/issuer-registry.md),
[public coordination](../runtime/public-coordination.md) and
[coordinated log](../runtime/coordinated-log.md).

### Contracts for integration

- `IdentityProvider` (`public_key`, `sign(purpose, message)`,
  `reserve_author(range)`) supplies identities; `FileIdentityProvider`
  keeps one in a directory, and a `PrivateKey` signs as itself. SDK
  `Open.identity`, `Database::device_of`, `Database::enroll_identity`;
  native `NativeIdentityProvider`, `file_identity`,
  `NativeDatabase.enroll_identity` and an `identity` argument on the other
  constructors and `prepare_device`; Kotlin `IdentityProvider`,
  `FileIdentityProvider`, `RockyDatabase.enrollIdentity` and `identity`
  parameters, `RockySession.revokeIdentity`.
- `Authority { identity, chain, manifest }`: `Authority::owner` for the owner,
  whose identity key is the publisher key, `Authority::issuer` through issuer
  links. `issue_issuer` grants issuer authority (extension field `issues`);
  issuer links name no manifest and may grant any grant and range below them.
  `CredentialChain::certify`/`certify_with` take over lane B's node leaf: the
  subject is the node, with exactly one number of the identity's range.
- `Endorsement` (the signer's issuer links and signature) signs
  `SignedManifest`, `SignedTransition`, `SignedHorizon` and `DiscoverySecret`;
  `verify` is authenticity, `verify_current` also needs a signer no held
  statement ended. Adopting a generation met for the first time uses it.
- `SignedRevocation::sign(statement, identity, chain)`; `Revocation` names
  `nodes`, `identities` and `capabilities`, signed by `issuer`, the identity.
  `RevocationSet::revoking_holder` tells a node's own revocation from its
  issuer's. SDK `revoke` takes `node` or `identity`; the intent's `Rotation`
  lists `nodes`.
- Coordinator: `DomainControl.trusted` lists identities; `Request` carries the
  node `certificate` (`SignedCapability::certifies`); in-sync `Member`s record
  their `identity`; `NetworkCoordinator::certify` sets the certificate.
- A generation's chain is placed by its leaf's manifest in `rocky-secure`, not
  its root's. Lane C's removal of `Capability.manifest` drops the issuer-link
  exemption in `CredentialChain::links` and `AuthorBinding::verify_chain`.
- Node numbers: a stored identity's one node takes its range's first number;
  nodes of a supplied identity reserve from the bottom of its range (at most
  256, half of it) through the provider, and that node's `Minting` starts
  above them.

### Formats

Values resolve with lane B's to these; none advances twice.

| Format | Current |
| --- | --- |
| Capability extension (`issues` added) | 4 |
| Author binding (`issues` added) | 2 |
| Revocation ID and signature labels; set entries carry the issuer chain | 3; at most 8 KiB each |
| SDK `identity.json` (`identity`, `stored_identity`, rotation `nodes`) | 17 |
| Invitation URI | 5 |
| Pair request labels; enrollment carries the issuer chain, no key | 3 |
| Renewal answer, signed by the answering identity with its issuer chain | 4 |
| Identity request and answer | 1 |
| Signed manifest and transition labels | 2 |
| Horizon statement (`HORIZON_FORMAT`) | 6 |
| Discovery secret label | 2 |
| Coordinator control label, request label, Raft `FORMAT` | 3, 5, 7 |

### Verification

- `cargo test -p rocky-credentials --test identities`: three nodes of one
  identity certify themselves with distinct numbers and no issuer; a
  certificate adds no rights and keeps the range; revoking an identity stops
  all three nodes, revoking one node certificate only that node; a revoked
  issuer's manifest no longer verifies as current while what it signed
  before stays authentic; another issuer renews its members; the owner is
  never revoked. 41–72 ms each.
- `cargo test -p rocky-sdk --test identities`: two further nodes join from a
  node of the identity with no issuer reachable and read its rows, a
  stranger's provider is refused; revoking one node makes it alone report
  `revoked`, revoking the identity stops the other two, unrecorded ones
  included (0.83 s and 1.09 s alone). `--test credentials`: a paired issuer
  holds no copy of the publisher key.
- `cargo test -p rocky-coordinator`: a control trusting an identity admits
  its certified nodes as separate in-sync members, refuses a missing,
  foreign or misbound certificate, and waits to drop an identity in sync.
- `./scripts/lint` and `NEXTEST_TEST_THREADS=4 ./scripts/check`: passed, 668
  and 239 tests. `./sdks/native/check`: passed.
  `sdks/kotlin/gradlew -p sdks/kotlin check`: passed, with `IdentityTest`
  (enrollIdentity, revokeIdentity) and the iOS simulator tests.
- `./examples/shoppinglist-app/check`: passed, 58 JVM tests and both app
  builds, against this lane's SDK published to a lane-local
  `-Dmaven.repo.local` given through `KOTLIN_CLI_JAVA_OPTIONS`.

- `cargo nextest run -p rocky-sdk -p rocky-credentials -p rocky-secure -p
  rocky-coordinator`, every tier except the 100,000-row migration: 615 of
  616 passed.

Limits: `scripts/check --e2e` was not run; the package run above covers these
crates' e2e tests except the 100,000-row migration. Of them, the e2e test
`a_registered_service_domain_commits_ordinary_rows_and_closes` now commits
through identity-trusted controls but fails its last assertion: a coordinated
`BEGIN` after closing the domain still succeeds, since it only reads the head.
On the base commit it failed earlier, and this does not involve identities. Further nodes of an issuer identity sign
and revoke but hand out no ranges; a non-owner issuer's own expiry grows
only when an issuer above it renews it. Lane C's adoption without renewal is
not part of these checks.
