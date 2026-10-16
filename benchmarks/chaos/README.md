# Rocky chaos harness

Fault-injection and fuzzing over the packaged `rocky-sdk` starter (`Engine`).
Each device runs in its own process (`rocky-chaos node`) on loopback iroh, so
the orchestrator can SIGKILL it at any instruction, then resume it from its
directory. It adds no unit tests and uses only the SDK's public native calls.
Findings are in [REPORT.md](REPORT.md).

This is an independent Cargo workspace, excluded from the application checks.

## Build and run

```sh
cd benchmarks/chaos
cargo build
./target/debug/rocky-chaos scenario all            # every scripted scenario
./target/debug/rocky-chaos scenario evict_resubscribe revoked_via_stale_peer
./target/debug/rocky-chaos fuzz <seed> <steps>     # e.g. fuzz 13 150
```

Data goes to `$CHAOS_ROOT` (default `$TMPDIR/rocky-chaos/<scenario>-<pid>`); each
node's stderr is kept next to its directory. Lines starting with `FINDING:` are
violations; everything else is observation.

| Variable | Effect |
| --- | --- |
| `CHAOS_VERBOSE=1` | Print every fuzz step |
| `CHAOS_LOG=1` | Always write the fuzz schedule to `schedule.log` |
| `CHAOS_EVICT_KILL=1` | `evict_with_full_node` kills the node during resync |
| `CHAOS_FIXED_PORT=1` | `network_cuts_during_bootstrap` keeps the joiner's fixed port |
| `CHAOS_SEED=n` | Seed of `request_fuzz` |
| `CHAOS_A_OFFLINE=1` | `cross_issuer_share` shares while the recipient is killed |

## Topology

`a` creates the domain (trusted issuer). Other nodes join by invitation:
`issuer` profile for trusted, a writer of `lists`, `items` and `notes` for full
and partial; a partial node sets `partition_role` Partial afterwards, as the
shopping app does. `notes` is unpartitioned; `lists`/`items` are partitioned by
list, and each list's key is shared explicitly with `partition_share`.

## Scenarios

| Name | What it forces |
| --- | --- |
| `kill_joiner_during_bootstrap` | SIGKILL a joiner at several import offsets, resume each time |
| `kill_source_during_bootstrap` | Kill the checkpoint signer mid-import; a second full node exists |
| `writes_during_bootstrap` | Inserts, updates, deletes on both sides while the joiner imports |
| `network_cuts_during_bootstrap` | Socket faults, networking/background/sync toggles during import |
| `kill_during_enrollment` / `enrollment_window` | SIGKILL inside `Database::join`, then every recovery path |
| `partial_role_race` | Role set after join, and SIGKILL between join and role |
| `revoke_during_bootstrap` | Revoke an importing device and one with unsent writes |
| `app_sql_abuse` | 36 hostile statements through the session's SQL |
| `kill_in_transaction` | Concurrent transactions on one session, kill with open tx and racing COMMIT |
| `evict_kill`, `evict_with_full_node`, `evict_resubscribe` | Partial eviction, kill, resubscribe |
| `prune_then_rejoin` | Retention window 0 with an offline writer that is killed mid-resync |
| `close_during_sync` | Graceful close during bootstrap, sync and a subscription |
| `foreground_rebind` | Socket fault and background/foreground on fixed and ephemeral ports |
| `pending_forever` | Readiness of full writers with none, some or all lists shared |
| `bootstrap_from_partial` | A new full node can reach only a partial peer |
| `revoked_via_stale_peer` | A revoked writer pushes through a peer unaware of the revocation |
| `double_open` | Same directory in two processes; stale locks after SIGKILL |
| `file_damage` | WAL deleted after SIGKILL; `identity.json` rolled back |
| `request_fuzz` | 600 random/malformed native requests |
| `half_bootstrapped_source` | Trusted device killed mid-import enrolls and serves a new device |
| `cross_issuer_share` | A trusted device re-shares a list another trusted device created (`CHAOS_A_OFFLINE=1`: it was offline at share time) |
| `orphaned_second_issuer` | Second trusted device invites after the original issuer vanished |
| `envelope_kill` | Sharer SIGKILLed while delivering envelopes to an offline trusted device (slow; not in `all`) |
| `partial_holds_shared` | What a partial replica holds after one shared list |

## Fuzzer

`fuzz` starts a trusted issuer with seed data, a second trusted device, a full
and a partial device, then runs a seeded random schedule of: note and item
writes/updates/deletes, new lists shared with every full device, shares to the
partial, syncs, SIGKILL, SIGKILL during sync, resume, graceful close during
sync, networking/background/sync toggles, concurrent transaction bursts, new
full members killed early in their bootstrap, retention with window 0, and
hostile SQL that may leave a transaction open.

The scheduler revives a node whenever fewer than two, or no issuer, are alive.
The heal phase restarts every node, enables networking and sync, re-shares
every list each round from whichever trusted node holds its secret, meshes
all nodes and syncs up to 12 rounds. It then requires that
full and trusted nodes agree on every table, nothing is still bootstrapping,
partial nodes agree on `notes` and on every list they hold, every acknowledged
insert that was never targeted by a delete exists, `PRAGMA integrity_check`
passes and every node reopens.

Socket faults (`network_fault_close`) are left out of the fuzzer because the
fixed-port rebind failure in the report would dominate every seed.
