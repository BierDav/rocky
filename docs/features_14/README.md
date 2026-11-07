 # features_14: identities hold access

Status: implementation in progress (2026-11-07). Phases 1, 2, 4 and 5a are
integrated. Phase 3 database-scoped membership is implemented and verified:
the full workspace gate passes native tests, supported feature checks,
browser tests and browser/native sync. See its evidence for measured results
and limits. Phase 5b SDK linking/discovery is implemented and verified; see its
[evidence](05-admission-and-discovery-evidence.md) for the passing native/browser
gate and unavailable external Paperless integration. Phase 6 is implemented and
verified; see its [evidence](06-trusted-many-nodes-evidence.md) for the passing
native/browser gate and its limits. Paperless integration remains outstanding
external consumer work, so features_14 consumer completion is not claimed.
Phase briefs record their own status; evidence goes beside each brief as
`NN-<name>-evidence.md`. Source code and tests define current behavior.

## Destination

An **identity** is the only principal. Issuers grant it permissions and
revoke it; the root trusts it and revokes that trust. An identity's key
works like the root's: a 24-word phrase or a passkey, supplied for one
call and never stored by Rocky.

The same linkable identity may hold access to several databases. Each database
independently grants permissions, links nodes and manages trust and revocation.
Using an identity in one database grants nothing in another.

A **node** is a key an identity linked to itself. It holds no rights of
its own: it acts with its identity's rights for as long as the identity
keeps it linked. One identity may link many nodes, all trusted when the
identity is, or each node may have an identity of its own. That choice is
made when the node is admitted, not when the invitation is written.

A **discovered device** is a node this node has seen: one asking to join,
or one certified by some identity and met through discovery, peers or
sync. It is something observed, not an authority record.

Two authorities, never mixed:

| Who | Decides | Signs with |
| --- | --- | --- |
| Root | which identities are trusted | root phrase or passkey |
| Issuer (a node of an issuer identity) | which identities exist and what they may do | its node key, under its identity's authority |
| Identity | which nodes are its own | identity phrase or passkey |

No issuer certifies or unlinks another identity's nodes. No identity
widens its own permissions.

## Why this shape

- **Devices are the wrong principal.** The registry keys records by node
  (`registry::Record`). Profile, partition grants and label hang off the
  device, and "users" are only a grouping of device records. A person
  with three devices has three sets of permissions to keep in step.
- **The credential chain already has the right layers.** Root → identity
  → node certificate exists in `rocky-credentials`. Trusted grants already
  name identities. What is device-shaped is the SDK, the registry and the
  calls above it.
- **One identity key per node today.** A stored identity "is its only
  node" (`identities.rs`). Several nodes per identity need an
  application-supplied `IdentityProvider` on every node, so in practice
  each device is its own user.
- **The identity key is in the hot path.** Seals, bookmark moves, tip
  openings, capabilities, revocations and renewal answers are signed by
  the identity (`node/tips.rs` `Signer`, `Authority::issuer`). A node
  certificate takes its capability's expiry (`CredentialChain::certify`),
  so each renewal needs a new identity signature. A phrase or passkey
  cannot stay offline until routine signing moves to node keys
  (phase 1).
- **Revoking a whole identity for one lost laptop costs too much.** It
  re-grants everything, re-admits every other node and, for a trusted
  identity, needs the offline root again. Unlinking that one node, signed
  by the identity, keeps the rest.

## Phases

| # | Phase | Delivers |
| --- | --- | --- |
| 1 | [Node-signed authority](01-node-signed-authority.md) | routine statements signed by node keys; node certificates independent of capability expiry |
| 2 | [Identity keys](02-identity-keys.md) | `IdentityKey` with phrase or passkey; single-use keys for identities with one node |
| 3 | [Membership](03-membership.md) | database-bound link and unlink statements, author numbers from each database's membership sequence, forks as equivocation within that database |
| 4 | [Identity registry](04-identity-registry.md) | registry and calls keyed by identity; no node records |
| 5 | [Admission and discovery](05-admission-and-discovery.md) | join requests as discovered devices, verified admission, approve-first invitations |
| 6 | [Trusted identities with many nodes](06-trusted-many-nodes.md) | trust and staleness per identity, alerts when a trusted identity links a node, lane cuts over every node |

1 → 2 → 3 is the critical path. 4 can start after 1. 5 needs 3 and 4. 6
needs 3.

The proposed [bounded live publication follow-up](../features_15/README.md)
is tracked separately in features_15 against phases 3 and 5a.

## Goals

| Measure | Baseline | Target | Phase |
| --- | --- | --- | --- |
| Identity signatures during routine operation (seal, renew, grant, revoke) | every one | none | 1 |
| Identity signatures per added node | provider on the new node | one, at linking | 3 |
| Nodes per identity, stored default | 1 | 32 (`MAX_CUT_LANES`) | 3 |
| Lost node of a multi-node identity | revoke identity, re-admit others; root for trusted | one identity-signed unlink; others untouched | 3 |
| Principal the registry keys on | node | identity | 4 |
| Calls naming a node as an authority target | `revoke {node}`, `device_label` | none | 4 |
| Admission without matching codes | possible | refused | 5 |
| Root signatures to add a trusted node | none (supplied provider) or a new identity plus root | none | 6 |

Sync results from features_13 must not regress; this roadmap does not touch
row payloads.

## Settled

These resolve the questions raised while planning. Phases don't reopen
them. A phase that finds one unworkable stops and reports the evidence
instead of choosing differently.

### Principals

- **Only identities hold permissions.** Capabilities, partition grants,
  claim rights and trusted grants name identities. A node certificate
  carries no grants; verifiers take the rights of the identity it names.
- **One identity, independent databases.** A supplied `IdentityProvider` may
  represent the same identity in several databases. Permissions, registry
  records, trust, membership heads, author numbers, node bounds and revocations
  stay local to each database. Revoking access in one does not revoke another.
- **Issuers revoke identities, never nodes.** `Revoke::Node` is gone from
  issuer authority. A revocation signed by an issuer names identities and
  capabilities only.
- **Identities unlink their own nodes.** An unlink is a revocation signed
  by the identity naming only nodes it linked. Verifiers refuse an
  identity-signed revocation naming any other node or any identity.
  Unlinking rotates the reader keys of every scope the identity reads,
  because the unlinked node held them.
- **Identity revocation ends every node.** Unchanged: naming an identity
  ends every chain through it.

### Identity keys

- **Like the root.** `IdentityKey` is a 32-byte secret from a 24-word
  phrase or a passkey PRF output, the same encodings as `RootKey`.
  `IdentityProvider` stays the port; an application signer (hardware key,
  server HSM) implements it as today. `FileIdentityProvider` stays for
  tests and servers.
- **Never stored.** The SDK's `StoredIdentity` is removed. An identity key
  is in memory only for the call that needs it: create, link, unlink or
  install an issued capability.
- **Single-use identities.** An identity meant for one node gets a fresh
  key that signs that node's certificate and is then discarded. It can
  never link or unlink a node. If the node is lost, an issuer revokes the
  identity. This is the default for a device admitted as a new identity,
  so most devices never show a phrase.
- **Linkable identities.** An identity whose holder wants several nodes
  shows its phrase once, or binds a passkey, when it is created. Without
  the phrase or passkey it cannot link more nodes. Its existing nodes keep
  working, and an issuer can still revoke the identity.
- **The owner uses one secret.** `Configuration::create` derives the
  owner's identity key from the root secret with a domain-separated
  derivation (`"rocky identity v1"`), so one phrase or passkey covers both.
  Root and owner identity were already one person's to lose; the trusted
  grant is still a separate root signature, so revoking the owner identity
  still works. An owner who wants them separate passes an
  `IdentityProvider` explicitly.
  An application reusing a personal identity across databases supplies that
  identity explicitly and keeps each database's root authority separate.

### Nodes

- **Membership is bound to the database.** Signed certificates and unlinks
  identify the database context, including its pinned publisher root and
  stable mesh/domain identifiers. Enrollment and authority-set verification
  reject foreign context before membership, fork or revocation state changes.
  A matching identity signature alone is insufficient. The binding survives
  schema changes and does not depend on a local path or bookmark.
- **Node certificates do not expire.** A certificate is valid until its
  identity unlinks the node or the identity is revoked. The identity's
  capability expires and is renewed; the certificate does not need
  re-signing. Verification is the intersection: capability chain valid at
  the time, certificate not unlinked.
- **Nodes sign routine statements.** Seals, tip openings, bookmark moves,
  checkpoint records, capabilities, revocations and renewal answers are
  signed by the node key. The endorsement names the identity as the
  authority and the node as the lane, matching the existing
  `Cut { identity, lane }`.
- **Author numbers come from the membership sequence.** The `n`th link of
  an identity takes `range.first + n`. Linking needs the identity's
  current membership head. Two links at one sequence are an equivocation:
  both certificates are void and the identity's other nodes raise an
  alert. The holder links again. Linking is rare and interactive, so this
  costs nothing in practice.
- **At most 32 linked nodes per identity per database**, equal to
  `MAX_CUT_LANES`, so a root cut of a trusted identity always covers every
  lane. Unlinked nodes free their slot but not their author number.

### Admission

- **A new device becomes a new single-use identity** unless it is linked.
  An application may instead supply an existing linkable identity for admission
  to a different database; that database's issuer must still grant it access.
- **Approval is the default.** `Session::invitation()` carries no profile;
  a device answering it becomes a pending discovered device until an
  issuer admits it with a profile. `Session::invitation_with(profile)` is
  the pre-approved shortcut for headless servers and tests; a device
  answering it is admitted at once as a new identity.
- **Linking does not go through an issuer.** On the new device the holder
  enters the phrase or uses the passkey. The new node fetches the
  identity's bundle and membership head from a node of that identity
  (today's `enroll_identity` exchange). The identity signs the
  certificate there. No issuer, no invitation.
- **Codes are compared before admitting.** Admission and linking show a
  6-digit code derived from both nodes' keys and the request nonce on both
  screens. The admitting call takes the code the user confirmed, and a
  mismatch is refused.
- **Pending requests are bounded.** At most 32 per issuer node, each
  expiring with its invitation (10 minutes by default). The oldest is
  evicted at the bound.

### Trust

- **Trust stays per identity.** `trust(identity)` and
  `revoke_trusted(identity)` keep their meaning. Every linked node of a
  trusted identity is a trusted node.
- **Adding a trusted node needs no root.** Instead, every trusted node
  records the link and raises `AlertKind::TrustedNodeLinked` with the
  identity, node and time, so an unexpected trusted node is seen.
- **Staleness is per identity.** `TrustedNodeStale` becomes
  `TrustedIdentityStale`: an identity is stale when none of its linked
  nodes has been heard from. The coordinator's trusted list already holds
  identities.

### Formats

- **No compatibility.** Capability, certificate, revocation, registry and
  enrollment formats each get one new version; old ones are refused.
  Example and test databases are recreated. Consumers (Paperless) move on
  the next release.

## Non-goals

- **Application accounts and onboarding.** Applications own passkey ceremonies
  and stable identity derivation, QR/deep links, database selection, and an
  encrypted personal directory of database IDs, pinned roots and connection
  hints. Directory bootstrap, synchronization and recovery are application
  concerns. Rocky accepts the supplied identity and verifies database-local
  authority; it provides no global account service or database directory.
- **Groups of identities.** A team or role identity is an identity whose
  holder links the members' devices. A delegation graph between identities
  is not part of this roadmap.
- **Recovering a lost identity phrase.** The holder's nodes keep working;
  an issuer revokes the identity and admits a new one. Escrowing the
  identity key the way the root escrow works is a later question.
- **Per-node permissions.** An application that needs a node with less
  access gives it its own identity.
- **Login providers.** `ExternalLogin` stays an input an issuer may
  consider; it is not an identity.

## Consumers

Paperless (`paperless-core`, `paperless-node`, `src/pages/DatabaseSettings.tsx`)
replaces its Devices and Users tabs with identities (their nodes nested
under them) and a waiting-to-join list. It drops `revoke {node}` and
`device_label`, shares book keys per identity, and replaces its
owner-approval join loop (`native/join_approval.rs`) with phase 5's
pending requests.
