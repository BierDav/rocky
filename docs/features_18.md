# Feature 18 — Passkey sign-in to a personal database

Status: design proposed; implementation and acceptance tests pending. Source
references describe the working checkout on 2026-11-15; concurrent SDK, transport
and coordinator refactors must preserve the contracts below. Personal sign-in
remains design work; the transport prerequisite below is implemented separately.
Phases run in order except the independent custody work.

## Contract and existing foundations

A personal database belongs to one identity: an application's private registry,
settings or chats. “Root = identity” means **one authentication secret**, never
equal signing keys. One platform passkey prompt with user verification evaluates
WebAuthn PRF; use `RootKey::from_secret(output)` and `RootKey::identity()` (HKDF
SHA-256, existing `rocky identity v1` label). Do not feed the same bytes directly
to both key constructors. These primitives already exist ([root.rs:22][root],
[root.rs:40][owner]); `keys::Vault` is installation secret storage, not a PRF
adapter ([keys.rs:12][vault]). WebAuthn acquisition/verification is a new app
integration, not an existing Rocky capability.

Sign-in adds a device and never revokes, rotates existing epochs, or grants a
fresh recovery identity. Reuse root-signed manifests, credentials and escrow;
do not call `Configuration::recover`, `Settings::recover` or
`revoke_before_recovery`: those deliberately replace trust and end earlier
grants ([configuration.rs:99][recover], [enrollment.rs:1518][recover-settings],
[enrollment.rs:1738][recover-revoke]). Keep shared-database
`enroll_identity`/`AwaitingLink` and its six-digit confirmation ceremony
([self_enrollment.rs:190][link], [enroll.rs:678][code]). In personal mode that
ceremony adds no protection against a passkey holder who can root-grant a node
and open escrow. Security is the passkey, the platform's user verification and
the app handling the resulting secret; whoever holds it can add and revoke
devices. A device's ordinary persisted node key is not the passkey.

## Transport prerequisite — scoped peer RPC

Status: implemented on 2026-11-15; no personal sign-in or coordinator plug added.

The core port carries bounded requests, live sessions and inbound work, with
address, scope registration and shutdown. `Connector::start` returns `Connected`:
the transport plus optional independent discovery and network-change ports.
`rocky-iroh::Options` implements the connector. The SDK defaults select no
network. The separate coordinator prerequisite below removes the SDK client feature.
Engine and Custody accept an application-supplied `Arc<dyn Connector>`; their
remaining configuration conversion belongs to the concurrent typed SDK work.
The `rocky-peer` launcher owns its transport configuration in its own crate.

`Transport::request` and `session` are required harness methods, with per-route
bounds that include dialing and request I/O. Low-level dialing takes an explicit
optional scope so wrappers can inject stream faults without losing database
routing. The memory harness also rejects mismatched registered scopes.

Canonical CBOR peer requests, replies, retryable errors and proof requirements
live in `rocky-credentials`, independently of storage, SDK and transport.
Dispatch exhaustively matches the request. The handler verifies each declared
proof; the admission class selects its SDK budget, after harness admission.

| Request | Required caller proof | Admission class |
| --- | --- | --- |
| Pair | Invitation | Enrollment |
| Renew | Enrolled node | Enrollment |
| Epochs | Enrolled node | Enrollment |
| Policy | Public lookup; issuer answers | Metadata |
| Identity | Identity possession and confirmation | Membership |
| LinkEnroll | Identity-signed node certificate | Membership |
| Unlink | Identity-signed unlink evidence | Membership |
| Resolve | Public challenge; reply proves pinned node | Metadata |
| TipSchema | Public lookup; record commitment verified by receiver | Metadata |
| StagedClose | Issuer-authorized signed close | Metadata |

The iroh harness owns `rocky/rpc`, `rocky/link`, and the unchanged coordinator
ALPN, framing, deadlines, fallback, caches, endpoint controls and QUIC path
observations. Every RPC/link stream starts with one current, mandatory database
scope frame; link lanes must all name the same scope. Unknown scopes and other
scope-frame versions receive typed refusals before database dispatch. There are
no prefix/JSON RPC readers or alternate resolution handshakes on live links.
Each scoped port has a separate incoming queue; configured discovery instances
are independent. Detaching
one scope preserves other databases and application protocols. Scope refusals
never invalidate unrelated live links.

Measured verification on 2026-11-15:

- `cargo test -p rocky-core -p rocky-iroh` passed. Native and wasm clippy passed
  for core, credentials and iroh. Four shared RPC/proof codec tests passed.
- Twenty focused SDK peer/membership/revocation tests passed, including all six
  native iroh gates. Two memory-harness tests passed for route-deadline fallback
  and closing a partially received RPC with an accept pending. All three fault cases and the
  borrowed/owned network lifecycle regression also passed after that shutdown fix.
- All six requested discovery/engine/fault e2e entries passed, plus real
  two-database routing and discovery-detach tests: eight tests passed together.
- The headless browser suites passed 194 core/credentials/iroh tests in 25 runs.
  `scripts/browser-sync` passed browser-to-native replication and coordinator
  claims over a local relay. The peer launcher and desktop consumers compile.
- `./scripts/lint` passed, including the default native/browser SDK dependency
  boundary. `cargo check -p rocky-sdk --no-default-features --lib` passed.
- The all-target SDK check still fails on fourteen concurrent fixture errors:
  `expected SqlValue, found Value` in `tests/coordinated.rs` and
  `tests/service/migration.rs`. These conversions remain with their owning agent.
- `./scripts/check` and `./scripts/check --wasm` stop at concurrent SDK conversion
  lints, including redundant fields, unused imports, an enum variant name and
  the SDK request enum's size. The coordinator operation enum lint is fixed
  by the coordinator prerequisite below.
- Broader regression runs passed all Lost/Compromised issuer/revocation cases
  and interrupted RPC answer retries. One converted registry fixture reports
  `Error: "routes"` because its snapshot omits `renewal_routes`; an Engine
  mutation fixture reports `left: Bool(false), right: Bool(true)` at its replay
  assertion. Both belong to the concurrent application-call conversion.

These transport checks do not establish the passkey, storage-adapter, desktop
or distributed-security acceptance criteria of the personal sign-in phases.

## SDK prerequisite — native application operations

Status: implemented and verified on 2026-11-15.

`Session` exposes named Rust methods with native request/result types. The
generic string/JSON calls, byte-envelope application dispatcher, JSON signer
inputs and `Root::of` are removed. Root and identity providers are scoped to
the operation that needs them. `CallFailure` retains stable codes and retry
classification; service errors remain downcastable.

Domain/generation inspection, identity labels, activity, held changes, retention,
coordinated status and partitions use native structs/enums. SQL results retain
native cells and scopes. `EngineOptions`/`EngineMode` and `CustodyConfiguration`
replace JSON configuration. The simulation and SDK callers invoke typed
methods. Test snapshots only project native results for assertions; current
credential/schema fixtures still use their serialized record formats.

Remaining SDK JSON belongs to SQL JSON TEXT values, schema defaults/catalogs,
current contact/invitation/identity/enrollment/bootstrap records and test
fixtures/assertions. No application request is selected or decoded from JSON.
The independent transport work above owns the peer protocol.

Measured verification:

- `./scripts/lint` passes. The final `./scripts/check` passes native workspace
  and standalone SDK clippy, 320 unit tests, 1,247 integration tests and all
  980 browser tests in 79 runs. Browser-to-native relay replication and
  authorized coordinator claims also pass.
- `cargo test -p todo-app` passes all 10 tests; `cargo check -p todo-desktop`
  passes. Six focused stalled-dial/backoff regressions pass after classifying
  transport session failures as `ServiceError::Unreachable`, preventing a
  failed dial from triggering credential renewal.
- The final SDK `serde_json::Value|\bValue\b` audit accounts for 80 matching
  lines in 22 files, all in the non-call categories above. The application
  dispatcher and JSON signer-input removal scan has no matches.

These checks do not implement or qualify personal sign-in, desktop UI behavior
or the remaining e2e acceptance gates.

## Coordinator prerequisite — application composition

Status: implemented on 2026-11-15; the voter restart regression is fixed.
Workspace acceptance remains blocked by concurrent SDK conversion checks.

The application supplies `Arc<dyn rocky_credentials::Coordinator>` through
`Configuration::coordination`, `Engine::open_composed` or custody configuration.
The trait extends the core `DomainLog` with typed counter claims and readings,
revocation push, node binding and credential/observation updates. Credential
ownership keeps this extension out of core without a dependency cycle.
`rocky_coordinator::Client::connect(&bundle, transport)` constructs the voter
adapter; tenant administration lives in its application-owned `ControlClient`.
There is no SDK coordinator constructor, `Coordination` enum, `iroh` feature or
normal dependency on rocky-coordinator/rocky-iroh. Unconfigured counter calls
return `CoordinatorNotConfigured`; local writes continue. The operation enum
boxes its revocation set, preserving its one wire encoding.

Attaching coordination after open binds the node key and current certificate
before replacing the slot; binding failures propagate to the caller. Each bind
owns independent certificate, claims and observer state, so applications can
reuse a client factory across databases. The custody launcher's outbound-only
coordinator endpoint uses an ephemeral port on the configured interface,
leaving the fixed listener port and persisted transport identity to custody.

Measured verification:

- Review follow-up: the four-package core/iroh/coordinator/voter run passes
  all 174 tests, including cancellation with a concurrent healthy stream and
  replacement dial. Five memory-harness tests, the late-attachment binding
  regression, all six native SDK iroh gates and the fixed-port launcher test
  pass. All SDK test targets compile; the peer launcher check passes.
- Targeted native all-target clippy and browser library clippy pass. The
  core/iroh/coordinator browser suites pass all 132 tests in 21 runs.
  The full `scripts/check --wasm` passes all 980 tests across 79 browser runs,
  plus browser-to-native relay replication and authorized coordinator claims.
  `scripts/lint` passes. The full native check stopped at the concurrent SDK
  fixture's unused `Transport` import in `tests/composition.rs:9`; it was left
  with the fixture conversion work.
- The transport-port shrink removed the coordinator's failed-exchange cache
  invalidation. A restarted voter's cached QUIC connection can still look open;
  retries reused it until the call returned `Unavailable`. The unchanged e2e
  test failed once and passed four times; parent `7e3aa644` passed four times.
  Iroh now marks cached outbound connections non-reusable when a stream drops
  with pending or failed I/O, resetting only that stream. A deterministic
  two-stream regression cancels one exchange, dials a replacement connection
  and completes the healthy exchange on the retired connection. The fencing
  assertions are unchanged.
- `cargo test -p rocky-coordinator -p rocky-voter` passes all 79 tests, and the
  configured voter e2e selection passes all six tests. The restart test also
  passes five consecutive nextest e2e stress iterations. All 25 iroh tests pass.
  Voter/coordinator/iroh native all-target clippy and coordinator/iroh browser
  library clippy pass. `scripts/lint` passes; `scripts/check` stops at 23 concurrent
  SDK lint errors, including unused imports and needless borrows.
- Two slot tests pass: typed absence while SQL works, and idempotent claims
  after reopening with an application-owned coordinator. The client test also
  verifies independent node authority and observers across bindings.
- `scripts/browser-sync` passes in headless Chromium: relay sync, authorized
  claims/retries, denied unrelated claims and OPFS reopen using the same
  application coordinator. Database shutdown does not close that transport.
- Peer launcher and todo app/desktop all-target checks pass. `scripts/lint`
  passes, including the tightened native/browser SDK dependency boundary.
- The requested SDK all-target check and SDK service e2e entries cannot build
  until the fourteen `SqlValue` fixture errors above are resolved. Full native
  and wasm checks stop at the concurrent SDK lints above; they do not qualify
  the rest of the workspace or personal sign-in acceptance.

## Phase 0 — Resolve authority and allocation prerequisites

Status: wide writer/row identities implemented (decision 1 = A); personal
mode, independently addable personal certificates and paged membership remain
open.

- **Pin personal mode in root-signed genesis metadata.** Add an immutable
  `DatabaseKind::Personal { owner: PublicKey, namespace: PersonalNamespace }`
  alongside `Shared` in the single current manifest format. Verify the derived
  publisher and owner locally; a request flag cannot disable shared linking
  confirmation. `Settings::create` already selects the root-derived owner and
  rejects equality with the root, but has no signed personal-mode discriminator
  and generates random mesh/domain IDs ([enrollment.rs:1449][create]). Reuse
  `SignedManifest::sign/verify` ([signed.rs:369][manifest]). Personal mode permits
  only owner devices plus explicitly granted custody principals; it does not
  invite other readers, writers or issuers.
- **Independent devices need independent certificates (open).** Writers no
  longer depend on the membership sequence, but `MembershipSet` still retains a
  same-sequence fork and voids affected links, including descendants
  ([identities.rs:75][certify], [membership.rs:62][membership]), so ordinary
  sign-in through it could still invalidate another device. New personal
  certificates must be independently addable, indexed by `(database, owner,
  node public key)`, with a permanent node tombstone dominating every replay.
  Bind the owner grant, descriptor digest and node possession; no shared
  `previous`/sequence allocator. Shared linking retains its existing semantics.
- **Implemented: wide writer and row identities.** A writer is its node's full
  Ed25519 public key (`WriterId`, 32 bytes); nothing allocates, reserves,
  truncates or hashes author numbers.
  - `RowIdentity { writer, sequence }` pairs the writer with the node's own
    monotonic creation sequence (1..=`i64::MAX`, never wrapping). Its one
    canonical form is 40 bytes: the key, then the big-endian sequence, so
    byte order is identity order.
  - Text and JSON use 80 lowercase hex digits; binary encodings use the bytes.
  - In SQL, `id` is `BLOB NOT NULL`, generated by every definition. Columns
    that reference an `id`, and partition columns of a group keyed by `id`,
    must be `blob`. Captured application rows require a 40-byte identity.
    Partition SQL accepts `X'…'` blob literals as partition values.
  - Apps receive `RowIdentity` from `next_id`/`next_partition_id`, bind it
    through `params!`/`IntoSqlValue` and read it with `row.get::<RowIdentity>`.
    `SessionInfo::writer` names the node's writer.
  - Stamps order by clock, then writer key, then sequence. A frame carries
    `authored: bool`; its writer is derived from the signing key, and every
    birth, creation proof, identity mark, receipt and seal frontier must name
    its signer. Local dense integers remain only storage indexes.
- **Removed:** `AuthorRange`, `MAX_AUTHOR` and the `RECOVERY_*` slots; the
  credential extension's and author binding's author ranges;
  `author_number`/`reserve_author`; the reserved node numbers; minting, granted
  ranges and the author high-water mark in enrollment settings, claims, terms
  and registry capabilities; `GrantTerms::authors`; the recovery slot in
  `RootRecovery` and `AlertKind::RecoveriesMet`; and the replica's stamp-author
  and node-author maps. A recovered node or a renewed leaf writes under its own
  key, so nothing it creates can collide.
- **One current format; older ones are refused.** Bumped together:
  - logical schema 7, row format 17, replica file `v59`, checkpoint 20
  - credential extension 12, author binding 4, seal 3, existence base 2
  - receipts 2, move proofs 3, migration publication 2
  - enrollment settings 33, enrollment answer 6
  - labels `rocky.row-creation-statement.v3` and `rocky.row-incarnation.v3`
  Integer or decimal-text identities, older persisted formats and older
  credential versions are rejected, with no migration or compatibility reader.
- The current 32-node membership bound cannot become an automatic eviction rule
  ([membership.rs:6][member-limit]). New personal membership must page
  historical certificates/tombstones and impose explicit resource limits without
  ending existing devices. A strict globally enforced live-device count would
  itself require coordination; do not promise it from independent offline
  additions.
- Evidence (2026-11-15, together with the owner reset): `./scripts/lint`,
  `./scripts/check --wasm` (81/81 browser runs) and `./scripts/check --all`
  (workspace unit 328, integration 1255, e2e 70; browser 81/81) pass. In the
  `rocky-sdk` no-default-features e2e tier,
  `rollback::a_compacted_member_waits_for_the_owners_kept_rollback_seal` failed
  once ("node author credential missing" during its 257 retention writes) and
  passed three serial reruns; the cause is not yet known. A 32-scope
  publication now carries ~735 more signed bytes after its first edit (most
  likely the wider writer keys), so it needs 10 instead of 9 link messages.

Acceptance: two devices sign in against the same old snapshot, author before
meeting, then merge in both orders: both memberships and row identities survive;
neither creates revocation/fork evidence. A cloned node key with conflicting log
history is still detected. Shared links still require both matching codes.
Wrong owner, mode downgrade, namespace substitution and tombstoned-node replay
fail without changing stored authority. Quota refusal never removes a sibling.

## Phase 1 — Keys, locator, typed API and current formats

Status: pending Phase 0; credentials/core and SDK work.

The app uses a stable, versioned `PersonalNamespace` containing RP ID and its
application/account-root purpose. Its deterministic WebAuthn PRF evaluation input
must be reproducible on an empty device; changing RP ID, input or selected
credential changes the database. The app obtains a discoverable credential with
`userVerification = required`, verifies challenge/origin/RP binding and actual
PRF output, then passes a non-cloneable, non-serializable, redacted,
zeroizing `VerifiedPasskeyPrf` into Rocky. Rocky cannot prove platform UV from
32 bytes; this is the trusted platform adapter's contract. Never substitute a
WebAuthn signature, credential ID, PIN or password for unavailable PRF. Today's
`IdentityProvider` supplies signatures only ([identity.rs:73][provider]); the
PRF adapter and secret-lifetime handling are new.

Derive a stable bootstrap topic by HKDF from the **identity secret**, with a new
`rocky.personal.discovery.v1` label and canonical namespace. Do not hash the
public identity. Derive separate deterministic `MeshId`/`DomainId` with distinct
labels; use the existing publisher/application-name `AppId` calculation to form
the first-frame `Scope` ([manifest.rs:69][app-id], [domain.rs:48][scope]). This
finds one personal root database per namespace; other databases are application
registry entries. Do not derive data keys from the PRF: retain random data-key
epochs and root escrow as in `Settings::create` ([enrollment.rs:1475][new-keys]).

Persist only the topic capability, node/transport secrets, reader epochs and
authenticated metadata needed to reopen. Root, identity secret and PRF remain
in memory for the interactive attempt and are erased on completion,
cancellation or failure; reopening uses node keys alone, as today
([configuration.rs:190][open], [keys.rs:74][vault-roles]). A keyless custodian
receives the topic capability during explicit custody enrollment, never root,
identity or data-key secrets. Reuse `Finder`'s sealed rendezvous/local claims;
current SDK topics require an admitted discovery root and revocation-dependent
values, so they cannot bootstrap a fresh passkey-only device
([mesh.rs:335][topics], [node/discovery.rs:17][announce],
[discovery.rs:76][rendezvous]). Keep shared topic rotation; personal bootstrap
topic stability is a deliberate privacy tradeoff, not authorization.

Proposed Rust surface (all newly named types; no JSON calls or serialized schema
arguments). Follow `Configuration`'s async lifecycle; use transport/coordinator
types supplied by their refactors, rather than inventing parallel abstractions
([configuration.rs:17][configuration], [database.rs:32][options]):

```rust
impl Configuration {
    pub async fn create_personal(
        &self, prf: VerifiedPasskeyPrf, namespace: PersonalNamespace,
    ) -> Result<PersonalOpened, PersonalSignInError>;
    pub async fn sign_in(
        &self, prf: VerifiedPasskeyPrf, namespace: PersonalNamespace,
        source: SignInSource,
    ) -> Result<PersonalOpened, PersonalSignInError>;
}
pub enum SignInSource {
    Discover,
    Backup(VerifiedBackup), // offline ciphertext, same non-destructive install
}
pub struct PersonalOpened {
    pub opened: Opened,
    pub restored: RestoreCoverage, // available/missing partitions and epochs
    pub freshness: Freshness,     // LocallyAnchored or Unwitnessed
}
impl Session {
    pub async fn revoke_personal_device(
        &self, node: PeerId, prf: VerifiedPasskeyPrf,
    ) -> Result<DeviceRevocationReceipt, PersonalSignInError>;
    pub async fn set_partition_custody(
        &self, scope: ResourceScope, policy: PartitionCustody,
    ) -> Result<(), Failure>;
}
```

`Configuration` takes a typed `Schema` after the concurrent JSON-surface removal.
`PersonalSignInError` distinguishes PRF unsupported, wrong local account,
source unavailable, unsupported format, genesis conflict, missing escrow,
capacity and revoked node. `SourceUnavailable` is not “no database exists” and
never creates an empty replacement. An explicit first create refuses existing
local state. Independent explicit first creates may conflict because current
creation generates keys/genesis locally; deterministic routing is not a
distributed create-once proof. Detect differing authenticated genesis descriptors
as `GenesisConflict`, preserve both stores and never select one by arrival order
([enrollment.rs:1485][new-keys], [self_enrollment.rs:50][bundle-scope]).

Wire/persistence: extend the one current signed manifest/capability format with
personal mode, owner, namespace binding and independent node certificates;
replace the affected writer/row identity representations in Phase 0. Add typed
bootstrap request/response and durable install intent (node keys, descriptor
digest, stage, imported epochs, publication ID); include descriptor, membership,
tombstones, escrow inventory and local verified heads in backups. Bump affected
settings, credential extension, row wire and backup versions together with their
owners; reject old versions, with no legacy readers, migrations or dual
encodings. Current anchors are `Settings::VERSION = 32`, extension 11, row 16,
backup 2 and shared self-enrollment `Bundle` 6
([enrollment.rs:44][settings-format], [extension.rs:14][authors],
[wire.rs:45][wire], [backup.rs:137][backup], [self_enrollment.rs:138][bundle]).
Use the transport refactor's bounded canonical binary codec; root escrow's
existing cryptographic envelope can remain, but encode its new payload and
database context in the new current format. No JSON protocol wrapper.

Acceptance: fixed PRF/namespace restores the same distinct root/identity and
locator on an empty device; other namespaces and credentials differ. Verify
existing root HKDF vectors. A recording secret store and serialized records
contain no PRF/root/identity secret. Unsupported PRF creates no files or network
messages. Resume interrupted imports under the same node and publication ID;
wrong-account reopen does not touch data. Old-format inputs and conflicting
genesis are explicit failures, preserving source ciphertext.

## Phase 2 — Transport bootstrap and non-destructive sign-in

Status: pending typed `PeerRequest`, scoped endpoint dispatch and Phase 1.

Replace `Node::serve`'s Renew-prefix dispatch with the planned typed variants
([node.rs:3153][serve]). `rocky/rpc` carries RPC, `rocky/link` carries authenticated
sync, and coordinator has its separate ALPN. Their single current frame version
replaces today's Pair/Renew ALPNs ([net.rs:112][protocol]). One application-owned
endpoint routes N databases by `Scope` in the first frame, then verifies that
same scope in every signature/credential, reply and link transcript. The scope
is routing input, never trust. Unknown scopes receive bounded refusals; a frame
cannot switch databases. Detaching one database must leave sibling attachments,
the endpoint/router and app-owned coordinator usable, preserving the existing
`Attachment` ownership contract ([iroh/lib.rs:312][attachment]).

| Request/path | Sign-in use and required trust |
| --- | --- |
| `Resolve` | Optional endpoint-to-node resolution; bounded untrusted challenge, signed responder proof; conveys no DB authority. Reuse resolution currently dispatched in `Node::serve`. |
| `Identity` | Required personal bootstrap. New personal branch carries descriptor-bound root authorization, identity signature and new-node possession over scope, audience and challenge/exchange. It can run before membership is known. Any authorized owner sibling **or custodian** answers with the stored signed descriptor/schema/bootstrap, owner grant, membership/policy and opaque root escrow pages. A custodian neither signs a grant nor returns plaintext keys. Existing `IdentityRequest` already binds database/audience/node/exchange with both signatures ([enroll.rs:617][identity-request]); extend its payload rather than accepting a bare identity claim. Shared branch keeps matching codes. |
| `Link-enroll` | Required publication of the independent owner-certified node and root authorization; signed bootstrap trust plus live node possession, not previously enrolled-peer trust. Idempotent insert by certificate ID after pinned personal-mode/owner checks. Extend `accept_link`, which today only accepts the responder's own identity and sequential head ([node/membership.rs:154][accept-link]); a custodian must also accept and relay owner certificates. |
| `Policy` | Required signed revocation/tombstone exchange, also included in bootstrap. Bounded untrusted reads may return verifiable public statements, never secrets; personal private metadata requires the bootstrap proof. Relayed updates need valid database-scoped authority, even from a revoked sender. Signatures prove authenticity, not completeness/freshness. Today's bare policy response is in `Node::serve`. |
| `TipSchema` | Optional if bootstrap already supplies it. Public schemas remain bounded public reads; personal/private schema retrieval requires bootstrap or live peer proof. Authenticate schema against signed genesis and tip history, as `Bundle::verify_database` does ([self_enrollment.rs:50][bundle-scope]). |
| `Epochs` | Not needed for initial personal sign-in: root opens opaque escrow carried with ciphertext. Existing plaintext epoch delivery requires a signed renewing credential and issuer ([node.rs:3221][epochs]); do not relax it for a custodian or possession of the topic. |
| `Pair`, `Renew` | No initial personal sign-in use. Preserve invitation-token/approval trust for Pair and signed, scoped existing-credential trust for Renew; routine renewal after installation can use surviving issuers. |
| `Unlink` | Used by Phase 4, not sign-in. Accept independently verifiable owner/root revocation even if the sender has lost live trust; never unauthenticated mutation ([node/membership.rs:114][accept-unlink]). |
| `StagedClose` | No sign-in use. Preserve its signed close-evidence authorization; no extra privilege from personal bootstrap. |
| `rocky/link` | Required data sync after installing the signed bootstrap and node certificate. Mutual node possession, valid owner/read or custody grants, scoped revocation checks. Carry opaque escrow with frames; no root secret on wire ([network/escrow.rs:1][carry]). |

Bootstrap requires a root-signed authorization, not just an identity signature:
this proves access to root material, prevents the shared identity-only path from
becoming a no-code bypass, and lets a different-identity custodian answer.
The initial bounded `Identity` challenge supplies only the root-signed personal
descriptor header, responder proof and nonce; validate the derived root/owner
before signing its digest. Release private bootstrap pages only after that
proof. Persist the certificate before sending; bind RPC requests to audience,
scope, challenge and exchange, with bounded replay state. A fresh nonce never
resurrects a tombstoned node; request retries reuse the certificate/publication
ID. Transport authentication alone is insufficient ([enroll.rs:644][request-verify],
[revocation.rs:1034][node-revocation]).

Include unscoped partition-naming-root escrow in the authenticated bootstrap
inventory. `Escrowed::Naming` is sealed locally with `Slot = None`, whereas
today's frame carry enumerates only `(ResourceScope, KeyEpoch)` records; it
cannot deliver that root to a custodian ([escrow.rs:21][escrow-payload],
[network/escrow.rs:14][escrow-slot]). This needs an opaque, vouched metadata-escrow
record bound to the descriptor. Never replace a missing naming root silently:
restoring existing partitions and retaining authority to name future ones are
separate coverage claims.

### End-to-end flows

1. **First device creates.** App evaluates PRF once with UV. Derive both keys and
   locator; explicitly create/pin personal descriptor, random node/transport and
   random data epochs. Root signs genesis/owner authority, identity certifies
   the node. Keep root escrow, including partition naming material, before
   advertising recoverable data. Persist local bootstrap/topic and attach scoped
   service; explicitly enroll any custodian and sync selected partitions. Drop
   root/identity/PRF. Reuse `Settings::create`, `escrow::keep` and custody-only
   validation, with the Phase 0 replacements ([create], [escrow-keep], [custody]).
2. **Second device, passkey only.** Derive the same locator and root/owner; save
   fresh node/transport intent. Discover hints, authenticate responder, fetch
   personal `Identity` bootstrap, verify genesis against derived root and pinned
   namespace/owner, merge policy, independently certify/publish node via
   `Link-enroll`. Sync ciphertext and escrow on `rocky/link`; unseal/validate each
   imported epoch locally and replay. Return coverage/freshness; existing nodes
   receive Phase 4's durable notice after commit. There is no invitation,
   matching code, existing-device confirmation or recovery revocation. Replace
   the peer-secret `Bundle` installation path with escrow import, not a shortcut
   around its database checks ([link], [bundle-scope], [escrow-open]).
3. **Siblings offline; only keyless custodian reachable.** Same flow; its durable
   descriptor, owner authority, membership, root escrow and retained ciphertext
   are sufficient. It persists/relays the new certificate; no enrollment
   upstream or issuer must be online. Current `CustodyPeer` merely holds
   custody grants and can forward enrollment, so bootstrap storage/serving and
   custody-credential renewal are concrete gaps ([custody], [serve]). Authenticate
   custodian membership independently of an expired transport session: root can
   renew the *same pinned custody-only grant* for a discovered node proving its
   key, scoped to partitions already authorized, or the app supplies renewed
   custody credentials. Never silently elevate rights or require a full reader.
   Registry restores if held; chats restore only for ciphertext actually present.
4. **All devices lost, passkey kept.** Same non-destructive sign-in to a surviving
   custodian, or `SignInSource::Backup` without networking. Preserve all old
   devices' trust: a later-returning device still works unless explicitly
   revoked. Recoverable state is exactly the authenticated ciphertext/checkpoint
   plus all required epochs and schema/bootstrap material the source retained.
   Escrow alone is not data; the passkey alone is not a backup. With no reachable
   source, return `SourceUnavailable`; with permanently lost ciphertext, that
   data is unrecoverable. Missing escrow is an explicit gap, never a newly
   generated replacement key. `Recovered::open` already checks every held epoch;
   factor that validation into online/offline import without `Settings::recover`
   or its fallback-key/authority behavior ([escrow-open], [recover-settings]).
5. **Device offline with its existing store.** Opening needs no prompt or
   coordinator and reads locally held data; it cannot discover missing ciphertext
   while disconnected. A fresh offline device can restore a supplied backup;
   PRF without ciphertext yields an unavailable sign-in, not an empty success
   ([open], [options]). Revocation flow is Phase 4.

No coordinator call is required for deriving keys, finding the personal root,
self-certification, policy verification or ciphertext/escrow sync. Use the
application's explicit optional coordinator slot for operations whose schema
already needs coordination; no embedded/default coordinator, credential
directory, allocation service or recovery oracle. `rocky_credentials::Coordinator`
is the typed application slot: it extends the core `DomainLog` protocol with
node binding, credential renewal, named claims, reads and revocation push.
`Configuration::coordination`, `Engine::open_composed` and custody configuration
accept an application-owned `Arc<dyn Coordinator>`. Applications construct the
voter adapter with `rocky_coordinator::Client::connect`; tenant administration
lives in `rocky_coordinator::control::ControlClient`. The SDK has no `iroh`
feature and no normal dependency on either coordinator or iroh clients.
Missing coordination returns typed `CoordinatorNotConfigured` for counter calls.
Sign-in succeeds with that slot absent;
coordinated writes may subsequently remain blocked. Never run external calls
inside a DB transaction or close application infrastructure on cancellation.

Acceptance: complete flows 1–5 with deterministic in-memory discovery and fault
injection, asserting no recovery statement, sibling revocation or data-key
rotation. Repeat bootstrap after dropped replies and restarts: one node, one
certificate, one notice. Custodian-only restore works with every full node and
coordinator absent, including an expired custody credential without broadening
rights. Wrong scope/root, revoked node, forged responder, invalid schema or
changed descriptor is refused. Two databases share one endpoint; detaching one
leaves the other and the app coordinator working. Crash at every install stage
leaves resumable state, never a partially ready database; publish only committed
metadata. Existing recovery tests remain destructive by design, separately from
personal sign-in ([recovery-tests]).

## Phase 3 — Partition custody and retention

Status: pending; may proceed alongside transport once typed policy is agreed.

Keep `ReplicaRole::{Full, Partial}` as local holding behavior. Replace global
`RoleConfig.full_nodes/custody` with explicit policies per `ResourceScope`
(partition identity), using typed peer sets, counts and signed coverage. Today's
configuration is replica-wide and counts **applied** full nodes, so merely adding
a keyless peer does not satisfy it ([roles.rs:21][roles],
[checkpoint/prune.rs:218][prune]). New `PartitionCustody` names eligible storage
peers and required durable ciphertext copies; registry can require one keyless
custodian, chats zero. Custody grants/selectors bound what that peer receives;
reuse existing partition offers without leaking other partitions
([partition.rs:743][offers]). App data and retention rules stay app-owned.

For a keyless custodian, count a signed durable receipt covering authenticated
frames/checkpoints **and associated root escrow/bootstrap dependencies**, not
application of plaintext. Retain a separate applied frontier for reducer/replay
requirements. A received network packet or a transport ACK is not a custody
receipt. Quotas, grant expiry/revocation and missing escrow prevent counting the
peer; a signed receipt still cannot prove a malicious custodian will keep data.
Reuse the escrow carry/vouch mechanism and replace applied-only coverage where
it represents durability ([carry], [evict.rs:74][coverage]).

`custody = 0` removes the custody prerequisite: pruning follows retention and
existing causal/checkpoint/required-peer constraints, with no implicit full-node
dependency. It does not disable retention's own copy rules or declare ciphertext
recoverable. Current pruning already permits custody zero, but eviction,
unsubscribe and tip-drop explicitly use `max(1)`; resolve all of them against
the partition policy ([prune], [evict.rs:45][evict], [roles.rs:951][unsubscribe],
[tip_drop.rs:118][tip-drop]). Explicit scope eviction at zero can discard the
last local copy; report that consequence and require the app's explicit eviction
call. Do not automatically evict an active chat or treat zero as a deletion
instruction. Preserve retained dependencies, restoration guards and resumable
eviction; zero only removes the custody veto.

Acceptance: registry and chat partitions in one database use different policies;
registry pruning/eviction blocks until a keyless custodian's complete durable
receipt, while custody-zero chat retention advances without any full node.
Fault after ciphertext but before escrow/receipt commit: no custody credit;
restart completes safely. Forged/replayed receipts, another partition's coverage,
and revoked custodians do not count. Reopening preserves policies. Custodian
storage contains no reader keys/plaintext and restores exactly its retained
partitions; explicitly evicting a last custody-zero copy reports loss accurately.

## Phase 4 — New-device notice and precise revocation

Status: pending independent certificates and typed SDK events.

Reuse durable `TrustedObservation`/`TrustedNodeLinked` and `TrustedNodeUnlinked`
for the owner identity ([membership.rs:145][observation],
[status.rs:426][notice]). Existing devices show “new device signed in” once per
new certificate/node after local commit, also when learned through a custodian
after offline periods. Retain read/acknowledged notice state; request replay,
sync retry, credential renewal and restart do not manufacture events. Deduplicate
by `(Scope, owner, node)`, independently of renewed certificate IDs.
Names/timestamps supplied by
the new device are untrusted; display a stable device fingerprint and local
first-observed time. This is a replicated in-app notice, not guaranteed OS push
while the app is closed.

**One-tap Revoke uses `Lost` and targets only the named node.** The tap invokes
the passkey/UV adapter if no fresh interactive authority is in memory, derives
the identity and signs a permanent owner unlink with the current held lane cut.
Reuse `Session::unlink`/`Node::unlink`: it already uses `RevocationMode::Lost`,
keeps sibling membership and triggers exposure rotation; replace its string node
argument with `PeerId` ([session.rs:286][unlink-api],
[node/membership.rs:5][unlink]). One tap means no matching-code/approval ceremony,
not an exemption from a platform UV prompt. Publish a durable receipt containing
the signed statement ID and local/pending peer coverage, then erase signers.
Rotate exposed reader/group epochs, escrow them to the root, stop the node's
live sessions and deliver unlink evidence even to revoked peers. Sign-in itself
never performs this rotation. Keep the held tip cut; later statements past it
are refused, preserving current unlink/revocation invariants ([unlink],
[accept-unlink], [revocation.rs:58][revocation]).

`Lost` and `Compromised` are **trusted-identity cascade policies**, not two
degrees of node unlink. `Lost` ends the targeted trusted identity but lets its
first enrollees renew elsewhere; `Compromised` is a root-only action also ending
identities first enrolled by it, even if another issuer later renewed them
([revocation.rs:29][modes], [revocation.rs:366][cascade]). For a stolen unlocked
personal device without the passkey, use the node-scoped Lost action: every
device shares the owner's identity, so revoking that identity would affect all
siblings. For a compromised delegated trusted identity in a shared database,
keep a separate explicit root-authorized Compromised action, never the notice's
default. Do not relabel a node's Lost unlink Compromised: current credentials
reject non-root Compromised statements ([revocation.rs:222][mode-check]).

If **the passkey or exported PRF/root is compromised**, neither mode can exclude
its holder: they can certify a fresh node and decrypt future root escrow.
Revoke affected platform credentials and move to a new root/identity/topic and
freshly encrypted personal database through an explicit incident flow. Removing
a provider's credential alone cannot invalidate an already exported root.
Historical plaintext/ciphertext already decrypted is not retractable. Do not
claim a single node revoke repairs root compromise ([owner], [escrow-open]).

`RevocationReason` remains the verified cause, not the chosen mode: personal
unlink reports `Node`; direct identity revocation `Identity`; a compromised
origin `Cascade { trusted }`; destructive root recovery `Recovery { recovered }`
([status.rs:124][reason]). Replaying a certificate after unlink never restores
that node. A legitimate passkey holder can sign in again with a new node; that
is the approved authority model, not a revocation bypass.

Acceptance: notice is durable across offline delivery/restarts and appears once;
Lost unlink ends exactly the selected owner node, rotates exposed epochs once,
preserves sibling writes/seals and rejects replays. Interrupted revoke resumes
with the same statement and epochs. Lost versus Compromised shared-identity tests
preserve/revoke origin enrollees respectively, and verify the four status causes.
Repeated revoke returns the same outcome; no root/identity secret persists.

### Owner reset — "This is everything: sign out everywhere else"

Status: implemented on the current shared/root model on 2026-11-15; the
personal sign-in flow that offers it (`Freshness::Unwitnessed`) is Phase 1/2.

After an `Unwitnessed` restore the device cannot list devices it never heard
of, so the action is an allow-list plus a data fence in one root statement.
It generalizes destructive recovery: `Revocation.recovery` is a `RootDecision`,
either `Grants(RootRecovery)` or `Reset(Reset)`, in the same set, merge, alert
and chain checks. Personal mode signs it with the passkey-derived root; no
matching code.

- **Statement.** Root-signed, mode `Lost`, naming no nodes, identities or
  capabilities. `Reset { follows, keep, fence }`: `keep` lists the signer
  node and named custodians (`PeerId`, at most 64); `follows` lists the
  reset heads the signer held; `fence` lists, per `ResourceScope` and
  writer node public key, `next`, the first unapplied position of that
  writer's per-scope log. Canonical order; never zero, never a kept writer;
  at most 2,048 logs and 192 KiB. The `cut` keeps held tip-statement heads
  of ended lanes (at most 32). IDs `rocky.revocation.id.v6`, signatures
  `rocky.revocation.v9`, replicated authority format 5; others are refused.
- **Budget.** Root resets are permanent, so the held resets together encode
  to at most 1 MiB (`MAX_RESET_SET_BYTES`) inside the 4 MiB set: at least
  3 MiB always stays for Lost, Compromised and other statements. A reset
  past the budget is refused before anything is recorded. That is about
  five resets with full 2,048-log fences over a database's life; a reset
  with an empty fence is under 1 KiB.
- **Who it ends, without a shared sequence.** Reset R ends node n when n is
  not in `R.keep` and no held reset that transitively follows R keeps n.
  This includes nodes R's signer never saw. A node signed in later must be
  kept by a following reset. Phase 0/1 independent certificates can then
  cite the head reset instead. Node links (`extended.node`) end; identity
  and trusted grants stay. Links an ended node signed stay too: a chain
  judges its signing nodes by the set without resets, so the signer or a
  custodian enrolled through a device the reset ends keeps its chain. This
  is safe because every node such a link can admit is ended unless `keep`
  names its key. Naming the node in a Lost or Compromised statement still
  ends what it signed. Statements an ended node signs (tip statements,
  endorsements) are refused as before. Sessions, renewal, frames and tip
  statements use the existing chain checks (`ends_at`, `ends_node`,
  `revoking`).
- **Fence.** For an operation of writer w at log position p in scope s:
  `Live` if no reset ends w; `Covered` if every reset ending w has
  `p < next(s, w)`; otherwise `Fenced`. Unlisted logs keep nothing. Covered
  operations are judged by the set without resets, so later operations,
  hidden earlier ones and those of never-seen nodes are refused on every
  replica. Survivors' operations are unaffected. The replica takes the fence
  from its applied frontier and merges the statement under one lock
  (`EncryptedReplica::install_reset`), so the signer never holds an applied
  operation outside its own fence. Under that lock the signer reads its
  follows and custodian standing from the replica's set and checks its own
  chain against the set with the reset merged. If the reset would end its
  own chain, nothing is recorded and the call fails with `ResetEndsSigner`.
- **Merge rules.** Statements merge by union, so results ignore order. Lost
  and Compromised keep their semantics and dominate: a covered operation of
  a node a Lost/Compromised statement ends is still refused, and keeping a
  node in a later reset never revives what they end. Concurrent resets do not
  follow each other, so each ends the other's devices (both signers, user
  signs in again); fences only shrink, because every applicable reset must
  cover. When a following reset admits a node, its stored rejected frames
  return to pending and are judged again, so replicas converge. A frame
  refused on receipt was never stored and must be sent again. A replica
  merges none of the resets that end it, however many arrive together. It
  reports the first, and its owner records that one. The handshake pages
  statements by encoded size, at least one per page. A 192 KiB reset
  therefore always fits a 256 KiB frame, and several large resets cross in
  separate pages.
- **SDK.** `Session::sign_out_everywhere_else(root, &custodians) ->
  ResetReceipt { reset, kept, follows, fenced_logs, pushed }`. It refuses,
  recording nothing, with `CallFailure` codes:
  - `device_signed_out`: the statement in `revoked.json` or the replica's
    set ended this device.
  - `custodian_signed_out`: a named custodian is already ended.
  - `reset_ends_signer`: the reset would end this device's own chain.

  `Session::freshness() -> Freshness` (`LocallyAnchored`, `Unwitnessed`,
  `FencedByOwner { reset }`) reads the replica's set, and reports
  `LocallyAnchored` on an ended device. Ended devices report
  `RevocationReason::Reset { reset }` and record
  `ActivityKind::NodeReset { node, reset }`. `RestoreChoice::ALL` gives
  "Wait for my other devices" (default) and "This is everything: sign out
  everywhere else" with the discard warning.
- **Limits.** A replica that applied an uncovered operation before learning
  the reset keeps it. Only ended devices can be in that state while kept
  custodians are keyless and the signer is the only kept full reader. The
  SDK does not yet verify custodians are custody-only. Every replica a reset
  does not keep is ended itself, so a full replica that must keep syncing has
  to be named in `keep`. A device joining after a reset is ended (never seen)
  until a following reset keeps it, and the SDK invite flow does not yet sign
  one; a following reset's signer must hold what it fences, because a log it
  never applied is discarded. The signer checks only custodians' standing,
  not their chains, which it does not hold. Coordinator push of a large
  reset statement is not separately qualified.

Acceptance (deterministic, no timing):

- Credentials (`reset/tests.rs`): a never-seen node is ended; standings
  Live/Covered/Fenced. Concurrent resets end both signers in either order; a
  following reset admits a new node in three merge orders. Lost dominates the
  fence and is not revived, with one digest over all six orders. Forged,
  tampered, non-root, Compromised-mode, target-naming, malformed-fence and
  format-4 inputs are refused. A laptop identity enrolled by a phone node
  the reset ends keeps a valid chain, which a Lost statement naming the
  phone still ends. Resets fill their budget, and a Lost and a Compromised
  statement still merge. **Measured in tree 2026-11-15: 6/6 pass.**
- Secure (`tests/reset.rs`): covered frames apply; a hidden frame is refused
  with the reset first, and rejected after it when it waited for its
  predecessor; later frames are refused; custodian and signer frames sync.
  Concurrent resets end both signers on a kept third replica. A frame
  stored as rejected under them is `Rejected`, and is pending again after
  the following reset with no resend. It applies once its predecessor
  arrives. Two resets that each end a replica, sent in one session, are
  both left unmerged. Three 2,048-log resets, together over one frame,
  cross a handshake. A reset that would end its signer records nothing.
  **Measured: 5/5 pass.** Without the fixes, the paging and
  self-ending tests fail.
  The whole `rocky-secure` suite passes (612, 5 ignored).
- SDK (`tests/owner_reset.rs`): the owner fences a synced laptop write and
  discards an unsynced one. The laptop reports `RevocationReason::Reset`,
  records `NodeReset` and is refused with `device_signed_out`. A reset
  keeping it as a custodian is refused with `custodian_signed_out`.
  Freshness moves to `FencedByOwner`. A second reset follows the first. A
  laptop joined through the owner's invite resets, keeps working and is
  `FencedByOwner`; the root-signed owner it ended is refused with
  `device_signed_out`. Without the link rule that laptop is refused with
  `reset_ends_signer` before anything is saved. **Measured: 2/2 pass in
  5/5 consecutive runs;** `rocky-sdk` lib tests pass (237, 22 ignored).

## Phase 5 — Security and release qualification

Status: review complete as a design proposal; mitigations/tests unimplemented.

| Threat | Required mitigation and residual risk |
| --- | --- |
| Stolen unlocked device | Hardware-backed app `SecretStore`, screen/app locking, no persisted passkey/root/identity. Attacker can read held plaintext and act with the node's existing authority until revocation is learned. Personal mode must reject issuing authority to other identities; node keys cannot mint new owner devices. File storage is plaintext at rest today ([keys.rs:22][file-store]); Lost unlink plus epoch rotation cannot erase copied history. |
| Stolen passkey or compromised app/JS | Require real UV, RP/origin/challenge checks and short-lived zeroized PRF; no matching code can constrain the root holder. Synced-passkey/cloud-account compromise and platform recovery are in the trust base. A compromised origin/process can export the PRF. Root replacement is the only exclusion remedy; document Phase 4's limit. |
| Malicious custodian | End-to-end encrypted frames, root escrow/vouches, signed descriptors/policy and bounded decoding; verify tags, epoch commitment, scopes and retained checkpoint/history before replay ([escrow-open], [carry]). Can withhold/delete ciphertext, lie about coverage or deny discovery. Independent custodians/backups improve availability; a receipt is not proof of future retention. It sees encrypted data sizes, activity and authorized partition-offer tuples ([offers]). |
| Network attacker | Topic/address is never admission. Bind RPC and live-link proofs to scope, audience, node possession and nonce/exchange; authenticate reply schema/history; enforce quotas before expensive work ([identity-request], [request-verify], [bundle-scope]). Confidentiality/authenticity hold under the existing crypto assumptions; availability and traffic-analysis resistance are not guaranteed. |
| Topic privacy/linkability | Derive from identity **secret**, separated by namespace. Public-key hashing would let anyone knowing the identity compute the topic. A stable rendezvous key is linkable to the discovery service; custodians and revoked devices knowing the capability can keep tracking/poisoning it because it derives the rendezvous signing key too ([rendezvous]). Never log topic/PRF; limit advertisements and allow explicit peers/LAN-only configuration. Rotation that excludes holders requires fresh secret distribution or a new root; no claim of anonymous or unlinkable discovery. |
| Self-certification replay/concurrency | Independent certificates, node-possession challenges and permanent tombstones; deduplicate publication and notice IDs. Bind signed personal mode/owner/descriptor and merge evidence in every order. Keep node log counters durable; a restored old live node key must not author a reused sequence. Existing membership-fork logic cannot be reused for personal additions ([membership], [certify]). |
| Custodian rollback/omission | Persist verified policy, per-node log heads, tip/checkpoint anchors and epoch commitments; reject regression against local anchors, gossip across independent sources and preserve conflicting evidence. Signatures cannot prove a virgin device received the latest state. With all devices lost and one custodian, return `Freshness::Unwitnessed` and precise coverage, never “fully up to date”; withheld revocations may temporarily admit a node until evidence arrives. Strong freshness needs an independent durable witness/anchor, optionally app-supplied; today's `DomainLog` is not automatically such a recovery witness ([domain-log], [cascade]). |
| PRF unavailable/inconsistent across platforms | Qualify the exact native/browser/authenticator combinations for stable PRF output across devices and restores. A synced credential's availability does not by itself prove its PRF works on every client. Fail with `PrfUnavailable`, retaining local state; no silent downgrade to destructive recovery or a server password. An additional independently generated passkey normally yields a different root; multi-passkey wrapping/recovery would be a separate design ([root], [provider]). |

Release acceptance: run deterministic native and browser scenarios for all flows,
concurrent sign-ins, revocation delivery orders, stale/forged/missing bootstrap
and escrow, interrupted installs/rotations, credential expiry and cross-database
routing. Exercise a real supported passkey/PRF adapter on the promised platform
matrix; fake PRF bytes only prove Rocky's derivation. Use in-memory transport
faults and fixed clocks/barriers, never sleeps or latency budgets. Existing
custody/root-recovery tests establish reusable ciphertext behavior, not this new
protocol ([custody-tests], [recovery-tests]). Implementation phases must record
measured evidence here and pass `./scripts/lint`, `./scripts/check`, supported
no-network/no-default SDK and wasm checks; no blanket `--all-features`.

Design verification: README/AGENTS and workspace metadata inspected; cited source
paths/functions reviewed; all Markdown source references resolve to existing
files and lines. `./scripts/lint` and `./scripts/check` were attempted on
2026-11-15; both failed on Rust formatting differences in the concurrent SDK
edits, including `tests/support/snapshot.rs`. No source was formatted or edited
by this design task. No acceptance test above has been implemented or run for
this feature. Baseline checks do not establish passkey portability, storage
durability, a desktop UX or distributed security.

## Decisions

1. **Decided: A.** Wider writer/row identities (full node public key plus a
   per-node sequence, Phase 0) instead of integer row IDs with an online
   author allocator.
2. **Decided: A, plus the owner reset.** Restoring with all devices lost is
   authenticated but may be stale, reported as `Freshness::Unwitnessed`, with
   no required recovery witness. The app offers "Wait for my other devices"
   (default) or "This is everything: sign out everywhere else" (Phase 4 owner
   reset), which ends every unlisted node and discards their data outside the
   fence.

[root]: ../crates/rocky-credentials/src/root.rs#L22
[owner]: ../crates/rocky-credentials/src/root.rs#L40
[vault]: ../crates/rocky-credentials/src/keys.rs#L12
[vault-roles]: ../crates/rocky-credentials/src/keys.rs#L74
[file-store]: ../crates/rocky-credentials/src/keys.rs#L22
[provider]: ../crates/rocky-credentials/src/identity.rs#L73
[recover]: ../crates/rocky-sdk/src/configuration.rs#L99
[recover-settings]: ../crates/rocky-sdk/src/enrollment.rs#L1518
[recover-revoke]: ../crates/rocky-sdk/src/enrollment.rs#L1738
[create]: ../crates/rocky-sdk/src/enrollment.rs#L1449
[new-keys]: ../crates/rocky-sdk/src/enrollment.rs#L1475
[manifest]: ../crates/rocky-credentials/src/signed.rs#L369
[certify]: ../crates/rocky-sdk/src/identities.rs#L75
[membership]: ../crates/rocky-credentials/src/membership.rs#L62
[member-limit]: ../crates/rocky-credentials/src/membership.rs#L6
[authors]: ../crates/rocky-credentials/src/extension.rs#L5
[app-id]: ../crates/rocky-core/src/manifest.rs#L69
[scope]: ../crates/rocky-sdk/src/domain.rs#L48
[topics]: ../crates/rocky-sdk/src/mesh.rs#L335
[announce]: ../crates/rocky-sdk/src/node/discovery.rs#L17
[rendezvous]: ../crates/rocky-iroh/src/discovery.rs#L76
[configuration]: ../crates/rocky-sdk/src/configuration.rs#L17
[options]: ../crates/rocky-sdk/src/database.rs#L32
[open]: ../crates/rocky-sdk/src/configuration.rs#L190
[settings-format]: ../crates/rocky-sdk/src/enrollment.rs#L44
[wire]: ../crates/rocky-secure/src/wire.rs#L45
[backup]: ../crates/rocky-sdk/src/backup.rs#L137
[link]: ../crates/rocky-sdk/src/self_enrollment.rs#L190
[code]: ../crates/rocky-credentials/src/enroll.rs#L678
[bundle]: ../crates/rocky-sdk/src/self_enrollment.rs#L138
[bundle-scope]: ../crates/rocky-sdk/src/self_enrollment.rs#L50
[identity-request]: ../crates/rocky-credentials/src/enroll.rs#L617
[request-verify]: ../crates/rocky-credentials/src/enroll.rs#L644
[serve]: ../crates/rocky-sdk/src/node.rs#L3153
[protocol]: ../crates/rocky-core/src/net.rs#L112
[attachment]: ../crates/rocky-iroh/src/lib.rs#L312
[accept-link]: ../crates/rocky-sdk/src/node/membership.rs#L154
[accept-unlink]: ../crates/rocky-sdk/src/node/membership.rs#L114
[epochs]: ../crates/rocky-sdk/src/node.rs#L3221
[node-revocation]: ../crates/rocky-credentials/src/revocation.rs#L1034
[escrow-keep]: ../crates/rocky-sdk/src/escrow.rs#L119
[escrow-open]: ../crates/rocky-sdk/src/escrow.rs#L145
[escrow-payload]: ../crates/rocky-sdk/src/escrow.rs#L21
[escrow-slot]: ../crates/rocky-secure/src/network/escrow.rs#L14
[carry]: ../crates/rocky-secure/src/network/escrow.rs#L1
[custody]: ../crates/rocky-sdk/src/custody.rs#L26
[coordination]: ../crates/rocky-credentials/src/coordinator.rs#L9
[domain-log]: ../crates/rocky-core/src/coordinated.rs#L123
[roles]: ../crates/rocky-secure/src/replica/roles.rs#L21
[prune]: ../crates/rocky-secure/src/replica/checkpoint/prune.rs#L218
[offers]: ../crates/rocky-secure/src/replica/partition.rs#L743
[coverage]: ../crates/rocky-secure/src/replica/evict.rs#L74
[evict]: ../crates/rocky-secure/src/replica/evict.rs#L45
[unsubscribe]: ../crates/rocky-secure/src/replica/roles.rs#L951
[tip-drop]: ../crates/rocky-secure/src/replica/tip_drop.rs#L118
[observation]: ../crates/rocky-sdk/src/membership.rs#L145
[notice]: ../crates/rocky-sdk/src/status.rs#L426
[unlink-api]: ../crates/rocky-sdk/src/session.rs#L286
[unlink]: ../crates/rocky-sdk/src/node/membership.rs#L5
[revocation]: ../crates/rocky-credentials/src/revocation.rs#L58
[modes]: ../crates/rocky-credentials/src/revocation.rs#L29
[cascade]: ../crates/rocky-credentials/src/revocation.rs#L366
[mode-check]: ../crates/rocky-credentials/src/revocation.rs#L222
[reason]: ../crates/rocky-sdk/src/status.rs#L124
[custody-tests]: ../crates/rocky-sdk/tests/integration/custody.rs
[recovery-tests]: ../crates/rocky-sdk/tests/integration/root_recovery.rs
