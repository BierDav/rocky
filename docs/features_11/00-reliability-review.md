# Reliability and performance review

Status: findings, 2026-10-25. Source: a read-only audit of the replica,
link, SDK recovery, storage and coordinator code at `0b0527df` plus the
manual device testing of 2026-10-24 (an Android phone and two JVM desktops,
one household and an isolated 100k load). [features_11](README.md) is the
response. Line references are to that revision; source code is the
authority once phases land. Numbers are single runs, not service levels.

## What the device testing showed

features_10 met its gates on the in-process fixture: a 100,000-row member
catches up in about 25 s and its file is 164 MB. On real devices the same
release:

- left a Full phone unable to write from 08:30 to 16:47 UTC behind
  verified-seal gates. Eleven builds each removed one blocker: a fatal read
  refusal in Room, unused partitions as unverified scopes, recovery asking
  a peer outside the scope, held proofs requiring the same tip, one
  unverifiable proof ending other scopes, lost retained line signers, reset
  replay leaving frames pending, import intent mistaken for missing
  history, and global readiness checks on every statement;
- repeatedly closed links with `link channel queue quota`, native-read
  and request-channel timeouts, and admission timeouts at 10 s;
- sent a 354,150-byte placement response against the 262,144-byte message
  limit, because its page bounded headers, not bytes;
- left two delivered frames pending on a desktop until a restart without
  networking applied them.

| Measure (2026-10-24, isolated 100k load) | Value |
| --- | ---: |
| Issuer author and publish, 100,000 rows | 96.7 s |
| Issuer file after close | 412.7 MB |
| Desktop member bootstrap | 24.8 s |
| Phone member bootstrap (50.7 MB received) | 215.3 s; 323.5 s in an earlier run |
| Member file after close (both member kinds) | 161.7 MB |
| One-row incremental sync, phone ↔ desktop | 0.8–9.1 s |
| One-row incremental sync, desktop ↔ desktop | 0.08–2.7 s |
| Empty reconciliation over relay | 10.43 s |

The in-process fixture, from the features_10 phase 4 evidence: fill 97.8 s,
publication 71.0 s, closing compaction 64.1 s, member catch-up 24.5 s,
member main file 163.8 MB, source main file 643.9 MB.

## Findings

1. **One thread does everything.** `Database::start` runs every database,
   node and link task on one `LocalSet` thread
   (`rocky-sdk/src/database.rs:281-293`). Storage `async` functions are
   synchronous (`rocky-storage/src/sqlite.rs:704-740`). Group flushes call
   `F_FULLFSYNC` on that thread (`replica/group.rs:245`), as do about 60
   `inner.flush()` calls held under the replica mutex; only
   `EncryptedReplica::flush` uses `spawn_blocking` (`replica.rs:1297`).
   While a page applies or a seal finalizes, link readers and writers do
   not run and the peer's 10 s deadlines expire.
2. **A late reply closes the link.** A requester that times out drops its
   `Io`; `mark_closed` keeps the slot because its sender is still present
   (`network/link/channels.rs:654-686`). The late final fragment's
   `try_send` fails on a closed receiver and is reported as
   `link channel queue quota` (`:196-200`), and `accept(frame)?` ends the
   link (`rocky-sdk/src/node/live_links/liveness.rs:46`).
3. **One failure ends more than itself.**
   - Any `Err` from `serve_all` closes the link (`liveness.rs:38`). There
     is no refusal reply, so a client cannot tell refusal from loss.
   - The client uses `?` across scopes in `rocky-sdk/src/sync.rs:350-471`,
     and bootstrap errors return before row reconciliation
     (`node.rs:2932-2937`).
   - `rebootstrap_at_seals` stops at the first failing scope
     (`network/checkpoints.rs:203-230`).
   - In apply, an error that is neither retryable nor a semantic refusal
     ends the pass for every log in the group (`replica/incoming.rs:87-99`,
     `:265`). Heads are ordered by sequence, so the same frame fails first
     every time.
   - More than 8 pending link handlers, pushes included, closes the link
     (`liveness.rs:81-83`).
4. **Recovery has no driver.** A successful exchange always returns
   `retry = false` (`sync.rs:142`). `node.rs:2470` then clears the peer's
   pending flag even while scopes need rebootstrap, bootstrap found no
   same-tip peer, or the transfer reported waiting scopes. Local steps
   (`FrameReplay::Local`, `restored()`, `finish_seal_rebootstrap`) run only
   inside a peer session after admission. `secure_dropped_tip_frames` is
   never cleared. Seven `Waiting` reasons in `apply_frame`
   (`incoming.rs:170-225`) are not recorded; a frame that fails to decrypt
   under a held key waits forever.
5. **Gates are domain-wide and repeated.** `require_workspace_base` and
   `require_scope_readable` (`replica/compaction/rebootstrap.rs:72-130`)
   refuse every scope of a domain for one scope's state. `begin` checks
   once per session scope (`replica/sql.rs:660`); `table_complete` checks
   every partition scope against every domain scope
   (`replica/partition.rs:438-446`). One statement is parsed five or six
   times. Reads take the writer lock twice (`replica/queries.rs:222-294`)
   and each cursor opens a new thread and connection
   (`rocky-storage/src/cursor.rs:271`).
6. **Nothing explains a stall.** No crate depends on `tracing` or `log`.
   Status has `bootstrap_pending` and `imports`; a refused write names only
   the first failing gate. Both manual investigations built temporary
   probe builds to see why a frame waited.
7. **Responses bounded by count, not bytes.** Tip pages (32 statements, one
   finality statement up to `MAX_FINALITY_BYTES` ≈ 513 KiB against the
   256 KiB `MAX_SECURE_FRAME`; `network/tips.rs:5,109-112`), frontier
   batches (64 scopes × 256 entries; `network/frontiers.rs:45-71`) and
   holdings proofs (`network/link.rs:488-504`) have the placement bug's
   shape.
8. **Sync is lock-step.** A range page costs a request, the page, a
   separate `Held`/`Acknowledged` round trip and its apply
   (`network/rows.rs:299-371`); the next request already carries the same
   frontier. Scopes go one at a time over one channel. Every session pulls
   and pushes the whole tip graph when digests differ (`network/tips.rs`).
   Checkpoint import keeps one page in flight. All six channels share one
   QUIC stream, so one lost packet stalls control and bulk alike.
9. **A frame is opened four or five times.** At receive
   (`replica/group.rs:512`), in `dependencies_ready` when a sealed base
   exists (`incoming.rs:324`), in `apply_frame` (`:221`) and twice in
   `row_payload` (`replica/apply.rs:151,499`), although `apply_frame`
   already holds the payload. The credential chain is verified in receive
   and again in apply, uncached, one Ed25519 check per link plus a
   revocation scan per link (`rocky-credentials/src/revocation.rs:283`).
10. **Rows are wide at rest.** In the 100k member: `secure_row_keys` 37.2 MB
    and its index 39.3 MB (scope stored as a 32-byte blob, the key twice;
    `replica/identity.rs:25-26`); `secure_checkpoint_rows` 27.4 MB with the
    same payload as `secure_rows` (29.4 MB), as a rowid table with an extra
    autoindex; `secure_creation_headers` 14.7 MB of pages for 5.9 MB of
    payload. `items` is 5.1 MB. Per row, apply reads the record twice
    (`apply.rs:193,577`), deletes from `secure_move_rows` unconditionally
    (`identity.rs:247`) and decodes the record about five times. The
    statement cache holds 128 of about 790 distinct statements; cache,
    mmap and temp-store pragmas are SQLite defaults; there is no
    `auto_vacuum`.
11. **Scans that grow with history.** Placement serving reads every header
    and body of `secure_operations` and `secure_voids` into memory per
    request (`replica/placements.rs:187`, again at `:424`).
    `refresh_closed_held` scans every applied or pending operation across
    scopes per held page (`replica/held/late.rs:218-415`). `maintain`
    re-runs domain checks per written scope and loops unbounded under the
    lock (`replica/checkpoint/create.rs:84-95`, `replica/compaction.rs:1160`).
12. **Coordinated writes lose work on a timeout.** Any append error,
    `Unavailable` included, aborts the staged entry
    (`replica/coordinated.rs:852`), although the coordinator may already
    have assigned it; the log then blocks until the deadline and the
    in-sync set is evicted. Deadlines come from the device clock
    (`replica/coordinated.rs:435`) and skew surfaces as `Conflict`. The
    proposer re-sends every frame and the full head every 10–500 ms
    (`:942-984`). The Raft store syncs files under its state lock on the
    async runtime (`rocky-coordinator/src/raft_store.rs:801-818`).
13. **No adversarial schedule is tested.** Tests are hand-written
    scenarios. The in-memory hub (`rocky-sdk/src/transport/memory.rs`) can
    stall, cut and sever, but has no seeded loss, latency or reordering,
    and no test runs random multi-node schedules with restarts and branch
    switches. Every blocker in the device log would have been a failing
    seed.
14. **Backoff synchronizes and never grows.** `Retry::failed` uses 1, 2, 4
    … 60 s without jitter (`rocky-sdk/src/scheduling.rs:11-17`), and a
    successful link open resets it, so a link that opens and then fails
    retries every second.

## What was sound

Observers and outgoing publication happen only after commit; projection,
operation, outbox and revision commit together. Coordinated joins and
appends are deduplicated by a persisted per-node sequence. Checkpoint
import resumes from a stored cursor and opens pages off the replica
thread. Production code in storage, runtime and core has no casual
`unwrap`. No transaction is held across a network send.
