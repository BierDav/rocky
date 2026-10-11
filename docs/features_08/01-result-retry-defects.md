# 1. Result retry defects

Status: implemented 2026-10-11; the gate's tests pass. Not yet qualified on
the shopping desktops that showed the defect.
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

- With no head, a device still pulls forward, so peers can bring results
  the coordinator would have announced. A sequence above `known_result_head`
  (the highest retained result or strong base) that the peer lacks ends the
  pull: it may not exist and is not a gap.
- A real gap bootstraps only `result_scopes`: the scopes bounding the
  results and every command's declared resources read with a key. It never
  bootstraps the whole holding.
- Importing a checkpoint whose signed statement equals the verified one
  already installed for the scope is a no-op. It stages nothing, switches
  nothing and leaves the revision unchanged.
- A coordinator that cannot be reached still makes the pass report `retry`,
  as the [change-driven sync](../runtime/always-on-peer.md#change-driven-sync)
  guide states. It no longer causes any local writes.

## Completion gate

- With an unreachable coordinator and an idle peer, repeated passes leave the
  revision and the projected tables unchanged
  (`an_unknown_head_leaves_an_idle_reader_unchanged` in `rocky-sdk`; without
  the fix, the revision rose from 4 to 10 over three passes).
- Bootstrapping a scope twice from the same checkpoint imports it once and
  leaves the revision unchanged (`an_installed_checkpoint_is_not_imported_again`
  in `rocky-secure`).
- The existing results, bootstrap and checkpoint tests pass.
