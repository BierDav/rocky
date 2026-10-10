# 0062. Node references and credential signing aliases

Status: implemented and qualified by Phase 20.

A member must retain routes, acknowledgments and verifiable checkpoint and command
proofs without retaining another installation's public node key. Removing the
application roster alone leaves those keys in SDK metadata and proof chains.

## Durable references

`PeerId` is the full `rocky.secure-node.v1` commitment of a node public key.
Its first 16 bytes are the existing command `NodeId`. A reference cannot sign,
decrypt or reconstruct a public key. Local contacts, issuer routing flags,
frontier, operation and command-result acknowledgments, horizon participants, role and retention
configuration, and outgoing delivery bookkeeping retain this reference.

Contacts retain their transport routes and local metadata. Public keys are
resolved in memory after restart. Before opening a native session, a peer answers
a fresh nonce with its node key and a signature. The initiator verifies both the
signature and the saved commitment. This exchange grants no database session,
reveals no credential chain, and changes no admission or grant rules. Unreachable
references retain their routes and retry with bounded backoff. An issuer reached
through custody is rediscovered through the existing authenticated contact request.
The same nonce proof can travel through a custody enrollment proxy, and schema
upgrade resolves its saved issuer routes before fetching a generation. Transport
keys remain separate from node keys.

## Credential signing aliases

The SDK creates a separate credential signing key. The user-signed node leaf
names that public alias and binds it to `PeerId` in its signed extension. Native
session authentication still proves possession of the actual node private key;
credential verification checks its commitment against the leaf's binding.
Cached chains therefore carry aliases and references rather than node keys.

Horizon acknowledgments carry the participant reference and the alias-signed
credential chain. Verification pins the publisher and manifest, checks the chain
and its reference binding, then checks the acknowledgment signature. The issuer's
participant expiry and the existing exact-cut checks continue to bound acceptance.

Typed checkpoints sign with the registered table pseudonym and its assigned author
credential. Verification checks the table, partition selectors, whole-scope read
and write authority, signed digest and frontier. Read-only participants still
import a writer's checkpoint; custody still stores only authenticated sealed pages.
Tables without a registered table pseudonym use the credential alias, with the
same publisher, node-reference binding and scope authority checks. Initial
bootstrap skips an older checkpoint made by its own signer; finalized horizon
recovery continues to accept its authenticated cut.

Strong command submissions and confirmed results sign with the credential alias.
Their durable node/executor references retain the existing command identities;
chain, grant, generation, coordinator receipt and row-package checks remain.
Node revocations name references. SDK revocation co-signatures use the credential
alias; the publisher signature continues to authorize the revocation.

Local shopping labels use a separate one-way label reference. Labels remain local
and are never replicated.

## Current formats

Only the current formats are accepted: SDK identity 16, replica 14, native session
14, credential extension 3, horizon 5 and checkpoint 9. No migration, legacy reader,
dual encoding or negotiated fallback is added. New proof formats and one-way
references are substantive protocol changes, not privacy-audit exclusions.

The full member-directory audit remains a literal byte search. Its keys, paths,
filters and budgets are unchanged. See [Phase 20 evidence](../features_07/20-shopping-without-a-roster-evidence.md).
