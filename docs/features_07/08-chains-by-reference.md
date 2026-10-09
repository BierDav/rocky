# 8. Chains by reference and sync budgets

Status: not started.
Owner: `rocky-secure` frames, operation storage and checkpoint transfer;
the ERP measurement fixture. Depends on
[phase 7](07-roster-free-admission.md); lands after
[phase 6](06-concurrent-rotation.md) on the frame. Wave 3.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read the [replica limits](../runtime/replica-roles.md#limits), the checkpoint
entry in [fix later](../fix_later/README.md), the
[partition budget](../runtime/partitioned-scopes.md#limits-and-budget),
[decision 0044](../decisions/0044-group-frontier-exchange.md) and the
measurements in [the shopping audit](../runtime/shopping-qualification.md#evidence).

Every encrypted frame carries its author's credential chain: tens of
kilobytes with 20 scope grants, so a page holds few frames. One confirmed
command's chains take 49 of a checkpoint page's 63 KiB. The four-table ERP
fixture misses three budgets: register and author 86 s of 60 s, catch-up
339 s of 100 s, checkpoints 121 s of 48 s. Whether chains cause the misses is
a hypothesis; measure first.

## Ordered slices

1. Profile the four-table fixture: bytes and time spent on chains against
   signatures, sealing, storage and round trips, per missed budget. Record it.
2. Decision record. A session sends each distinct chain once and frames name
   it by digest. Verification stays per request against the current
   revocation set; only the bytes are shared, not authority.
3. Storage: a retained operation references a stored chain by digest; a chain
   is kept while an operation needs it.
4. Checkpoint page: the latest command's chains travel by reference or beside
   the page, and byte strings are CBOR byte strings. Remove the entry from
   fix later.
5. Rerun the fixture. If a budget still misses, fix the largest remaining
   cost the profile shows; otherwise stop.

## Completion gate

- A page of frames from one author carries its chain once; the frame count
  per page for 20 grants is recorded before and after.
- A frame naming a chain the receiver lacks is refused as incomplete and
  retried, never admitted.
- A chain revoked during a session admits no later frame of that session.
- A scope whose latest command carries 20-grant chains bootstraps by
  checkpoint.
- The three four-table budgets are met, or each remaining miss is recorded
  with the profile that explains it and moved to
  [fix later](../fix_later/README.md).
- The single-table budgets still pass.

## Not in this phase

Smaller credentials, selector compression and per-partition command
sequences.
