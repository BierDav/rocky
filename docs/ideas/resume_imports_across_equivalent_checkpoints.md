# Resume an import across equivalent checkpoints

Status: idea, not designed in detail and not prototyped. Accepted limitation
for now.

## Problem

A joining device imports a scope from one peer's signed checkpoint, page by
page ([checkpoint import](../runtime/checkpoint-import.md)). Two peers that
hold the same rows still offer different checkpoints: each signs its own
statement, and the statement also commits to per-replica state such as
retained mutation handles, custody and confirmed commands.

The SDK keeps a staged import with its signer while that signer is linked
([join](../runtime/sdk-bootstrapping.md#join)). When the signer is gone, the
next peer restarts the scope from its own checkpoint:

- every staged page is discarded and progress returns to 0%;
- a direct import also removes the rows it already showed.

A household with a 10,000-row list imports about 220 pages, so losing the
source phone midway costs the whole transfer again.

## Idea

Keep the staged rows when the new checkpoint describes the same rows.

- Compare the statements' row, tombstone, creation and move summaries, scope,
  definition and key epoch. Equal summaries mean the staged rows are valid
  for the new checkpoint.
- Rebind the staging to the new signed checkpoint and keep its cursor. A
  signer cuts pages from any row, so the new peer can continue after it.
- Fetch the new checkpoint's first page for its strong state, without storing
  its rows a second time.
- `finish_rebootstrap` still verifies every summary against the new
  statement, so a wrong match fails as today.

## Cost and open questions

- A relay serves only its signer's page cuts and cannot continue after an
  arbitrary row; it would still restart.
- Strong state travels in the first page today; splitting it from the rows
  changes the page protocol.
- Staging records the signed checkpoint once; rebinding must stay atomic with
  the page cursor across crashes.
