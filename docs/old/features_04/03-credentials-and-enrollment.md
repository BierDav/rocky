# 3. Independent credentials and enrollment

Status: implemented 2026-10-06 and qualified on loopback only; see
[decision 0023](../decisions/0023-credentials-profiles-and-key-epochs.md),
[credentials](../runtime/credentials.md) and [SDK enrollment](../runtime/sdk-enrollment.md)
for the result and its [limits](../runtime/credentials.md#limits). Owner: `rocky-credentials`,
secure admission and enrollment service/bindings. Depends on phases 1–2. Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [permissions/sync](../runtime/permissions-and-sync.md), [SDK enrollment](../runtime/sdk-enrollment.md),
[secure sessions](../runtime/secure-sessions.md), [Directory](../applications/directory/registration.md)
and [decision 0018](../decisions/0018-secure-transaction-bundles.md).
Inspect signed credential/manifest/policy types and every admission caller.

Extract reusable verification/issuance and identity/key-provider contracts from
database/network ownership. Keep mandatory enforcement at every protected runtime
boundary. Enrollment is optional plumbing over those contracts, not the only
way to install externally supplied valid credentials.

## Ordered slices

1. Move credential logic into the reusable module with compatibility re-exports
   where practical. Preserve encoded signatures, attenuation, epochs, expiry,
   manifest binding and paired row/field rights. Share key types without cycles.
2. Define explicit login principal, signing device, transport endpoint, publisher,
   reader scope and coordinator-control identities. An external login assertion
   alone cannot grant row access, scope keys or executor eligibility.
3. Support caller-supplied verified chains and key providers plus a separate
   invitation enrollment/renewal service. Invitations pin authority, bind the
   requested grant profile and retain retry identities; no default full-rights
   grant for every enrollment. Provide at least reader and scoped writer profiles.
4. Make issuer/publisher ownership recoverable through delegated issuer rights
   ([resolved choices](resolved-choices.md)); no cloned device author logs. Renewal need
   not depend on the original database creator remaining the sync hub.
5. Preserve session generation invalidation and commit linearization on learned
   revocation, expiry, renewal, logout and key eviction. Specify reader-set/key
   rotation: distribute fresh keys to remaining readers for future protected
   writes, retain needed historical keys under policy, and handle interrupted
   rotations without widening access or rewriting original signed bytes.

Whole-row read encryption boundaries remain until a separately accepted design
changes them. A key reveals data encrypted under it; subscription filters do not
enforce narrower cryptographic rights. Previously decrypted data is not retractable.
No login-provider or platform keystore implementation is required in this phase,
but adapter contracts and the file-store fallback's threat model must be explicit.

## Completion gate

- Credentials verify without opening a database or endpoint; a secure runtime
  enforces them regardless of the transport/issuer used.
- Scoped readers/writers cannot escalate through SQL, commands, renewals or
  result transfer. Device enrollment grants neither executor nor schema authority.
- Prepare/step/commit/cursor races with learned policy changes deny further
  unauthorized work and publish no provisional result.
- Renewal through an authorized issuer and publisher recovery retain identities
  correctly; deleted/missing secrets never silently generate replacement keys.
- Revoked devices cannot decrypt fresh rotated-scope ciphertext acquired elsewhere;
  crash/retry of rotation preserves old recovery evidence and current access.
- Offline latest-learned-policy limits and local plaintext-at-rest are documented.

Run credential, secure, enrollment and fault tests plus `./scripts/check`;
native/Kotlin checks when bindings change. Update current security/session guides.
