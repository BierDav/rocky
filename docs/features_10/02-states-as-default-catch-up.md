# 2. States as the default catch-up

Status: complete (2026-10-23); [all gates passed](02-states-as-default-catch-up-evidence.md). Owner: `rocky-secure` range serving and
link messages; `rocky-sdk` migration completion, finality maintenance and
catch-up. Depends on nothing; benefits from
[phase 3](03-incremental-finalization.md) once seals are frequent. Effort:
one to two weeks. Follow the [execution contract](README.md#agent-execution-contract).


## Inputs and objective

Inspect the owning modules and their tests for current behavior, and the
[performance review](00-sync-performance-review.md) for the measured baseline.

After this phase a device far behind imports a base at a seal of its path
and replays only the tail, a finished migration leaves a checkpoint that
joiners take instead of its bundles, and every device compacts at a final
seal without being asked.

## Settled

- **Advice travels with the page.** A `RangeFrames` page gains
  `state: Option<StateAdvice>` naming a seal of the sender's followed path,
  the checkpoint identity, its signer and its stored rows. The sender fills
  it when the base covers the requester and the requester lacks at least
  64 covered frames. The page still carries frames; advice never replaces
  them, and `behind` keeps its meaning and its mandatory handling.
- **Covered means every log.** For each log in the requester's frontier,
  its `held` is at or below the base frontier's sequence for that author
  plus one, and the requester names no log the base lacks. A requester with
  any frame beyond the base gets no advice: it is close, and frames are
  right for it.
- **The receiver takes the advice** unless this process already failed to
  import that exact checkpoint for that scope three times, or the scope's
  staged import belongs to another signer it should go on with
  (`deferred`). It imports through `bootstrap_scope`, which already handles
  a peer that retains history, then continues `catch_up` from its new
  frontier. Local operations are reapplied or reported by the
  `RebootstrapReport`, as on any import.
- **A migration ends with its closing seal.** When the publishing issuer
  observes its migration's `published == bundles`, the SDK seals the
  successor tip with `continue` and the same schema, moves the followed
  bookmark, finalizes, compacts with `keep = 0`, signs existence bases and
  lists the checkpoint records, in that order, in the same sync pass. On a
  coordinated tip the seal is the log's terminal, so it is final and
  trusted nodes acknowledge it; on a tip without a log the issuer compacts
  anyway: its frames are never pruned as a sole copy, so losing the race
  costs it a rebuild from its own frames, never data.
- **Rocky compacts at final seals.** The SDK's `finalize_seals` pass, at
  the start of every sync pass and after publishing statements, also runs
  `compact(0)` when the newest finalized seal of the path is final and a
  held scope's base is not at it. Nodes that may sign tip statements then
  list their bases. `compact(keep)` remains the application's call for
  seals that are not final; nothing else about compaction changes.
- **The migration fixture changes its contract.** The member of the 100k
  fixture imports the closing seal's checkpoint; the assertion that no
  checkpoint covers the open migration is replaced by one that both files
  hold a base at the closing seal and the member's transfer report names
  it.
- **Format.** The link moves to `rocky.link.v15` for the new page field.
  Checkpoint, frame and replica formats are unchanged.

## Ordered slices

1. `StateAdvice` on range pages: sender rule, receiver import, the retry
   guard, the link version.
2. The migration's closing seal, compaction and listing in the SDK; status
   reports `migration: null` only after the listing.
3. Automatic compaction at final seals in `finalize_seals`; the Kotlin
   `compact` documentation notes when it is no longer needed.
4. The 100k fixture's new assertions and measurements; evidence.
5. Update phase status and evidence in this directory.

## Completion gate

- A joiner of a scope with 50,000 rows and 100 seals of history on the
  path imports the newest listed base and pulls no frame below it.
- A device one page behind receives frames and no advice; a device whose
  every log is at or below the base receives advice and imports.
- A device with unsent local writes on the tip imports the advised base and
  its writes are reapplied, with the same rows on every device afterwards.
- The 100k fixture: `member_syncs == 1`, the member's transfer names the
  closing seal's checkpoint, member stage ≤ 120 s, both files hold the base,
  and the combined e2e suite fits its 900 s budget.
- A final seal is compacted on every trusted and member device within one
  sync pass of becoming final, with no application call; a seal that is
  not final is compacted only by `compact(keep)`.
