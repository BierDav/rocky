# Arbitrary paging of a table

Status: idea, not designed in detail and not prototyped.

## Problem

A device can keep a table small by holding only some of its partitions
([partitioned scopes](../runtime/partitioned-scopes.md)). That
covers windows over immutable key columns, such as the booking month. It does
not cover "the 100 most recently edited orders" or any other page defined by a
sort order or predicate over mutable columns.

## Idea

A full node serves the matching rows as state, and the device merges them.

- The device sends a predicate, a sort order and a limit, building on remote
  reads.
- The full node answers with the current rows, without their history. Rows
  merged per column with the latest write winning can be merged as state, so
  the device can combine a page with rows it already holds.
- The device keeps the page as a cache and asks again to refresh or to move
  the window.

## Cost

The trust is that of a bootstrap checkpoint: the device believes the serving
node about content and about completeness. In addition the device cannot

- serve peers from the page,
- evaluate unique indexes or foreign keys over it locally,
- answer any query other than the one the page was fetched for. The
  [completeness rule](../runtime/partitioned-scopes.md#sql-and-completeness)
  for partially held tables would need a notion of "this page" beside "these
  partitions".

## Open

- Whether a page is a separate read-only result or rows in the typed table.
- How local writes to a paged row are published without the row's history.
- Whether the serving node signs the page, so that a wrong answer is at least
  attributable.
