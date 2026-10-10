# 0059: One-hop command forwarding

Status: implemented and qualified locally; see [phase evidence](../features_07/16-strong-commands-evidence.md).
Date: 2026-10-10.

## Context

A roster-free caller may know only a neighbor, while the coordinator names an
executor by node ID. Phase 14 supplies contact requests but no command carrier.

## Decision

Replace `rocky/command/1` with `rocky/command/2`. TCP uses command preamble
tag 6 and refuses the old tag 4. No older carrier is accepted. Native sync,
coordinator requests, command signatures, tickets, receipts and results keep
their current formats.

The SDK resolves the coordinator's executor ID through local contacts, then
Phase 14's authenticated contact request. A returned node key must hash to
the requested 16-byte ID. The caller tries direct delivery first and, if that
route fails, asks its admitted contacts to forward once. A forwarder sends
only a direct request to the executor; it never calls another forwarder.

The carrier is a JSON request with `version`, `sender`, `credentials`,
`nonce`, optional `forward` executor ID, the unchanged `envelope` bytes and
`signature`. Its Ed25519 signature covers the canonical JSON tuple of the
request label, version, sender, chain, nonce, target and envelope. The reply
contains `version`, `sender`, `credentials`, the request commitment,
`delivery` bytes and `signature`, covering the analogous reply tuple.
Labels are `rocky.command-carrier.request.v2` and
`rocky.command-carrier.reply.v2`; the request commitment uses the request
label. Both signatures and the peer's current node chain are verified.
The reply commitment binds the entire request, including its random nonce.

A forward request must be signed by the envelope's caller. Before forwarding,
the runtime verifies the unchanged command under the held revocation set.
The executor verifies that caller again, independently of the carrier sender.
The caller verifies the executor's confirmed result and coordinator receipt
through the existing settlement path. Replaying either route uses the same
request identity and cannot cause a second confirmation or handler decision.

A forwarder admits eight concurrent requests per caller without waiting for
space. A ninth receives a retryable refusal. Slots have no persistent state
and drop on completion, cancellation or the 30-second forwarding deadline.
There is no offline queue. Transport failures after sending leave the outcome
unknown; the caller retries the same ticket. Exhausting routes is retryable.

## Consequences

A forwarder sees caller and executor identities, credential grants, method,
request identity and commitments, ciphertext sizes and timing. Invocation and
result payloads retain their existing encryption; a forwarder possessing the
relevant reader keys can read them. This protocol adds no anonymity.

The application-owned envelope/execute/settle carrier remains available.
Results for an offline caller continue through ordinary result sync and
custody; forwarding adds no result push or storage at the forwarding hop.
