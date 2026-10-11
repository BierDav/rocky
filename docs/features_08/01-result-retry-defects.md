# 1. Result retry defects

Status: in progress.
Owner: `rocky-sdk` result pulling in `sync.rs`; `rocky-secure` checkpoint
import in `network/checkpoints.rs` and `replica/checkpoint/transfer.rs`.
No dependency. Follow the [execution contract](README.md#agent-execution-contract).

## Observed

A trusted desktop that joined a household with the shopping application
raised its committed revision by about 50 every 15 seconds while idle. Its
log showed only checkpoint re-imports: each round restaged and switched all
15 scopes to the checkpoints it already held, rewriting their projected tables.

The chain, in `results()` of `rocky-sdk/src/sync.rs`:

1. The coordinator head did not answer within `HEAD` (2 s), so `head`
   became `u64::MAX`.
2. `missing_results(domain, u64::MAX, 64)` listed every sequence after the
   strong base, sequences no device holds, because none was ever confirmed.
3. The peer returned no result for the first of them, and the device
   bootstrapped **every** scope from the peer's checkpoint.
4. The pass reported `retry`, so the per-peer back-off ran it again.

Each step above is a separate defect. A live link (phase 2) would trigger
the same loop on every signal, so this phase lands first.

## Settled

- An unknown head bounds nothing. With no head, a device pulls only
  sequences below the highest one it already knows to be confirmed: the
  highest retained result or strong base. If it knows none, it pulls none.
- An absent result bootstraps only the scopes the result can touch: the
  scopes of retained results and of command-controlled tables that this
  device holds with a key. It never bootstraps the whole holding.
- Importing a checkpoint whose signed statement equals the verified one
  already installed for the scope is a no-op. It stages nothing, switches
  nothing and leaves the revision unchanged.
- A coordinator that cannot be reached still makes the pass report `retry`,
  as the [change-driven sync](../runtime/always-on-peer.md#change-driven-sync)
  guide states. It no longer causes any local writes.

## Completion gate

- With an unreachable coordinator and an idle peer, repeated passes leave the
  revision and the projected tables unchanged.
- With the head unknown and one result missing at the peer, only the result's
  scopes are offered a checkpoint, and an identical checkpoint changes nothing.
- The existing results, bootstrap and checkpoint tests pass.
