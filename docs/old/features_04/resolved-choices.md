# Resolved choices for the features_04 phases

Status: accepted 2026-10-06. These settle every choice the phase briefs left
open, so an agent needs no further product decision to start a phase. Internal
mechanisms stay the agent's call. Numeric defaults are initial, configurable
values; change them only with measured evidence and an updated guide. A choice
that changes persisted or wire formats still needs its own decision record.

## Phase 1: contracts and composition

- **Dependency direction.** `rocky-core` ← `rocky-credentials` ← `rocky-runtime`
  and `rocky-secure` (siblings, no dependency between them) ← `rocky-sdk`.
  `rocky-iroh` depends on `rocky-core` only and implements the network port;
  `rocky-sdk` composes everything. No crate depends on `rocky-sdk`.
- **No separate command crate.** Command contracts live in `rocky-core`;
  execution and storage stay in `rocky-runtime` and `rocky-secure`, preserving
  one atomic commit per profile.
- **Compatibility.** The existing `create/join/resume` SDK becomes a thin facade
  composed from the new services for this release, then is removed. No second
  maintained implementation of the old behaviour.
- **Adapter mechanism.** In-process UniFFI bindings per
  [decision 0020](../decisions/0020-in-process-uniffi-bindings.md).

## Phase 2: networking

- **Supported transports.** iroh is the only Internet transport. TCP and the
  in-process adapter remain supported for tests, LAN and deterministic faults.
  Application-supplied transports arrive through a callback stream interface.
- **Identity migration.** Domain identity stays the manifest-bound definition
  identity plus the creator key. Existing enrollment records embedding addresses
  migrate on open into versioned contact records; unknown record versions refuse
  to open. Addresses become unverified reachability hints.
- **Peer selection.** Sync targets are the configured authorized peers. No
  discovery, election or mesh replication in this release; third-party iroh
  stacks such as p2panda-net are only ever application-supplied endpoints.

## Phase 3: credentials

- **Key storage.** The file secret store remains the fallback; document it as
  plaintext at rest protected by OS file permissions. Platform keystore adapters
  are later work behind the same provider contract.
- **Issuer recovery.** Delegated issuer rights to a second enrolled device are
  the primary path. An encrypted exported backup is optional later work.
- **Grant profiles.** Reader, scoped writer and issuer. No full-rights default.
- **Rotation.** Revocation starts a new key epoch for future writes. History is
  not re-encrypted; retained historical keys follow policy.

## Phase 4: queries and observation

- **SQL handling.** Replace the token rewriter with AST-based rewriting using a
  Rust SQL parser in SQLite dialect. Supported surface: SELECT with aliases,
  joins, subqueries, CTEs and window functions; INSERT, UPDATE and DELETE with
  WHERE; savepoints. Rejected before side effects: triggers, app-created views,
  ATTACH, write PRAGMAs, virtual tables and multi-statement strings.
- **Merge rules.** Built-in only: per-field last-writer-wins with the existing
  `changed`/`assigned` update policies and permanent tombstones. Custom reducers
  through host callbacks are deferred.
- **Secure gaps.** Deleted-key reincarnation and derived-column gaps are
  diagnosed at installation and refused; no reconciliation work in this phase.
- **Cursor limits.** Default cursor idle deadline 30 s, maximum lifetime 5 min,
  page size 256 rows, at most 64 open cursors per session.

## Phase 5: typed commands

- **Encoding.** Reuse the existing canonical operation encoding for arguments,
  results and tickets; one versioned descriptor per command method.
- **Compatibility horizon.** Executors accept every command version within the
  application's declared support horizon; older versions fail admission with an
  explicit error. No automatic argument translation.
- **Gateway identity.** Forwarding signature: the originating device signs the
  invocation, and a gateway forwards it. A trusted-submitter model is deferred.
- **Handler host defaults.** Deadline 10 s per callback, one handler thread per
  domain, timeout and foreign exception map to a retryable preparation failure.

## Phase 6: coordinator retention

- **Sequencing.** Per-session strictly increasing sequence numbers with a sliding
  window of at most 32 outstanding requests. Retirement moves the floor past
  acknowledged or expired entries only.
- **Clock.** Leader-proposed time committed through Raft; followers never consult
  local clocks for authority decisions. Backward jumps clamp to the last committed
  value. Tests use the controllable clock.
- **Initial budgets.** Recovery horizon 7 days. Session lifetime 24 h, renewable.
  Response window 32 entries and 64 KiB per session. Per tenant: 16 domains,
  256 sessions per domain, 4 MiB coordinator state. Snapshot every 1,024 entries
  with at most two retained snapshots.
- **Domain closure.** Owner-initiated only, plus tenant retirement. No automatic
  inactivity closure. A closed domain's identity is never reusable.

## Phase 7: public service

- **Client transport.** iroh through `rocky-iroh`, with the provider's endpoint
  identity pinned in the client trust bundle. Voter-to-voter traffic keeps the
  existing certified TCP inside the provider network.
- **Control tokens.** Signed capabilities from the phase 3 credential module.
- **Tenant identity.** An owner keypair. No login provider is required.

## Phase 8: recovery and SDKs

- **Claimed desktop targets.** macOS arm64 and Linux x86_64 required for the
  packaged consumer check; Windows x86_64 is best effort.
- **Backup format.** A single signed archive of database files, retained receipts
  and credential state, restored only into a fresh signing identity.

## Phase 9: qualification

- The optional custodian scenario is included in the release gate.
- Real-network evidence is recorded on one Internet relay path; NAT traversal
  classes beyond that are reported, not required.
