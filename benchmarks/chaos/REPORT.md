# QA report: irrecoverable states under faults

Environment: macOS arm64 (Darwin 27), working copy on top of `78763038d`
(`refactor(sdk)!: expose native observations…`), debug build of
`rocky-sdk` with the `qualification` feature, loopback iroh (relay, lookup and
discovery disabled), 2026-10-13. Reproduce with the [harness](README.md); every
command below runs from `benchmarks/chaos`.

The question was: can misbehaving peers or local application code leave a node
in a state it cannot leave once the environment is healthy again? Six of the
eleven findings below are such states, or come close. The rest are recoverable
but misleading.

## Summary

| # | Severity | Finding | Recovers in a healthy environment? |
| --- | --- | --- | --- |
| 1 | Critical | Interrupted join plus any newer invitation bricks the directory | No: only by deleting the directory by hand |
| 2 | High | A peer that relayed a revoked writer's rows can never reconcile toward the issuer again | No: survives restart |
| 3 | High | Evict → resubscribe never restores a partition; a kill during the resync makes it read as complete and empty | No: survives restart |
| 4 | High | A dead endpoint never heals by itself; with a fixed bind port, `network_foreground` cannot rebind | Only by restarting the process |
| 5 | Medium | Pending horizon plus one dead participant freezes writes in the scope for every device | Only by revoking the dead device (or a 30-day wait) |
| 6 | Medium | `identity.json` older than `rocky.sqlite` leaves partitioned tables permanently unreadable | No |
| 7 | Medium | A full node that reaches only a partial peer reports `bootstrap_pending=false` with no items | Yes, once a full peer is reachable |
| 8 | Low | Transient refusals marked non-retryable; permanent ones marked retryable | n/a (API) |
| 9 | Low | Transport failures reported as `local storage I/O`; missing install reported as raw `ENOENT` | n/a (diagnostics) |
| 10 | Low | One session is one shared transaction across concurrent callers | n/a (API) |
| 11 | Low | `sync` returns `status: ok` when every peer attempt failed | n/a (API) |

## 1. Interrupted join plus a newer invitation: unrecoverable directory

`Database::join` persists `identity.join.json` (the join intent) before
contacting the issuer, so the intent is meant to make a retry come from the same
device. If the process dies after the issuer has *consumed* the invitation but
before `identity.json` is written, the directory holds `identity.join.json`,
`bootstrap.json` and `secrets/`, but no identity:

- `resume` fails with `No such file or directory (os error 2)`.
- Retrying the *same* invitation works, but only while the issuer has not
  created another one. `invite` replaces the issuer's single invitation and
  clears the claim that allows the retry (`crates/rocky-sdk/src/enrollment.rs`,
  `vault.replace("invitation")` … `issuing.claimed = None`). The retry is then
  refused with `invalid invitation`.
- A *new* invitation is refused locally: `a different join is unresolved; retry
  its invitation` (`Intent::resume`, `enrollment.rs:456`).

The SDK has no call to abandon the pending join. The app only escapes by
deleting the directory, which also strands the vault keys of that installation
in the platform secret store. In the shopping app the issuer's natural reaction
to a failed pairing, tapping **Add device** again, triggers this state. The
window was 60–120 ms on loopback. On a phone it covers the whole pairing
exchange: a process kill, an OS background kill or a crash during pairing. I did
not check whether a dial *timeout* inside that window leaves the same files, or
whether the issuer's registry keeps a ghost device.

```sh
./target/debug/rocky-chaos scenario enrollment_window
# killed at 72ms (issuer re-invited: true): resume: No such file or directory (os error 2)
#   same invitation: invalid invitation
#   newer invitation: a different join is unresolved; retry its invitation
# killed at 78ms (issuer re-invited: false): same invitation: ok
```

## 2. Revoked writer's rows relayed through an unaware peer break that peer

Full nodes B and C are synced with issuer A. A is cut off, then A revokes B.
B keeps writing notes and items and pushes them to C, which has not learned of
the revocation yet. C accepts 10 rows. When C next meets A, C learns the
revocation and correctly purges B's rows from its projection. From then on,
**every reconciliation C starts toward A fails**:

- A closes each link with `replication data rejected: invalid or revoked credential`.
- C records `reconcile=failed` with `{"kind":"transport","code":null,"detail":"local storage I/O: unexpected end of file"}` and backs off.
- The same happens after C restarts.

C's new writes reach A only when A happens to dial C. Under the live-link
roadmap, a device that cannot be dialed (a phone on cellular) never delivers
its own writes again. A healthy device is broken permanently by having relayed
for a revoked one. Most likely C still retains, and offers, B's
post-revocation operations in its history and frontier.

```sh
./target/debug/rocky-chaos scenario revoked_via_stale_peer
```

## 3. Evicted partition never comes back; a kill makes it read complete and empty

A partial replica (role Partial, the issuer as full node, custody 1) holds one
shared list with 200–240 items. `partition_evict` succeeds. Then
`partition_subscribe` (or `partition_bootstrap`) succeeds, the partition reports
`holding: held` for both tables, and ten syncs over three seconds follow:

- **No kill:** every read of that list fails with `items: held partition is
  incomplete` (code `storage`, `retryable: true`). This persists after a
  restart and is independent of whether the partial had written to the list.
- **SIGKILL 15 ms into the first resync:** reads succeed and return **0 rows**
  while the issuer has 240. After restart: still `Ok(0)`. The application sees
  a complete, empty list, which is silent data loss from its point of view.

Documentation (`docs/runtime/replica-roles.md#eviction`): "`subscribe` brings
the scope back; sync it before authoring in it again". Sync never brings it
back. `evict_with_full_node` also shows that writes into the resubscribed list
are refused with a foreign-key error rather than a "not held" refusal.

```sh
./target/debug/rocky-chaos scenario evict_resubscribe
# q1 kill=false own_writes=true via partition_subscribe -> healed=false (rows: held partition is incomplete; issuer 220)
# q2 kill=true  own_writes=true via partition_subscribe -> healed=false (rows Ok(0); issuer 240), after restart: Ok(0)
# q3 kill=false own_writes=false ... q4 kill=true via partition_bootstrap ... q5 ... : same
```

## 4. Dead endpoint never heals; fixed bind port cannot rebind

`network_fault_close` (the qualification hook that closes the endpoint, as an
OS does when it reclaims a suspended app's sockets) leaves the node offline:

| After the fault | Fixed `bind` port | `127.0.0.1:0` |
| --- | --- | --- |
| Automatic sync / explicit `sync` | attempts fail forever | attempts fail forever |
| `networking_control` off/on | still failing | still failing |
| `network_foreground` | `peer unreachable: Failed to bind sockets`, on every call | recovers |
| Process restart | recovers | recovers |

`rocky-iroh` `foreground` binds a replacement endpoint with the same options
(`crates/rocky-iroh/src/lib.rs`, `foreground`). With a fixed port that bind
always fails, presumably because the closed endpoint still holds the socket,
and nothing retries afterwards. Fixed ports are what
[always-on peer](../../docs/runtime/always-on-peer.md) recommends ("use a fixed
UDP bind"), and any app that pins a port for LAN reachability is affected.
Detection is also missing: nothing notices the dead endpoint until the app calls
`network_foreground`, and `sync` keeps returning `ok` (finding 11).

```sh
./target/debug/rocky-chaos scenario foreground_rebind
```

## 5. One dead horizon participant freezes the scope for everyone

The issuer declares a horizon on `notes` while participant C is dead.
Immediately, writes on the issuer and on healthy member B are refused:
`horizon cut is sealed; finalize or rebootstrap before writing`. An issuer
restart does not help. The freeze lasts until C's deadline, which is up to 30
days (`HORIZON_WAIT_SECONDS`), unless an operator revokes C. After revocation,
`maintain` finalizes, the issuer writes again, and B writes after its next sync.
[Mergeable horizon](../../docs/runtime/mergeable-horizon.md) documents this as an
availability cost. Recording it here because one lost phone can stop a whole
household's writes for a month, and nothing in the status tells the user why.

```sh
./target/debug/rocky-chaos scenario horizon_freeze
```

## 6. `identity.json` behind `rocky.sqlite`

After a list key arrived, `identity.json` was replaced with the copy from
before (models a backup that restores one file, or any crash path that commits
SQLite before the settings file). The node opens, but every unfiltered read of
a partitioned table fails (`a partial table needs held partition equality/IN`)
and the late list reads `named partition is not held`. Syncing never repairs
it. The two stores have no cross-check on open. Deleting the WAL after a kill,
on the other hand, recovers cleanly (integrity `ok`, converged in one round).

```sh
./target/debug/rocky-chaos scenario file_damage
```

## 7. Readiness claims completion from a partial peer

The issuer enrolls full node F, shares all lists, then dies before F syncs. F
can reach only a partial peer. After three syncs F reports
`bootstrap_pending: false` with 200 notes and **0 items**: the list envelopes
only travel from the issuer. F converges once the issuer is back, so it
recovers. However, an app gating "ready" on `bootstrap_pending` shows an empty
household as complete.

```sh
./target/debug/rocky-chaos scenario bootstrap_from_partial
```

## 8–11. API and diagnostics

- **Retryability is inverted in places.** These are `code: storage,
  retryable: true` although they never succeed: `PRAGMA journal_mode = DELETE`,
  `VACUUM`, `REINDEX`, `ANALYZE`, a partial-table read without a partition
  filter, `named partition is not held`, `held partition is incomplete`
  (finding 3). Meanwhile, a joiner writing to a shared list during its import
  gets `identity allocation requires a writable table` with `retryable: false`
  (`writes_during_bootstrap`), although it succeeds once the import ends. An
  app that honours the flag loops forever in the first case and gives up in the
  second.
- **Misleading messages.** A peer closing a link surfaces as `local storage
  I/O: unexpected end of file` (`rocky-storage` `Error::Io`), and resuming a
  directory without `identity.json` gives a bare `No such file or directory`.
- **One session, one transaction.** Concurrent callers on one session share its
  transaction: of 8 concurrent insert transactions, 1 succeeded and 7 got
  `cannot start a transaction within a transaction`. A failed caller's
  `ROLLBACK` aborts whichever transaction is open, including another caller's.
  No partial apply was ever observed; this is a contract to document or
  enforce.
- **`sync` reports `ok` on total failure.** The status is `ok` when every
  attempt failed or all peers are in backoff; only the nested attempts show it.

## Verified robust

Each of these ran at least once with the stated checks passing:

- SIGKILL of a joiner at 1, 200, 700 and 1,200 of 1,087 checkpoint rows, resume
  each time: converged, `integrity_check` ok.
- SIGKILL of the checkpoint signer mid-import: the joiner finished from the
  other full node within 20 s; all three converged once the signer returned.
- Inserts, updates and deletes on both sides during an import: converged, and
  no acknowledged write was lost.
- 30 rounds of link faults and networking/background/sync toggles during an
  import (joiner on an ephemeral port): converged in one heal round.
- Revoking an importing device and one with unsent writes: both report
  `revoked`, keep local reads, reopen; a new member bootstraps; unsent writes of
  the revoked device stay out (by design).
- 36 hostile SQL statements (ATTACH, DROP, `writable_schema`, writes to
  `secure_*` and `sqlite_master`, forged ids, unbounded recursion, 900 MB blob,
  dangling transactions): all refused or bounded (2 s interrupt, 16 MiB result
  quota); integrity ok, convergence intact.
- SIGKILL with an open transaction: nothing leaked. Kills racing COMMIT: reopen
  and convergence fine.
- Graceful close during bootstrap, sync and subscription: closes (≤ 5 s),
  reopens, resumes the import.
- Same directory opened twice: refused (`data directory is already in use`);
  stale lock files after SIGKILL do not block reopen.
- 600 random and malformed native requests: no hang, crash or divergence.
- A trusted device killed mid-import enrolled a new device while the issuer was
  down: it served no incomplete checkpoint, and all three converged later.
- Partial role set after join (as `ShoppingNode.join` does): the partial never
  held an unshared list.

## Fuzzing

With the final scheduler, ten seeds of 150 steps (21–25, 31, 32, 41–43) all
converged in the first heal round. Every acknowledged insert survived, every
node reopened, and `integrity_check` passed. Across the logged runs that is
roughly 90 plain SIGKILLs, 84 SIGKILLs during a sync, 24 new members killed
during their bootstrap, 62 graceful closes during a sync, 64 concurrent
transaction bursts, 66 hostile statements, 133 networking/lifecycle toggles and
about 600 row writes. Earlier batches (seeds 1–3 and 11–14) also converged.
Their scheduler often left every node dead, so they count for less. Two
divergences seen along the way were harness errors: a share naming the sharer
itself, and re-sharing before the envelope had arrived. Both are fixed.

The fuzzer avoids socket faults (finding 4) and eviction (finding 3), because
each would fail every seed; it also does not revoke devices.

## Limits

- Loopback iroh only: no relay, NAT, discovery, mobile lifecycle or power loss.
  SIGKILL keeps OS page cache, so buffered-durability loss is not exercised;
  the WAL deletion in finding 6 is the closest model.
- Retention with window 0 covered no operations in these topologies, so pruned
  history and behind-horizon rebootstrap were not reached by the fuzzer.
- Strong commands, the coordinator, custody-only peers (`rocky-peer`), backup
  and restore, and Room/Kotlin layers were not exercised.
- Timings are single runs on one machine; windows such as finding 1's are
  hardware-dependent, so the probes sweep a range of offsets.
