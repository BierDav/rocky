# Directory registration and deletion

Status: implemented deterministic `directory-app` command handler, composed with
persistent app runtime and the [shared coordinator](../../runtime/shared-coordination.md).
Directory code is never linked into the voter executable. See
[qualification](../../runtime/coordinator-evidence.md#coordination-and-crash-boundaries).

## Application boundary

Install `DirectoryApp` as a manifest-bound domain in `Runtime`. Check its manifest
commitment against root-certified `directory_definition`. Publisher, domain,
bootstrap registrar and initial domain ownership bind the installed definition.
The application uses the ordinary `DirectoryTransition` sealed-input command API;
mergeable directory writes are denied. Strong snapshot bytes are projected in the
same transaction as operation/result history and revision changes.

`RegisterUser` is limited to the bootstrap registrar. Each user principal is
bound to its public key. Users register their own deployment/node keys; deployment
membership must name a deployment owned by that user. User/deployment/node keys
are distinct and cannot reuse deleted identities. A client-only phone can be
registered and invoke permitted commands without becoming an eligible executor.

`Delegate` grants only explicit Directory rights: register deployments, register
nodes and further bounded delegation. The owner/subject, rights, parent, decreasing
depth and validity interval are retained. Descendants cannot expand rights or
lifetimes. Revoking a parent revokes descendants and tombstones their IDs.

`Policy` supplies a trusted domain, local caller, explicitly permitted remote
callers and policy reference. The app checks `StrongRequest.caller`, never a
payload-supplied actor. The runtime author map and certified invocation controls
must bind each node to that caller. Registration commands with a timestamp must
pass current admission freshness (30 seconds); deterministic execution evaluates
delegation at the admitted timestamp. Historical replay bypasses current admission.
Clock skew can extend the effective boundary by that allowance.

The composition signs client commands with `SignedCommandRequest` and admits them
through `Replica::receive_signed_request`. This verifies the signature and node
identity before the installed author map and application policy bind its caller.
Wrong node keys and forged requests fail before durable admission. The executor
remains trusted to run its installed reducer. Do not expose `receive_request`
bytes directly as a public unauthenticated endpoint. The
[encrypted row path](../../runtime/permissions-and-sync.md) remains the boundary
for signed application data capabilities. Directory identity registration itself
creates neither application data grants nor encryption keys.

## Independent application authority

Directory state has no universal administrator/CEO role and emits no resource
grants for other apps. Its domain ownership registry is initialized from explicit
bootstrap ownership; it is descriptive administrative metadata, not a replacement
for an application's own certified control or data policies.

A node registered by Directory still needs an Accounting-issued credential chain
with Accounting's manifest binding, row/field rights and validity before writing
Accounting. The runtime rejects a Directory signature pretending to be that issuer.
The qualification test permits only a specific account row and Balance field,
and rejects another row even after Accounting grants access.

## Deleting a user

`DeleteUser` is the user's own command. It must give exactly one disposition for
every owned domain: transfer to another live user, or retire. Missing, duplicate,
self-transfer or unknown-recipient dispositions fail atomically.

The transition removes the user, deployments and nodes, revokes their Directory
delegations and dependent grants, and keeps permanent key/grant-ID tombstones.
The resulting registry rejects learned-revoked node credentials. Key reuse and
user resurrection are denied.

`TrackArtifacts` monotonically records externally issued app grant references,
key custody scopes and pending command identities. Deletion retains an explicit
plan with revoked nodes/grants, scopes needing key rotation, pending requests
requiring historical outcome lookup and each domain disposition. Submitted work
may already be committed: cancellation must never undo a confirmed outcome.
Unsubmitted work is discarded by the client/session owner.

The application owner consumes this plan: stop/revoke sessions and executor
eligibility through new controls, publish the app's signed revocation policy,
rotate affected keys, remove local secret copies and resolve pending commands.
These are explicit external actions, not unimplemented side effects reported as
success. Directory cannot sign another app's revocation policy. The Accounting
fixture demonstrates denial after Accounting learns its own signed revocation.

Offline holders can retain old credentials/plaintext/keys until they learn a
revocation or credentials expire; previously disclosed data cannot be clawed back.
Artifact tracking cannot discover undisclosed external credentials or copied keys.
Strong deletion records the disposition and revocation plan; app/session owners
must complete it. External effect retry/delivery belongs to phase 5.

The registry has a combined 1,024-record budget and a 256 KiB snapshot budget;
command arguments are bounded to 16 KiB. Deletion evidence and tombstones count
against these budgets and are not silently pruned.
