# SDK checkpoints and catch-up

How the SDK configures retention and uses
[encrypted checkpoints](encrypted-checkpoints.md) for join and catch-up.

## Retention

Every granted SDK collection gets a persisted `RetentionPolicy` when no policy
exists. The defaults are `copies = 1`, `window = 256`, and empty `required` and
`behind` sets. Pruning still requires a verified, flushed checkpoint and at
least one other acknowledged copy. It never releases a sole copy. No timer
silently declares peers behind; an issuer schedules
[horizons](mergeable-horizon.md#sdk) explicitly.
Existing policies survive reopening.

The native session `retention` request names `collection`; optional `policy`
replaces its policy with `required`, `behind` (node public keys), `copies` and
`window`. The response includes the policy, covered/uncovered operation counts,
horizon and maintenance error. `maintain = true` completes checkpoint creation
and pruning under those rules. Kotlin exposes `session.retention(collection,
policy, maintain)`.

`retention_limits` reads or replaces the open replica's limits via `limits`.
Kotlin exposes `session.retentionLimits(limits)`. Defaults are 1,024 operations
per checkpoint, 32 MiB soft bytes, 64 MiB hard bytes, and 65,536 retained
mutation handles. These limits are not persisted: configure them after every
open. Invalid limits and policies fail. See
[encrypted checkpoints](encrypted-checkpoints.md) for backpressure and pruning
blocks, and [replica roles](replica-roles.md) for custody requirements.

## Catch-up

SDK reconciliation restores the peer's base before pushing local operations.
`NativeSession::sync_scope` uses the existing behind-horizon signal to stage,
verify and atomically adopt a checkpoint, then pull its tail. Local operations
not covered by the checkpoint are reapplied under the runtime's admission rules;
mutation handles survive. An interrupted import resumes from durable staging.
A peer behind this device's own horizon must initiate its own catch-up; a push
reports that condition instead of replaying missing history.

## Join

Enrollment persists checkpoint-first intent before installing an identity. Join
starts a sync in the background; applications may also call `sync`. For every
granted collection it requests a current checkpoint, imports it through durable
staging, and pulls the ordinary tail. A writable source creates the checkpoint
on demand, including an empty one. Interrupted transfers resume on sync or reopen;
Each completed scope is recorded after catch-up and a local flush, so retries
resume only incomplete scopes. Intent clears after all scopes complete. Session rollover resumes
the tail after a completed import instead of repeatedly importing its base. A
failed join sync leaves that intent in place, never claiming completion.

Intent covers only collections the device reads in whole; a keyless custody node
syncs its ciphertext rows without one. Such a peer offers no checkpoint, so a
joining device exchanges rows with it and keeps its intent for a peer that does.

Enrollment and renewal send only the current reader key for each granted
collection. Existing devices retain the epochs they already hold. Renewal that
brings revocation statements the device lacked persists fresh bootstrap intent
before adopting its keys, so a device that missed intervening key epochs, or
rows of a revoked author, does not claim complete catch-up from what it cannot
open or admit. A device enrolled after revocation reads earlier rows through a
checkpoint verified under the held [revocations](revocations.md), without
receiving keys for rotated-out ciphertext.

Checkpoint creation and offers refresh a checkpoint signed under another
revocation digest as well as a stale key epoch. Automatic bounded maintenance also detects both changes, even if
there are no uncovered operations; a previous verified base remains until the
new one installs successfully.

## Readiness

The `status` call and Kotlin `readiness()` Flow carry `state`: `bootstrapping`
while checkpoint-first intent or a staged import remains, `syncing` during an
ordinary pass, `ready` when idle with valid authority, `revoked` after a verified
[revocation notice](sdk-enrollment.md#renewal-revocation-and-logout), or `unavailable` after
logout, expiry or during a generation rebuild. `ready` is true only in the
`ready` state. Reads during import still see the previous committed projection;
a new join is not complete until bootstrap finishes. A failed bootstrap remains
`bootstrapping` across reopen. Cancelling an ordinary sync clears its activity.

`status_next` wakes on sync state changes as well as committed revisions; its
token covers the state. Offline local databases can be ready without a network.

## Bootstrap signers

A checkpoint signer needs whole-scope read and a write grant on the same scope.
The existing `Insert`, `Update` and `Delete` rights qualify as writes; read-only
signatures are refused at offer, before staging and again before switching. A
read-only peer can relay a checkpoint signed by an authorized writer; its own
checkpoint serves only local replay. Rationale: a reader that cannot write rows
must not be able to define the state other devices adopt.

Checkpoint offers reuse a still-authorized cut so page/session retries remain
stable. Missing checkpoints or stale authority/key epochs trigger creation;
admitted previous-generation checkpoints retain their generation's policy rules.

Renewal that advances authority replaces bootstrap intent. An older import
attempt cannot overwrite or clear that newer intent when recording completion.
