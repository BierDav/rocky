# Coordinator retention and command recovery

Bounded admission and retirement in `rocky-coordinator` (`retention.rs`,
`machine.rs`, `raft_store.rs`, `client.rs`). The coordinator retains generic
receipts, never application payloads. A [public service](public-coordination.md)
applies the same windows per tenant. Evidence: [coordinator evidence](coordinator-evidence.md#retention-probe).

The [coordinated log](coordinated-log.md) instead retains one latest join/append
outcome per trusted node and 1,024 committed hashes, plus its running hash. The
first admitted node request reserves 60,928 bytes against tenant and mesh limits;
window growth cannot exhaust commit capacity. Partial storage acknowledgments are
leader memory; resolving Raft entries bind signed proofs and snapshots keep only
committed member positions. Its evidence is [recorded separately](../features_09/08-coordinated-log-evidence.md).

## Request identities and windows

Request sequences are per domain and per invoking node. The upper 40 bits are
the issue time in Unix seconds; the lower 24 bits distinguish requests issued
in that second. Issuers durably allocate strictly increasing sequences,
independently of replication author counters. A request binds its scope, node,
principal and immutable input commitment; secure invocation signatures bind the
same fields, and certified controls bind invocation keys to exact domains.

An identity may be confirmed for 24 hours after issue, at most 300 seconds
ahead of authority time (further ahead gets `Backpressure` until authority time
catches up). Its response stays retrievable for seven days after issue unless
acknowledged earlier. Renewal issues a fresh increasing identity; it never
reopens an old identity's admission. A retry must keep the same identity. Local
clock rollback still yields increasing sequences.

| Coordinator answer | Meaning | Caller action |
| --- | --- | --- |
| Retained receipt | Confirmed exactly once | Retrieve matching app payload, verify evidence, apply atomically |
| `None` from lookup | Admission open; confirmation may still arrive | Keep the request pending; retry this identity |
| `Expired` | Admission closed without confirmation | Persist `expired`; no automatic resubmission |
| `Retired` | Floor or recovery horizon passed; outcome unavailable | Persist `retired`; reconcile app state/effects before any explicit new action |
| `RequestConflict` | Same retained identity, different input hash | Stop and diagnose changed input |
| `Backpressure` | Session count/bytes, tenant quota or forward clock skew | Retry the same identity after capacity/time recovers |
| `Unavailable` | No authoritative answer | Keep outcome unknown and retry the same identity |

A lost reply can become retired even though the command committed. Expiry is not
rollback. Neither an empty live lookup nor a transport failure proves nonexecution.
Trusted `CommandState` and secure/SDK `TicketState` expose `Retired`/`Expired`;
runtimes persist these terminal states before reporting them, they survive
reopen and the handler never runs again. Kotlin exposes the same states, with an
unavailable typed outcome until result evidence is recovered.
`Delivery::Settled` carries an executor's terminal answer back to the submitter.

## Acknowledge only durable knowledge

`Coordinator::retire(scope, node, through)` says every own identity through that
sequence is settled. It closes unconfirmed gaps too; neither a higher confirmed
sequence nor lookup absence closes a gap. Only the issuing node's key may sign
it. Never acknowledge work whose outcome you still need to recover.

The trusted runtime acknowledges its settled prefix after verified local application
and configured peer application; peers explicitly behind use checkpoints. A local
release alone does not acknowledge execution. In the secure profile,
`acknowledge_tickets` retires only identities whose results other devices can
still verify. Those are identities with no result, results whose receipt the
coordinator exports as portable evidence, and results that retention already
pruned past every required reader. Each scope's prefix stops at the first
identity that is not yet safe, and later calls resume it. Applications may call it
at any time ([decision 0047](../decisions/0047-verifiable-identity-retirement.md)).
With `MemoryCoordinator`, which exports no evidence, unpruned results stay until
the horizon. Nothing pins coordinator responses past the horizon: outstanding
responses consume the 32-entry / 64 KiB session window, and abandoned sessions
expire instead of reserving storage.

A compact per-session floor rejects every older sequence, so copying an old
signed invocation cannot resurrect it. Floors survive until their issue range is
older than the seven-day horizon; then timestamps alone reject the covered
identities and the session record is removed. There is no per-device revocation ledger.

## Keep verifiable results on application nodes

Export `NetworkCoordinator::evidence(receipt)` with the matching application result
and the independently root-pinned `CertifiedGenesis`. Import with `retain_evidence`
before application. The client atomically persists evidence before reporting a
receipt, and verifies it offline after server retirement, voter membership changes,
control changes and client restart; the 3–7 candidate voter keys in genesis are
the whole historical trust universe. A changed signature, result or commitment
fails verification. Without evidence, a retired receipt reports `Retired`.

Secure peers serving a confirmed result send the evidence with it; the receiver
verifies it before applying, so a second executor or custodian can assemble
confirmed state after the identities retired
([design rationale](encrypted-commands.md#portable-result-evidence)).

Only client evidence format 3 is accepted, with explicit acknowledged floors;
other versions are refused before lock-file or cache writes. No cache is
migrated. The client evidence file (at most 32 MiB) holds at most 4,096 receipts
and 64 observed heads. When full, it evicts the oldest receipt that is neither
the newest of its scope (the head-regression watermark) nor this client's own
receipt above its acknowledged floor; if every receipt is pinned, new evidence
gets `Backpressure` until the client acknowledges. Exceeding the encoded
32 MiB file bound also returns `Backpressure`, without replacing the durable
file. Eviction precomputes the newest sequence per scope in one cache pass.
Export evidence needed for long-term history before eviction. Importing an old receipt never rewinds the
live head. Unknown formats are refused with `UnsupportedFormat`; formats 1 and 2
still migrate on open until the
[compatibility removal](../features_06/01-remove-compatibility-code.md).

## Authority time and operation

The leader proposes authority time with each Raft entry; the state machine clamps
backward jumps to its durable high-water mark. Followers and replay never read
wall clocks; network frame freshness uses local clocks only for admission. A
forward clock jump can expire work early: correct clocks, never reset committed time.

Administrative requests (controls, acquire/takeover, closure, registration,
membership) bind signer, operation, ID and signed issue time. Retry outcomes
live one hour, at most 64 per signer and 4,096 per mesh. Controls require the
next revision; takeovers the exact epoch; closure the exact head. Membership
plans also bind a monotone `expected_revision`, preventing replay after a
three-to-two-to-three cycle.

Owner closure keeps a bounded final-head marker; a closed identity never reopens.
Quiet open domains keep their heads. A root-bound mesh has at most 64 scopes,
including closed ones; a public service at most 4,096, and each tenant's
closed-domain markers count against its own byte budget, so churn ends in that
tenant's `Backpressure`, not service growth.

## Storage formula and budgets

`state <= live_domains * domain_budget + live_sessions * session_budget + bounded_response_bytes + bounded_consensus_storage`

| Component | Default / maximum |
| --- | --- |
| Admission / response horizon / forward skew | 24 h / 7 days / 300 s |
| Session response count / bytes, including floor | 32 / 64 KiB |
| Sessions per domain | 256 |
| Open domains / accounted bytes per tenant | 16 / 4 MiB |
| Accounted state per mesh (`total_bytes`) | 24 MiB, at most 3/4 of the object ceiling |
| Root-bound scopes, including closed markers | 64 |
| Domain control keys | 32 executors / 64 invokers |
| Mutation retry records | 64 per signer / 4,096 total, 1 h |
| Persistent encoded object / snapshot input / client evidence file | 32 MiB |
| Consensus log files / encoded entry | 2,048 / 64 KiB |
| Snapshot threshold / retained log tail | 1,024 entries / 8 entries |
| Retained snapshots / transfer chunk | 2 / 64 KiB |
| Client receipts / observed heads | 4,096 / 64 |

`Limits` are fixed when a store is provisioned (or by a public genesis) and
`Limits::validate` refuses inconsistent values. Session accounting is 64 bytes
per node/floor plus 16 bytes and the encoded receipt per retained response; a
domain adds 1,024 bytes and its encoded control. This is an admission budget,
not a measurement of RSS. The encoded-object ceiling independently fails closed.

Logs are separate synced files; hard state is an atomic checksummed file
(format 4; other formats, including the old single-image `raft.cbor`, are
refused with `UnsupportedFormat`, never reset). Snapshots preserve floors,
time, controls, heads and retry windows together. Physical purge waits for a
durable snapshot covering the purge point; an interrupted pending snapshot is
removed on reopen. At the hard caps, logs plus three snapshot files occupy at
most 224 MiB of logical bytes. An exhausted consensus cap stops storage
acknowledgment rather than accumulating backlog. Preserve `PROVISIONED`, hard
state, snapshots, required logs and client watermarks together; backups keep
the same bounded retention, never a lifetime archive.

## Design rationale

- Raft log compaction alone does not bound state: retain-all receipts and
  unbounded session/domain churn grow forever, so retirement needs timed
  identities, floors and quotas.
- Timed identities plus floors replace a lifetime ledger of revoked devices:
  after the horizon, issue time alone rejects old identities.
- Exact historical lookup is not promised forever; application nodes keep
  signed evidence, so no historical server configuration ledger is needed.
- An expired identity is never confirmed, a retired one may have succeeded;
  neither allocates a replacement identity automatically.
