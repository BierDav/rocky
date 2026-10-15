# Signed seals, tip openings and bookmark moves

`rocky-credentials` owns the signed graph records; `rocky-secure` admits and
stores them. All records name their domain `Scope`. Graph records contain
metadata, never application rows. See [the phase 14 contract](../features_09/14-seals-and-bookmarks.md).

## Statements and identities

`SealStatement` names the ended `TipId`, an author-sorted frontier of
`AuthorMark { author, sequence }`, and `SealAction::Continue { schema }` or
`Close`. A continue seal must keep its parent's schema until phase 17. Its
successor's `TipId` wraps the signed seal's digest.

`TipOpeningStatement` names the seal supplying its starting state and a
positive `minimum_in_sync`. The runtime supplies the domain's minimum when
an application does not override it. Its tip wraps the signed opening's
digest. Explicit openings never receive the parent line's late frames.

`BookmarkMoveStatement` names a nonempty bookmark and optional `from` and
`to` tips. Creation moves from nothing; deletion moves to nothing. A move
must change the pointer. Names are case-sensitive UTF-8, at most 128 bytes,
without control characters. Rocky reserves no bookmark name.

The deterministic genesis identity is
`TipId(commitment("rocky.genesis-tip.v1", schema_digest))`. The digest is the
signed manifest's schema identity. A domain's `TipGraph.scope` qualifies all
tip references, including genesis; equal genesis IDs in two scopes do not
share graph records or coordinated logs.

Seal, opening and move formats each start at 1. Their `Signed*` wrappers
expose `sign`, `endorse`, `verify`, `verify_current`, `id`, `to_bytes` and
`from_bytes`. Decoding refuses other versions and noncanonical encodings.
Verification checks graph dependencies as well as authority and signatures.
Persisted verified records can use `verify`; newly received records require
`verify_current` with the domain's revocations and the current time.

## Authority

The owner and issuer identities can seal a whole domain. Issuer authority
is the existing `issues` extension, which authorizes all grants in its
domain. An identity with only row or field grants cannot seal a tip.
Issuers can move bookmarks forward through one or more continue seals.

The owner can grant whole-domain tip control through
`Authority::issue_trusted(subject, scope, grants, authors, validity)`. The
capability extension's `trusted: Option<Scope>` is format 5. Only an
owner-signed root identity capability can carry trust. Issuers cannot grant
or renew it; neither delegation nor node certification copies it. A trusted
identity can seal, open tips and create, delete or move bookmarks anywhere
within that scope. Trust grants no issuer authority and no row-read rights;
those remain separate grants. A trusted identity with no row grants can
therefore operate on graph metadata without reading application data.

The coordinator's current `DomainControl.trusted` membership is independent.
A later integration must derive or validate that list from current,
owner-granted trusted identity capabilities; a coordinator list entry alone
never establishes credential authority.

## Deterministic graph resolution

`TipGraph` is a pure value containing the scope, genesis schema and verified
seals, openings and moves. `seal(tip)` picks the lower signed seal digest.
`bookmark(name)` picks the lower signed move digest for each `(name, from)`
and walks from creation. Receipt order never decides either conflict.
A move to a losing forward continuation resolves to the winner's
continuation. A backward pointer revisit ends the walk at that tip.

`schema` resolves only active tips; `schema_any` also resolves retained
losing histories, so learning a lower seal never makes authentic historical
graph statements unverifiable. `continues` checks the active continue line;
`continues_any` checks structural history for statement verification.
`line_root` returns genesis or an explicit opening, allowing runtime nodes
to keep distinct author logs on different lines. `canonical_tip` maps a
losing forward continuation to its winning counterpart. An explicit opening
on a losing seal is retained but inactive.

Frame `author_sequence` counts across every resource log of the node and is
reserved with its frame in one transaction. A seal frontier uses this position;
the scoped p2panda sequence remains the dependency-log position.
