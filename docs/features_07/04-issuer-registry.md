# 4. Replicated issuer registry

Status: not started.
Owner: `rocky-sdk` enrollment and identity; `rocky-credentials` grant
profiles; Kotlin `devices` surface. No dependency. Wave 0.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [equal issuers](scope-mesh.md#1-equal-issuers),
[enrollment](../runtime/sdk-enrollment.md), its
[issuer recovery](../runtime/sdk-enrollment.md#issuer-recovery) and
[formats](../runtime/credentials.md#formats), and
`crates/rocky-sdk/src/enrollment.rs`.

Each issuer keeps `authority.devices`, partition holders and pending rotation
in its own `identity.json`. Another issuer knows a device only if it renewed
it. Make the registry one replicated collection that issuers alone read, and
let every issuer act from it.

## Ordered slices

1. Decision record: where the registry lives and who reads it. It is an
   SDK-owned collection of the domain, sealed under a key only issuers hold,
   synced by the ordinary encrypted path and absent from application schemas
   and grants. A keyless custody peer may hold it sealed.
2. Record: per device its node key, user key, capability ID, profile,
   enrolling issuer, recorded partition grants and delegations, and a list of
   pseudonyms per table. Records merge per device; a revoked flag only ever
   becomes set. Author ranges stay disjoint per issuer.
3. Writes: enrollment, renewal, `grant`, partition grants and delegations
   write the registry durably before the chain is returned, as partition
   holders are recorded today.
4. Issuer admission: pairing a device as an issuer delivers the registry key
   with the publisher key. Rotating it follows the revocation of an issuer.
5. Any-issuer renewal from the registry: the answering issuer needs no earlier
   contact with the device. `remove_peer` no longer refuses the last issuer;
   a member with no issuer contact reports that in `peers` and readiness.
6. `identity.json` drops `authority.devices` and partition holders; the Kotlin
   `devices()` call reads the registry.

## Completion gate

- A device enrolled at A is listed at B after one sync, with the same profile
  and partition grants.
- B renews a device it never met, A being stopped; the device then removes A.
- A member and a non-issuing writer hold no registry key and open no registry
  row; a custody peer relays it without reading it.
- Two issuers enrolling at once produce disjoint author ranges and both
  records.
- A crash between the registry write and the returned chain leaves a record
  and no usable chain, never the reverse.
- The registry of 1,000 devices syncs and reopens within a recorded bound.
- [Enrollment](../runtime/sdk-enrollment.md) and
  [credentials](../runtime/credentials.md) describe it.

## Not in this phase

Revoking from the registry ([phase 5](05-mergeable-revocations.md)), rotating
pseudonyms and showing the registry to members.
