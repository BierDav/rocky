# 0051: Roster-free admission and the revocation exchange in the native session

Status: accepted.
Date: 2026-10-10.

## Context

A device served only the node keys it configured, and an issuer also the
devices its registry recorded. The handshake proved the node key only: both
sides sent their hello at once, so whoever connected was shown the acceptor's
credential chain, and the chain was checked first on each request.
[Phase 6](0049-mergeable-revocation-set.md) left an interim: the hello carried
the revocation set's digest and a mismatch was a named refusal that made the
lagging side renew. [Phase 9](../features_07/09-roster-free-admission.md)
admits a peer by its chain and the set, and lets the set travel in the session;
its "Settled" answers are the product decisions this record implements.

## Decision

**Version.** `rocky_core::net::NATIVE_SESSION_VERSION` (9) names the transport,
`rocky/native-session/9`, and the label both sides sign the hellos under,
`rocky.native-session.v9`. The hello carries it too. A session change adds a
message module under `rocky-secure/src/network/` and raises the constant.
Nothing is negotiated: another version is refused in the handshake as `native
session authentication failed`, the error a failed key proof gives.

**Messages.** One module per family holds its requests, the client calls that
send them and the answers that serve them: rows, checkpoints, horizon,
frontiers, existence, commands and revocations. The session's `Message` wraps
one family each, beside a shared `Acknowledged`.

**Handshake.** In order:

1. The initiator sends its hello: version, node key, challenge and chain.
2. The acceptor checks the version and the chain, without revocations, and
   only then sends its own hello. A refused initiator reads nothing.
3. Both sign the two hellos; the initiator then checks the acceptor's chain.
4. Each sends its live set's digest. Equal digests end the exchange.
5. Otherwise each sends its live statement IDs, 1,024 per page, then the
   statements the other lacks, 32 per page, each in its own encoding. Pages
   alternate, the initiator first; IDs and statements are CBOR byte strings
   and every page fits one 256 KiB frame. A peer may name at most 4,096 IDs;
   the merge of [0049](0049-mergeable-revocation-set.md) bounds the set.
6. Each merges what it received and checks the merged set: a side whose own
   chain is ended returns `network::Revoked` with the statement, inside
   `Error::Application`; a side whose peer's chain is ended refuses it as
   `the peer is revoked`. A revoked device so receives the statement that
   revokes it before it is refused.

**Chain check.** The leaf is a node credential for the presented key, every
link verifies under the pinned publisher, lies within its validity and the
chain's root manifest is the current generation or one the generation policy
admits. A chain of a newer generation, which the replica cannot read yet, is
checked under its own manifest and must grant only this application; an older
one behind the horizon is refused as before.

**The replica never holds its own revocation.** A statement that ends this
device's chain is not merged: its owner records it. A replica whose set ended
its own chain could not open.

**Admission.** A session is admitted when the chain verifies and no statement
revokes its node, user, capability or pseudonym. `admitted()` and the accept
filter are gone; a custody peer and a non-issuing member serve by the same
rule. Grants bound every request, every request verifies the chain again and
no session caches authority.

**Bounds for strangers.** Nothing is written before both chains verified. A
service serves 32 inbound streams; more are refused and counted.
`secure_horizon_credentials` and `secure_peer_frontiers` keep at most 256
unconfigured peers: when a new peer's first row would exceed that, the least
recently served since open lose theirs. The replica's owner names the peers
it keeps whatever the bound (`configure_peers`); full nodes are kept too. The
order is kept in memory: no stored format changes.

**SDK.** Peers become contacts: routes that grant nothing, still the `peers`
map of `identity.json`, which keeps version 11 since only its bound changed.
64 contacts are kept and 16 dialed per sync pass, issuers first, then the least
recently attempted. `peers` lists every contact and up to 64 peers met only
through their own sessions, each with `configured`. A device that ends a
session as revoked records the statement in `revoked.json` as after a refused
renewal and reports `revoked`. Rotated keys still arrive by renewal: a sync
whose session changed the set ends with `Learned` and renews once, and a pass
renews first when a served session brought statements. Without an issuer the
rows still sync; the new keys wait.

## Consequences

- The digest refusal `revocations_differ` is gone; a member learns a
  revocation from any admitted peer it meets, issuer or not.
- A replayed hello of a member shows the acceptor's chain: the acceptor speaks
  before the initiator proved its key. Chains were already visible on a route.
- On an issuer with more than 256 unconfigured devices, a horizon deadline may
  wait for a device it met before to sync again.
- Two issuers that rotated one scope at once still fail the first checkpoint
  import until both hold both keys, as under 0049, until
  [phase 8](../features_07/08-concurrent-rotation.md).
- Measured in the debug profile, under the full test run: with 1,024 equal
  statements the exchange after the hellos is 440 bytes and the handshake
  about 0.1 s; 1,024 against none moves 719,214 bytes in about 0.24 s.
- `rocky/native-session/8` and every earlier session are refused.
