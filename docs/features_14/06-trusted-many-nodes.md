# 6. Trusted identities with many nodes

Status: planned (2026-11-05). Needs phase 3.

Phase 3 now also requires database-bound membership. Consume that evidence
without widening trust across databases: the same identity may be trusted in
one database and ordinary or revoked in another. Node bounds, staleness, link
alerts and root cuts are evaluated within the current database. Keep existing
trust/coordinator integration and extend its checks for this isolation.

## Changes

- **Trusted nodes by membership.** A node is trusted when its identity
  holds an unrevoked trusted grant and the node is linked. No root
  signature is needed for a new trusted node.
- **Visible links.** Each trusted node raises
  `AlertKind::TrustedNodeLinked { identity, node, linked }` the first time
  it sees a new link of a trusted identity, including its own identity.
  An unlink raises `TrustedNodeUnlinked`.
- **Staleness per identity.** `TrustedNodeStale` becomes
  `TrustedIdentityStale`: no linked node of the identity heard from within
  `trusted_stale_after`. Health still reports per node, informational.
- **Root cuts cover every lane.** `revoke_trusted(identity)` builds a `Cut`
  for every linked node (at most 32, equal to `MAX_CUT_LANES`). Unlinked
  nodes' lanes are cut at their unlink.
- **Coordinator.** The trusted list still names identities. The coordinator
  admits a node certificate of a listed identity that is not unlinked; it
  learns unlinks with revocations.

## Done when

- Two nodes of one trusted identity seal alternately; the identity is not
  stale while either is reachable.
- Root revocation of that identity keeps both nodes' statements up to the
  cut and refuses later ones.
- Linking a third node raises `TrustedNodeLinked` on every trusted node
  that syncs it.
- Reuse one identity in two databases with different trust grants. A link,
  unlink or root revocation in one changes neither the other's trusted lanes
  nor its staleness and alerts. Foreign membership evidence is refused.
