# Importing an encrypted checkpoint

Implemented behavior of `EncryptedReplica`. A replica adopts a peer's signed
[encrypted checkpoint](encrypted-checkpoints.md) instead of replaying history:
on [join](sdk-bootstrapping.md#join), and when a peer pruned past what the
replica holds. This guide is for a transport author driving the import and for
an application author who wants to know what is readable while it runs.

## Calls

`NativeSession::rebootstrap(replica, scope, quota)` fetches the peer's signed
checkpoint and its rows in pages and switches to it. `bootstrap_scope` does the
same even when the peer still retains its history, then pulls the tail;
`sync_scope` imports only on the behind signal. `offered_rows` returns the
identity and stored-row count of the checkpoint a peer offers for each scope
without importing anything, which is what a caller plans and reports progress
with. Without a session, use `checkpoint_offer`, `begin_rebootstrap`,
`checkpoint_page`, `stage_checkpoint_page` and `finish_rebootstrap`.

An interrupted import of the same checkpoint continues after the last stored
page, reported by `rebootstrap_progress`. `imported_rows(scope, checkpoint)`
counts the stored rows imported so far, and all of them after the switch.

## Signed pages

The signed statement commits to the rows as a set, so a subset cannot be
checked against it. The checkpoint signer therefore cuts its rows into pages in
row order and signs each one: the signature, under `rocky.checkpoint-page.v1`,
covers the checkpoint identity, the row the page follows, the row it ends at and
its row records. It travels inside the page's sealed plaintext.

`stage_checkpoint_page` opens the page and verifies its place, its order and
that signature before anything is stored; decryption, signature check and
decoding run off the replica's thread. A page a key holder changed, shortened,
reordered or moved to another place is refused and stores nothing.

A replica that imported a checkpoint keeps the signer's cut and signatures and
serves exactly those pages onward. It cannot cut pages of its own for a
checkpoint it did not sign.

Row records name their author's bindings by digest (see
[stored row records](#stored-row-records)). A page carries each set its rows
name, once. The importer refuses a page with a missing, repeated, unnamed or
undecodable set, and stores the sets with the rows.

## What is readable during an import

| The scope held at `begin_rebootstrap` | Import | Readable |
| --- | --- | --- |
| Nothing: no stored rows, checkpoint or applied operations | Direct | Every verified page, as soon as its transaction commits |
| Anything | Staged | The previous projection, until the switch |

A direct import stores and projects each page's rows in the page's
transaction and advances the revision, so observers see rows arrive page by
page. Reads report the scope incomplete until the switch. Operations of other
peers for the scope wait and apply after the switch. A local write applies at
once; the switch restores the checkpoint's rows and replays such writes on top.
Starting over with another checkpoint, or a failed final check, removes the
rows a direct import stored.

The switch verifies counts, set digests, creations, moves and strong state
against the statement, then installs the checkpoint record and frontier.
`RebootstrapReport` accounts for every local operation: `superseded` when the
checkpoint covers it, otherwise `reapplied`, `rejected` with the reason, or
`pending` on a dependency or key. A checkpoint that ends before the replica's
own horizon, or whose signature, signer or definition does not verify, is
refused. The signer needs whole-scope read and a write grant; see
[bootstrap signers](sdk-bootstrapping.md#bootstrap-signers). Its credentials
must be valid at admission and again at the switch; an interrupted import does
not pin expired authority.

## While a scope is importing

`set_importing(scopes)` names the scopes an owner intends to import. Such a
scope counts as importing while it holds no stored rows, and any scope does
while pages of it are staged. For an importing scope the replica

- reports it in its frontier exchange, so peers push it no history;
- skips its bounded maintenance: checkpoint creation, sealing and pruning;
- offers no checkpoint of its own.

A session asks for the next page before it stores the one it holds, so its
peer cuts a page while another is verified and stored. A page then costs the
slower of the two, or one round trip, not their sum.

## Strong state

The first page also carries confirmed command rows and the latest result under
the current scope key; the switch installs their sequence and head as the
[strong base](strong-history.md#read-and-catch-up). A cached offer behind the
source's strong pruning base is refreshed. The latest result names its caller's
and its executor's credential chain by digest. The chains travel beside the
first page: in `CheckpointPage::chains`, and on a session in a message of their
own before the page. Staging rebuilds the signed result from them and refuses
the page, storing nothing, unless exactly the named chains arrived
([decision 0053](../decisions/0053-chains-by-reference.md)).

## Stored row records

A row record is stored, digested and transferred with every row, so it is
kept small. Identifiers, commitments and values are CBOR byte strings and a
stamp is a tuple. A creation proof holds the digest of its author bindings; the
bindings, about 760 bytes that repeat on every row an author creates, are
stored once in `secure_author_bindings`. A [move](row-moves.md) reads them back
to carry the complete proof, whose signature covers the bindings themselves.

A one-line item of the shopping schema takes 1,129 bytes, down from 2,468.
Checkpoint row tables are ordinary rowid tables: SQLite moves any row above
about 1 KiB of a `WITHOUT ROWID` table to an overflow page of its own.

## Limits

- A page holds at most 256 rows and 63 KiB of row records and binding sets:
  its sealed plaintext must fit one 64 KiB package.
- The switch is one transaction proportional to the scope's rows. For a direct
  import it replays only local writes made meanwhile.
- A direct import needs a scope that holds nothing. A replica with rows keeps
  its previous projection until the switch, however long the import takes.
- Binding sets are never removed: evicting a scope leaves the sets its rows
  named. There is one per author credential chain.
- A session imports within its byte budget; the next session resumes at the
  next page.

Measured on a Mac, release build, 10,000 one-line items over the
in-memory transport: first rows readable 0.39 s after the join began, all rows
after 2.1 s. Before signed pages the same join showed nothing for 7.9 s.
The [Android measurement](checkpoint-import-evidence.md) records a fresh
10,000-item join over LTE, progressive visibility, live-edit latency and the
remaining first-data delay.
