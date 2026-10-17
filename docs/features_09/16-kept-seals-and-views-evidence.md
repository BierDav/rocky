# Phase 16 evidence

See the [brief](16-kept-seals-and-views.md) and the
[guide](../runtime/kept-seals.md).

## Completion gate

| Gate item | Test and result |
| --- | --- |
| A Room DAO runs unchanged against a connection bound to a seal kept two seals ago | `aDaoReadsASealKeptTwoSealsAgoUnchanged` (Kotlin Room qualification) |
| Five kept seals at 1% change add about 5% | `five_kept_seals_of_one_percent_changes_add_about_five_percent` (`rocky-secure` `tests::kept`): 25 history rows for 500 rows, between 4% and 7% of the table's bytes |
| A seal before a field's removal still shows the field | `a_seal_before_a_removal_still_shows_the_removed_field_and_table`: frozen copies of only the two affected tables |
| A seal learned before its frames is shallow until they arrive | `a_seal_learned_before_its_frames_is_shallow_until_they_arrive` |
| Opening a branch on a seal not held is refused; after a pull from a node that keeps it, the same call succeeds | `a_node_holds_a_seal_it_compacted_past_once_it_pulls_it_from_a_node_that_keeps_it` (replica calls, base at the seal); `a_node_pulls_a_seal_another_keeps_over_a_session_and_opens_a_branch_on_it` (native session, signed from the peer's history tables); `an_owner_pulls_a_seal_an_issuer_keeps_and_opens_a_branch_there` (`rocky-sdk` tips: `open` refused, `pull`, one sync, `open` succeeds) |
| Writes through a read-only connection fail with SQLite's read-only error | `a_connection_bound_to_a_kept_seal_reads_it_and_never_writes` (`rocky-sdk` driver) |
| Switching to a branch two seals back and returning costs the rows changed since the seal | `switching_to_a_branch_two_seals_back_and_back_costs_the_rows_changed_since`: with 400 rows and six changed, the switch writes 38 rows each way where a rebuild from the replay base writes about 4,000, and both reach the same stored rows, digests, table and existence index |

The rewind is checked against a rebuild from the replay base, with a test
switch that forces it, in
`a_branch_on_a_kept_seal_keeps_its_own_writes_across_switches` and, with an
existence log on every table,
`a_branch_on_a_kept_seal_keeps_its_existence_log_across_switches`. Running
the whole `rocky-secure` suite with every seal kept by default changed only
the four compaction tests that expect a base past a seal, which compaction
never passes while the seal is kept.

## Formats

Stored rows gain `secure_stored_history`; kept marks record the schema and
each scope's bound; existence log entries record their frame position and
row id; a node keeps the checkpoints it signed of kept seals; the native
checkpoint family gains `SealOffer`. Phase 16 is unpushed, so link
`rocky.link.v9` and replica file `rocky.encrypted-replica.v26` stay current
with only these encodings.

## Remaining limits

- Returning to a path finalizes again, at the next maintenance step, the
  seals the other path did not pass. Finalizing scans the scopes a seal
  changes, as for any seal.
- A scope in a constraint group, or whose replay base lies off the new path,
  rebuilds from its base on a switch.
- A seal is held once every held scope has its state; a peer serves only the
  scopes it reads in whole.
