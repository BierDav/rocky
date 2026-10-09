# 12. Relayed horizon

Status: not started.
Owner: `rocky-secure` horizon and session; `rocky-sdk` horizon surface.
Depends on [phase 4](04-issuer-registry.md) and
[phase 7](07-roster-free-admission.md). Wave 3.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [horizon and strong commands](scope-mesh.md#6-horizon-and-strong-commands),
[the mergeable horizon](../runtime/mergeable-horizon.md) and the outstanding
"horizon through custody alone" in
[the shopping audit](../runtime/shopping-qualification.md#outstanding).

`horizon_statement(scope, peers)` captures a proposal from the credential
chains of peers the issuer verified, at most 64. Statements and
acknowledgments travel only between a participant and the peer it syncs with,
and a keyless custody peer relays neither, so every participant meets an
issuer. In a mesh a member may never meet one.

## Ordered slices

1. Participants: the declaring issuer names them from the registry: every
   unrevoked device whose record reads the scope. State how a device enrolled
   by another issuer after the statement is treated.
2. Decision record for relay. Any admitted peer that holds the scope, with a
   key or in custody, admits a signed statement, stores it and forwards it;
   it does the same for participants' signed acknowledgments. A relay
   verifies signatures and scope; it cannot acknowledge for another device.
3. Bounds on relayed statements and acknowledgments per scope, and their
   removal at finalization.
4. Two issuers declaring one scope at once: one statement wins by a
   deterministic rule, the other is withdrawn; participants acknowledge the
   winner only.
5. Finalization and pruning rules are unchanged; `horizon_progress` counts
   relayed acknowledgments.

## Completion gate

- [Acceptance 9](scope-mesh.md#acceptance) passes on the hub and in the
  shopping harness, the participant and the issuer never sharing a session.
- A custody peer forwards an acknowledgment it cannot forge; an altered one
  is refused.
- Two concurrent statements leave one pending cut on every device, and writes
  resume everywhere after it finalizes.
- A participant revoked while a cut is pending stops being required.
- A restarted relay keeps what it held.
- The outstanding entry leaves the audit.

## Not in this phase

A horizon declared by a non-issuer and the 30-day wait.
