# Large command-controlled tables

Status: deferred. The current contract is described in
[typed commands](../runtime/typed-commands.md#limits) and fixed by
[design rationale](../runtime/typed-commands.md#design-rationale).

## Trigger

Build this when an application needs a strong invariant over a table that does
not fit one command envelope. Examples: stock levels of a full article
catalogue, account balances, or globally unique document numbers in an ERP.

## Current cost

A handler receives every confirmed row of the tables in its `resources`, and a
result carries each of those tables before and after the decision.

- A command envelope stays under 128 KiB, so the confirmed tables of a schema
  together must stay under that size.
- Every command costs time and bytes proportional to the whole confirmed
  state, whatever it reads or writes.
- A decision is limited to 32 row writes and 31 jobs.
- A device verifies a result by comparing complete row maps.

## Sketch

Make a command's cost proportional to what it touches:

1. **Declared reads.** A handler asks for rows by key or by a bounded range of
   a declared index. The executor records the keys and ranges it served as the
   command's read set.
2. **Results as row changes.** A result carries the changed rows and a
   commitment to the confirmed state before and after, not whole tables. The
   set digest of [encrypted checkpoints](../runtime/encrypted-checkpoints.md#design-rationale)
   already supports adding and removing members without a scan.
3. **Verification.** A device applies the changes to its own confirmed rows
   and compares the resulting commitment. A device that reads only some of the
   tables verifies its part, as it does today.
4. **Storage.** Confirmed rows move from command records into indexed tables,
   so that keyed reads and range scans do not load a table.

This changes the handler contract, the result wire format and the stored
command state. It needs a decision record, a new command format version and a
refusal path for devices that only know the old one.

## Prepared now

The host-language handler interface offers keyed reads and bounded scans
instead of whole tables. Until this optimization is
built, the runtime serves them from the complete rows it already holds. A
handler written against that interface does not change when tables grow.

## Must not break

- Executors must still reach the same decision from the same input, and a
  retried call must still return the same result.
- A phantom must not pass: a range a handler scanned is part of its read set,
  so a row inserted into it by an earlier command changes the input.
- A device that lacks part of the confirmed state must still report it as
  unavailable, never as an empty table.
- A result must stay verifiable by a reader of one table alone.
- Nothing about arguments, rows or descriptors may reach the coordinator.
