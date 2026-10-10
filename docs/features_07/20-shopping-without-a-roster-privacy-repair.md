# Phase 20 node-key privacy repair

Status: complete; the [final verification](20-shopping-without-a-roster-evidence.md#final-verification-2026-10-11) passed.

The repair preserves the existing implementation and strict directory audit.
[Decision 0062](../decisions/0062-node-references-and-credential-aliases.md)
describes the new current protocol: one-way references, credential signing aliases,
table-pseudonym checkpoints and signed nonce resolution of saved contacts.
No audit exclusions, test-tier exceptions or budget changes were introduced.

Evidence is under `local/p20-sdk-repair` in the Phase 20 workspace.
`privacy-qualification/evidence.json` records a passing JVM release gate: gate
32.809 s, exclusion/revocation 6.096 s, relink 5.655 s and creator-loss 6.357 s.
The unchanged full-directory byte audit found **zero foreign node-key matches**.
The gate finalized all nine horizons through restarted custody, imported C's
checkpoint and recovered C after the long-offline horizon. All three members
refused the removed physical tables.

The acknowledgment regression refuses mismatched node bindings, altered cuts and
unrelated signing keys. Saved-contact regressions cover route and issuer-flag
recovery without persisting foreign keys. The initial native gate encountered a
stack overflow; a larger-stack diagnostic is not qualification. Saved-contact
resolution is now boxed, and the JVM gate passed on the normal stack.

The current shared JVM suite passed all 29 tests
(`privacy-shopping-tests-final-19.log`). The rosterless approvals regression now
scans the member directory after bidirectional result acknowledgment; it found no
foreign executor key. The SDK stalled-bootstrap regression installs the same
credential alias as production; all nine active change-driven sync tests passed.
Routing tests passed for custody renewal after restart, generation fetch and
transport-specific route refusal.

`privacy-lint-final-19.log` records lint exit 0. The full baseline checks,
the rebuilt Android and iOS applications and budget measurements are in the
[final verification](20-shopping-without-a-roster-evidence.md#final-verification-2026-10-11).
