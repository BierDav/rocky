# 5. Confirmed rollouts

Status: implemented; rollout gates and full workspace verification passed
after isolating the Phase 9 invoice repair. See [evidence](05-confirmed-rollouts-evidence.md).
Owner: `rocky-sdk` `database.rs` open and generation signing, and status; `sdks/kotlin/rocky-core` `RockyDatabase`. Depends on
[phase 3](03-removal.md). Follow the
[execution contract](README.md#agent-execution-contract). Shares decision record 0070.

## Inputs and objective

Read [generations](../runtime/generations.md) and
[SDK enrollment](../runtime/sdk-enrollment.md).

Removal cannot be undone: once signed, every device that adopts the
generation drops the data. The issuer that first opens such a version must not sign it
unasked. After this phase it reports the impact and signs only after the
application confirms it.

## Settled

- **When.** Opening a schema whose transition removes anything fails with a
  structured "confirmation required" result before any change. Additive
  transitions need no confirmation.
- **Impact report.** It lists removed tables and fields and the rows affected locally. It does not list devices: Rocky cannot know
  them, and a key may be used by several.
- **Confirmation.** The application opens again with the report's digest.
  A digest for a different report is refused, so a confirmation never
  approves a change the user was not shown.
- **Members.** Only issuers sign. A member that opens such a version before
  any issuer has signed it waits and reports why.
- **Wording.** The application writes the user-facing text; the report only
  carries data.

## Ordered slices

1. Impact report and the confirmation result at open.
2. Digest-bound confirmation and signing.
3. Kotlin API and the shopping app's dialog.
4. Guides: generations, SDK enrollment.

## Completion gate

- Opening a removing version without confirmation changes nothing and
  returns the report; with its digest, it signs.
- A digest from an earlier report is refused after the schema changed.
