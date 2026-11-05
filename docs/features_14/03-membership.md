# 3. Membership

Status: planned (2026-11-05). Needs phase 2.

An identity links and unlinks its own nodes. Any node can verify which
nodes an identity holds.

## Link

- The node certificate gains `sequence` and `previous` (the ID of the
  identity's previous certificate, `None` for the first). Its author
  number is `range.first + sequence`.
- Linking: the new node runs the `enroll_identity` exchange
  (`self_enrollment.rs`) against any node of the identity and receives the
  bundle plus the membership head. The holder supplies the identity key on
  the new node; it signs certificate `head + 1` there. The new node
  presents it to the answering node, which stores it before the new node
  syncs.
- Bound: 32 linked nodes per identity (`MAX_CUT_LANES`). A link past it is
  refused until a node is unlinked.
- Fork: two certificates of one identity at one sequence are a membership
  equivocation. Both are void wherever both are known, and every node of
  the identity raises `AlertKind::MembershipFork`. Admission keeps the
  certificate it saw first until the second arrives.

## Unlink

- An identity-signed `Revocation` naming only nodes the identity linked.
  `Revocation::verify` refuses an identity-signed statement naming another
  identity's node, any identity or any capability. Issuer statements may no
  longer name nodes.
- On the unlinked node's arrival anywhere it is refused like any revoked
  chain, and reader keys of every scope the identity reads rotate. The
  existing revocation key rotation runs, keyed by identity.
- Unlinking the last node is allowed. The identity keeps its permissions
  and can link again.

## Membership view

- Linked certificates travel with revocations, in the replicated set every
  node holds. `Credentials` verification needs the certificate alone; the
  set answers "which nodes does identity X hold" for phase 4 and phase 6.

## Done when

- An identity with three nodes unlinks one: the others keep writing, the
  unlinked node's frames are refused, and read keys rotate.
- Two concurrent links at one sequence void both, alert, and a relink
  succeeds.
- An issuer-signed revocation naming a node is refused.
