# Durable delegation provenance

Prerequisite: [one identity](../one_identity.md)

Status: idea, not implemented or scheduled. Phase 7 uses precise registry
selection for recorded devices and full rotation for unrecorded devices;
see [the phase brief](../features_07/07-registry-revocation.md).

## Problem

A non-issuer can delegate a narrower credential chain to an offline device.
That delegation is not recorded in the issuer registry. An administrator
who later has only the recipient's node key cannot recover its delegator
from a capability hash, delegation depth or enrolling issuer key.

In an ERP, Alice's registered laptop reads Sales and Inventory and delegates
Inventory access to a warehouse tablet. If the tablet is lost before it
contacts an issuer, Bob cannot identify Alice as its delegator. Today's
fallback rotates every held collection key and every known partition secret
held by the revoking issuer, including Payroll and Purchasing.

## Idea

Persist authenticated delegation provenance, available to every issuer, so
`revoke(node)` can resolve a recipient to its recorded delegator without a
caller supplying a receipt. Evidence must bind the recipient node to its
delegating principal and connect that principal to a registered device;
a bare claimed node-to-node link is insufficient.

In the ERP example, Alice's delegation would durably publish evidence of
Tablet -> Alice's laptop. Bob could then select Alice's recorded Sales and
Inventory scopes while leaving Payroll and Purchasing unchanged. Alice
would remain authorized. This is the former phase 7 delegator-selection
rule; selecting only the tablet's narrower grants is a separate choice.

Store sufficient signed evidence to validate the relationship and its
history, with explicit bounds and deterministic merge rules. Keep the
registry issuer-readable: members must not receive a replicated roster or
learn other devices' grants. A non-issuer needs an authenticated submission
path; giving it the registry reader key is not acceptable.

## Contracts that would change

- [Phase 5](../features_07/05-issuer-registry.md)'s final record fields and
  [decision 0048](../decisions/0048-replicated-issuer-registry.md)'s record,
  ordering, merge and size contracts.
- The roadmap's rule that only phase 5 owns the registry format.
- The exclusion of non-issuer delegation recording, and the revocation
  selection and acceptance rules in phase 7.
- Native/Kotlin delegation publication and retry behavior, including when
  delegated credentials may become usable.

Any adopted format has one current version, with no legacy readers,
migrations or dual encodings. This idea does not change `identity.json`.

## Open decisions

- Does delegation require a durable issuer acknowledgement before use, or
  work offline and publish later? Until evidence is available, revocation
  must retain the full-rotation fallback or report an explicit error.
- Who submits evidence when the recipient has never connected, and how is
  the recipient node bound before it completes its user-to-node chain?
- How are multiple delegators, renewed chains, distinct devices sharing a
  user principal and multi-hop delegations resolved?
- Does revocation select the recipient's grants, the delegator's recorded
  access, or their union? Key possession can exceed a presented grant.
- How is evidence retained after capability expiry or replacement, and
  bounded without forgetting access relevant to revocation?
- Revoking a delegator can end descendants' chains through the shared
  principal; whether to enumerate descendants and rotate their scopes needs
  a separate cascade contract.

## Qualification before adoption

Exercise offline delegation, durable publication, lost acknowledgements,
issuer sync, invalid signatures, conflicting provenance and restart at each
persistence boundary. An issuer with only the recipient node must resolve
verified ancestry once published; one without the evidence must explicitly
follow the chosen fallback. Never infer successful resolution from the fact
that some registered member was allowed to delegate.
