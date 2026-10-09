# 11. Chains by reference

Status: not started.
Owner: `rocky-secure` frames, operation storage, checkpoint transfer and
session pages. Depends on [phase 3](03-catch-up-cost.md) and
[phase 8](08-concurrent-rotation.md); lands after
[phase 9](09-roster-free-admission.md) on the session. Wave 3.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read the [replica limits](../runtime/replica-roles.md#limits), the checkpoint
entry in [fix later](../fix_later/README.md),
[encrypted checkpoints](../runtime/encrypted-checkpoints.md), the
[partition budget](../runtime/partitioned-scopes.md#limits-and-budget),
[decision 0044](../decisions/0044-group-frontier-exchange.md) and the profile
phase 3 recorded.

Every encrypted frame carries its author's credential chain inside the signed
header (`Extensions.credentials` in `rocky-secure/src/wire.rs`): tens of
kilobytes with 20 scope grants, so a page of at most 32 frames holds few. A
checkpoint page is 248 KiB, but its sealed plaintext must fit one 64 KiB
package, a budget of 63 KiB; one confirmed command's two chains take 49 KiB
of it. The four-table ERP fixture missed three budgets: register and author
86 s of 60 s, catch-up 339 s of 100 s, checkpoints 121 s of 48 s. Phase 3
profiled them and fixed what needed no format change. Send and store each
chain once, and free the checkpoint page.

## Settled

- The chain digest is a signed frame extension of the next row format; the
  chain itself leaves the header. Operation hashes change with the format.
- A checkpoint page carries the latest command's chains beside the page, in
  their own message. `EncryptedCommand` and `ConfirmedCommand` keep their
  signature formats, and `rocky/command/1` does not change.
- Verification stays per request against the current revocation set. Only
  bytes are shared, never authority.
- The checkpoint work is done whatever the profile says. The frame work
  follows the rule in slice 1.

## Ordered slices

1. Scope rule, from phase 3's profile. "Chain bytes" means encoding, hashing,
   signing over, transferring and storing chains. If they account for at
   least 20% of any one of the three missed budgets, do every slice. If they
   account for less than 20% of each, do slices 2 and 5 only: the row format,
   stored operations and session pages stay as phase 8 left them, slices 3
   and 4 are skipped, and the page limit of 20-grant frames is moved to
   [fix later](../fix_later/README.md) with the profile's figure. Record
   which case applied in the status line.
2. Decision record 0053, covering the case that applied.
3. Frames and session. The header names its chain by digest. A session keeps
   a chain table: `Page`, `RangeFrames` and `Push` carry each distinct chain
   once, before the first frame that names it; a later page of the session
   does not repeat it. Add the module and raise the session version constant
   of phase 9.
4. Storage. A retained operation references a stored chain by digest; a chain
   is kept while an operation, a registered row author or a horizon
   credential (where [phase 13](13-relayed-horizon.md) kept that table) names
   it, and is removed with the last of them.
5. Checkpoint page. The first page no longer embeds the latest command's
   chains: a message beside the page carries them, verified against the
   digest the strong state names. Byte strings are CBOR byte strings, so the
   budget in `checkpoint_page` stops reserving two bytes per stored byte.
   Remove the entry from fix later.
6. Rerun the four-table and single-table fixtures and record the figures
   beside phase 3's.

## Completion gate

- A scope whose latest command carries two 20-grant chains bootstraps by
  checkpoint, and so does one with 40-grant chains; the strong state's bytes
  on the first page are recorded before and after.
- A beside-page chain that does not match the named digest refuses the
  checkpoint and stages nothing.
- When slice 3 applied: a page of frames from one author carries its chain
  once; frames per page for 20 grants are recorded before and after.
- When slice 3 applied: a frame naming a chain the receiver lacks is refused
  as incomplete and arrives on the next pass; it is never admitted or stored.
- When slice 3 applied: a chain revoked during a session admits no later
  frame of that session, although the session already carried its bytes.
- When slice 4 applied: after pruning every operation of an author, its chain
  is gone; an interruption between the two leaves no operation without its
  chain.
- A build with the earlier row format or session version is refused.
- Each of the three four-table budgets is met, or its remaining miss is in
  [fix later](../fix_later/README.md) with a profile that attributes at least
  80% of it.
- The single-table budgets still pass.
- [Secure sessions](../runtime/secure-sessions.md),
  [encrypted checkpoints](../runtime/encrypted-checkpoints.md) and
  [replica roles](../runtime/replica-roles.md) describe it.

## Not in this phase

- Profiling, and fixes that need no format change: phase 3.
- Smaller credentials, selector compression and per-partition command
  sequences.
- Any change to command signatures, receipts or `rocky/command/1`.
- A cache that lets a known chain skip verification.
- Sharing chains across sessions on the wire: the table lives one session.
- The epoch identity in the header ([phase 8](08-concurrent-rotation.md)).
- Raising the 64 KiB package or the 256 KiB frame.
