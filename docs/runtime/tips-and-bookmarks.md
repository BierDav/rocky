# Tips and bookmarks

Implemented at the `rocky-credentials` and `rocky-secure` boundaries. The SDK,
JSON and Kotlin developer APIs and coordinated terminal entries are a separate
integration step in [phase 14](../features_09/14-seals-and-bookmarks.md).

## Select a workspace

A domain's `TipGraph` contains signed seals, openings and bookmark moves.
Every admitted peer exchanges the complete graph, including peers following
other tips. `NativeSession::sync_tip_graph` refreshes it on existing links;
SDK reconciliation does so before exchanging rows. Row subscriptions do not
filter this metadata. Statements are
verified before the union and any resulting workspace switch commit together.
Invalid statements or a failed switch leave the installed graph and rows intact.

The application chooses a bookmark name. The creator signs its initial move
from `None` to genesis; `main` is a convention, not an implicit Rocky bookmark.
Genesis is `commitment("rocky.genesis-tip.v1", manifest_digest)`, qualified by
the graph's domain scope. `EncryptedReplica::follow_bookmark(scope, name)`
selects it; `followed_bookmark()` and `followed_tip()` report the selection.
The current implementation supports one live selection per replica file.

A seal names its source tip, a frontier of global sequence marks per node
author, and `Continue { schema }` or `Close`. Continue preserves the schema
until phase 17 and opens the tip whose `TipId` wraps the seal digest. An
explicit opening starts a branch at a seal's frontier; its tip wraps the
opening digest and declares its log's minimum in-sync size.

`publish_tip_statements` and `receive_tip_statements` accept these signed
values. For a configured coordinated log, seals and bookmark moves return an
expected error until terminal log-entry integration supplies their ordering.
They never bypass the coordinated log.

## Authority and conflicts

Issuers with whole-domain authority seal tips and move bookmarks forward along
continue seals. The owner and owner-trusted identities also open tips and
create, delete or move bookmarks backwards or sideways. Trust comes from
`Authority::issue_trusted`: an owner-direct credential extension
`Extended.trusted: Option<Scope>`. A node certificate adds no control rights;
statements use the identity's signer and authority chain.

The lower statement digest wins concurrent seals of one tip or moves of one
bookmark from the same source. Losing signed histories remain graph evidence.
A backward or sideways move off an open tip must include its close seal.
A bookmark at a sealed tip, or one deleted without a replacement, cannot write.

## Nodes and row isolation

A node's author log belongs to one continue line. Switching to a branch
rebuilds rows and existence state from its sealed base plus its own frames.
`enroll_line_node(identity_provider)` creates a node without an issuer;
`register_line_node` supplies its credentials and key for that line. Until
registered, the new workspace is read-only. Returning to a line selects its
previous node. A node that wrote into a losing continuation needs a new
node before it can write on the winning fork. Keys stay in the caller's secret store: after reopening a
branch, supply its saved node again before writing.

Peers declare the followed tip in their authenticated hello. Row offers,
frontiers, range transfers and remote reads require the same followed tip;
frames of unrelated tips are refused. Discovering another schema never adopts
it. Checkpoints must name the selected tip. A switch that needs history already
compacted away fails with an expected error; target-checkpoint switching is
not implemented by this slice.

`drop_tip(tip)` removes an inactive workspace's retained frames atomically.
It requires acknowledged custody of local writes and refuses active ancestry,
retained dependencies, checkpoints or held evidence. It preserves graph, handles
and line-node reservations; returning to that line must restore its dropped
original frames before writing.

## Late and held writes

Late frames beyond a continue frontier retain their original signatures and
walk continue seals to the live tip. Fields apply under the installed schema's
existing removal rules. Branch openings inherit only their seal's prefix and
take no later writes from the parent line.

A close, or leaving a line through a backward or sideways bookmark move, makes
its excluded writes [held changes](held-changes.md). Held transfer carries
original encrypted frames; a relay never signs for their author. Every readable
holder can offer them through `held_frame_page`; `NativeSession` supplies
`pull_held_page`, `push_held_page` and bounded `sync_held_changes`, independently
of followed tips. Ordinary scope sync exchanges these when a tip graph exists.
Re-authoring writes new frames on the selected tip with the existing held-change
transaction API.

Adopting a seal or a forward move changes metadata only. Row rebuilding and
held classification belong to leaving a line or requesting held evidence,
not to seal receipt. The sealing regression compares unchanged stored rows and
equal SQLite write counts with 10 and 10,000 rows.
