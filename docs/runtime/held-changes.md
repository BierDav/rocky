# Held changes

A device may commit changes that touch a table or field a newer
[generation](generations.md) [removes](removal.md) before it learns of the
removal. Sending such a change half would split its transaction; holding its
frames would block the author logs behind them. Instead, when the device
adopts the generation, the change is held whole, its frames are voided and
its effects rolled back; a newer application version re-authors or drops it.
[Decision 0069](../decisions/0069-held-changes-and-void-records.md) records why.

## What is held

A frame counts as *offered* once it may have left the device: a range or
transfer page served or pushed to a peer (a push that fails midway counts),
the first page of a checkpoint covering it, or a recovery package. Pages and
packages are marked offered in a flushed transaction before they are sent.
Pruning and eviction drop the mark with the frame.

When a device adopts a generation that removes a table or field, each bundle
whose every frame was never offered is held when any of its writes:

- names a table the generation removed, or
- writes a field it removed to other than the field's default (NULL, or the
  typed column's declared default).

A never-offered bundle that depends on a held one (it saw its rows) is held
with it. A bundle with any offered frame is not held: receivers apply its
kept fields, [field by field](removal.md#frames-and-checkpoint-pages).

## Holding

In the activation transaction the device:

- stores the held change: each row's table, row, kind and image with field
  names as its generation named them, which fields it writes, and when it was
  made;
- consumes each held frame's log position and deletes the frame as pruning
  does; a handle committed with it is refused afterwards like a pruned one;
- queues the frames' scopes, so after the switch each is rebuilt from its
  checkpoint base and retained frames: the kept effects roll back to what
  the device's peers hold.

## Void records

For each log, the device signs `rocky.void.v1` over the held frames' signed
headers, at most 64 per record, with the log's key: the node key or a table
pseudonym. A pseudonym registered after open signs its logs' records then.

A receiver admits a record whose headers verify under one author and scope
and whose signature verifies, for a scope it holds. It refuses the whole
record when it holds any covered frame, when another frame or void fills a
position, or when its log already passed one: a void never covers an offered
frame. Admitted positions count as consumed: they continue the frontier, a
later frame whose backlink or dependency is voided applies once the frames
before the void did, and a frame arriving at a voided position is a fork and
is not stored.

Range pages, pushes on sessions and live links, and transfer pages carry the
records their frames continue after. The file identity is
`rocky.encrypted-replica.v18` and link handshake `rocky.link.v3`, as
[coordinated writes](coordinated-writes.md) set them; other versions are
refused.

## The list, re-authoring and dropping

| API | Effect |
| --- | --- |
| `EncryptedReplica::held_changes` | Each change: id, time, rows; per written kept field, the stored value with the time and key of its last change |
| `SecureSql::commit_reauthoring(handle, id)` | Commits the open transaction as an ordinary new bundle stamped now, and removes the change in the same transaction |
| `EncryptedReplica::drop_held(id)` | Removes the change; nothing is sent |
| SDK `held_changes`, `drop_held`, SQL `COMMIT` with `reauthors` | The same over the JSON contract; `generation` reports the count as `held` |
| Kotlin `heldChanges()`, `dropHeld(id)`, `reauthor(id) { ... }` | The same on `RockySession` |

Re-authoring maps the change to the current schema. Its stamp is later than
every value it read, so it wins over what `current` showed on every device. No
backdated stamp is ever signed.

A re-authoring handle binds both the writes and the held ID; using it to
resolve another held change conflicts. Missing IDs and empty re-authoring
fail and leave the held list and projection unchanged.

## Limits

- Existence records a held creation signed stay; a puppet may stand for the
  row on devices without its reader scope until a deletion is recorded.
- Remote reads served before the hold showed its effects; nothing stored them.
- An author that offers a frame and voids it forks its log; each receiver
  keeps what it admitted first, as for any fork.
- The recovery-package path requires complete frame dependency closure and
  does not carry voids or the local held list. It refuses export when retained
  frames depend on voids; transfer via range sync or a checkpoint instead.
- Void history retention and long-running bounds are not qualified here.

## Evidence

- `rocky-secure` `tests::generations::held`: a bundle over a kept and a
  removed table is held, voided and rolled back; the author's later edit
  applies on a peer with no lag; the list shows the peer's newer value and
  re-authoring wins on both; a void of an offered frame is refused; a default
  value holds nothing; dropping writes nothing.
- `rocky-sdk` `tests/removal.rs`
  `an_unsent_change_a_removal_splits_is_held_and_reauthored_after_the_update`.
- Kotlin `RoomGenerationsTest.anUnsentChangeTheNewerVersionRemovesIsHeldAndReauthored`.

Run results and remaining qualification limits are in
[Phase 4 evidence](../features_09/04-held-changes-evidence.md).
