# 10. Holdings after authentication

Status: not started.
Owner: `rocky-secure` frontier exchange and custody offers; `rocky-sdk`
sync. Depends on [phase 6](06-concurrent-rotation.md) and
[phase 7](07-roster-free-admission.md). Wave 3.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [holdings only after authentication](scope-mesh.md#4-holdings-only-after-authentication),
decisions [0044](../decisions/0044-group-frontier-exchange.md),
[0045](../decisions/0045-custody-partition-offers.md) and
[0046](../decisions/0046-per-scope-frontier-refusal.md), and
[syncing a scope](../runtime/replica-roles.md#syncing-a-scope).

Frontier exchange already answers only for scopes on which the asker's
credentials grant read or custody, and refuses each other member alone. With
roster-free admission the asker may be any device of the domain. Audit what
it can learn, and bind answers to a proof of holding.

## Ordered slices

1. Audit every session message for what it reveals about a partition to a
   peer that holds neither its key nor a custody grant: scope hashes,
   per-scope refusals, counts, sizes, timing of answers, horizon and
   existence metadata. Record the findings.
2. Decision record for the proof. A reader proves a partition by a value
   derived from its current key, bound to this session so it cannot be
   replayed; with two sibling epochs either is accepted. A keyless custody
   peer proves by its custody grant.
3. Answers: a request naming a partition the asker did not prove is answered
   exactly as one naming a partition that does not exist. Per-scope refusal
   keeps its meaning for proved scopes.
4. Unpartitioned scopes keep the grant rule; state why.
5. Expose proved holdings to the ranking of
   [phase 9](09-discovery-and-partner-choice.md): which peers serve what this
   device needs, kept in memory for the contact and never replicated.

## Completion gate

- [Acceptance 7](scope-mesh.md#acceptance) passes: transcripts of a session
  asking for an existing list without its key and for a random scope are
  indistinguishable in content and message count.
- A proof captured from one session is refused in another.
- A device holding the losing sibling epoch is still answered.
- A custody peer with a grant on "any list" and no key still receives offers
  and relays; one without the grant learns nothing.
- The idle-round budget of the 200-partition fixture still passes with
  proofs.
- [Replica roles](../runtime/replica-roles.md) describes it.

## Not in this phase

Hiding sizes and timing from a peer that proved a partition, and hiding the
chain's own grants from a session peer.
