# 10. Holdings after authentication

Status: not started.
Owner: `rocky-secure` session, frontier exchange and custody offers;
`rocky-sdk` sync. Depends on [phase 8](08-concurrent-rotation.md) and
[phase 9](09-roster-free-admission.md). Wave 3.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [holdings only after authentication](scope-mesh.md#4-holdings-only-after-authentication),
decisions [0044](../decisions/0044-group-frontier-exchange.md),
[0045](../decisions/0045-custody-partition-offers.md) and
[0046](../decisions/0046-per-scope-frontier-refusal.md),
[syncing a scope](../runtime/replica-roles.md#syncing-a-scope) and
[group secrets](../runtime/partitioned-scopes.md#group-secrets).

Frontier exchange answers for scopes on which the asker's grants resolve to
whole-scope read or custody and refuses each other member alone; an unknown
scope and an ungranted one already get the same `Refused(scope)`. The leak is
the grant itself. A profile reads every partition of its tables, so any
member's grant resolves for a list it was never given the key of, and the
peer answers with that list's frontier. `ScopesQuery` (`held_scopes`) goes
further and enumerates every held partition scope to such a member. With
roster-free admission the asker is any device of the domain. Bind answers
about a partition to a proof of holding it.

## Settled

- A proof is required on top of the grant check, never instead of it.
- A reader proves a partition by a value derived from a key epoch of that
  partition and bound to this session. The answering peer accepts a proof
  under any epoch it retains, including a losing sibling and a superseded
  generation. The asker sends at most four per scope, newest first.
- A peer that retains none of the epochs the asker proved answers as for a
  partition that does not exist. The asker catches up by renewal or by
  envelope delivery ([phase 12](12-direct-key-delivery.md)), not here.
- A keyless custody peer cannot check a key-derived value. Toward it the
  proof is the partition tuple, which hashes to the scope and is learned only
  from an envelope or a custody offer. Limits, stated in the guide: a
  guessable tuple proves nothing, and a former reader keeps the tuple.
- A keyless custody peer asking proves by its custody grant and the tuple.
- Unpartitioned scopes keep the grant rule: the manifest names every table
  to every member, so existence is no secret, and a reader's profile already
  delivers the collection key.
- An unproved partition is answered with the existing `Refused(scope)`; no
  new reply kind. Proved and refused replies are what
  [phase 14](14-contacts-and-partner-choice.md) ranks on; nothing else is
  exposed for it.

## Ordered slices

1. Audit every session message family for what it reveals about a partition
   to a peer holding neither key nor tuple. Start with the two findings
   above, then `Pull` and its `pruned` list, `Range`, `Push`,
   `FrontierQuery`, `CheckpointOffer` and `CheckpointPull`, `Read`, the
   horizon messages, `Execute` and result scopes, error texts and whether a
   refusal ends the session. Record the findings in the decision record.
2. Decision record 0052: the derivation, its domain separation from the
   epoch identity of phase 8, the session binding, the bound of four and the
   tuple proof.
3. Proofs travel with the request that names the scope: `FrontiersQuery`
   carries them per member, and the `partitions` field of decision 0045
   becomes the tuple proof whether or not the server knew the tuple. Add the
   module and raise the session version constant of phase 9.
4. Answers. A proved scope within the grants is served as today. Every other
   partition scope gets `Refused(scope)` in a batch and the one existing
   denial in a per-scope request, with the same text, message count and
   session outcome as a scope that does not exist. `ScopesQuery` lists
   unpartitioned scopes and proved partitions only.
5. A session remembers which scopes its peer proved, in memory, for that
   session; later per-scope requests need no second proof.
6. The client still withholds its own frontier from a server whose grants do
   not cover the scope, as decision 0046 requires.

## Completion gate

- [Acceptance 7](scope-mesh.md#acceptance) is a CI run: a member with a
  profile grant and no key asks a reader, and then a keyless custody peer,
  for an existing list and for a random scope, through every message of
  slice 1. The two transcripts are equal in content, error text and message
  count.
- `ScopesQuery` from that member returns no partition scope.
- A proof captured from one session is refused in another.
- A device holding only the losing sibling, and one holding only the previous
  generation, are answered by a peer retaining that epoch and refused by one
  that bootstrapped after it.
- A custody peer with a grant on any list and no key receives offers and
  relays; one without the grant learns nothing from the same requests.
- Five proofs for one scope refuse the request.
- The idle round of the 200-partition four-table fixture stays under its
  10 s budget with proofs; the figure is recorded.
- [Replica roles](../runtime/replica-roles.md),
  [partitioned scopes](../runtime/partitioned-scopes.md) and
  [secure sessions](../runtime/secure-sessions.md) describe it.

## Not in this phase

- Ranking peers or keeping holdings per contact: phase 14 reads the replies.
- Relaying horizon messages for proved scopes
  ([phase 13](13-relayed-horizon.md)).
- Hiding from the server which scopes the asker names.
- Hiding sizes and timing from a peer that proved a partition, and hiding a
  chain's own grants from a session peer.
- A verifier key for custody peers, or any change to envelopes, the key
  store or the epoch identity.
- Existence logs: partitioned existence logs are refused already.
- Narrowing profile grants to the partitions a device was given.
