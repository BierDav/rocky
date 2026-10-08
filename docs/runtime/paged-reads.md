# Paged reads and cursor kinds

Trusted `Runtime`/`Replica` path. The encrypted path pages by row key through
`snapshot_page`; see [permissions and sync](permissions-and-sync.md).

A [bound read](app-runtime.md#read-authorized-local-data) returns one page: at
most `max_rows` (1 to 1,024) rows and `max_bytes` (at most 1 MiB).
`ReadSnapshot::next` is set when more rows remain; pass it as
`ReadOptions::cursor` with the same query, values and scope. A cursor used
with any other query is refused.

There are two kinds of cursor. Which one a read gets decides what deep pages
cost and how they behave under concurrent writes. This page is for application
authors, who declare view keys, and for UI and SDK consumers, who write queries.

## The two kinds

| | Keyset cursor | Offset cursor |
| --- | --- | --- |
| Continues by | Seeking to the ordering values of the last row read | Skipping the rows already read |
| Cost of a page | About the same at any depth | Grows with the depth, about 45 ns per skipped row |
| Full pass over N rows | Linear in N | Quadratic in N |
| A row inserted or deleted before the position | Not noticed: no repeat, no gap | Shifts later pages: a row repeats or is skipped |
| Result order | The query's order, then the view's key | Exactly the query's order |
| Available for | Ordered plain queries on a view with a key | Every accepted query |

Measured at 500,000 rows, 12,500 rows deep in a 25,000-row table
([scaling limits](scaling-limits.md#measured-limits)):

| Page | Keyset cursor | Offset cursor | First page |
| ---: | ---: | ---: | ---: |
| 10 rows | 30 µs | 579 µs | 32 µs |
| 100 rows | 54 µs | 589 µs | 57 µs |
| 1,000 rows | 285 µs | 668 µs | 280 µs |

`ReadCursor::kind()` returns `CursorKind::Keyset` or `CursorKind::Offset`. SDK
snapshots carry it as `next_kind`: `"keyset"`, `"offset"` or null with `next`.
Check it when a list can grow large; an offset cursor is not an error and is
never reported as one.

## When a read gets a keyset cursor

All of these must hold. Otherwise the read still works and pages by offset.

1. **The view declares a key.** The application sets `ReadView::key` to columns
   of the view that together identify a row and are never NULL.
2. **The select list is `*` or plain column names.** No aliases, expressions,
   aggregates or `DISTINCT`.
3. **There is an `ORDER BY` of plain column names in one direction**, all
   ascending or all descending. No expressions, positions, `COLLATE` or `NULLS`.
4. **A descending order names only never-NULL columns**: key columns or columns
   the view lists in `ReadView::not_null`.
5. **No `GROUP BY`, `HAVING`, `LIMIT` or numbered parameters** (`?1`).

```sql
-- keyset
SELECT note FROM drafts WHERE sequence > ? ORDER BY sequence
SELECT * FROM drafts ORDER BY node DESC, sequence DESC
-- offset
SELECT note FROM drafts                          -- no order
SELECT note FROM drafts ORDER BY note DESC       -- note is not declared never NULL
SELECT note AS text FROM drafts ORDER BY note    -- alias
SELECT COUNT(*) FROM drafts GROUP BY node        -- grouped
SELECT note FROM drafts ORDER BY sequence LIMIT 50
```

## What a keyset cursor costs

- **The order gains a tiebreak.** The runtime appends the key columns that the
  `ORDER BY` does not name, in the same direction. Rows that tie on the query's
  order come in key order, on the first page too.
- **The engine must be able to seek.** Index the ordering columns. Without an
  index both kinds sort the whole result for every page; the keyset cursor then
  only saves the skipping. `Replica::explain_read` shows the plan of a continued
  page.
- **The declaration is trusted.** The runtime does not verify `key` or
  `not_null`. A key that is not unique makes pages skip rows that tie on it. A
  `not_null` column that holds NULL makes descending pages skip those rows. Wrong
  declarations lose rows from paged results; they never widen what a read may see.
- **NULL in an ascending order is handled, by offset.** NULL sorts first, so
  pages that end on a NULL ordering value continue by offset until the values
  begin.
- **Ties are sorted unless the index holds the key.** With an index on the
  ordering column alone, the engine sorts each group of equal ordering values
  by the key. That is cheap for mostly distinct values and costly for a column
  with few values. An index on the ordering columns followed by the key
  columns removes the sort.
- **The seek uses the leading ordering column's index.** The runtime places
  its seek before the query's own filter, because of two lower bounds on one
  indexed column the engine seeks with the first.
- **A cursor is larger.** It carries the ordering values of one row, as text
  the client may store. It is client data: whatever it holds, the seek runs
  inside the authorized view. At most 16 ordering and key columns.

## What an offset cursor costs

Use it knowingly for short lists, grouped results and first pages. For a deep
pass over a large table either change the query to meet the rules above, or
page yourself: keep the last row's key and bind it into an indexed predicate,
such as `WHERE a > ? ORDER BY a`. That costs the same as a keyset cursor and
needs no declaration, but every caller writes it.

## Neither kind is a snapshot

Each page is its own read transaction and reports its own revision. A pass is
one consistent snapshot only if every page reports the same revision. A keyset
pass over changing data returns every row that existed throughout the pass
exactly once; rows written during it may or may not appear.

## Design rationale

Offset-only paging was rejected: deep passes are not proportional to the page
and shift under concurrent writes. A general keyset for mixed directions and
arbitrary NULL placement was rejected too: its predicate is an OR chain that
the engine does not serve as one index range. The accepted costs are a
token-level parser for the ordering clause, the two trusted view declarations
and a defined key tiebreak.
