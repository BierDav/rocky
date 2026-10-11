# 4. Remove per-pass sessions

Status: not started.
Owner: `rocky-core` `net.rs`; `rocky-secure` `network.rs`; `rocky-sdk`
`sync.rs`, `node.rs`, `node/mesh.rs`, `enrollment.rs` and transports;
`rocky-iroh`; the always-on peer; the shopping qualification harness.
Depends on [phase 3](03-data-over-the-link.md). Follow the
[execution contract](README.md#agent-execution-contract).

## Objective

After phase 3, every exchange runs on a link, and the per-pass session
remains only as a fallback and in callers that were never moved. This phase
deletes it, so the codebase has a single sync path. See
[what remains](README.md#what-the-old-sessions-are-gone-means).

## Settled

- `Protocol::Sync`, its versioned name, its inbound handling and the dialer
  in `sync.rs` are removed. An inbound stream naming the old protocol is
  refused as any unknown protocol is.
- Explicit `sync` (Rust, JSON and Kotlin `syncOnce`) uses the peer's open
  link, or opens one, reconciles and keeps it while the worker runs. With
  the worker disabled, it closes the link after the reconcile. The response
  shape does not change.
- The always-on peer, custody, bootstrap and the qualification harnesses use
  links. A short link replaces each one-shot session.
- **Over the link budget.** A refused link is retried with back-off. It does
  not fall back to anything. The [useful peers idea](../ideas/useful_peer_links.md)
  is the route to larger meshes.
- No compatibility: a device on the previous release cannot sync with one on
  this one, as [decision 0039](../decisions/0039-single-current-formats.md) requires.

## Ordered slices

1. Move the remaining callers: explicit sync, the always-on peer, custody,
   enrollment bootstrap and the harnesses.
2. Delete the protocol, the server path, the dialer and the fallback, along
   with tests that exercised only them. Port tests that check behavior.
3. Search the workspace, the Kotlin SDK and the examples for the old protocol
   name and the removed calls; none remains.
4. Guides: SDK transports, always-on peer, SDK enrollment, secure sessions,
   the shopping qualification and the module map.

## Completion gate

- `rg` finds no `Protocol::Sync` and no old session name in sources or guides.
- The full `./scripts/check` passes, including the shopping and market
  qualifications, on links only.
- An idle pair makes no dial after its link is open; a lost link is dialed
  again with back-off.
