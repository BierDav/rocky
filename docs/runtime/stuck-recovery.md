# Stuck recovery

A coordinated domain is stuck when every node of its in-sync set is lost: the
coordinator knows the commit index, but no reachable trusted node holds it.
[Coordinated writes](coordinated-writes.md#catch-up) first collect the missing
entries from any device that still holds them. What no device holds is
abandoned only by an owner-signed cut ([decision 0074](../decisions/0074-owner-signed-recovery-cuts.md)).

## The cut

A `rocky_credentials::SignedCut` names the exact head it replaces `(a, r_a,
epoch)`, the retained prefix end `(c, h_c)`, the last position its proposer
verified `(k, r_k)`, its sequence in the log and the previous cut's digest.
The domain's control authority signs it; the same key holds a publisher-rooted
issuer chain granting whole-scope writes over every scope of the domain, or
is the publisher itself. The chain travels beside the record as its
authority; together they are a `CutEvidence`.

The [coordinator](coordinated-log.md#owner-signed-cuts) commits the cut as the
entry at `a+1` whose hash is the record's digest, so positions are never
reused: `c+1..=a` stay abandoned and new entries start at `a+2`. The in-sync
set empties, so every trusted node applies the cut before it writes again.

## Proposing and submitting

`EncryptedReplica::cut_proposal` first collects what reachable devices hold,
then proposes a cut of the exact head that retains this node's position. It
refuses with `Behind` until the node holds every earlier cut and the prefix
the last one retained, and with `Conflict` while an entry is pending. Its base
report lists each projected scope of the domain and whether its verified
checkpoint survives the cut; one that does not fails closed with
`cut_base_missing` after the cut. The report covers this node's own bases;
peer bases are classified when offered for import.

`CutProposal::sign` signs with the owner's key and pairs the issuer chain.
`submit_cut` checks the owner locally, flushes the record, then sends it.
A lost reply keeps it, and the next resolution sends the same record again;
a refused one is discarded unless the head's window shows it was admitted.
The node admits and rolls back the cut once the coordinator commits it.

## Fencing

Every device keeps every verified cut of a log, in order; a record whose
predecessor is missing waits. A frame whose signed bundle position `p` lies
in a held range `c < p ≤ a` is consumed without effect: its status becomes
`abandoned`, its author log continues and checkpoints cover it, but it is
never projected. A later write to a row only an abandoned frame created is
consumed the same way. A frame naming a cut's own position is refused.

| Message | Cut records |
| --- | --- |
| Range pull | The frontier states the cut count held; the page carries the rest before its frames |
| Subscription pull | The request states held counts per log; the page carries the rest |
| Push | The records the peer's frontier lacks, before the frames |
| Checkpoint offer | The records the offered checkpoint's binding names |

So every holder of a frame after a cut holds the cut. Trusted nodes take cuts
from the coordinator head only; members take them from peers. Keyless custody
relays them with frames.

## Checkpoint binding

A checkpoint statement (format 11) records `coordinated: { cuts, cut,
highest }`: the number and digest of the applied cuts of its domain's log and
the highest entry position among the frames it covers. Against the held cuts
it is clean when, for every held cut it did not apply, `highest ≤ c`; a cut
count with another digest is another history and taints it. A binding naming
more cuts than the device holds is refused as not ready until they arrive.

No checkpoint is sealed over a tainted base, and the verified import refuses
a tainted checkpoint. A base finalized by a [mergeable horizon](mergeable-horizon.md)
is classified the same way; a device never falls back to an older base.

## Rollback

Admitting a record starts five steps. Each commits its work with its progress
in `secure_cut_work`; observers are notified after the commit, and a crash
resumes at the cursor when the replica opens again.

1. **Admit.** Verify the record, its authority and its place after the held
   cuts; store it with the work row and flush. Fencing applies from here.
2. **Classify.** In pages of 256, mark applied frames in the abandoned range
   `abandoned` and drop the log's entries there. A retry of their mutation
   handle reports `abandoned` instead of success.
3. **Rebase each scope.** A clean base runs the ordinary rebuild, which skips
   abandoned frames; rows, projection and revision change in that
   transaction. A tainted base installs a durable guard instead.
4. **Head.** A node that holds the retained prefix at `c` records the cut's
   position and running hash; otherwise it stays `Behind`.
5. **Finish.** The work row is deleted.

Readmitting a held record changes nothing. A second record of a held sequence
is refused with `cut_conflict`: the owner signed two histories, which Rocky
detects but cannot repair.

## Guarded scopes

A guarded scope refuses writes and sync with `cut_base_missing` and reports
itself behind, so sync rebootstraps it. Importing a clean checkpoint through
the verified import (`begin_rebootstrap`, pages, `finish_rebootstrap`) lifts the
guard in the switch transaction; ordinary local writes made since replay on
top. Reads still return the stored rows until then. With no clean base
anywhere the scope stays guarded: Rocky does not endorse a tainted base, and
replacing the domain is the application's decision.

## Catch-up across a cut

A trusted node at `c` moves straight to `a+1`. Positions before the hash
window are verified only by a held cut: the served entries must chain from
the node's own running hash to `h_c`, checked before any of them applies.

## Formats

| Format | Identity |
| --- | --- |
| Encrypted replica | `rocky.encrypted-replica.v21` |
| Checkpoint statement | 10 |
| Live link | `rocky.link.v6` |
| Coordinator store and snapshot | 5 |

Every other version is refused.
