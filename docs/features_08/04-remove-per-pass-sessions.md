# 4. Remove per-pass sessions

Status: implemented and verified (2026-10-12); see [evidence](#evidence).
Owner: `rocky-core` `net.rs`; `rocky-secure` `network.rs`; `rocky-sdk`
`sync.rs`, `node.rs`, `node/mesh.rs`, `enrollment.rs` and transports;
`rocky-iroh`; the always-on peer; the shopping qualification harness.
Depends on [phase 3](03-data-over-the-link.md). Follow the
[execution contract](README.md#agent-execution-contract).

## Objective

After phase 3, every exchange runs on a link, and the per-pass session
remains only as a fallback and in callers that were never moved. This phase
deletes it, so the codebase has a single sync path. See
[what remains](README.md#what-the-old-sessions-are-gone-means).

## Settled

- `Protocol::Sync`, its versioned name, its inbound handling and the dialer
  in `sync.rs` are removed. An inbound stream naming the old protocol is
  refused as any unknown protocol is.
- Explicit `sync` (Rust, JSON and Kotlin `syncOnce`) uses the peer's open
  link, or opens one, reconciles and keeps it while the worker runs. With
  the worker disabled, it closes the link after the reconcile. The response
  shape does not change.
- The always-on peer, custody, bootstrap and the qualification harnesses use
  links. A short link replaces each one-shot session.
- **Over the link budget.** A refused link is retried with back-off. It does
  not fall back to anything. The [useful peers idea](../ideas/useful_peer_links.md)
  is the route to larger meshes.
- No compatibility: a device on the previous release cannot sync with one on
  this one, as [decision 0039](../decisions/0039-single-current-formats.md) requires.

## Ordered slices

1. Move the remaining callers: explicit sync, the always-on peer, custody,
   enrollment bootstrap and the harnesses.
2. Delete the protocol, the server path, the dialer and the fallback, along
   with tests that exercised only them. Port tests that check behavior.
3. Search the workspace, the Kotlin SDK and the examples for the old protocol
   name and the removed calls; none remains.
4. Guides: SDK transports, always-on peer, SDK enrollment, secure sessions,
   the shopping qualification and the module map.

## Completion gate

- `rg` finds no `Protocol::Sync` and no old session name in sources or guides.
- The full `./scripts/check` passes, including the shopping and market
  qualifications, on links only.
- An idle pair makes no dial after its link is open; a lost link is dialed
  again with back-off.

## Evidence

Slice 1: explicit sync, contact requests, custody, bootstrap and the harnesses
reach peers only through `request_link`, which returns a link handle or an
error. A refused link, a busy one included, backs off; `Upstream` has no
transport, addresses or optional link. Resolving a pinned contact reference
over a contact's routes dials `rocky/link/1`; the acceptor answers before any
handshake (`Link::accept_or_resolve`). Enrollment-only references still use Renew.

Slice 2: `Protocol::Sync`, its versioned name, `NATIVE_SESSION_VERSION`, the
SDK's inbound `Sync` branch, `serve_contacts`, the dialer in `sync.rs` and the
busy fallback are deleted. `rocky-secure` keeps the handshake only as the link
handshake (`rocky.link.v1`, version 1); `NativeSession` exists only as a request
channel. `Link::drive` carries channels for embedders without the SDK worker:
the employees demo and the runtime-scaling roles probe. Tests that checked
behavior run on links through a test-only pair helper; holding tests account
for proofs made when a link opens. Deleted, as they exercised only the removed
path: the native-name test, `link_handshake_refuses_native_session_identity`
and `an_earlier_session_version_is_refused`, whose link equivalent is
`another_session_version_is_refused_with_the_authentication_error`.

New gates: `an_idle_linked_pair_dials_nothing_for_an_hour`,
`explicit_sync_without_the_worker_closes_its_short_link` and
`a_busy_link_backs_off_without_another_sync_path_and_reopens_when_a_slot_frees`.
`three_missing_pongs_close_and_reconnect_with_backoff` still shows a lost link
dialed again after back-off. `inbound_streams_over_budget_are_dropped_and_served_again_later`
now shows that a full transient-stream budget leaves links usable.

Slice 3: the search below finds nothing in sources, the Kotlin SDK, the
examples, the benchmarks or the current guides. Decision records, `docs/old`
and recorded benchmark evidence keep their history.

```sh
rg -n 'Protocol::Sync|rocky/native-session|rocky\.native-session|NATIVE_SESSION|serve_contacts|NativeSession::(authenticate|accept|accept_or_resolve)\b' \
  crates sdks examples docs benchmarks README.md \
  --glob '!target' --glob '!docs/old/**' --glob '!docs/decisions/**' \
  --glob '!**/evidence/**' --glob '!docs/features_08/04-remove-per-pass-sessions.md'
```

Slice 4 updates SDK transports, live links, secure sessions, always-on peer,
SDK enrollment, permissions and sync, replica roles, revocations, mergeable
horizon, the shopping qualification, the module map and the README.

## Pre-merge qualification (2026-10-12)

Before the stack merge, the code slice was `vkmusspr / 4ee1a218`; the guides
slice was separate.
The resumed review read both diffs, confirmed the explicit-sync close regression
is in the code slice and completed these checks against it:

- `./scripts/lint`: passed, exit 0; formatting required no fixes.
- `./scripts/check`: passed, exit 0; formatting, strict workspace Clippy,
  152 unit tests (136 workspace, 16 SDK without defaults) and 917 integration
  tests (696 workspace, 221 SDK without defaults). Unit wall time was 123.7 ms;
  integration wall time was 168.53 s, 87.82 s after the checker's load adjustment,
  within the unchanged 300 s budget. Isolated timing checks also passed.
- `./scripts/check --e2e`: passed in the foreground, exit 0; all 84 tests
  (59 workspace, 25 SDK without defaults), wall time 396.44 s within the
  unchanged 900 s budget. Both shopping release gates passed in both builds.
- All four idle, explicit-sync close, busy-link and reconnect gates named
  above passed in both builds. The idle gate retained the same link for a
  paused-clock hour with no dial; the close gate reconciled the row, closed
  both registries and produced no traffic over the following paused-clock hour.
- The removal search above produced no output, exit 1 (no matches).
  An unfiltered search across crates, SDKs, examples and docs found only
  historical decision records, `docs/old` and this removal brief.

The first resumed full check failed during compilation with `No space left on
device`. Only this workspace's artifacts for the six rebuilding packages
(`rocky-sdk`, `rocky-secure`, `rocky-coordinator`, `rocky-runtime`,
`runtime-scaling`, `todo-desktop`) were cleaned; the complete rerun above
supersedes that failed attempt. No test budget or assertion changed.

These are Rust workspace and SDK qualifications, including native shopping and
command/confirmed-table coverage. Standalone Kotlin shopping/market harnesses,
the excluded native ABI checks, doc tests, desktop UI and real-network/mobile
qualification were not rerun. Phases 5–6 remain unstarted.

## Post-merge qualification (2026-10-12)

The merged code is `vkmusspr / 909be856`, the merged guides are
`lrnuoozr / 3f3ca107`, and the scheme change `ttlykuxu / 02cb158f` is retained.
None of those commits was edited. Verification started from the empty
`wsvqorwr` change; only this workspace's own target directory was cleaned
with Cargo, removing 36.4 GiB before the clean build.

The first full check passed all 917 integration executions but failed the
2 s timing gate for `iroh_authority_gate` and `iroh_liveness_gate`: each was
3.15 s alone in the SDK build without defaults. Instrumentation located the
authority test's delay after its assertions: 184 ms before detach, then about
3 s closing the second endpoint. Repair `wsvqorwr / 48da7818` stops both
workers before either endpoint closes and detaches both concurrently. It
changes test teardown only; no assertion, production deadline or budget changed.
The four affected TCP/iroh authority and liveness tests passed serially in
0.67 s. Temporary timing instrumentation was removed.

Final checks against that repair:

- `./scripts/lint`: passed, exit 0; no formatting fixes were needed.
- `./scripts/check`: passed, exit 0; strict workspace Clippy, 165 unit tests
  (149 workspace, 16 SDK without defaults) and 917 integration tests
  (696 workspace, 221 SDK without defaults). Unit wall time was 102.3 ms;
  integration wall time was 142.71 s against the unchanged 300 s budget.
  Isolated iroh authority/liveness confirmations were 229.4 ms and 154.9 ms,
  within the unchanged 2 s per-test budget.
- `./scripts/check --e2e`: passed in the foreground, exit 0; 84 tests
  (59 workspace, 25 SDK without defaults), wall 277.08 s against the
  unchanged 900 s budget. Both shopping release gates passed in both builds.
- Idle, explicit-sync close, busy-link back-off and lost-link reconnect
  regressions passed in both builds. The explicit-sync regression remains
  in the merged code slice.
- The removal search above produced no output, exit 1. The requested
  unfiltered search across crates, SDKs, examples and docs found only
  historical records and this removal brief.

Standalone Kotlin shopping/market harnesses, excluded ABI checks, doc tests,
desktop UI and real-network/mobile qualification were not rerun. Phases 5–6
remain unstarted. The passing full rerun supersedes the timing failure above.
