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
