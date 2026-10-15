//! Reads against stored rows: paging, authorization boundaries, observers and
//! independence from the file's writer.
use super::*;
use inventory_app::{InventoryApp, Policy};
use rocky_core::*;
use std::sync::atomic::Ordering;
use tempfile::TempDir;

const NODE: NodeId = NodeId([3; 16]);
const CALLER: PrincipalId = PrincipalId([7; 16]);

fn scope(domain: u8) -> Scope {
    Scope {
        mesh: MeshId([11; 16]),
        app: inventory_app::app_id(),
        domain: DomainId([domain; 16]),
    }
}
/// Two domains in one file; domain 1 may read only notes with `prefix`.
async fn open(dir: &TempDir, prefix: Option<&str>) -> Runtime<InventoryApp> {
    let installations = [1, 2]
        .into_iter()
        .map(|domain| DomainInstallation {
            app: InventoryApp,
            policy: Policy {
                scope: scope(domain),
                caller: CALLER,
                reference: [9; 32],
                note_prefix: prefix.filter(|_| domain == 1).map(Into::into),
            },
            identity: Identity {
                scope: scope(domain),
                node: NODE,
                caller: CALLER,
            },
            config: RuntimeConfig {
                authors: vec![(NODE, CALLER)],
                delivery_targets: Vec::new(),
                full_nodes: Vec::new(),
            },
        })
        .collect();
    Runtime::open(dir.path().join("reads.db"), installations)
        .await
        .unwrap()
}
async fn note(replica: &Replica<InventoryApp>, text: &str) -> Envelope {
    replica
        .mutate(Mutation {
            entity: None,
            payload: text.as_bytes().to_vec(),
        })
        .unwrap()
        .committed()
        .await
        .unwrap()
}
fn query(sql: &str) -> BoundQuery {
    BoundQuery {
        sql: sql.into(),
        values: Vec::new(),
    }
}
fn texts(snapshot: &ReadSnapshot) -> Vec<String> {
    snapshot
        .rows
        .iter()
        .flat_map(|row| row.iter())
        .filter_map(|value| match value {
            SqlValue::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}
/// Every page of a query, following its cursors.
async fn pages(
    replica: &Replica<InventoryApp>,
    sql: &str,
    options: ReadOptions,
) -> Vec<ReadSnapshot> {
    let mut pages = vec![replica.read(query(sql), options.clone()).await.unwrap()];
    while let Some(cursor) = pages[pages.len() - 1].next.clone() {
        pages.push(
            replica
                .read(query(sql), options.clone().after(cursor))
                .await
                .unwrap(),
        );
    }
    pages
}

#[tokio::test]
async fn pages_cover_a_result_once_and_cursors_are_bound_to_their_query() {
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, None).await;
    let (a, b) = (
        runtime.domain(scope(1)).unwrap(),
        runtime.domain(scope(2)).unwrap(),
    );
    for index in 0..5 {
        note(&a, &format!("note-{index}")).await;
    }
    note(&b, "other domain").await;
    let ordered = "SELECT note FROM drafts ORDER BY sequence";
    let passes = pages(&a, ordered, ReadOptions::new(scope(1), 2)).await;
    assert_eq!(
        passes
            .iter()
            .map(|page| page.rows.len())
            .collect::<Vec<_>>(),
        [2, 2, 1]
    );
    assert_eq!(
        passes.iter().flat_map(texts).collect::<Vec<_>>(),
        (0..5)
            .map(|index| format!("note-{index}"))
            .collect::<Vec<_>>()
    );
    assert!(passes.iter().all(|page| page.revision.get() == 5));
    // A page that ends exactly at the result's end reports no cursor.
    let exact = pages(&a, ordered, ReadOptions::new(scope(1), 5)).await;
    assert_eq!((exact.len(), exact[0].rows.len()), (1, 5));
    // The byte limit ends a page too; each note weighs six bytes.
    let narrow = ReadOptions {
        max_bytes: 13,
        ..ReadOptions::new(scope(1), 1024)
    };
    let passes = pages(&a, ordered, narrow).await;
    assert_eq!(
        passes
            .iter()
            .map(|page| page.rows.len())
            .collect::<Vec<_>>(),
        [2, 2, 1]
    );
    // One row beyond the limit cannot be paged and is a quota error.
    let tiny = ReadOptions {
        max_bytes: 3,
        ..ReadOptions::new(scope(1), 8)
    };
    assert!(matches!(
        a.read(query(ordered), tiny).await,
        Err(Error::Quota {
            resource: "read result bytes",
            limit: 3
        })
    ));
    for max_bytes in [0, MAX_FRAME_BYTES + 1] {
        let options = ReadOptions {
            max_bytes,
            ..ReadOptions::new(scope(1), 8)
        };
        assert!(matches!(
            a.read(query(ordered), options.clone()).await,
            Err(Error::Quota { .. })
        ));
    }
    // A cursor continues only the query, values and scope that issued it.
    let cursor = a
        .read(query(ordered), ReadOptions::new(scope(1), 2))
        .await
        .unwrap()
        .next
        .unwrap();
    let continued = ReadOptions::new(scope(1), 2).after(cursor.clone());
    assert!(matches!(
        a.read(query("SELECT note FROM drafts"), continued.clone())
            .await,
        Err(Error::Rejected(_))
    ));
    let bound = BoundQuery {
        sql: format!("{ordered} LIMIT ?"),
        values: vec![SqlValue::Integer(4)],
    };
    let other_value = BoundQuery {
        values: vec![SqlValue::Integer(5)],
        ..bound.clone()
    };
    let first = a.read(bound, ReadOptions::new(scope(1), 2)).await.unwrap();
    assert!(matches!(
        a.read(
            other_value,
            ReadOptions::new(scope(1), 2).after(first.next.unwrap())
        )
        .await,
        Err(Error::Rejected(_))
    ));
    assert!(matches!(
        b.read(
            query(ordered),
            ReadOptions::new(scope(2), 2).after(cursor.clone())
        )
        .await,
        Err(Error::Rejected(_))
    ));
    assert!(matches!(
        b.read(query(ordered), continued).await,
        Err(Error::Protocol(ProtocolError::ForeignScope))
    ));
    // The text form round-trips and tampering with it does not widen a read.
    assert_eq!(ReadCursor::decode(&cursor.encode()).unwrap(), cursor);
    for text in ["", "2", "2:zz", &format!("x:{}", "0".repeat(64))] {
        assert!(ReadCursor::decode(text).is_err(), "{text}");
    }
    let forged = ReadCursor::decode(&format!("0:{}", "0".repeat(64))).unwrap();
    assert!(matches!(
        a.read(query(ordered), ReadOptions::new(scope(1), 2).after(forged))
            .await,
        Err(Error::Rejected(_))
    ));
    runtime.close().await.unwrap();
}

#[tokio::test]
async fn ordered_reads_continue_by_key_and_stay_inside_the_view() {
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, None).await;
    let a = runtime.domain(scope(1)).unwrap();
    for text in ["public b", "private c", "public d", "public f", "public h"] {
        note(&a, text).await;
    }
    let two = ReadOptions::new(scope(1), 2);
    let by_note = "SELECT note FROM drafts ORDER BY note";
    // A `LIMIT` leaves the same order but only an offset can continue it.
    let by_offset = "SELECT note FROM drafts ORDER BY note LIMIT 100";
    let first = a.read(query(by_note), two.clone()).await.unwrap();
    let offset = a.read(query(by_offset), two.clone()).await.unwrap();
    assert_eq!(texts(&first), ["private c", "public b"]);
    assert_eq!(first.rows, offset.rows);
    let (keyed, counted) = (first.next.unwrap(), offset.next.unwrap());
    assert_eq!(
        (keyed.kind(), counted.kind()),
        (CursorKind::Keyset, CursorKind::Offset)
    );
    assert_eq!(ReadCursor::decode(&keyed.encode()).unwrap(), keyed);
    // A row written before the position: the key cursor continues after the
    // last row read, the offset cursor repeats it.
    note(&a, "a new first row").await;
    let next = |sql: &'static str, cursor: &ReadCursor| {
        a.read(query(sql), two.clone().after(cursor.clone()))
    };
    assert_eq!(
        texts(&next(by_note, &keyed).await.unwrap()),
        ["public d", "public f"]
    );
    assert_eq!(
        texts(&next(by_offset, &counted).await.unwrap()),
        ["public b", "public d"]
    );
    // Each kind continues only the query that issued it.
    assert!(matches!(
        next(by_offset, &keyed).await,
        Err(Error::Rejected(_))
    ));
    assert!(matches!(
        next("SELECT note FROM drafts", &keyed).await,
        Err(Error::Rejected(_))
    ));
    // Descending over the key continues by key; over a column that is not
    // declared never NULL, by offset. Both cover the result once.
    for (sql, kind, expected) in [
        (
            "SELECT note FROM drafts ORDER BY sequence DESC",
            CursorKind::Keyset,
            [
                "a new first row",
                "public h",
                "public f",
                "public d",
                "private c",
                "public b",
            ],
        ),
        (
            "SELECT note FROM drafts ORDER BY note DESC",
            CursorKind::Offset,
            [
                "public h",
                "public f",
                "public d",
                "public b",
                "private c",
                "a new first row",
            ],
        ),
    ] {
        let passes = pages(&a, sql, two.clone()).await;
        assert_eq!(passes.iter().flat_map(texts).collect::<Vec<_>>(), expected);
        assert!(
            passes
                .iter()
                .filter_map(|page| page.next.as_ref())
                .all(|cursor| cursor.kind() == kind),
            "{sql}"
        );
    }
    // A continued page seeks in the key's index instead of skipping rows.
    let by_key = "SELECT note FROM drafts ORDER BY node, sequence";
    let cursor = a
        .read(query(by_key), two.clone())
        .await
        .unwrap()
        .next
        .unwrap();
    let plan = a
        .explain_read(query(by_key), two.clone().after(cursor))
        .await
        .unwrap();
    assert!(
        plan.iter().any(|step| step.starts_with("SEARCH"))
            && plan.iter().all(|step| !step.contains("TEMP B-TREE")),
        "{plan:?}"
    );
    runtime.close().await.unwrap();
    // A key cursor is client data. Whatever values it holds, the seek runs
    // inside the authorized view.
    let runtime = open(&dir, Some("public")).await;
    let a = runtime.domain(scope(1)).unwrap();
    let cursor = a
        .read(query(by_note), two.clone())
        .await
        .unwrap()
        .next
        .unwrap();
    let encoded = cursor.encode();
    let (binding, _) = encoded.rsplit_once(':').unwrap();
    // An empty text, an empty blob and zero sort before every stored row.
    let forged = ReadCursor::decode(&format!("{binding}:t,b,i0")).unwrap();
    let all = ReadOptions::new(scope(1), 100).after(forged);
    let seen = texts(&a.read(query(by_note), all).await.unwrap());
    assert_eq!(seen, ["public b", "public d", "public f", "public h"]);
    for key in ["", "n", "i", "t0", "x00", "i1,"] {
        assert!(
            ReadCursor::decode(&format!("{binding}:{key}")).is_err(),
            "{key}"
        );
    }
    // Values of another shape are not this query's cursor.
    let short = ReadCursor::decode(&format!("{binding}:i1")).unwrap();
    assert!(matches!(
        a.read(query(by_note), two.clone().after(short)).await,
        Err(Error::Rejected(_))
    ));
    runtime.close().await.unwrap();
}

#[tokio::test]
async fn denied_rows_protocol_tables_and_other_domains_are_unreachable_in_every_read_shape() {
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, None).await;
    let (a, b) = (
        runtime.domain(scope(1)).unwrap(),
        runtime.domain(scope(2)).unwrap(),
    );
    for text in [
        "public one",
        "private one",
        "public two",
        "private two",
        "Public upper",
    ] {
        note(&a, text).await;
    }
    note(&b, "public elsewhere").await;
    runtime.close().await.unwrap();
    let runtime = open(&dir, Some("public")).await;
    let (a, b) = (
        runtime.domain(scope(1)).unwrap(),
        runtime.domain(scope(2)).unwrap(),
    );
    let one = ReadOptions::new(scope(1), 1);
    for sql in [
        "SELECT * FROM drafts",
        "SELECT note FROM drafts ORDER BY note DESC",
        "SELECT note FROM \"drafts\" ORDER BY sequence",
        "SELECT note FROM drafts AS d ORDER BY sequence",
        "SELECT note FROM drafts d WHERE note<>'x'",
        "SELECT MIN(note), MAX(note) FROM drafts",
        "SELECT note, COUNT(*) FROM drafts GROUP BY note",
        "SELECT note FROM drafts WHERE note LIKE '%private%' OR 1=1",
        "SELECT note FROM drafts LIMIT 100",
    ] {
        // One row per page, so every row also crosses a paging cursor.
        let seen: Vec<_> = pages(&a, sql, one.clone())
            .await
            .iter()
            .flat_map(texts)
            .collect();
        assert!(!seen.is_empty(), "{sql}");
        assert!(
            seen.iter()
                .all(|text| text.starts_with("public") && !text.contains("elsewhere")),
            "{sql}: {seen:?}"
        );
    }
    assert_eq!(
        a.read(query("SELECT COUNT(*) FROM drafts"), one.clone())
            .await
            .unwrap()
            .rows,
        [[SqlValue::Integer(2)]]
    );
    let private = BoundQuery {
        sql: "SELECT note FROM drafts WHERE note=?".into(),
        values: vec![SqlValue::Text("private one".into())],
    };
    assert!(a.read(private, one.clone()).await.unwrap().rows.is_empty());
    // The unfiltered domain still reads its own rows, and only those.
    assert_eq!(
        pages(&b, "SELECT note FROM drafts", ReadOptions::new(scope(2), 1))
            .await
            .iter()
            .flat_map(texts)
            .collect::<Vec<_>>(),
        ["public elsewhere"]
    );
    // Columns outside the view, stored names, protocol tables and the other
    // domain's namespace are not addressable.
    let other = crate::runtime::namespace(scope(2));
    let own = crate::runtime::namespace(scope(1));
    for sql in [
        "SELECT lamport FROM drafts".to_string(),
        "SELECT stamp_node FROM drafts".to_string(),
        format!("SELECT * FROM {other}_drafts"),
        format!("SELECT * FROM \"{other}_drafts\""),
        format!("SELECT * FROM {own}_drafts"),
        format!("SELECT * FROM {own}_rocky_operations"),
        format!("SELECT * FROM \"{other}_rocky_operations\""),
        "SELECT * FROM rocky_operations".to_string(),
        "SELECT * FROM rocky_mutation_handles".to_string(),
        "SELECT * FROM rocky_runtime".to_string(),
        "SELECT * FROM sqlite_schema".to_string(),
        "SELECT * FROM sqlite_master".to_string(),
        "SELECT * FROM pragma_table_info('drafts')".to_string(),
        "SELECT note FROM drafts WHERE sequence IN (SELECT sequence FROM rocky_operations)"
            .to_string(),
        "SELECT (SELECT COUNT(*) FROM rocky_operations) FROM drafts".to_string(),
        "SELECT * FROM drafts UNION ALL SELECT node,sequence,bytes FROM rocky_operations"
            .to_string(),
    ] {
        let result = a.read(query(&sql), one.clone()).await;
        assert!(result.is_err(), "{sql}: {result:?}");
    }
    runtime.close().await.unwrap();
}

#[tokio::test]
async fn point_reads_use_the_stored_tables_indexes_through_a_filtered_view() {
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, Some("public")).await;
    let a = runtime.domain(scope(1)).unwrap();
    let created = note(&a, "public point").await;
    let point = BoundQuery {
        sql: "SELECT note FROM drafts WHERE node=? AND sequence=?".into(),
        values: vec![
            SqlValue::Blob(created.id.node.0.to_vec()),
            SqlValue::Integer(created.id.sequence.get() as i64),
        ],
    };
    let options = ReadOptions::new(scope(1), 1);
    assert_eq!(
        texts(&a.read(point.clone(), options.clone()).await.unwrap()),
        ["public point"]
    );
    let plan = a
        .explain_read(point.clone(), options.clone())
        .await
        .unwrap();
    assert!(
        plan.iter()
            .any(|step| step.starts_with("SEARCH") && step.contains("USING INDEX")),
        "{plan:?}"
    );
    assert!(
        plan.iter().all(|step| !step.starts_with("SCAN")),
        "{plan:?}"
    );
    // The plan of a continued page is the same search behind an offset.
    let scan = a
        .explain_read(query("SELECT note FROM drafts"), options.clone())
        .await
        .unwrap();
    assert!(scan.iter().any(|step| step.starts_with("SCAN")), "{scan:?}");
    assert!(matches!(
        a.explain_read(query("SELECT * FROM rocky_operations"), options.clone())
            .await,
        Err(Error::Rejected(_))
    ));
    runtime.close().await.unwrap();
}

#[tokio::test]
async fn observers_wake_for_their_domain_only_and_share_one_execution() {
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, None).await;
    let (a, b) = (
        runtime.domain(scope(1)).unwrap(),
        runtime.domain(scope(2)).unwrap(),
    );
    let options = ReadOptions::new(scope(1), 16);
    let drafts = "SELECT note FROM drafts ORDER BY sequence";
    let mut watchers = Vec::new();
    for _ in 0..8 {
        watchers.push(a.observe_read(query(drafts), options.clone()).unwrap());
    }
    let mut idle = b
        .observe_read(query(drafts), ReadOptions::new(scope(2), 16))
        .unwrap();
    assert!(idle.next().await.unwrap().rows.is_empty());
    let reads = a.inner.reads().unwrap().clone();
    let executed = || reads.executed.load(Ordering::Relaxed);
    for watcher in &mut watchers {
        assert!(watcher.next().await.unwrap().rows.is_empty());
    }
    // Eight observers of one query, one execution.
    assert_eq!(executed(), 1);
    let drafts_before = reads.counters(drafts);
    for text in ["one", "two", "three"] {
        note(&a, text).await;
    }
    // Draft writes advance the drafts view; the other domain sees nothing.
    assert_eq!(
        reads.counters(drafts),
        (drafts_before.0 + 3, drafts_before.1)
    );
    // Three commits coalesce into one snapshot, executed once for all eight.
    for watcher in &mut watchers {
        let snapshot = watcher.next().await.unwrap();
        assert_eq!(texts(&snapshot), ["one", "two", "three"]);
        assert_eq!(snapshot.revision.get(), 3);
    }
    assert_eq!(executed(), 2);
    // Nothing changed since: a further `next` waits instead of reading again.
    let mut waiting = Box::pin(watchers[0].next());
    assert!(futures_poll(&mut waiting).is_none());
    assert!(futures_poll(&mut Box::pin(idle.next())).is_none());
    note(&a, "four").await;
    assert_eq!(texts(&waiting.await.unwrap()).len(), 4);
    assert_eq!(executed(), 3);
    note(&b, "other domain").await;
    assert_eq!(texts(&idle.next().await.unwrap()), ["other domain"]);
    drop(watchers);
    runtime.close().await.unwrap();
    assert!(matches!(idle.next().await, Err(Error::Closed)));
    assert!(matches!(
        a.read(query(drafts), options.clone()).await,
        Err(Error::Closed)
    ));
}
/// Polls once without waiting.
fn futures_poll<T>(future: &mut std::pin::Pin<Box<impl Future<Output = T>>>) -> Option<T> {
    let waker = std::task::Waker::noop();
    match future
        .as_mut()
        .poll(&mut std::task::Context::from_waker(waker))
    {
        std::task::Poll::Ready(value) => Some(value),
        std::task::Poll::Pending => None,
    }
}

/// A read does not pass through the writer: it answers while the writer is
/// held inside a group, and reports the committed revision it was read at.
#[tokio::test]
async fn reads_answer_while_the_writer_is_inside_a_group() {
    let dir = TempDir::new().unwrap();
    let runtime = open(&dir, None).await;
    let a = runtime.domain(scope(1)).unwrap();
    note(&a, "committed").await;
    let (entered, paused) = oneshot::channel();
    let (resume, resumed) = oneshot::channel();
    crate::tests::fault(&a, worker::Fault::Pause(entered, resumed)).await;
    let receipt = a
        .mutate(Mutation {
            entity: None,
            payload: b"in flight".to_vec(),
        })
        .unwrap();
    paused.await.unwrap();
    let options = ReadOptions::new(scope(1), 8);
    let mut observer = a
        .observe_read(query("SELECT note FROM drafts"), options.clone())
        .unwrap();
    for snapshot in [
        a.read(query("SELECT note FROM drafts"), options.clone())
            .await
            .unwrap(),
        observer.next().await.unwrap(),
    ] {
        assert_eq!(texts(&snapshot), ["committed"]);
        assert_eq!(snapshot.revision.get(), 1);
    }
    resume.send(()).unwrap();
    receipt.committed().await.unwrap();
    let after = observer.next().await.unwrap();
    assert_eq!((texts(&after).len(), after.revision.get()), (2, 2));
    runtime.close().await.unwrap();
}
