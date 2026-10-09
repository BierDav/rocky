# 3. Small fixes

Status: not started.
Owner: `rocky-coordinator` client, `rocky-runtime` strong requests, the
service surface and its Kotlin binding. No dependency. Wave 0.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [fix later](../fix_later/README.md). The market application
([phase 15](15-market-example.md)) runs strong commands against a coordination
service from Kotlin and meets three of its entries. Fix each with a
regression test and remove its entry.

## Fixes

1. **Coordinator client cache.** The client appends an observed head per
   scope without a cap, and opening refuses more than 64. Bound the heads on
   insert and on open with one constant sized for a public genesis, and drop
   heads of closed domains.
2. **Stale fence.** `execute_pending` leaves an attempt `unknown` after
   `StaleHead` or `StaleEpoch`, so `release_command` refuses the request and
   its checkpoint hold persists. First confirm in `rocky-coordinator` that a
   confirmation certified with a lost reply is answered with its receipt and
   never with a stale error. Then record a stale fence as a definite refusal
   that discards the preparation and returns the request to queued.
3. **Closing a domain outside Rust.** Add `close_domain` to the service
   command surface and the Kotlin binding.

## Completion gate

- A client that observed 65 scopes, and one that observed a closed domain,
  reopen.
- A request whose confirmation met a stale fence is prepared again against the
  new head and applies once; it can be released while queued. A lost reply to
  a certified confirmation still yields the receipt.
- A Kotlin tenant closes a domain and registers another within its quota.
- The three entries are gone from fix later.

## Not in this phase

Reads on the writer thread stay in fix later. The checkpoint page's credential
chains are [phase 8](08-chains-by-reference.md); the two sync entries are
[phase 2](02-change-driven-sync.md).
