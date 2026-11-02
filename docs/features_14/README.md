# features_14: application-facing guarantees

Status: requirements only (2026-10-29). Not an implementation plan, phase
sequence or claim of available functionality. These requirements complement
[features_13](../features_13/README.md); they do not reopen its settled design.
Some may already be satisfied by existing capabilities or by features_13.
Qualification must distinguish those from capabilities that need further work.

## Rocky's responsibility

Rocky stores data locally, replicates it securely, enforces application-declared
rules and exposes reliable transaction outcomes. Applications own their schemas,
business transitions, calculations and external integrations. The coordinator
handles generic metadata, never application payloads or business rules.

A relational accounting application motivates these requirements, but none
requires Rocky to understand invoices, taxes, accounting or ERP workflows.
The contracts must be language-neutral and usable through packaged SDKs and
qualified adapters without requiring a Rust project.

## R1. Exact values and deterministic arithmetic

- Applications must be able to persist, bind, read and replicate exact amounts,
  quantities and rates without conversion through binary floating point,
  including across SDK boundaries.
- Supported representations must state their precision, scale, range and
  canonical encoding. A bounded scaled-integer representation is acceptable;
  it must not silently claim the range of a larger decimal type.
- Arithmetic used in SQL constraints, assertions and replay must have explicit,
  deterministic rounding and overflow behavior. Unsupported calculations or
  values must fail explicitly rather than lose precision or promote to REAL.
- Applications may calculate with their own exact arithmetic and bind the
  results. Rocky must make clear which exact operations can also be performed
  and verified inside the database, including sums and comparisons.
- UUID, date/time and JSON bindings must have defined storage representations
  and round-trip behavior. Application timestamps remain bound values, as in
  features_13; Rocky does not supply a business clock during replay.

This does not prescribe a native decimal type or a particular arithmetic
implementation. It requires an honest, usable exact-value contract.

Status (2026-11-02): satisfied for single-replica and two-replica replay;
the contract is in the `rocky-sdk` crate docs ("Exact values").

- Exact values are `i64` scaled integers whose scale the application fixes.
- Rocky registers `checked_add`, `checked_sub`, `checked_mul` and `checked_div`
  (`rocky-storage` `exact.rs`) on every connection. They match SQLite except
  that an INTEGER result outside `i64` fails with `integer overflow`.
  Replayed statements, author-evaluated reads they bind and assertions are
  rewritten to use them. Replayed `CHECK`s, generated columns and index
  expressions must call them; installation refuses `+ - * /` there
  (`Incompatibility::Arithmetic`). Overflow is a commit error at the author
  and a `Constraint` drop in replay.
- `avg`, `total`, `round` and `mod` left the deterministic allowlist.
- UUIDs bind as 16-byte BLOBs (`[u8; 16]`), `Timestamp` as INTEGER Unix
  microseconds, JSON as the TEXT bound (`serde_json::Value`, not canonicalized).
- Evidence: `rocky-sdk` `tests/exact.rs` (author commit, two-replica drops
  for a statement, a `CHECK` and an assertion, round trips), `rocky-storage`
  `exact::tests`, `rocky-core` `deterministic::tests`.
- Engine identity (2026-11-03): each tip's schema manifest commits
  `EngineIdentity`, a digest of the bundled SQLite version, the function
  allowlist, the collations and `REPLAY_SEMANTICS` (`rocky-core`
  `engine.rs`). A build on another engine reads the followed tip but neither
  writes nor replays it (its frames wait as for a missing schema, and a seal
  it signs covers only what it replayed); an owner on the new build opens a
  tip with its engine at open, as for a schema change, and older builds stay
  read-only at the old tip. Status: `GenerationStatus::engine`
  (`upgrade_required`, `awaiting_owner`). Evidence: `rocky-secure`
  `tests::tip_schemas::engine`, `rocky-sdk`
  `a_tip_on_another_engine_leaves_the_same_schema_ahead`. Formats: manifest
  v3, replica v44, settings 21, tip schema record 2.

## R2. Relational invariants with an explicit scope

- Schema declarations must support composite tenant-scoped keys and references,
  unique indexes, deterministic row checks, deferred references and
  transaction-end cross-row/table assertions, as specified by features_13
  phases 4/5.
- Applications must be able to express same-owner references, aggregate
  equality/caps and subtype completeness. Validation must cover inserts,
  updates and deletes, both at author commit and during replay.
- Schema installation must identify unsupported relationships and assertion
  scopes before writes begin. Diagnostics must name the offending declaration.
- The SDK contract must distinguish legal tenancy, partition identity,
  partition groups, access grants and coordination. A tenant column is not
  automatically a partition or an authorization rule.
- The restrictions on cross-partition checks must be exposed consistently to
  schema authors. Applications need a supported way to reference identity data
  without requiring inaccessible partitions.

No cross-partition assertion engine, automatic partition selection or
application trigger support is required. Application authors choose the layout.

## R3. Observable finality

- Local visibility, durable storage, provisional replay outcome and seal-final
  outcome must be distinguishable. A successful local commit must not imply
  that it can never be dropped.
- Stable transaction identities must let an application observe and query an
  outcome after restart or reconnect, including a named drop reason and whether
  that decision is final. Notifications alone are insufficient for recovery.
- Applications must have a supported way to determine when a transaction is
  seal-final before an irreversible action.
- Offline, revoked-access and unsealed cases must have explicit outcomes and
  recovery behavior. No owner online must not silently turn a provisional write
  into a final one.

Database changes and coordination are independent Rocky capabilities.
Rocky does not combine them: it has no coordinated tables or coordinated
writes. An application that must not accept two competing attempts (the last
unit of capacity, a unique invoice number) obtains a coordinator decision and
commits ordinary writes, joining the two in its own code, including retries
after an uncertain result. Rocky states each guarantee on its own and never
presents a seal-final outcome as a coordinator decision or the reverse.

## R4. Durability and atomic application effects

- SDKs must expose the durability boundary and a way to obtain the required
  flushed guarantee. Buffered local success must remain clearly distinguishable
  from power-loss durability and publishability.
- Applications must be able to commit related domain rows, operation identity,
  audit records and an application outbox record atomically. All must roll back
  together on a failed or dropped transaction.
- After recovery, applications must be able to find committed eligible work
  and its transaction outcome. Dispatchers must not depend on having observed
  a transient notification or a currently editable projection alone.
- Replication and observer publication must follow the committed state. External
  calls remain outside database transactions.

Rocky need not implement an email/payment dispatcher. Applications own delivery,
provider idempotency and reconciliation. Rocky must not promise exactly-once
external effects or confuse its replication outbox with an application outbox.

## R5. Enforceable write restrictions and immutable records

- Applications must have supported mechanisms to restrict inserts, updates and
  deletes and preserve append-only records or immutable accepted snapshots.
  State-dependent restrictions must be enforceable on every supported write
  path, including replay, rather than only in a UI.
- The contract must distinguish final-state assertions from transition checks:
  a final-state assertion alone does not establish that an existing immutable
  row was never edited or deleted.
- Admission must use the correct authority and replayed before/after state.
  Revocation, retries, late arrivals and schema changes must not bypass it.
- SDK consumers must be able to associate writes with authenticated actors and
  map application membership/permissions to Rocky grants. Application role rows
  must not be implied to grant authority automatically.

Status (2026-11-02): schema restrictions cover the transition-check part.
A restriction names a replayed table, an optional rule predicate over the
row before the transaction, and whether it refuses updates, deletes or both
(`LogicalRestriction::immutable`, `append_only`). Author commit refuses a
violation; replay drops the transaction with `DropReason::Restricted`,
including through delete actions and delete-and-reinsert moves. Evidence:
`rocky-secure` `tests::replayed_constraints::restrictions` (every delivery
order of five concurrent transactions against a trigger-based reference
replay). Actor association and grant mapping are not addressed by it.

Applications define which records become immutable and how corrections work.
These requirements do not prescribe a new trigger system or a business audit
service, and do not change features_13's unsupported-trigger boundary.

## R6. Qualification for relational applications

The guarantees above need evidence beyond single-row or shopping-list workloads:

- Multi-table transactions with composite references, deferred checks, aggregate
  assertions and competing uncoordinated attempts to consume one remaining
  capacity (one is dropped deterministically).
- Outcome queries after restart, idempotent application retries and rejection
  of unauthorized mutation/deletion of immutable records.
- Exact-value round trips and arithmetic boundaries through supported SDKs.
- Crashes around commit, flush, replay and sealing; recovery of
  outcomes and application outbox eligibility without duplicate accepted writes.
- Partial partition access, offline reconnect, late arrivals and revocations;
  replicas at the same seal agree on rows and drop outcomes.
- Representative large partitions, indexed assertions and multi-row transactions
  during bootstrap, migration, rewind and compaction on supported device classes.

Record supported limits and measured costs. Single-row budgets do not establish
aggregate-assertion or company-sized-partition performance. Evaluate features_13
phase 10's existing trigger against these workloads; do not claim bounded
interactive latency for transitions that have not been qualified.

## Outside Rocky

Application schema design and partition choices; accounting/tax/rounding policy;
business posting and correction commands; invoice numbering policy; files/PDFs,
OCR, bank/email integrations and provider credentials; migration mappings and
business reconciliation; application UI and reports. Rocky supplies the generic
guarantees these use, not their business implementation.

## Required evidence

Each requirement must ultimately be identified as satisfied, unsupported or
unverified, with its public contract, enforced limits and relevant executable
evidence. Existing features_13 work should satisfy requirements where it already
provides the guarantee; this document does not require duplicate mechanisms.
Architecture choices, implementation phases, estimates and scheduling are left
for a later plan.
