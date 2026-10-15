# Cloned node detection

Status: deferred. Node keys and certificates are planned in
[phase 13](../features_09/13-identities-and-domain-credentials.md);
[hardware-bound node keys](../ideas/hardware_bound_node_keys.md) would
prevent clones instead of detecting them.

## Trigger

Build this when devices or servers are copied while running, such as VM
snapshots, restored disk images or copied app data, and both copies keep
writing. [Signed backups](../runtime/backup-and-restore.md) already restore
under a fresh identity and are not a trigger.

## Current cost

Both copies hold the same node key and counters, so their first writes take
the same position of the node's log and the same row identity. Admission
keeps the first history it sees and refuses the other as an author fork
([decision 0069](../decisions/0069-held-changes-and-void-records.md)).
Devices that met different copies first keep different histories of that
node: they never converge on its writes after the copy. In a coordinated
domain both copies count as one in-sync node, so an entry may be stored
only on the copy that is later lost.

## Sketch

- **Proof.** A node's log chains each record to its predecessor. Two
  different signed records at one position, or of one row identity, prove
  the key was copied. Any device verifies the pair without an issuer and
  spreads it like a revocation.
- **Burning, not revoking.** Revocation refuses every frame of the author,
  earlier ones included. A burned key keeps its records before the first
  conflicting position. From there on, only the branch whose record at that
  position has the lower digest is admitted, and nothing the key signs after
  it learns the proof.
- **The losing copy** receives its branch back as held changes; its
  application re-authors or drops them.
- **Both copies continue** under fresh node certificates from their
  identity provider, without an issuer.
- **Coordinator.** A trusted node presents the proof; the coordinator drops
  the burned key from the in-sync set. Duplicate request sequences already
  return `RequestConflict`.

## Must not break

- A node's records before the copy stay admitted on every device.
- Every device that holds the proof admits the same branch.
- Detection needs both branches to meet; until then devices may disagree,
  and the outcome afterwards must not depend on which they met first.
- A copy that only reads is never detected; revoking its node certificate
  remains the answer for a copied device.
