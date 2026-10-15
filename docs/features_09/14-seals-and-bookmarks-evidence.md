# Phase 14 evidence

## Lane S — seals, tips, bookmarks and late frames

### Slice 1: statements and formats

`rocky-credentials` defines signed seals, tip openings and bookmark moves at
format 1. `TipGraph` resolves concurrent seals and same-source moves by lower
signed digest, retains structurally valid losing histories, and redirects a
losing continuation to its canonical successor. Until phase 17 a continue seal
must preserve its parent's manifest digest. Genesis is
`commitment("rocky.genesis-tip.v1", manifest_digest)` wrapped in the shared
`rocky_core::TipId`; the graph's `Scope` qualifies that identity.

The capability extension is version 5. `Extended.trusted: Option<Scope>` is an
owner-direct grant to an identity; a node never inherits control authority.
`Authority::issue_trusted` issues this grant. The coordinator's trusted list
can later derive membership from these verified credentials; this lane does
not alter coordinator control certificates.

Row frame 10, existence record 4, checkpoint 13, encrypted file identity 23,
mergeable horizon 7 and link handshake 7 carry tips. Previous versions are
refused. Row frames also sign `author_sequence`, a transactionally reserved
position across the node's resource logs, because their existing p2panda
sequences count independently per scope. Seal frontier marks use this global
position, not the p2panda sequence.

Verification during implementation: `cargo test -p rocky-credentials --locked`
passed (36 unit, 13 credentials, 5 identities, 6 tip tests).
`cargo test -p rocky-secure --lib formats --locked` passed the three format
refusal tests before the global author-position assertion was added; the
final verification below records the complete run.
