# Single-row author statement baseline

Change `uuzywvqz`, phase-start change `txzxmumx`; Apple M5 Pro, arm64 macOS, test profile.
One row updates from `second` to `third` through the local author entry point.
The source already holds 32 rows, and the metadata cache is warm. Authoring includes
its transaction, revision publication, post-commit maintenance and WAL check.

```sh
ROCKY_TRACE_AUTHOR=1 cargo test -p rocky-secure --lib --features qualification \
  an_ordinary_update_applies_in_few_statements_per_row -- --nocapture
```

Author: **87 statements**. The paired phase-start receive corpus is unchanged:
**83** statements for one row and **173 / 32 = 5.40625** per row for 32 rows.
The roadmap's 6.40625 value does not match this current corpus. The author count
is a new baseline; no pre-instrumentation author count was asserted.

Grouping uses the first table after `FROM`, `INTO` or `UPDATE`; control and
scalar statements share one group. This is a trace grouping, not a query plan.

| Table/group | Statements |
| --- | ---: |
| `secure_admission_context` | 3 |
| `secure_admission_entries` | 1 |
| `secure_applied` | 1 |
| `secure_author_sequences` | 1 |
| `secure_chains` | 2 |
| `secure_compaction_lost` | 2 |
| `secure_dropped_tip_frames` | 1 |
| `secure_evicted_logs` | 1 |
| `secure_finalized` | 3 |
| `secure_handles` | 2 |
| `secure_identity_ledger` | 1 |
| `secure_identity_marks` | 2 |
| `secure_identity_own` | 1 |
| `secure_identity_partitions` | 1 |
| `secure_identity_summary` | 1 |
| `secure_keys` | 1 |
| `secure_logs` | 3 |
| `secure_metadata` | 4 |
| `secure_migration_frames` | 1 |
| `secure_migration_publications` | 1 |
| `secure_node_authors` | 1 |
| `secure_node_tips` | 1 |
| `secure_operations` | 7 |
| `secure_outbox` | 1 |
| `secure_position_sources` | 1 |
| `secure_pulled` | 4 |
| `secure_quarantine` | 1 |
| `secure_read_epoch` | 2 |
| `secure_rebootstrap` | 2 |
| `secure_retained` | 4 |
| `secure_row_positions` | 1 |
| `secure_row_records` | 4 |
| `secure_schema_switch` | 1 |
| `secure_scope_state` | 7 |
| `secure_seal_anchors` | 3 |
| `secure_seal_bounds` | 2 |
| `secure_unoffered` | 1 |
| `secure_voids` | 1 |
| `transaction/control` | 10 |

## secure_admission_context (3)

```sql
SELECT context,signature,head,head_signature FROM secure_admission_context WHERE id=1
UPDATE secure_admission_context SET context=?,signature=?,head=?,head_signature=? WHERE id=1
SELECT context,signature,head,head_signature FROM secure_admission_context WHERE id=1
```

## secure_admission_entries (1)

```sql
SELECT entry FROM secure_admission_entries WHERE frame=? AND kind=0
```

## secure_applied (1)

```sql
INSERT INTO secure_applied(scope,hash) VALUES(?,?)
```

## secure_author_sequences (1)

```sql
INSERT INTO secure_author_sequences VALUES(?,0) ON CONFLICT(node) DO UPDATE SET sequence=sequence+1 WHERE sequence<9223372036854775807 RETURNING sequence
```

## secure_chains (2)

```sql
SELECT 1 FROM secure_chains WHERE digest=?
SELECT 1 FROM secure_chains WHERE digest=?
```

## secure_compaction_lost (2)

```sql
SELECT seal FROM secure_compaction_lost WHERE domain=?
SELECT seal FROM secure_compaction_lost WHERE domain=?
```

## secure_dropped_tip_frames (1)

```sql
SELECT tip FROM secure_dropped_tip_frames d WHERE NOT EXISTS(SELECT 1 FROM secure_operations o WHERE o.hash=d.hash)
```

## secure_evicted_logs (1)

```sql
SELECT sequence,hash FROM secure_evicted_logs WHERE scope=?
```

## secure_finalized (3)

```sql
SELECT depth,seal,compacted FROM secure_finalized WHERE domain=? ORDER BY depth
SELECT depth,seal,compacted FROM secure_finalized WHERE domain=? ORDER BY depth
SELECT depth,seal,compacted FROM secure_finalized WHERE domain=? ORDER BY depth
```

## secure_handles (2)

```sql
SELECT input,hash FROM secure_handles WHERE handle=?
INSERT INTO secure_handles VALUES(?,?,?) ON CONFLICT(handle) DO NOTHING
```

## secure_identity_ledger (1)

```sql
SELECT author,signer,sequence FROM secure_identity_ledger WHERE scope=? ORDER BY author
```

## secure_identity_marks (2)

```sql
SELECT author,signer,sequence FROM secure_identity_marks WHERE scope=? ORDER BY author
SELECT author,signer,sequence FROM secure_identity_marks WHERE scope=? ORDER BY author
```

## secure_identity_own (1)

```sql
SELECT author,sequence FROM secure_identity_own WHERE scope=?
```

## secure_identity_partitions (1)

```sql
SELECT t.id,t.scope FROM secure_identity_partitions p JOIN secure_identity_tables t ON t.id=p.allocation WHERE p.scope=?
```

## secure_identity_summary (1)

```sql
SELECT author,sequence FROM secure_identity_summary WHERE scope=?
```

## secure_keys (1)

```sql
INSERT INTO secure_keys VALUES(?,?,?) ON CONFLICT(scope) DO NOTHING
```

## secure_logs (3)

```sql
SELECT author,sequence,hash,clock,pruned_prefix FROM secure_logs WHERE scope=? AND author>? AND pruned_prefix>=? ORDER BY author LIMIT ?
SELECT sequence,hash,clock,pruned_prefix FROM secure_logs WHERE scope=? AND author=?
INSERT INTO secure_logs VALUES(?,?,?,?,?,-1) ON CONFLICT(scope,author) DO UPDATE SET sequence=excluded.sequence,hash=excluded.hash,clock=excluded.clock
```

## secure_metadata (4)

```sql
UPDATE secure_metadata SET revision=revision+1 WHERE id=1
SELECT revision FROM secure_metadata WHERE id=1
SELECT revision FROM secure_metadata WHERE id=1
SELECT revision FROM secure_metadata WHERE id=1
```

## secure_migration_frames (1)

```sql
SELECT f.publication,f.proof,p.signed,p.conflict FROM secure_migration_frames f JOIN secure_migration_publications p ON p.publication=f.publication WHERE f.hash=?
```

## secure_migration_publications (1)

```sql
SELECT coalesce(sum(retained_bytes),0) FROM secure_migration_publications
```

## secure_node_authors (1)

```sql
SELECT number FROM secure_node_authors WHERE node=?
```

## secure_node_tips (1)

```sql
INSERT INTO secure_node_tips VALUES(?,?,?) ON CONFLICT(scope,node) DO UPDATE SET tip=excluded.tip
```

## secure_operations (7)

```sql
SELECT header FROM secure_operations WHERE scope=? AND author=? AND status='held' ORDER BY sequence DESC LIMIT 1
SELECT header,body FROM secure_operations WHERE hash=?
SELECT status,header FROM secure_operations WHERE hash=?
SELECT header,body FROM secure_operations WHERE hash=?
SELECT hash FROM secure_operations WHERE author=? AND scope=? AND sequence=?
INSERT INTO secure_operations(hash,author,scope,sequence,header,body,status,chain) VALUES(?,?,?,?,?,?, 'pending',?)
UPDATE secure_operations SET status='applied' WHERE hash=?
```

## secure_outbox (1)

```sql
INSERT INTO secure_outbox VALUES(?)
```

## secure_position_sources (1)

```sql
INSERT INTO secure_position_sources VALUES(?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(hash,kind) DO NOTHING
```

## secure_pulled (4)

```sql
SELECT signed FROM secure_pulled WHERE scope=?
SELECT scope FROM secure_pulled
SELECT scope,seal FROM secure_pulled
SELECT 1 FROM secure_pulled LIMIT 1
```

## secure_quarantine (1)

```sql
DELETE FROM secure_quarantine WHERE hash NOT IN(SELECT hash FROM secure_operations WHERE status='pending')
```

## secure_read_epoch (2)

```sql
UPDATE secure_read_epoch SET epoch=epoch+1 WHERE id=1 AND total_changes()>?
UPDATE secure_read_epoch SET epoch=epoch+1 WHERE id=1 AND total_changes()>?
```

## secure_rebootstrap (2)

```sql
SELECT signed,cursor,complete,sets,direct FROM secure_rebootstrap WHERE scope=?
SELECT 1 FROM secure_rebootstrap WHERE scope=?
```

## secure_retained (4)

```sql
SELECT records,bytes FROM secure_retained
UPDATE secure_retained SET records=records+1,bytes=bytes+?
SELECT handles,handle_digest FROM secure_retained
UPDATE secure_retained SET handles=?,handle_digest=?
```

## secure_row_positions (1)

```sql
INSERT OR IGNORE INTO secure_row_positions SELECT ?,?,position FROM secure_applied WHERE hash=?
```

## secure_row_records (4)

```sql
SELECT fields FROM secure_row_records WHERE scope=? AND row=?
SELECT fields FROM secure_row_records WHERE scope=? AND row=?
SELECT fields FROM secure_row_records WHERE scope=? AND row=?
INSERT INTO secure_row_records VALUES(?,?,?)
```

## secure_schema_switch (1)

```sql
SELECT 1 FROM secure_schema_switch WHERE id=1
```

## secure_scope_state (7)

```sql
SELECT id FROM secure_scope_state WHERE scope=?
SELECT id FROM secure_scope_state WHERE scope=?
SELECT id FROM secure_scope_state WHERE scope=?
SELECT id FROM secure_scope_state WHERE scope=?
SELECT rows,tombstones,digest,applied FROM secure_scope_state WHERE id=?
UPDATE secure_scope_state SET rows=?,tombstones=?,digest=?,applied=? WHERE id=?
SELECT id FROM secure_scope_state WHERE scope=?
```

## secure_seal_anchors (3)

```sql
SELECT 1 FROM secure_seal_anchors WHERE scope=?
SELECT 1 FROM secure_seal_anchors WHERE scope=?
SELECT 1 FROM secure_seal_anchors WHERE scope=?
```

## secure_seal_bounds (2)

```sql
SELECT depth,position FROM secure_seal_bounds WHERE scope=? ORDER BY depth
SELECT depth,position FROM secure_seal_bounds WHERE scope=? ORDER BY depth
```

## secure_unoffered (1)

```sql
INSERT INTO secure_unoffered VALUES(?1,?1,1,?2)
```

## secure_voids (1)

```sql
SELECT header FROM secure_voids WHERE scope=? AND author=? ORDER BY sequence DESC LIMIT 1
```

## transaction/control (10)

```sql
BEGIN IMMEDIATE
SAVEPOINT request
RELEASE request
COMMIT
SELECT total_changes()
BEGIN IMMEDIATE
COMMIT
SELECT total_changes()
PRAGMA freelist_count
PRAGMA freelist_count
```
