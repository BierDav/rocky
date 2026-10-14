# Encrypted recovery and application effects

Protocol-artifact recovery and app-owned jobs on `rocky_secure::EncryptedReplica`.
See [encrypted commands](encrypted-commands.md) and
[shared coordination](shared-coordination.md) for admission and trust.

## Recovery packages

`export_recovery` captures a coherent local read snapshot under the replica
scheduler. It does not hold a write transaction or call a coordinator inside a
transaction. The signed inventory binds the compiled definition, domain/reader
scopes, schema/handler generations, policy and encryption epochs, retained
operation receipt set, observed coordinator heads, local revision, recipient and
payload closure hash. The `frontier` field is the complete retained receipt set,
not a compact maximal frontier; retention is bounded to 4096 artifacts and 32 MiB.

The canonical CBOR payload contains original encrypted operations and their
credential/dependency evidence, scoped confirmed results/receipts, immutable
preparations, invocation deduplication, mutation handles, pending requests,
durability settings and signed encrypted effect observations. It carries no SQL
projection export or signing/data keys. Recovery encrypts the whole package with
an externally managed XChaCha20-Poly1305 recovery key, in addition to existing
per-scope ciphertext. The inventory is authenticated associated data; the
exporter signs the inventory and ciphertext. Inspectable inventory/commitment
metadata is deliberately public.

Only currently authorized whole-scope readers/custodians can export or receive
selected scopes. Private companion payloads are omitted. Signed invocation
ciphertext cannot be stripped from a command envelope, so command artifacts are
omitted unless the recipient also has custody of its invocation scope. A scoped
reader package may therefore lack the state necessary for complete execution.
Export rejects incomplete operation dependency closure, missing strong
predecessors and mixed encryption generations instead of claiming completeness.

## Fresh-node recovery

1. Enroll a new signing identity, grants and coordinator eligibility. Open a fresh
   encrypted replica with the exact signed compiled definition. Supply data keys
   from external secret storage; never copy the old signing identity.
2. Transfer the recovery package through authenticated transport or external
   artifact custody, inspect its inventory and hash, and call `import_recovery`.
   Receipt verification and lookup happen outside storage transactions. Imported
   operations/results retain their original authors and request IDs.
3. Import resolves confirmed-but-unapplied preparations using coordinator lookup.
   Its report returns still-unconfirmed invocations for explicit execution. It
   never automatically repeats external calls. Operation and invocation handles
   remain deduplicated; new writes use the fresh signing identity.
4. Fetch any missing payload/history packages from an authorized peer and repeat
   import. Each artifact is durable/idempotent; an interrupted import is resumable.
   Import is not one large atomic database replacement and preserves local edits.
5. Fence the old executor with coordinator takeover, then call `recovery_status`
   again. The report separately identifies missing keys/payloads, incomplete
   scopes, generation mismatches, fencing and a head that changed during checking.
   Commands still check the exact confirmed predecessor and epoch at confirmation.

Use every invariant-relevant reader scope when checking execution readiness.
An empty scope list is rejected. Missing keys leave encrypted artifacts retained
and snapshots incomplete. Install keys and reconcile results to retry recovery;
missing payloads never become a fabricated empty confirmed state. A newer policy
must be learned explicitly before importing its package. Revoked/expired pending
invocations remain denied; import cannot renew their authority.

## Stuck coordinated domains

A trusted node that lost committed entries can rebuild from a member's complete
retained bundles, using the [coordinated catch-up path](coordinated-writes.md#catch-up).
The coordinator verifies metadata only; its genesis anchor and exact retained
entry hashes let the node verify the original frames before rejoining. Missing
entries or a gap outside the hash window keep appends refused.

Owner-signed cuts are not implemented. Existing command-domain replacement below
is a different operation and cannot recover a coordinated log. A cut needs a
history identity to fence abandoned frames at reused positions and a verified
canonical checkpoint base when the abandoned effects have already been pruned.
The present checkpoint format supplies neither coordinated-history binding nor
an authenticated replacement base for this case. See
[decision 0073](../decisions/0073-stuck-recovery-and-strong-commands.md);
[decision 0074](../decisions/0074-owner-signed-recovery-cuts.md) designs the cut.

## Explicit acknowledged loss

`MemoryCoordinator::replace_domain` and the certified
`NetworkCoordinator::replace_domain` require an exact old head and a distinct
replacement scope, with both acknowledged loss and uncertain effects recorded.
The certified operation requires the old scope's control authority and a valid
replacement control certificate. Both exact scope authorities must already be
root-certified in the mesh genesis.

Replacement permanently closes the old execution role and increments its fence.
Its confirmed sequence/head and receipt ledger stay available for audit; they are
never rewound. The new domain starts at its own genesis and is visibly separate.
Applications must explicitly install a signed definition/handler supporting that
replacement domain and decide which recovered business data to seed. The sample
Alice/Accounting definitions support their original domain only; replacement does
not invent compatible business state or silently retarget their commands.

## Recovery custody before confirmation

Async peer replication remains the default and supplies no preconfirmation
recovery guarantee. For stronger durability, persist `RequiredCustodians` through
`set_recovery_durability`, then:

1. Call `prepare_encrypted` to commit the immutable candidate.
2. Export it and its complete retained dependency closure to each configured
   custodian. `retain_recovery` verifies hashes, signatures, scope authority and
   closure, commits encrypted custody, and returns a signed `CustodyAck`.
3. Pass the acknowledgments to `confirm_encrypted`. The persisted threshold and
   any additional per-call threshold must pass before a new confirmation. Plain
   `execute_encrypted` cannot bypass a configured requirement.

Acknowledgments bind the stored package/closure and exact prospective strong
heads. A node counts once; the executor cannot count itself. Custodians may be
keyless with respect to application scopes, but need the external recovery key
to inspect the protocol closure. Application data stays individually encrypted.
Stored packages can be retrieved with `retained_recovery`. Successful confirmation
of an already accepted request reconciles its outcome without another custody
round. A custody acknowledgment proves retention at that point, not permanent
availability after device/disk loss. Renewed credentials and custody health remain
an operator responsibility. This profile trusts authorized custodian assertions;
coordinator voters do not inspect application payloads or custody policy.

## Durable effects

Deterministic commands emit reader-scoped `Job` outputs. `effect_jobs` discovers
only locally retained confirmed jobs. The runtime derives a stable job identity
from the signed request hash and output position, independent of ciphertext
randomness and delivery attempts. Applications own payload meaning, provider
selection and downstream grants.

`begin_effect` verifies the receipt, current execution fence/generation, complete
local confirmed state and current execution/read authority. It commits a signed,
encrypted start observation before returning the attempt. `dispatch_effect` then
calls the supplied asynchronous provider adapter without runtime/database or
coordinator locks, and `finish_effect` records the authenticated outcome.
Recorded inputs can contain time, randomness, human/AI decisions or provider
choices. Success/failure responses are retained as encrypted observation inputs;
replay and recovery never repeat the provider call.

| State | Meaning |
| --- | --- |
| Pending | No committed start observation |
| Uncertain | A start lacks an outcome, or the provider reported uncertainty |
| Failed | Attempts have recorded failures and no uncertain/successful outcome |
| Succeeded | A recorded success; further dispatch is denied |

A crash after start but before sending is indistinguishable from a crash after
sending but before recording. Explicit `retry_uncertain=true` permits another
attempt. Always pass the stable `EffectJob.id` as the provider's idempotency key;
the API exposes it but cannot force a provider to honor it. Duplicate delivery is
possible without provider deduplication. Retries get different attempt IDs.
An old executor can finish recording an already-started send after takeover if
its credentials remain valid; fencing cannot retract a webhook. Revocation or
outcome-record failure leaves uncertainty. Observations travel in recovery
packages or through `receive_effect_observation`, with receipt and signature
verification, not as unauthenticated status flags.

## Alice and Accounting

`accounting_app::Alice` owns checkbox rows and review-intent job rows in separate reader
scopes. `CheckEvent` records the event ID, observed Accounting revision/head and
checkbox value, and atomically emits a review intent. A local workflow revision
gives repeated checkbox toggles distinct stable intent rows. An identical checkbox
transition emits no additional job. The local checkbox is an optimistic workflow
choice, not proof that Accounting has accepted it.

`accounting_app::Accounting` owns versioned event rows. Composition first uses an authorized
Accounting `command_snapshot` to join events with Alice's checkbox state; the
observed revision/head is an explicit input to Alice's command. Alice does not
authenticate an arbitrary supplied Accounting observation as current state.
The downstream `ReviewEvent` command independently requires an Accounting Invoke
and exact row/field update grant and rechecks the current event revision and
settled condition. `SetEvent` advances the business revision. Read-only grants,
revoked credentials and stale/unsettled events deny the downstream update.
No Alice receipt grants Accounting rights; no atomic cross-app transaction is
claimed. A failed downstream command is an effect failure requiring application
workflow handling, not a rollback of Alice's committed checkbox.

## Qualification

```sh
cargo test -p rocky-secure --locked tests::recovery
cargo test -p rocky-secure --locked tests::effects
cargo test -p rocky-coordinator --locked replacement
```

Recovery tests exit an executor process before confirmation, after
confirmation before projection and after publication, then recover through
phone custody to a new signing identity. Fault tests cover repeated imports,
retained deduplication, dependency closure, private scope exclusion, key loss,
a withheld newest payload and a head advancing during import. Effect tests
cover an interrupted start, send-success/record-failure, takeover, stable IDs,
revocation and a loopback HTTP provider receiving two retries with one
idempotency key. This is local-process and loopback evidence: no power-loss,
Raft, recovery-key escrow or permanent custodian availability is qualified.

## Design rationale

- A recovery package holds retained protocol artifacts, never physical tables
  or SQL projections: a fresh node replays verified evidence under its own
  signing identity, and import stays resumable artifact by artifact. It is
  not a compact checkpoint or an atomic database restore.
- Strong confirmation orders a command but proves no recoverable payload, so
  preconfirmation custody is a separate persisted requirement and the
  coordinator still sees commitments only.
- Acknowledged loss creates a separate domain and closes the old one; the old
  confirmed head is never rewound.
- An external send cannot commit atomically with its outcome record. Job IDs
  therefore derive from the signed request and output position, start and
  outcome are separate signed observations, and replay never calls a provider.
  Exactly-once delivery depends on the provider honoring the idempotency key.
