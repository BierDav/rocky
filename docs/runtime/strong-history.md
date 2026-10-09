# Encrypted strong history

Bounded confirmed-command history on the encrypted path. The trusted path has
its own rules in [checkpoints](checkpoints.md).

## Configure a window

Use the existing `RetentionPolicy` for every scope holding confirmed rows of a
domain. The SDK's default window is 256; direct replicas configure their policies.
Each domain has one monotonic strong horizon: the minimum local applied head and
the highest sequence applied by each required reader outside `behind`, less the
largest scope window. A missing reader or a custody-only acknowledgment blocks
it. With no required readers, local application determines it. Scope policies
combine conservatively, so a partial reader cannot advance another scope. Strong
sequence/head evidence comes from coordinator confirmation; `copies` adds no
strong acknowledgment requirement.

Maintenance runs after confirmed-result commit, result acknowledgment and the
existing mergeable group-commit step. Each domain scans at most 64 results,
preparations and settled commands per step. A persisted scan cursor prevents old
pending jobs from starving later results. Each batch and its base advancement
commit atomically; a failure rolls both back and the status records the error.
Outgoing acknowledgments follow a flush. Explicit mergeable `prune` remains the
operation-history API.

Preparations disappear once their request settles, including stale attempts at
older heads; an obsolete preparation does not settle its request. Unknown
outcomes remain recoverable. Settled commands behind the horizon leave
handle/input/identity markers and a persisted sequence high-water mark. Their
retries return `Error::Pruned`; new input conflicts. The marker follows recovery
to a new signing identity and is retained independently of the mutation handle
window. The latest command/result stays available, even with a zero window.

A result through the horizon is deleted only after a base holding its after-rows
is installed and every held job has a finished success/failure observation. A
started or uncertain attempt does not release it. Readers retaining a subset
handle only jobs in their retained scopes. Observations and retry markers have
independent storage costs; the plateau qualification covers command, preparation
and result payloads, not every byte of the SQLite file.

## Read and catch up

Each scope keeps authenticated confirmed rows at a base sequence/head. The first
command read after open or an authority change audits that base and the retained
suffix, verifying predecessor heads and rows. Later reads continue incrementally.
Full reconcile applies only projections beyond the already installed head.
Historical checkpoint verification checks explicit table grants even when the
signer also holds selectors for unrelated partitioned tables. It still requires
a node credential with whole-scope read and write authority; a partitioned
checkpoint must carry and verify its concrete tuple.

`missing_results` starts above the local base. When the source cannot supply a
missing prefix, SDK result sync uses [checkpoint bootstrap](sdk-bootstrapping.md);
it also bootstraps a readable scope whose held ciphertext cannot complete its
rows. The signed checkpoint's `strong.base` binds confirmed rows at its head;
result, command and preparation summaries cover only the suffix above it
(unsettled requests and unknown outcomes stay bound; observations are bound
separately). The encrypted page carries those rows and the latest result under
the current scope key. Import installs them atomically with the mergeable
checkpoint switch; a checkpoint behind local strong application is refused.
Current signer credentials and current keys let a device enrolled after
revocation catch up without old keys or earlier-policy result parts.

## Recovery

Recovery packages carry strong bases, retained suffixes and pruned
command-handle identities. They verify head closure from the base, then the
retained suffix. Base rows authenticate against their result or a scope writer's
signed checkpoint; receipt verification stays outside database transactions. A
missing suffix predecessor or altered base is refused. Recovery still requires
complete **mergeable** operation history; an operation-pruned scope uses
bootstrap. No coordinator format or result acknowledgment message is involved.

Qualification and reopen timings: [end-to-end evidence](end-to-end-evidence.md).

## Design rationale

- One strong horizon per domain, not per scope: results span scopes, and
  combining scope policies conservatively keeps a partial reader from advancing
  history another scope's readers still need.
- Pruned commands leave a handle/input marker and a sequence high-water mark, so
  a retry is answered `Error::Pruned` instead of being signed again.
- The base travels in the checkpoint under current keys, so catch-up after
  revocation needs neither old keys nor a new acknowledgment message.
- Only strong history is bounded here; recovery of mergeable rows still needs
  complete operation history.
