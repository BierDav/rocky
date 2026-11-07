# 3. Membership

Status: implementation complete and verified (2026-11-07). Database-scoped
certificate evidence, link acknowledgments, fork enforcement, identity-owned
unlink and reader-key rotation are integrated. The required workspace gate
passes, including native and browser checks. See
[membership evidence](03-membership-evidence.md) for measured results and
limits. Phases 5b and 6 remain separate; this is not a features_14 completion
claim.

An identity links and unlinks its own nodes. Any node can verify which
nodes an identity holds.

## Database scope

- The same identity can join independent databases. Membership heads,
  sequences, author ranges, the 32-node bound, forks and unlinks belong to
  `(database context, identity)`, never the identity globally.
- Bind node certificates and identity-owned unlinks to a signed database
  context containing the pinned publisher root and stable mesh/domain IDs.
  Do not use local paths, bookmarks or changing schema generations as the
  database identity. Certificates remain independent of capability expiry.
- Verify the expected context in certificate/chain validation, enrollment
  requests and answers, link acknowledgments, authority-set import/merge and
  membership views. Refuse foreign evidence before storing or applying it,
  even if signed by the same identity. A foreign certificate must not create
  a fork, consume a slot or author number, or invalidate a local node.
- Reuse existing database context types where possible and keep the
  credential contract language-neutral. Update affected persisted/wire formats
  and signed fixtures together; accept only the current format.
- Integrate with the already implemented phase 2 identity-provider boundary,
  phase 4 registry and phase 5a admission. Do not replace their identity
  lifecycle or approval flows. Phase 5b linking and phase 6 trust must consume
  the same scoped evidence. Passkey UI and a personal database directory stay
  in the application.

## Link

- The node certificate gains `sequence` and `previous` (the ID of the
  identity's previous certificate in this database, `None` for the first).
  Its author number is `range.first + sequence`.
- Linking: the new node runs the `enroll_identity` exchange
  (`self_enrollment.rs`) against any node of the identity and receives the
  bundle plus the membership head. The holder supplies the identity key on
  the new node; it signs certificate `head + 1` there. The new node
  presents it to the answering node, which stores it before the new node
  syncs.
- Bound: 32 linked nodes per identity per database (`MAX_CUT_LANES`). A link
  past it is refused until a node is unlinked.
- Fork: two certificates of one identity in one database at one sequence
  are a membership equivocation. Both are void wherever both are known, and
  every node of the identity raises `AlertKind::MembershipFork`. Admission
  keeps the certificate it saw first until the second arrives.

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
- One supplied identity joins two databases with different grants and roots;
  both can start at sequence zero and advance independently without a fork.
  Repeat with a shared publisher root but distinct mesh/domain context.
- Foreign certificates, unlinks, enrollment replies and authority-set pages
  are refused without changing local membership or credentials. Exercise
  signed evidence from the same identity at the same sequence.
- Linking, unlinking, identity revocation and reader-key rotation in one
  database leave the other database's nodes, grants and reads/writes usable.
  The active-node bound is independent in each database.
- Record credential and SDK regression results for this scope separately
  from earlier phase 3 checks, then run the required workspace gates.
