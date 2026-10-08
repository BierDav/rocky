# Coordinator qualification evidence

Native macOS/loopback evidence for [shared coordination](shared-coordination.md),
[retention](coordinator-retention.md) and [public coordination](public-coordination.md).
Not claimed: WAN or Internet relay/NAT, production throughput or sustained load,
hostile connection floods, disk-full recovery, hardware power cuts, mobile
behavior or Byzantine voters.

## Reproduce

```sh
cargo test -p rocky-coordinator --locked
cargo test -p rocky-coordinator --test applications --test cluster --test public --test public_runtime --locked -- --nocapture
cargo test -p rocky-coordinator --lib coordinator_state_plateaus --locked -- --ignored --nocapture
cargo test -p rocky-coordinator --no-default-features --locked
cargo test -p rocky-sdk --test service --locked
cargo test -p directory-app --locked
./scripts/check
```

Multiprocess tests start three independent `rocky-voter` processes with separate
keys and persistent stores on OS-assigned loopback ports; tenant requests use
iroh, administration and consensus TCP. The harness certifies a plan and
discards the root secret before spawning, polls bounded conditions instead of
sleeping, and kills/reaps children on success and failure. The upstream
`openraft::testing::Suite::test_all` runs against the persistent adapter.

## Coordination and crash boundaries

| Exercise | Established behavior |
| --- | --- |
| Bootstrap and authentication | Wrong roots or modified genesis fail; exact issuer/scope binding; exclusive store ownership; corrupted stores fail closed |
| Applications | Persistent Directory and Inventory executors share one voter set; Directory-issued credentials cannot open Accounting; application crates are dev dependencies only |
| Fencing and retry | Same/other-holder takeover, stale executor, altered request hash and reused transport ID fail; duplicate confirmations keep sequence and outcome |
| Membership/control | Joint-consensus 3→2→3 and duplicate plan IDs; control revision fences/reacquires without losing history |
| Quorum loss | Strong work stays queued/prepared/unknown while permitted mergeable edits commit; restored quorum completes the same command |
| Historical evidence | Imported receipt verifies with all voters down and after reopen; tampered evidence fails |
| Lost coordination | Deleted hard state with its provisioning marker cannot restart as genesis; client watermarks reject head regression |
| Crash points | Aborts before/after log sync, after commit and apply sync, before reply: each retried confirmation has one outcome and one successor |
| Snapshots | Interrupted generation/rename and transfer to a voter lagging beyond the log tail reopen safely; purge never passes a recoverable snapshot |

Fault injection is enabled only by `ROCKY_COORDINATOR_CRASH` or
`ROCKY_COORDINATOR_FAULT_FILE`; it is not a callable service API.

## Retention probe

`coordinator_state_plateaus` drives the persistent adapter through append,
commit, apply, snapshot and purge with deterministic authority time: four open
domains, eight invoking devices each, twelve confirmations per domain per day,
half the devices abandoning responses, weekly key rotation, daily takeovers,
two close-and-replace cycles and a mid-run reopen. Eleven weekly windows (one
warm-up): 3,696 confirmations, 5,954 Raft entries, zero capacity refusals. The
one-window variant runs in every workspace check.

| Gate | Budget | Last measured |
| --- | --- | --- |
| Post-warm-up maxima | within 125% of window 1 | passed; ~108 KB state, 180 responses |
| Snapshots / retained logs | ≤ 2 / ≤ 1,096 | 2 / 1,012 |
| Logical and allocated disk | ≤ 16 MiB | 1.05 MB logical (1.16 MB swap peak), 4.4 MB allocated |
| Encoded live metadata | ≤ 256 KiB | 129 KB |
| Process RSS | ≤ 128 MiB | 14.3 MB |
| Durable batch latency mean / p99 | < 50 / < 250 ms | 28.9 / 101.5 ms |

| Exercise | Established behavior |
| --- | --- |
| Reordered requests and holes | Higher sequence may confirm first; gaps stay open until acknowledged or expired |
| Retirement | Confirmed and never-confirmed prefixes reject execution forever, including an empty session |
| Abandoned sessions and churn | Response windows backpressure; horizon removes responses and old floors |
| Renewal / copied invocation | Fresh timed identities proceed; old admission never reopens |
| Clock rollback / leader change / snapshot | Authority time clamps backwards; expired stays expired |
| Quotas | New confirmations backpressure without evicting retained responses |
| Administrative retries | Per-signer quota and expiry; membership replay after a cycle refused |
| Closure | Exact-head close; old identity never reopens |
| Old or corrupt store | Explicit refusal, never genesis reset |
| Client evidence | 4,096 cap keeps per-scope newest evidence; imports cannot rewind heads; unknown formats refuse |
| Runtimes | Trusted and secure terminal `Retired`/`Expired` states survive reopen without rerunning handlers; acknowledgment follows verified (peer) application |

## Public service

| Exercise | Established behavior |
| --- | --- |
| Independent owners | Two publishers' tenants register different applications on the same voters |
| Isolation | Cross-tenant acquire, head, lookup, control, confirmation and registration fail; foreign or missing admission fails; tenant requests over voter TCP fail |
| Fairness | Tenant A fills its quota and floods its two lanes per voter with 64 requests on four connections; B registers and confirms within a 3 s budget (measured 274 ms, 60/64 A requests `Backpressure`, none unanswered) |
| Registration churn | Daily limits; closed markers count against the tenant's bytes; closed identities never reused |
| Administrative bounds | Lanes and retry budgets are scope-bound per tenant; refused registrations add no retry record |
| Outages | Leader loss preserves progress; quorum loss returns `Unavailable`; restart yields exactly one successor |
| Backup and restore | Running-voter backup, wrong-voter restore and nonempty destination fail; a restored voter stays fenced until readmission; all-voter restore stays unavailable |
| Application runtime | Directory over iroh survives quorum loss; voter files hold no user payload key |
| SDK composition | Issuer executes a typed command via a registered service; a second device joins with the JSON bundle; non-issuers cannot mint grants |
| Diagnostics | Status exposes bounded counters and sizes; logs carry no request contents |

Accounted state in these scenarios stays in single-digit KB (disk tens of KB).
