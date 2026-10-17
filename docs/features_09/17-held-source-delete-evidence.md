# Held source validation and incarnation deletes (rounds 41–42)

Status: bounded repairs implemented from the accepted HIGH41 and HIGH42 decisions.
Fresh independent HIGH review and final-source qualification remain pending.
This record accepts neither core phases 14/17/19 nor phase 20.

## Original failures and corrections

The [round40 staging record](17-staging-atomicity-evidence.md) preserves two
actual failures. The full 2→3 DELETE + INSERT SELECT migration failed because
an additive SQL image's NULL field made the visible incarnation look like an
extra hidden row. The full 3→4 variant then exposed original source images
being validated by the target app during final switch → moved-line holding.
Neither failure was waived or replaced by the INSERT SELECT-only control.

Q2 was repaired first. Pure SQL deletion builds the visible delete once and
requires its numeric identity. Each indexed live candidate must agree with its
image identity, materialized identity, incarnation commitment and current key
binding. Other IDs retain their original before images and empty masks;
tombstones contribute nothing. Sparse/default/NULL semantics, the separate
relink branch and bundle uniqueness/mask checks retain their contracts.

Q1 resolves each held frame's structural source schema in its full domain
and requires its exact catalog definition. Complete source images retain their
fields, values, masks and original signed/encrypted bytes. Matching definitions
borrow the installed compiled app directly. Different definitions require the
fail-closed source-application capability, checked against the exact manifest.
Typed rows read the one current replicated shape with shared scalar/default
and key helpers; they build no historical SQL projection and retain no old app.
The reader lives for one frame, with no provider, network call or new cache.
Current frame authority, source-epoch opening, readable custody and current
ACL fields remain mandatory alongside source validation and source ACL fields.

## Focused controls

- Full preserved 2→3 and 3→4 migrations: signed F before genuine offered W;
  deterministic stale-stage refusal; identical signed staging at the publisher
  holding exactly F; real line node, frozen-parent DELETE + INSERT SELECT,
  endorsement/release and normal follow/intake; exactly copy-one on both nodes.
- Non-NULL source note, exact original images/masks/id/names/bytes and historical
  scope/hash binding; audits, target reopen without an old source app, held
  paging, and additional genuine multi-member source ingress with a removed table.
- Final signed source-epoch foreign-field and partition A/header–B/image
  negatives, including a later invalid write and a member ordered after a valid removed-table
  member; real current credentials,
  post-intake authoritative-state comparisons and valid controls.
- Missing exact source refusal, deliberate normal branch acquisition, wrong
  same-version hash refusal and retry after exact catalog installation. Generic
  unsupported/wrong-manifest factories refuse; matching Employees callbacks
  and ACL checks, and the current guard with a different source factory, remain.
- Actual held-insert failure during the final switch transaction compares the
  staged-plan SQL/graph/app/author/workspace boundary, then retries the same instance.
- Two genuine live IDs at one key after nullable and required-default additions:
  two deletes, each once, empty masks, indexed tombstones skipped, unchanged
  unrelated keys, genuine late edits of both deleted IDs, an independent replacement,
  rebuild/audit/reopen. A remove/re-add case deletes a sparse retained row once
  without restoring its old field value.
  A partitioned SQL control preserves the other partition. Duplicate same-ID
  deletions and invalid masks still refuse at the real author gate.
- Publication fault after capture/authorship immediately before SQL COMMIT:
  complete authoritative SQL, graph/app/author context and revision rollback,
  followed by one successful retry with the same publication ID.
- Round40 F1 and all three F2 races, INSERT SELECT-only positive, source-epoch
  rotation, all-original-write refresh, removal/re-add, sideways/kept history,
  original-prefix guard and natural-key/identity/mask controls are retained.

## Execution and limits

The round41 secure selection passed 111/111 in 13.499 s; its slowest test was
0.531 s. Core typed-row/source-shape tests passed 6/6; scoped core and secure
Clippy with all targets passed. These are focused results on round41 code,
including the corrected member ordering and the original signed metadata.
The no-default SDK two-step migration passed in 0.981 s, and the real private
service migration passed in 4.456 s under its e2e profile.

Exact commands, exits, durations and logs are recorded in
`/tmp/rocky-round41-commands.jsonl`; final results accompany the implementation
handoff. Integration remains 2 s alone / 300 s suite; e2e remains 600 s alone /
900 s combined. Focused selections do not qualify either complete suite.

## Matching typed source key binding (round42)

HIGH42 found that matching-manifest holding borrowed the installed TypedRows
correctly, but its per-write callback checked only foreign-key publication units.
Partition and numeric-incarnation checks did not bind the complete business key.
`TypedRows::validate_write` now calls its existing typed validator after the
foreign-key unit refusal. Both complete images must name the supplied canonical
key; scalar/default, partition and identity semantics use the existing checks.
Generic application callbacks and matching-manifest selection are unchanged.

The actual held-ingress regression re-seals and signs a genuine source3 update,
changing only the later write's row commitment. It retains the original envelopes,
valid complete images and unchanged numeric identities, and asserts no creation
proof for either update. A receiver with the same manifest follows an empty-cut
close and refuses specifically with `items row identity differs from its key`.
No held rows/frames, resolution, void, admission evidence, identity marks or
projection effects appear. Repeated apply preserves the post-intake snapshot;
normal custody bookkeeping is retained. The valid original becomes Held, serves
byte-exact frame data and audits. Core controls cover both image directions and
foreign-key-unit error precedence.

Core tests passed 7/7. The secure integration selection passed 42/42 in 5.350 s,
slowest 0.518 s, including the new matching-source case (0.119 s), generic
Employees/current ACL, different-source, partition, rotation and SQL controls.
These runs were retained during usage-reset recovery because code was unchanged.
The earlier secure attempt failed an incorrect empty-creation-witness assertion;
updates legitimately retain witnesses. The earlier core attempt failed an
unsupported reference-schema fixture. Correcting those tests changed no
production gate. Exact failed and passing commands remain in
`/tmp/rocky-round42-commands.jsonl` and their named logs.

Recovery checks also passed: core/secure all-target Clippy, the no-default SDK
two-step migration (0.948 s test / 0.951 s runner), and the real private-service
migration with default iroh features under its e2e profile (4.589 s).

Root owns the fresh independent review and actual final matrix. The last
completed combined e2e result remains the earlier 1035.29 s timing failure;
matrix9 e2e was deliberately interrupted. The older proper SDK 100k pass
(428.249 s) and platform passes qualify their earlier sources only. No full
matrix, 100k workload, platform/app/device or artifact work was run here.
