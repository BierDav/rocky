# 0052: Session-bound partition holding proofs

Status: accepted.
Date: 2026-10-10.

## Context

A profile's read selector covers every partition of its tables. Session admission
is now roster-free, so a grant alone lets a domain member enumerate or probe a
list whose key it never received. Holdings need evidence beyond that grant.

## Decision

Native session version 10 replaces version 9, behind the shared version constant,
transport name and handshake signing label. There is no negotiation or older
reader. No persisted format, epoch identity or envelope changes.

A reader supplies `(epoch, value)` proofs for each named partition. For each
installed table key, compute canonical CBOR of `(key, scope, epoch, binding)`,
then `commitment("rocky.holding-proof-key.v1", bytes)`. This is the key for
ChaCha20-Poly1305 with a zero nonce, empty plaintext and associated data
`rocky.holding-proof.v1`; its 16-byte tag is the proof value. The binding is
`commitment("rocky.holding-session.v1", handshake_signed_bytes)`, including
both fresh challenges and both chains. The derived key changes with the session,
scope and epoch; it is never used to encrypt rows. These labels are separate
from phase 8's epoch identity and the group/table key derivation. Verification
uses the AEAD tag verifier, including its constant-time comparison.

A requester sends at most four installed epochs per scope, newest first under
the phase 8 total order. The verifier accepts any supplied epoch for which it
retains the key, including a losing sibling and a previous generation. It does
not accept an identity without its secret. No common retained epoch means refusal;
renewal or later envelope delivery supplies missing keys.

`FrontiersQuery` carries proofs per member. Its existing `partitions` pairs
also prove the tuple to keyless custody peers, including when already registered.
A keyless custodian asking a reader proves by its whole-scope custody grant and
tuple. A reader asking a reader cannot substitute a tuple for a key. Both sides'
current grants still apply; a proof grants no access on its own. Offers register
only after proof and grant verification, in the existing atomic registration.

For a scoped call before a frontier exchange, `Holdings::Prove` carries up to
64 distinct named scopes, the same proof lists and tuple pairs; `Proved` returns
the admitted subset in input order. Client scoped calls establish this evidence
automatically. `prove_scopes` also lets a caller establish it before scope
listing. The session keeps peer proofs and accepted outgoing proofs in memory.
The server continues checking current credentials for each served request.

An absent proof, an unknown partition and no common key epoch produce the
existing `Refused(scope)` in frontier batches. Per-scope requests fail with
`ciphertext custody denied`, before reading any scope state, and terminate the serving
loop with no reply. Five proofs, duplicate proof/tuple members, out-of-batch
members and malformed batches refuse the entire request before registering
anything. `ScopesQuery` filters unproved partitions before pagination, including
its continuation cursor. Unpartitioned tables keep their grant rule.

The client still filters its own frontier and tuple disclosures by the serving
peer's verified whole-scope grants (0046). No metadata on a refused member is
recorded as an acknowledgment. Valid other batch members keep syncing.
The SDK sync catalog includes concrete partitions and unpartitioned tables,
excluding partitioned tables' empty root addresses. Those roots are selectors,
not holdings to prove or command-result subscriptions.

## Message-family audit

| Family | Finding before this change | Version 10 gate |
| --- | --- | --- |
| `FrontiersQuery` | Profile grants resolve partitions whose reader has no key | Proof plus whole-scope grant per member; otherwise `Refused` |
| `ScopesQuery` | Enumerates every granted held partition | Only proved partitions; filtering precedes page/cursor selection |
| `Pull`, including `pruned` | A granted request can expose frames or pruning state | Every named partition proved before horizon checks or page construction |
| `Range`, `Held`, `Push`, `FrontierQuery` | Grant alone exposes frontier, behind state or acknowledgment behavior | Common gate before any scope-dependent checks or writes |
| `CheckpointOffer`, `CheckpointPull` | Offer presence and page/error behavior reveal holdings | Common gate before checkpoint lookup or page validation |
| `Read` | Grant alone can distinguish a keyed existing scope by package/error | Common gate before remote-read construction |
| Horizon cut/log/rejoined/ack | State, errors and ack admission vary with the partition | Common gate before state lookup or acknowledgment validation |
| `Execute`, result pull/push | Requested result scopes expose scoped results or admission errors | Common gate over subscription/part scopes and the sealed invocation scope before command execution or result lookup/import |
| Row `Ack` | Names a frame ID, not a scope; existing acknowledgment of an unknown ID succeeds, while a retained ID checks its grants | Only an ID actually served in this session is acknowledged; all unserved IDs get the common denial |
| Existence families | Partitioned tables cannot expose existence logs; IDs/logs are table-level | Common proof gate precedes lookup; existing partitioned-existence refusal remains |
| Handshake/revocations | Domain membership, chains and domain revocations are intentionally visible to admitted peers | No partition information is added |

Refused frontier batches continue the session; failed per-scope requests end
its serving loop. These outcomes do not vary between unproved and nonexistent
partitions. Refusal text carries no tuple, epoch or internal lookup result.

## Consequences

A tuple can be guessed, and a former reader keeps it. A custody-only node
cannot check a key-derived proof; tuple knowledge is a deliberately weaker
boundary there. The protocol hides neither named request scopes from the server,
nor sizes/timing from a proved peer, nor the chains' grants. Proof memory lasts
only for the session, never in `identity.json` or a holdings roster. Horizon
relay and contact ranking remain later phases.

[Phase 10 evidence](../features_07/10-holdings-evidence.md) records qualification.
