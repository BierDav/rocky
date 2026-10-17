# Coordinated tip logs

Implemented in `rocky-coordinator`, independently of application bundle storage.
[Decision 0072](../decisions/0072-coordinated-log-and-writes.md) fixes the ordering
and durability contract; [decision 0075](../decisions/0075-generations-as-sealed-forks.md)
gives each tip its own log. [Shared coordination](shared-coordination.md) describes
certified voters, and [public coordination](public-coordination.md) describes
registration and tenant isolation.

## One log per tip

Every log is a domain's log of one tip, `rocky_core::TipLog { scope, tip }`;
the coordinator treats the `TipId` as opaque. Each tip's log has its own head,
commit index, hash window, in-sync set and minimum, and its genesis anchor
`log_genesis` names the tip. The domain's control stays shared: trusted
identities, control revisions and the default minimum.

## Configure and join

The control certificate lists trusted identities; a node presents the
certificate its identity signed for its key with every request. The in-sync
set still counts nodes, each recorded with its identity. An ordinary member
cannot read or mutate this metadata log. The control authority sets the
domain's default minimum with `Operation::Configure { scope, minimum }` before
any node joined one of its logs (`NetworkCoordinator::configure`). Default:
one. A tip's opening names its own minimum: the authority or a trusted node
relays it as `Action::Configure { minimum }` (`DomainLog::configure`) before
the log's first join; a log nobody configured takes the domain's. Trusted
nodes relay it for every tip of the opened line before they join. Only a
configuration, a join or a terminal entry opens a log; a domain holds at most
8 open logs and refuses more with `Backpressure`. A node is in sync on one
open tip of its domain: joining another leaves the first and moves its epoch,
and waits with `Backpressure` while the first has a pending entry. A tip no
trusted node follows has an empty set that takes no append. A control revision cannot
remove an identity whose node is in sync or occur while an entry is pending;
renew it before expiry. Removing a key that already missed a
write does not erase its bounded retry watermark. Exhausting the domain's retained
node slots refuses further identities with `Backpressure`.

`NetworkCoordinator::coordinated_head(log)` returns the membership epoch, set,
minimum, assigned head, commit index, running hash, pending descriptor and retained
hash window. A node catches up from trusted peers, then uses
`NetworkCoordinator::coordinated(log, Action::Join { position, running_hash,
sequence })`. An unopened log reads as its genesis head. Both position and running hash must match current committed state.
Joins are refused during an entry; already joined nodes need no idle renewal.

Persist strictly increasing join/append sequences per log and node. Retry the
same sequence and identical action after a lost reply. Its first retained outcome
is returned, even after the entry resolved. Only the latest identity per node is
retained; an older sequence returns `Retired`, changed input returns
`RequestConflict`. Bounds refused before retry-state admission hold no outcome.

## Assign, store and resolve

Append `Action::Append { expected, epoch, hash, deadline, sequence }` against the
current committed head and epoch. It returns `Result::Assigned(pending)`; this is
an assigned, uncommitted entry. A second append at that head returns `StaleHead`.
The deadline is an absolute authority-time second, within the 30-second cap.
A set below its minimum returns `Backpressure`.

After storing that exact entry durably, each in-sync node sends
`Action::Acknowledge { pending }`, including the proposer. `Acknowledged` means
only that the current leader has its promise. Resolution verifies its signature
against the control valid at the acknowledgment's signed time, so later control
expiry cannot strand an admitted entry. A leader change loses that partial
knowledge, so nodes acknowledge again. The final acknowledgment resolves through
Raft and returns `Committed { position, running_hash }`; read the head after any
unknown outcome. No row data reaches the coordinator. Staging, storing,
applying and publishing bundles are [coordinated writes](coordinated-writes.md);
they reach this log through the `rocky_core::DomainLog` port, which
`NetworkCoordinator` implements and `coordinated::MemoryLog` runs in process.

A pending-entry timer commits at its deadline with the nodes that acknowledged
if they meet the minimum; otherwise it returns an aborted history through the
head: the pending descriptor disappears, the commit index stays, and the assigned
head returns to that index. In both cases missing nodes leave. Membership epoch
changes fence old acknowledgments, including a later attempt at the same position.
A node outside the set must catch up before rejoining. An entire missing set
cannot invent a committed continuation; only the owner's
[recovery seal](stuck-recovery.md) ends such a log.

## Terminal entries

A seal of the tip, or a bookmark move off it, ends its log:
`Action::Seal { terminal }` (`DomainLog::seal`) carries a `LogTerminal`, the
statement's opaque digest and the continuation tip a `continue` seal opens.
The coordinator never reads the statement. Before the log committed any
entry anyone of the authority or the trusted nodes submits it; afterwards
only a node of the in-sync set, which holds every committed entry, so a
seal never leaves one behind. It commits at once at `commit+1` with the
digest as its entry hash, aborting a pending entry, clears the in-sync set
and moves the epoch.
`LogHead::terminal` reports it. A retry of the same terminal returns the same
commit, whoever sends it; any other entry, join, configuration or terminal is
refused with
`CoordinationError::Sealed`: the first admitted ends the log, and no entry
commits after it. Which of concurrent seals wins is the devices' graph's
decision: by authority, then the lower digest
([races](tips-and-bookmarks.md#races-and-revocation)). The sealed state is every entry
committed before it; what happens to an aborted pending write is in
[coordinated writes](coordinated-writes.md#seals-and-moves).

## The owner's recovery

`Action::Recover { seal }` (`DomainLog::recover`) carries the owner's
encoded recovery seal of a stuck tip: the one terminal entry committed
without the in-sync set. The coordinator checks that the domain's
authority signed it directly, that it names this tip and the exact commit
and running hash, and that its retained prefix chains through the window;
a prefix ending before the window is endorsed by the signature alone. It
then commits like any terminal. A retry of the same seal answers the same
commit; a moved head is `StaleHead`. See [stuck recovery](stuck-recovery.md).

## Retention and limits

The current log machine fixes caps at 32 trusted keys per live tenant and domain,
60 admitted appends per authority minute per tenant across its logs, 8 open and
8 retained ended tip logs per domain, and a 30-second acknowledgment deadline.
Beyond 8 ended logs the earliest ended one is dropped and reads as unopened;
devices never write a tip their graph sealed, and a trusted node holding
committed entries of a dropped log refuses to write it
([limits](coordinated-writes.md#limits)). Existing `Usage` byte/domain budgets remain fixed at
provisioning or root-certified for a public service. Changing the log's fixed
policy requires a new store format; it does not add fields to signed genesis.
Retries consume no additional append rate. Quiet domains generate no log traffic.

The first admitted node request reserves 60,928 accounted bytes per tip log for
bounded members, retry outcomes and the full window; this is admission accounting, not
RSS or actual encoded size. Commits cannot need more accounted capacity after an
append. The newest 1,024 hashes occupy a 32,768-byte string; their positions are
`commit - hashes.len() + 1` through `commit`. The running hash persists after
older entry hashes are dropped. Each kept terminal acknowledgment adds its
record plus 64. Store and snapshot format 9 holds the tip logs, terminal entries and
their [acknowledgments](seal-finality.md), and requests sign as `rocky.coordination-request.v6`; every
other format is refused before writes. See [retention](coordinator-retention.md).

## Evidence

[Phase 8 evidence](../features_09/08-coordinated-log-evidence.md) records the
machine, durable replay, leader-change and deadline gates, measured encoded
footprint and per-write message count. This log supplies no proof that an
application has stored a bundle beyond its trusted node's signed promise.
