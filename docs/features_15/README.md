# features_15: bounded live publication

Status: proposed (2026-11-07). Investigation is complete; runtime changes and
performance qualification remain unimplemented. This work is independent of
features_14 phase 6 and consumes its phases 3 and 5a authority contracts.

## Scope

Replace repeated credential-heavy direct pushes and premature full-peer
reconciliation with one bounded publication across affected scopes, explicit
completion and scoped repair. Keep existing authorization, atomic intake and
transport allocation bounds.

| # | Phase | Delivers |
| --- | --- | --- |
| 7 | [Bounded live publication](07-live-publication.md) | shared credentials, publication completion and scoped repair for live refresh; [investigation evidence](07-live-publication-evidence.md) |

The phase keeps its original number after moving from features_14. Its brief
defines the implementation and acceptance work; evidence records measured
results and limitations. Implement against the latest integrated source and
preserve the independent [features_14 work](../features_14/README.md).

Before changing runtime code, finalize the wire state machine, early byte and
metadata quotas, captured export bounds, completion predicate and absolute
publication deadline in the phase brief. Resolve routine engineering choices
from source. Performance gains remain unproven until measured; compression
and set reconciliation are follow-up research outside the initial fix.
