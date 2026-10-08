# Encrypted commands and scoped results

The byte-oriented command API of `rocky_secure::EncryptedReplica`, at method
version 1. [Typed commands](typed-commands.md) add descriptors, versions,
host-language handlers, tickets and delivery on the same admission,
confirmation and result application. The runtime owns admission, transactions
and encryption; application code owns deterministic transitions and the scopes
of its outputs. Coordinator calls carry metadata only.

## Application contract

`RowCommandApplication` extends `RowApplication`. The signed compiled manifest
registers method names and active schema/handler generations. Each method declares
its complete invariant-relevant `ResourceScope` set and a required invocation
scope. The latter prevents a caller from putting private command arguments into
an encryption scope shared with public readers.

The handler receives `CommandRows` and returns precise before/after changes plus
scoped result, job, blob or checkpoint outputs. Every change must pass the app's
old/new row validator and both the caller's and executor's paired row/field
grants. Generated cascades get the same checks; one denied effect rejects the
entire preparation before coordinator confirmation. There is no SQL-write channel
for handlers. Deterministic handlers must perform no external I/O.

`Invoke` and `Execute` grants identify a registered method by its manifest item ID
in `fields`, paired with the required resource scope. Invocation requires whole-
scope authority for its argument scope. Execution requires whole-scope read and
execute authority, plus installed keys, for every declared invariant scope.
Insert, update and delete rights remain independently necessary for effects.
Permission-controlling fields can change through confirmed commands; mergeable
row mutations still deny them. Changing an app ownership field does not issue
new credentials or rotate keys automatically.

Strong state lives in a separate projection from mergeable field registers.
Use `command_snapshot`/`command_companion` for confirmed command state and
`snapshot`/`companion` for mergeable rows. A command does not implicitly import
ordinary row operations into its input state. The employee demonstration shows
public edits as an overlay on its separately confirmed public snapshot.
All methods sharing a strong domain must use a stable complete resource set;
independent resource sets should use independent invariant domains.

## Invocation, execution and recovery

`invoke_encrypted(handle, scope, method, arguments)` commits an immutable signed
invocation before returning bytes for transport. Handles bind the complete input;
a conflicting reuse fails. The node's request sequence and encrypted arguments
commit together. Signing credentials remain attached for offline issuer use.
Node and user public keys map to coordinator IDs through domain-separated hashes;
`secure_node_id` and `secure_principal_id` expose those mappings for registration.

`execute_encrypted(command, coordinator)` follows these boundaries:

1. Verify the caller's signature, current credentials, method, app, generations
   and invocation scope. Check the coordinator's retained request receipt first.
2. For an unconfirmed request, verify complete local command projections against
   retained signed ciphertext and the exact confirmed predecessor. Read the
   coordinator head outside the database transaction; reject a fenced executor.
3. Read the complete input under the replica lock, release it and run the
   deterministic handler with no transaction open. Read the coordinator head
   again, re-read the input inside the preparation transaction and refuse a
   decision whose input or fence changed. Then commit the immutable
   preparation. Each scope gets an independently encrypted before/after
   projection; other outputs retain their declared scopes. `PackageSet` binds
   all ciphertext commitments. One command executes at a time per replica.
4. Confirm the resulting `StrongPackage` through the coordinator outside the
   transaction. The coordinator receives hashes, request identity and fencing
   metadata, without arguments, projections or output bytes.
5. Verify the receipt and atomically commit available authorized projections,
   ciphertext forwarding history, key fingerprints and revision changes. Publish
   revision watches after commit.

A lost confirmation reply leaves the outcome unknown. Retrying the same command
looks up its stable request identity and recovers the exact retained preparation,
including across reopen. Local apply rollback leaves all scopes unchanged; retry
uses the confirmed bytes. Preparations are retained per request and fencing/head
configuration, so a known unconfirmed request can be prepared after takeover.
A confirmed request whose executor payload is unavailable fails explicitly.
Coordinator ledger loss cannot be repaired by trusting application-held receipts;
see the [coordination limits](strong-publication.md).

## Receiving, subsets and completeness

`receive_encrypted_result` verifies the trusted coordinator receipt before its
transaction, then checks caller/executor signatures, scoped rights and ciphertext
commitments. It commits every currently available authorized row projection in
one transaction. A keyless relay stores the original encrypted result and creates
no plaintext command rows. Missing scopes and keys retain ciphertext for later
materialization. Reordered results wait for their per-scope predecessor; a newly
received predecessor also processes retained successors in order.

`command_snapshot.complete` requires a local key and a contiguous retained scope
history. It is local coverage, not proof of global synchronization. An incomplete
snapshot withholds rows. `command_companion` returns `Unavailable` for missing
coverage/key/row, `Present` for a materialized row, or `Deleted` only when retained
confirmed history proves that a previously present row was removed. After
`install_key`, call `reconcile_encrypted_results(coordinator)` to process retained
results with the newly available key.

Neither call scans the whole retained history per command. A replica audits each
scope's retained history against its projection on the first snapshot after open
and after any policy, credential or key change. Later snapshots verify only newer
results and compare the stored projection with the audited rows. The first result
received after open or such a change revisits every retained result; later ones
revisit only their successors. See the
[measured latency](end-to-end-evidence.md#strong-command-latency).

`encrypted_result_for` validates a named peer's current read/custody grants and
filters all payload kinds by subscribed scope. The complete commitment set and
receipt remain verifiable. `PackageSet::verify_confirmed_subset` verifies the
available subset without obtaining private parts. `open_package` independently
checks current read authority and the matching key generation. A receipt grants
no access. Subset verification cannot establish a complete business replay; the
executor must possess all invariant-relevant state.

Historical projection replay verifies original signatures and attenuated chains
under their original epochs; it does not erase already applied state after a
learned revocation. New admission and current reads use current policy. Obsolete
pending credentials cannot newly materialize results through reconciliation.

### Portable result evidence

A transferred result (`Message::Result`) carries the sender's voter-signed
`ReceiptEvidence`, at most 16 KiB. `NetworkCoordinator::import_evidence`
verifies it offline against the pinned genesis and caches it (4,096 entries)
before `verify`; it never changes an authoritative head. A receiver can
therefore verify a result whose identity the coordinator already retired. A
result whose receipt verifies but whose credentials fail the current policy is
checked with `verify_history`: caller and executor chains at the epoch and
time the command was issued, and its row effects under those historical
grants. The receipt makes this safe: a revoked device cannot obtain one for a
backdated command. Mergeable frames carry no receipt and keep the
current-epoch rule ([credentials limits](credentials.md#limits)).

Result sync bounds its coordinator `head` call to 2 s. When the head is
refused or unavailable, it pulls missing sequences in order until the peer has
none; each still needs a verified receipt, from evidence or a live lookup. A
result stays importable while a sender holds its evidence or the coordinator
retains it. This lets an executor take over after acknowledgment and a
custodian recover after a revocation from application-held state alone.

## Native transfer and limits

`NativeSession::execute_command` transfers a signed encrypted invocation and a
subscription-filtered confirmed result. `push_result` and `pull_result` transfer
original ciphertext through peers and relays. `serve_command_once` handles these
exchanges with the same fresh challenge authentication, signed session sequence,
I/O timeout and byte budget as native row sync. Each result pull requests an
explicit domain sequence and current subscribed scopes; entering a scope starts
at sequence one to obtain its predecessor history.

Named result acknowledgments persist per peer, domain, sequence and scope.
Custody acknowledges retained ciphertext; application acknowledgment requires
read authority and a materialized scope head. Public scope acknowledgment never
acknowledges a private projection. Retry keeps application evidence monotonic.

Command envelopes are limited to 128 KiB, including signatures and receipt.
Preparation reserves room for the receipt before confirmation. A handler declares
at most 32 scopes, emits at most 32 changes and 32 other outputs. Command requests,
preparations and results together retain at most 4,096 records and 32 MiB, separate
from the row-operation budget. Whole-snapshot result size bounds state growth;
limits fail explicitly. There is no pruning or automatic job dispatch.
`execute_encrypted` serializes executions; 32 may wait and further ones get
`Backpressure`.
Local projections remain plaintext at rest; relay protocol artifacts remain
ciphertext. This profile requires externally supplied keys and trusted coordinator
receipt verification, and does not qualify durable consensus or key distribution.

## Desktop demonstration

```sh
cargo run -p todo-desktop --bin employees-desktop --locked -- --data-dir local/employees-demo
```

An admin, a public editor and a keyless relay sync over authenticated loopback
TCP; the admin joins public names with payroll by stable employee ID, the
editor sees payroll access denied. It uses public fixture keys and an
in-memory coordinator, so strong progress stops after a restart; use a fresh
data directory.
