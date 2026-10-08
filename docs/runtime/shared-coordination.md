# Certified bootstrap and shared coordination

`rocky-coordinator` provides `VoterService` (the `rocky-voter` binary) and the
`NetworkCoordinator` client on OpenRaft 0.9.21 (`serde`, `storage-v2`);
`MemoryCoordinator` remains for in-process tests and demos. Related guides:
[retention and recovery](coordinator-retention.md),
[public coordination](public-coordination.md) (tenant registration, client lane),
[operations](coordinator-operations.md) and [evidence](coordinator-evidence.md).

## Bootstrap from empty state

Construct `CertifiedGenesis` with an independently pinned root public key and
mesh ID. It fixes configuration identity, 3–7 distinct voter keys/addresses,
initial membership (at least three), the membership authority key, the Directory
manifest commitment and at most 64 root-authorized scope bindings.
`DomainControl` bounds executor keys, invocation node/principal pairs, generations,
control revision and validity. Root-authorized issuers are bound to exact scopes;
certifying the Directory issuer gives it no Accounting control or data authority.
The maximum control lifetime is itself root-certified and cannot exceed a year.

Sign genesis offline. Verify its root, mesh and commitment in every composition;
verify the installed Directory manifest against `directory_definition` before
starting its executor. Provision independent voter directories once, then use
the membership administration key to initialize the certified voter set. Normal
startup never provisions a missing image. The root secret is never a voter or
executor configuration field.

```sh
cargo run -p rocky-coordinator --bin rocky-voter --locked -- CONFIG.json --provision
# Later restarts omit --provision.
```

The JSON configuration contains `data`, `genesis` (certified CBOR path),
`trusted_root`, `mesh`, `node` (numeric voter ID) and its private `key`, using
p2panda's serde encodings, plus `client` for a public service's iroh lane. Store
configurations and keys under ignored `local/` with owner-only permissions. The
executable serves the address certified for that voter ID.

## Authority and transitions

App publishers issue `ControlCertificate`s for their certified scope. The service
verifies signatures, lifetime, quotas and the strictly next control revision; a
revision by another issuer is `Denied` (an authority change is a migration).
Updating control fences the old executor, preserves the head, and requires
acquiring the new assignment. A control revision cannot change schema/handler
generations (`Generations`); the scope authority moves generations with an
`Activate` operation at an exact head ([generations](generations.md#activate-strong-execution)).

`NetworkCoordinator::call` accepts stable IDs for controls, acquire/takeover and
membership plans; retry rules and windows are in
[retention](coordinator-retention.md#authority-time-and-operation). Historical
retained outcomes cannot reverse a newer target: use `status` for current voters
and `head` for the current assignment. Every takeover, including the same
holder, increments the fencing epoch. Membership changes keep at least two
voters drawn from the certified candidates; OpenRaft catches up learners and
commits joint then uniform membership, and a retried plan completes an
interrupted change.

Confirmation verifies executor identity/epoch, invocation node/principal pair,
configuration, generations, predecessor and result commitment. Changed input under
a retained identity fails; alternate attempts receive the retained receipt.
The coordinator receives no application payloads or definitions.

Signed, nonce-bound TCP frames (at most 256 KiB) authenticate requests and
responses; frames more than 30 seconds from local time are refused. TCP frames
carry generic metadata unencrypted; a public service carries client requests
over iroh and keeps voter TCP inside the provider network. Voter RPC signatures
bind the sender to the Raft leader/candidate ID and current membership.

Mutations traverse Raft. Head and lookup reads pass a linearizable read barrier
and add no log entry while committed authority time is at most 30 seconds old;
otherwise they go through the log, which advances that time. Evidence of a read
names the read barrier's log position. The client tries
certified voters with the same identity until a 12-second deadline.
Cancellation or timeout can leave a committed outcome: lookup/retry the same identity.
No network/coordinator call runs inside an application transaction.

## Receipts and outages

A voter signs `ReceiptEvidence` only after Raft returns a durably applied result.
The signature binds genesis/configuration, scope, executor/epoch, generations,
request/result commitments, predecessor, sequence and log reference. Lookup can
produce fresh evidence for a retained historical receipt.

The client exclusively locks its evidence file (`<file>.lock`; a second open is
`Denied`) and atomically persists evidence before returning receipt success.
Export, import and eviction rules are in
[retention](coordinator-retention.md#keep-verifiable-results-on-application-nodes).
Clients reject regression to an older sequence/epoch or a different head at the
same sequence (`CoordinationStateLost`). Evidence files are bound to one
certified genesis. During quorum loss, strong work remains pending; admitted
mergeable edits still commit locally. No success receipt is synthesized from
minority state.

## Durability and recovery authority

Separate synced log files, atomic hard state and at most two snapshots preserve
votes, committed/applied recovery, controls, fencing, heads, retirement floors,
authority time, retry windows and membership; see
[storage and budgets](coordinator-retention.md#storage-formula-and-budgets).
Log flush callbacks and apply responses follow durable persistence. Snapshot
installation is one recoverable transition. A store lock prevents a second
process; checksums detect corruption.

A missing hard-state file with a provisioning marker returns `CoordinationStateLost`,
even with `--provision`. A fresh path requires explicit initial provisioning; it
is not recovery for an existing mesh. Never initialize another cluster under the
same certified identity or erase provisioning markers/evidence to bypass this.

Ordinary membership changes require quorum and the membership authority.
Permanent quorum loss requires exceptional offline-root operator authority,
externally retained head/receipt evidence and app payload recovery, with an
explicitly deployed new configuration and client trust migration. There is no
automatic reset/force-recovery API; root possession cannot reconstruct history.

## Design rationale

- OpenRaft was chosen for separate log/state-machine interfaces, its storage
  conformance suite (`openraft::testing::Suite` runs against the real adapter),
  an async network port and joint-consensus membership.
- Receipts are signatures of a trusted crash-fault service, not quorum
  signatures or BFT proofs; voters and authorized executors are trusted.
- Root certification authorizes metadata control only; application publishers
  alone issue application data grants, and the root key never sits on a voter.
- Configuration identity is fixed by genesis; Raft membership and control
  revisions evolve separately. New voter keys, endpoints or roots need a new
  certified genesis and client trust migration, not an in-place edit.
