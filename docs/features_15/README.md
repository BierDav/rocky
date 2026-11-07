# features_15: bounded live publication

Status: implemented and native/wasm correctness checks passed (2026-11-08);
consumer performance unqualified.
Consumes the completed features_14 authority contracts integrated at `5b937088`.

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
preserve the integrated authority implementation.

The phase brief records the implemented wire state machine, early byte and
metadata quotas, captured export bounds, completion predicate and absolute
publication deadline. Evidence records the final native/wasm and browser/native
relay correctness gates and disposable native diagnostics. Real consumer,
relay and browser performance still need measurement; compression and set
reconciliation remain follow-up research outside the initial fix.
