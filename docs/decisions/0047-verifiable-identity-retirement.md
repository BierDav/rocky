# 0047: Retire only identities whose results stay verifiable

Status: accepted.
Date: 2026-10-09.

## Context

`acknowledge_tickets` lets the coordinator retire an invoking device's settled
request identities and drop their retained receipts. After that, another
device verifies a result only with portable evidence. `NetworkCoordinator`
exports voter-signed evidence and peers send it with results.
`MemoryCoordinator` exports none, so a reader that pulled a result after
retirement failed with `Retired` on every sync. Applications had to know which
coordinator they ran on before acknowledging, which put a runtime safety rule
in app code.

Waiting until every reader that needs a result confirms it was considered and
rejected as a general rule. A reader's acknowledgment (`ResultAck`) is stored
on whichever peer served it the result, often a relay or another custodian, so
the invoking device usually never learns it. The set of readers is not defined
either: any device whose grants project a result scope, including ones that are
offline or not enrolled yet. Retention policies name required readers, and
retention pruning already waits for them.

## Decision

`acknowledge_tickets` retires a settled identity only when it is safe:

- Cancelled, retired and expired identities have no result to verify.
- A confirmed identity whose retained result has a receipt that
  `Coordinator::export_evidence` can export.
- A confirmed identity whose result retention already pruned, after every
  required reader of its retention policy applied it. Readers behind that
  horizon rebootstrap from a checkpoint instead of verifying the result.

Retirement is per scope "through" a sequence, so each scope's prefix stops at its
first identity that is not yet safe. Later calls resume from there. The
replica's settled cursor advances only through safe identities, and evidence is
checked outside the replica lock. Identities that never become safe are left to
the coordinator's response horizon.

Applications call `acknowledge_tickets` (SDK `command_acknowledge`, Kotlin
`acknowledgeCommands`) unconditionally. The SDK records a `results_failed`
activity event when a sync moved rows but its result pull failed, instead of
discarding the error.

## Consequences

With an evidence-backed coordinator, retirement is unchanged. Without evidence
and without a retention policy, a device's response slots stay occupied until
the horizon. A busy device can then hit the coordinator's session window and get
`Backpressure`. Neither formats nor wire messages change.
