# 4. Authentication and encrypted transaction bundles

> Historical foundation plan/evidence. Implemented contracts and recorded
> limits remain valid; future sequence and ownership follow
> [architecture V4](../../proposals/architecture-proposal-v4.md) and
> [features_04](../../features_04/README.md). Old future-tense instructions do not
> override that roadmap or require optional Room work before generic services.

Status: implemented on `EncryptedReplica` and in the SDK; see
[secure sessions](../../runtime/secure-sessions.md), [secure SQL](../../runtime/secure-sql.md),
[SDK enrollment](../../runtime/sdk-enrollment.md), [SDK transports](../../runtime/sdk-transports.md)
and [decision 0018](../../decisions/0018-secure-transaction-bundles.md). Open:
policy views for row-level reads and the limits of typed secure storage; see
[what remains](#what-remains). Owner: `rocky-core`
evidence types, `rocky-secure` admission/storage/transport, SDK
session configuration. Depends on phases 1–3.

## Reuse the authenticated path

The [secure runtime](../../runtime/permissions-and-sync.md) already verifies signed
manifests, attenuated capability chains, policy epochs, expiry and per-row/field
rights. Its single-resource row authoring is atomic and durable. The earlier
Kotlin creator/shared-token transport was a trusted development profile; the
SDK no longer contains it.

Build generated typed SQL storage on explicit secure admission contracts. Keep
the trusted profile visibly separate and regression-tested. Sharing capture or
schema code may be appropriate; merging storage formats or weakening signed
admission is not an incidental refactor.

## Session authority

Bind each SQL session to a principal, node/device signing identity, verified
definition, explicit permitted resource set, policy generation and key provider.
Authentication/enrollment supplies these at construction; DAO parameters never
claim a principal or grant. Separate login identity from device author identity,
coordinator eligibility, peer transport identity and encryption reader scopes.

Pin publisher/mesh trust anchors at installation and verify certified bootstrap
and control authority independently of application row grants. Directory identity
registration or an external login assertion does not authorize application data.
An identity-provider integration must verify its assertion and obtain a narrowed
signed capability chain; a username/tenant string supplied by the app is not proof.
See [Directory registration](../../applications/directory/registration.md) and
[shared coordination](../../runtime/shared-coordination.md).

Verify authority when a statement/cursor executes and immediately before commit,
not only during preparation. Credential renewal, learned revocation, expiry,
logout and domain rebind invalidate affected handles and authorized caches.
Previously returned bytes cannot be retracted; prevent further result delivery
and roll back unauthorized pending writes. Time/epoch checks and commit admission
must have a documented linearization point, including a revocation racing a
durable commit. Offline nodes can enforce only their latest learned policy.

SQL privilege restriction composes with the existing authorizer. Allow only
generated logical objects and approved functions/PRAGMAs. Prevent raw protocol
names, another domain's physical namespace, extension loading, application ATTACH,
dangerous schema edits and view/trigger escape paths. Errors and query diagnostics
must avoid disclosing unauthorized values or resource inventories.

## Row and field admission

Authorize every exact generated effect, including SQL triggers, foreign-key
cascades, REPLACE deletions and all-assigned-value masks. Keep row/field grants
paired on the same resource and row. Query visibility is not write permission;
Custody/Invoke/Execute/PublishSchema do not imply Insert/Update/Delete.

Expose authorized rows/columns through generated policy views and a checked
write plan. A writable visible entity containing read-only columns can use
changed-values mode without demanding rights to unchanged values. All-assigned
mode requires rights to every republished field. Permission-controlling fields
still require confirmed commands. One denied effect rejects the whole SQL commit.

Capture hooks cannot safely run asynchronous grant checks or crypto. Capture
inside the transaction, admit before commit, and refuse speculative SQL results
that would expose fields the session cannot read. App validators see exact
before/after images and the merged candidate, preserving existing semantics.

## Multi-resource encrypted commit

Current batches cover one resource scope, including one table. Define a versioned
transaction bundle containing a durable transaction identity, per-resource
operation/log identities, exact write masks, causal evidence and schema/policy
generations. Partition row bodies by reader scope; encrypt each partition under
its own key. Never widen a reader set to make a transaction easier.

Atomically store all local partitions, projections, immutable retry evidence,
outbox and revisions in the atomic placement group. Same-scope log continuity
must not introduce private predecessors into public logs. Bundle commitments
and membership proofs need a metadata disclosure analysis: a public recipient
must not learn private row/field values, and resource/participant identifiers
may themselves be sensitive.

Bulk Room writes must exceed today's 32-row single-frame limit without becoming
separate commits. Design bounded multi-frame bundles, durable assembly and quotas
for total transaction size; never split a successful list into independently
visible frames or raise limits without measuring memory and denial-of-service
cost. Receipt pages may stream while required projection application waits for
the complete authorized bundle.

Decide peer visibility before selecting a format. Proposed gate: a recipient
atomically applies all subscribed authorized partitions required for its view;
unheld private partitions do not block an independently valid public view.
Views/invariants that require an unavailable partition remain unavailable.
Tampered/omitted required members, mixed epochs and unavailable keys cannot yield
a partially valid transaction presented as complete. Receipt, custody and applied
acknowledgments describe exactly which partitions they cover.

This extends the current single-scope causal model. Record the wire/retention/
checkpoint design before implementation, including how pruned or scope-filtered
bundle evidence remains verifiable. Do not reuse a strong receipt to authorize
mergeable writes or to infer a global multi-domain confirmation.

## Keys, enrollment and pairing

Expose SDK-owned authenticated enrollment/pairing, capability renewal and pinned
peer identity without repository code. Invitations must not distribute broader
grants than approved, clone signing identities or store data/signing keys in
ordinary replicated entity tables. Supply keys through platform secret storage.

Retain native session challenges, pinned peer keys, signed frame sequencing and
original-byte forwarding through opaque relays. Authenticated encrypted payloads
do not imply TLS or hidden routing/credential metadata. Discovery, NAT traversal,
Internet transport and production login-provider deployment require explicit
qualification rather than inheriting loopback desktop evidence.

Retain current rules: installing a shared scope key requires whole-scope,
whole-row read authority; narrower disclosure needs separate reader scopes.
Reader-set changes require fresh keys and explicit migration. Revocation cannot
erase previously decrypted data. Local authorized projections remain plaintext
at rest unless a separately qualified disk-encryption adapter is selected.
Define cache/key eviction and logout cleanup consistently with that threat model.

## Completion gate

- Read-only/field-limited principals use unchanged DAOs without gaining write or
  ciphertext-key rights; denied aggregate/join/write paths fail safely.
- Mixed-domain, mixed-reader and cascade batches commit all or nothing locally;
  filtered peers satisfy the selected bundle visibility contract.
- Expiry/revocation between prepare, step and commit closes stale authority;
  offline policy limits are visible at configuration/status level.
- Tampering, credential attenuation, reader-key mismatch, log fork, omitted
  partition, lost acknowledgment and restart retain existing secure guarantees.
- Pairing/renewal/logout need no repository hooks, and no production profile uses
  the trusted shared-token transport by accident.

Run secure protocol, native-session and crash regression tests plus
`./scripts/check`. This evidence is not a cryptographic audit or Internet rollout.

## What remains

The bundle format, visibility gate, write masks, session authority, typed
secure storage with SQL sessions, SDK enrollment with a key provider, the
native ABI and pluggable transports are implemented. The Kotlin SDK uses only
the secure profile. These items are not:

- Generated policy views: rows are read whole, so a field-limited reader still
  needs its own reader scope. Aggregates and joins are limited by table.
- Typed secure storage serves one domain per file, refuses derived columns
  and cannot insert a deleted key again.
- The SDK grants every enrolled device full rights on every collection, syncs
  through the creator only and does not rotate reader keys after a revocation.
  The publisher key has no transfer or backup. No platform keystore adapter or
  login-provider integration ships; see
  [SDK enrollment](../../runtime/sdk-enrollment.md#qualification).
- Transports are qualified on loopback. NAT traversal, relays and Internet
  operation need their own qualification.
