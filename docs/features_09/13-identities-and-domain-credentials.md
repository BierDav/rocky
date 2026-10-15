# 13. Identities and domain credentials

Status: proposed; not started.
Owner: `rocky-credentials` chains, grants and revocations; `rocky-secure`
admission; `rocky-coordinator` control certificates; `rocky-sdk` and
`sdks/kotlin` enrollment, adoption and identity port. Depends on
[phase 10](10-stuck-recovery.md)'s implemented cuts, whose SDK and Kotlin
surface [phase 19](19-recovery-as-a-bookmark-move.md) supersedes, and
[phase 11](11-removing-strong-commands.md).
Follow the [execution contract](README.md#agent-execution-contract). Takes
decision record 0075.

## Inputs and objective

Read [the generation model](README.md#generations-as-sealed-forks),
[credentials](../runtime/credentials.md),
[revocations](../runtime/revocations.md),
[SDK enrollment](../runtime/sdk-enrollment.md),
[adopting generations](../runtime/adopting-generations.md) and the
[one identity idea](../ideas/03_one_identity_and_passkeys.md).

Today permissions belong to devices, issuers copy the publisher key, a
stolen issuer can only be excluded by a new domain, and the publisher key
has no backup. Credentials also name a generation's manifest, so every
schema change needs an issuer to renew every device. After this phase
permissions belong to identities, each node signs under its own certified
key, the publisher key never leaves its owner, and a device moves to
another generation without contacting anyone. The passkey module is not
part of this phase; its interface is. A cloned node key stays a
[deferred optimization](../optimizations_on_demand/cloned-node-detection.md);
[hardware-bound node keys](../ideas/hardware_bound_node_keys.md) would
prevent it.

## Settled

- **Identities.** An identity is a signing key that an `IdentityProvider`
  supplies. Grants, issuer authority, trusted-node rights and the owner's
  authority name identities. Core never sees how an identity is stored or
  derived; a passkey module will be one provider.
- **Node certificates.** Each node generates its own node key. Its identity
  signs a node certificate that adds no rights. A chain runs publisher →
  issuer identities → identity → node. Author logs, existence logs and
  coordinated-log membership stay per node.
- **Author ranges per identity.** A grant gives an identity a range of
  author numbers; each node certificate carries one number from it. Row
  identities derive from the node's number, so no node needs an issuer to
  create rows.
- **Pseudonyms are removed.** The node key signs every frame, existence
  record and checkpoint of the node. Pseudonym requests and assignments,
  sealed device bindings, their issuer-registry records and per-table
  signing keys go. Frames and existence logs name the node to every device
  that can see them; an application that needs complete isolation uses
  separate domains.
- **Self-enrollment.** A node of an identity that already holds grants
  enrolls by obtaining a node certificate from its provider; no issuer and
  no invitation is involved. Invitations grant to an identity.
- **The provider signs; it does not export.** `IdentityProvider` exposes
  the public key and signing of node certificates, revocations and the
  statements the identity's authority requires. It never returns the
  private key.
- **The publisher key belongs to the owner's identity.** The domain is the
  owner identity's key and the application. Issuers sign manifests, seals
  and bookmark moves through their own chains; no issuer, invitation or
  pair request carries the publisher key any more. A provider that derives
  the owner's key, such as a passkey, is therefore its backup, and a
  revoked issuer is fully excluded.
- **Revocation.** Revoking an identity ends every chain through it, so
  every node of that identity at once. Revoking one node certificate ends
  only that node.
- **Trusted nodes by identity.** The coordinator's control certificate names
  trusted identities; a node presents its certificate. The in-sync set
  still counts nodes.
- **Grants name tables and fields.** Names are identities
  ([phase 1](01-stable-identities.md)), so a grant holds in every
  generation. Reader keys follow key epochs only; a generation never
  rotates them.
- **No renewal on adoption.** A device verifies a publisher-rooted
  generation and adopts it on its own. `generation_pending` and the renewal
  step of adoption are removed.
- **One format version.** Credential, chain, revocation, invitation,
  frame, existence-record and checkpoint formats move to their next version; earlier versions are refused.

## Ordered slices

1. Decision record 0075: generations as sealed forks, covering phases 13–19,
   with identities, node certificates and the removal of pseudonyms.
2. Identities, node certificates and the `IdentityProvider` port, with a
   file-backed provider for tests and servers.
3. The publisher key held only by the owner's identity; issuers sign
   through chains.
4. Revocation by identity and by node; control certificates naming
   identities.
5. Author ranges per identity; removing pseudonyms, with frames, existence
   records and checkpoints signed by the node key.
6. Credentials, grants and reader keys without a manifest; adoption without
   renewal.
7. Guides: credentials, credential primitives, revocations, issuer
   registry, existence logs, SDK enrollment, adopting generations; the one
   identity, pseudonym rotation and partitioned existence log ideas.

## Completion gate

- A second node of an enrolled identity joins without an issuer, writes
  every table its identity is granted under its own node key and author
  number, and creates rows that never collide with the first node's.
- No pseudonym key, request, assignment or registry record remains in code
  or formats.
- Revoking an identity stops all three of its nodes; revoking one node
  certificate stops only that node.
- A revoked issuer can no longer sign a manifest, seal or bookmark move, and
  no device it paired holds the publisher key.
- A member adopts a newer generation while no issuer is reachable and keeps
  writing every table it is granted.
- An invitation created before a schema change enrolls a device after it.
