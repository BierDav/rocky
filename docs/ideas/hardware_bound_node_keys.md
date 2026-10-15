# Hardware-bound node keys

Status: idea; not planned. Builds on identities and node certificates of
[phase 13](../features_09/13-identities-and-domain-credentials.md).

## Problem

A node key held in software can be copied with the device's storage. Two
copies then write as one node, which Rocky can at best
[detect after the fact](../optimizations_on_demand/cloned-node-detection.md).
A copied key also outlives the device it was meant for.

## Idea

A node generates its key inside the platform's secure hardware, where it
cannot be exported, and proves that once with the platform's key
attestation. A trusted node verifies the attestation and signs a statement
that the node key is hardware-bound. Copies of the device's storage then
hold no usable key, and clones are impossible rather than detected.

## Enrollment

1. The node creates a non-exportable key in secure hardware and obtains the
   platform's attestation chain for it.
2. It sends the chain to a trusted node, with a challenge the trusted node
   issued so the attestation is fresh.
3. The trusted node verifies the chain up to a manufacturer root it holds and
   checks the attested properties: non-exportable, hardware-backed, the
   application's identity and signing certificate, and a verified boot state
   where the platform reports one.
4. It signs an **attestation statement** binding the node key to those
   properties. The node's identity signs its node certificate as usual; the
   statement travels with the chain.

Attestation happens once per node key. Other devices verify the trusted
node's statement, never the manufacturer chain, so only trusted nodes need
the roots.

## Manufacturer roots

Trusted nodes hold every manufacturer root they accept:

| Platform | Mechanism | Root |
| --- | --- | --- |
| Android | Keystore key attestation, TEE or StrongBox | Google's hardware attestation roots |
| Apple | App Attest with Secure Enclave keys | Apple App Attestation root |
| Windows and Linux | TPM 2.0 keys certified by the endorsement key | Each TPM vendor's endorsement roots |
| Security keys | FIDO attestation | Vendor roots from the FIDO metadata service |

The root set is versioned configuration of the trusted node, updated as
manufacturers rotate roots and publish revoked attestation keys. A trusted
node refuses an attestation it cannot verify; it never accepts one on trust.

## Policy

Grants may require an attested node key, for example for trusted nodes and
issuers. Nodes without secure hardware, such as most servers and VMs, stay
unattested and can only receive grants that do not require it.

The attested security level is part of the policy. A TEE-backed key is
non-exportable and enough to prevent clones; that is the default
requirement. A secure element such as StrongBox also resists physical
attacks on the device and is slower; a policy may require it for the
owner's or issuers' nodes only.

## One key per node

[Phase 13](../features_09/13-identities-and-domain-credentials.md) removes
pseudonyms, so a node signs everything with its one node key and only that
key needs secure hardware.

## Signing in batches

Hardware signing takes milliseconds per signature. Today a commit signs
every frame of at most 32 rows and every existence record, one per created
or deleted row, inside the commit. Two changes make the cost independent
of row count and keep it off the commit path:

- **One signature per batch.** The node signs one Merkle root over every
  frame and existence record of a batch. Each carries its inclusion proof,
  so it still verifies alone; frames travel independently and readers
  often cannot open the other members of a bundle.
- **Signing after commit.** A commit writes projection, outbox and an
  unsigned marker atomically. A signer then signs everything pending in the
  outbox under one root and marks it ready; a crash in between signs on
  reopen. Nothing is published unsigned, so publication still follows the
  local commit, only later by one signature.

A 100,000-row migration into a logged table then costs a handful of
signatures instead of about 103,000. Coordinated-log acknowledgments still
cost one signature per entry and trusted node, in parallel, within the
log's 60 entries per minute.

## Open questions

- Secure hardware mostly signs P-256, not Ed25519: node certificates need
  to name the node key's algorithm, and every verifier must support both.
- Batch signing changes frame, existence-record and checkpoint formats
  once more; it is worth it only when node keys move into hardware, since
  software signatures take microseconds.
- Attestation reveals the device model and, on some platforms, unique
  identifiers; what the attestation statement carries beyond the verified
  properties.
- Whether a trusted node rechecks attestation revocation lists after
  enrollment, and what happens to a node whose attestation key the
  manufacturer later revokes.
