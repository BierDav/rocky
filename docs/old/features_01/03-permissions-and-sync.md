# Phase 3 — Permissions and opaque synchronization

> Historical foundation plan/evidence. Implemented contracts and recorded
> limits remain valid; future sequence and ownership follow
> [architecture V4](../../proposals/architecture-proposal-v4.md) and
> [features_04](../../features_04/README.md). Old future-tense instructions do not
> override that roadmap or require optional Room work before generic services.

Status: implemented for the [bounded native profile](../../runtime/permissions-and-sync.md)
after [phase 2](02-app-runtime.md), including [encrypted strong commands and the
employees desktop demo](../../runtime/encrypted-commands.md). V3 references: sections
3–5, 11–12. Owners: core verified protocol types; runtime cryptographic admission,
storage and replication; apps own resource policies. Add focused modules/crates
only where responsibility/dependency boundaries require them.

## Ordered slices

1. **Authenticated definitions and principals.** Select canonical signing and
   capability formats and pin compatible p2panda versions after an API spike.
   Distinguish user, deployment and node identities with separately delegated
   credentials. Verify publisher-bound manifest hashes, bounded delegation,
   actions, row/field scope, expiry and policy epochs. Offline issuers remain
   usable under retained evidence; define learned revocation/offline admission.
2. **Row enforcement and encryption.** Enforce whole-row read grants and paired
   row/field write grants. Check precise changes and old/new rows. Insert/delete,
   command invocation, execution, custody and schema publication are distinct
   rights. Permission-controlling fields default to strong transitions. Use
   separately encrypted 1:1 companion tables for different reader sets; a shared
   decryption key must not expose forbidden rows.
3. **Atomic p2panda adapter.** Implement operation/store/dependency integration
   inside framework transactions. Projection, original signed/encrypted bytes,
   cryptographic counters/state, outbox and revision commit atomically. Retry
   conflicts safely; never reuse persisted counters/nonces. Incoming forwarding
   preserves original signatures and must not echo newly authored operations.
4. **Native sync and relay.** Replace the fixture transport with authenticated
   native network sessions, named peers, byte quotas and continuation sync.
   Transfer ciphertext through a relay without plaintext/write grants. Support
   subscription entering/leaving, dependency closure and explicit custody versus
   application acknowledgments. Keep replication/encryption scopes distinct
   from invariant domains and storage partitions.

## Required demonstration

Two clients and an opaque relay use a signed manifest while its publisher is
offline. One user may edit a public field but cannot read or write private salary
rows. Authorized readers can join public/private companions with completeness;
an unavailable/evicted companion is never interpreted as deletion.

Scope-filter operations, before/after images, command results, jobs, blobs and
checkpoints. A multi-scope strong result binds separately encrypted subpackages;
its receipt does not grant access to private subpackages. Explain where a caller
can verify only an authorized subset and where an executor needs complete state.

## Gate

Test invalid signatures/delegation chains, cross-app replay, wrong scope/generation,
mixed allowed/forbidden patches, unrelated row/field grant combinations,
insert/delete without rights, ownership changes and cascade side effects. Prove
no private data escapes through result/transfer/query packages or relay storage.

Exercise duplicate/reordered frames, missing encrypted dependencies, lost keys,
revocation propagation, crypto-state rollback/conflict/crash and offline issuer.
Inspect stored/relayed artifacts for plaintext, with deterministic denied-operation
checks. Local projection encryption at rest is a separate declared policy.
Run `./scripts/check` and actual native transport tests. Record supported formats,
pinned dependencies, threat assumptions and resource limits; BLE/web are later
qualification work.

## Completion evidence

All four slices are implemented through the explicit `secure::EncryptedReplica`
boundary. The supported formats, pinned cryptographic dependencies, offline
revocation semantics, plaintext local projection policy and native transport
limits are recorded in the [runtime contract](../../runtime/permissions-and-sync.md)
and [decision 0002](../../decisions/0002-encrypted-row-protocol.md).

The secure command runtime uses the existing coordinator metadata protocol and
requires complete declared invariant scopes at the executor. Signed invocations
use an app-selected encryption scope. Durable preparations survive lost replies;
confirmed results bind independent encrypted row projections and result/job/blob/
checkpoint outputs. All available authorized projections, retained ciphertext,
key fingerprints and revision changes commit together. Receivers can verify an
authorized subset without private keys. Custody and application acknowledgments
are recorded per named peer and result scope.

The employees desktop composition uses actual authenticated loopback TCP between
two clients and a keyless relay. The public editor can edit names offline and
cannot read or write payroll. Scope completeness and missing companions remain
explicit. The trusted todo demo continues to use its original fixture transport.

Verification:

- `cargo test -p rocky-runtime secure::tests --locked` covers signed admission,
  scope/field pairing, tampering, delegated rights, offline issuers, revocation,
  missing dependencies/keys, original-byte forwarding and crash recovery.
- `secure::tests::commands` covers encrypted native invocations, lost replies and
  reopen, atomic rollback/retry, strong permission fields, mixed denied effects,
  filtered outputs, missing/reordered results and named scope acknowledgments.
- `cargo test -p todo-desktop secure_demo --locked` exercises both clients and
  relay over real TCP, public edits and encrypted multi-scope command results.
- `./scripts/check` passes formatting, workspace Clippy and all workspace tests.

Qualification remains bounded native TCP with externally managed keys and a
trusted coordinator port. Durable consensus/bootstrap belongs to phase 4;
external job delivery belongs to phase 5. These tests do not establish a
cryptographic audit, power-loss durability, BLE/web support or public Internet
transport. The desktop executable compiles; interactive window behavior has not
been manually qualified.
