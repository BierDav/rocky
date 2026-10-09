# 13. Relayed horizon

Status: not started.
Owner: `rocky-credentials` horizon statement; `rocky-secure` horizon and
session; `rocky-sdk` horizon surface. Depends on
[phase 5](05-issuer-registry.md) and
[phase 10](10-holdings-after-authentication.md). Wave 4.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [horizon and strong commands](scope-mesh.md#6-horizon-and-strong-commands),
[declare, acknowledge, finalize](../runtime/mergeable-horizon.md#declare-acknowledge-finalize)
and the outstanding "horizon through custody alone" in
[the shopping audit](../runtime/shopping-qualification.md#outstanding).

`horizon_statement(scope, peers)` takes the credential chains of at most 64
peers, and the SDK finds those chains only for devices the issuer has synced
with: `horizon_credentials` refuses otherwise. A session accepts an
acknowledgment only from the device that signed it, and a keyless custody
peer relays neither statements nor acknowledgments. Every participant
therefore meets the declaring issuer. In a mesh a member may never meet one.

## Settled

- Participants come from the registry: every unrevoked device whose record
  holds the scope whole, by read or custody, as `authorize_peer` requires of
  a chain today. The registry stores each device's latest chain expiry, which
  becomes the participant's deadline input.
- More than 64 participants: the declaration is refused with a named error.
- A device enrolled after the statement is not a participant. It imports the
  finalized checkpoint.
- Of two concurrent statements with the same sequence and predecessor, the
  one with the lower statement ID wins.
- A peer relays a statement or an acknowledgment only for a scope the other
  side proved under [phase 10](10-holdings-after-authentication.md).
- The statement's policy reference is the one
  [phase 6](06-revocation-set.md) gave it; this phase changes the statement
  after phase 6.

## Ordered slices

1. Participants: `declare` reads the registry instead of the chains of met
   peers. The explicit `participants` request stays and each node must have
   a registry record. The "sync with the required device" refusal is
   removed. The table of met peers' horizon credentials stops being the
   source of participants; remove it only if no other reader remains, and
   keep the bound [phase 9](09-roster-free-admission.md) set while it stays.
2. Decision record 0055 for relay. Any admitted peer that holds the scope,
   with a key or in custody, admits a signed statement, stores it and offers
   it. It stores and offers participants' signed acknowledgments the same
   way. A relay verifies the publisher signature, manifest, scope, policy
   reference and each acknowledgment's signature against a participant the
   statement names. It does not check the cut and cannot acknowledge for
   another device.
3. One new session message family carries relayed statements and
   acknowledgments. The rule that an acknowledgment comes from the session
   peer is replaced by the signature check.
4. Bounds: one pending statement per scope and at most 64 acknowledgments
   for it; an acknowledgment of any other statement is refused. A relay
   removes both when it learns the finalized cut.
5. Concurrent statements: a device holding the higher ID replaces it with
   the lower and acknowledges again; acknowledgments of the replaced
   statement are dropped. The record shows what happens when the higher one
   had already finalized somewhere: the lower stands and holders of the
   other take the existing rebootstrap path.
6. A participant in the revocation set stops being required from the moment
   a device holds the statement revoking it.
7. Finalization and pruning rules are unchanged; `horizon_progress` counts
   relayed acknowledgments.

## Completion gate

- [Acceptance 9](scope-mesh.md#acceptance) is a CI run on the hub and in the
  shopping harness: the participant and the declaring issuer never share a
  session, and the cut finalizes on both.
- A custody peer forwards an acknowledgment; one with an altered byte, one
  for another statement and one signed by a non-participant are refused.
- A peer that did not prove the scope is offered no statement and no
  acknowledgment, and its transcript matches that of a scope that does not
  exist.
- Two concurrent statements, under a deterministic sweep of acknowledgment
  orders, leave one pending cut on every device, and writes resume
  everywhere after it finalizes. The sweep includes the higher one
  finalizing first.
- A participant revoked while a cut is pending stops being required.
- A declaration naming 65 participants fails with the named error.
- A device enrolled after the statement imports the finalized checkpoint.
- A restarted relay keeps the statement and acknowledgments it held.
- The outstanding entry leaves the audit, and
  [the mergeable horizon](../runtime/mergeable-horizon.md) describes relay.

## Not in this phase

- Raising the limit of 64 participants or splitting a statement.
- A horizon declared by a non-issuer.
- Changing the 30-day wait, the cut checks, finalization or pruning.
- Acknowledging on behalf of another device.
- Relaying to a peer that proved nothing, or to peers outside the scope.
- Adding fields to the registry record: the expiry is phase 5's.
- Wiring the shopping example's horizons to registry participants
  ([phase 20](20-shopping-without-a-roster.md)).
