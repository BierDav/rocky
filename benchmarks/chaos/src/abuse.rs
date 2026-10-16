//! Misbehaving local application code and lifecycle abuse.
use crate::scenarios::{
    Failure, ITEMS, LISTS, NOTES, OPEN, Slot, author_all, finding, heal, integrity, populate,
    report_divergence, root, short, status_data, writer_all,
};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// Statements a buggy or hostile app might run through the session's SQL.
const HOSTILE: &[&str] = &[
    "ATTACH DATABASE '/tmp/rocky-chaos-evil.sqlite' AS evil",
    "PRAGMA writable_schema = 1",
    "PRAGMA journal_mode = DELETE",
    "PRAGMA foreign_keys = OFF",
    "DROP TABLE notes",
    "ALTER TABLE notes ADD COLUMN extra TEXT",
    "CREATE TABLE side(x)",
    "CREATE TRIGGER t AFTER INSERT ON notes BEGIN DELETE FROM notes; END",
    "CREATE INDEX evil ON notes(body)",
    "DELETE FROM secure_operations",
    "UPDATE secure_outbox SET rowid = rowid",
    "DELETE FROM sqlite_master",
    "INSERT INTO secure_roles VALUES(1, x'00')",
    "VACUUM",
    "REINDEX",
    "ANALYZE",
    "SELECT load_extension('/tmp/x')",
    "UPDATE notes SET id = id + 1",
    "UPDATE notes SET key = 'collide' ",
    "INSERT INTO notes(id,key,body) VALUES(1,'forged-id','x')",
    "INSERT INTO notes(id,key,body) VALUES(12884901889,'stolen-id','x')",
    "INSERT INTO notes(key,body) VALUES('no-id','x')",
    "INSERT INTO lists(id,title) VALUES(999,'unprepared list')",
    "INSERT INTO items(id,list,title,done) VALUES(5,424242,'orphan',0)",
    "UPDATE items SET list = list + 1",
    "DELETE FROM lists",
    "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM r) SELECT count(*) FROM r",
    "SELECT randomblob(900000000)",
    "SAVEPOINT s1",
    "RELEASE s2",
    "COMMIT",
    "ROLLBACK",
    "BEGIN; DELETE FROM notes; COMMIT",
    "BEGIN EXCLUSIVE",
    "BEGIN",
    "BEGIN",
];

/// Each hostile statement, then health checks: the node writes, syncs and
/// converges with a peer, and its file passes integrity_check.
pub async fn app_sql_abuse() -> Result<(), Failure> {
    let root = root("sql-abuse");
    let mut a = Slot::new("a", &root);
    a.start(json!({"mode":"create"})).await?;
    author_all(a.n(), a.n()).await;
    let lists = populate(a.n(), "a", 30, 50, 2, 5).await;
    let mut b = Slot::new("b", &root);
    b.start(json!({"mode":"join","invitation": a.n().invite(writer_all()).await}))
        .await?;
    author_all(b.n(), a.n()).await;
    let bid = b.n().node_id().await;
    for list in &lists {
        let _ = a.n().share(list, std::slice::from_ref(&bid)).await;
    }
    let _ = b.n().sync().await;
    for (i, sql) in HOSTILE.iter().enumerate() {
        let started = Instant::now();
        let r = b
            .n()
            .call_for(
                json!({"type":"sql","sql":sql,"values":[]}),
                Duration::from_secs(30),
            )
            .await;
        let took = started.elapsed();
        let accepted = r.is_ok();
        println!(
            "  [{i}] {sql:.60} -> {} in {:.0?}",
            if accepted {
                "ACCEPTED".into()
            } else {
                short(&r)
            },
            took
        );
        if took > Duration::from_secs(10) {
            finding(format!(
                "application SQL `{sql}` blocks the session for {took:?}"
            ));
        }
        // Leave no open transaction behind between probes.
        let _ = b.n().sql("ROLLBACK", json!([])).await;
        let probe = b.n().add_note(&format!("probe{i}"), "p").await;
        if let Err(e) = probe {
            finding(format!(
                "after `{sql}` the session can no longer write: {e}"
            ));
            let _ = b.n().sql("ROLLBACK", json!([])).await;
        }
    }
    let ok = heal(&[a.n(), b.n()], &[a.n(), b.n()], 5).await;
    if !ok {
        finding("hostile application SQL left the replicas unable to converge");
        report_divergence(&[a.n(), b.n()]).await;
    }
    b.close().await?;
    a.close().await?;
    println!("  b integrity: {}", integrity(&root.join("b")));
    let mut b = Slot::new("b", &root);
    match b.resume().await {
        Ok(_) => {
            b.close().await?;
        }
        Err(e) => finding(format!("node does not reopen after hostile SQL: {e}")),
    }
    Ok(())
}

/// SIGKILL with an open transaction, during COMMIT and with concurrent
/// transactions from one session; nothing applies in part.
pub async fn kill_in_transaction() -> Result<(), Failure> {
    let root = root("kill-tx");
    let mut a = Slot::new("a", &root);
    a.start(json!({"mode":"create"})).await?;
    author_all(a.n(), a.n()).await;
    let lists = populate(a.n(), "a", 10, 10, 1, 3).await;
    let mut b = Slot::new("b", &root);
    b.start(json!({"mode":"join","invitation": a.n().invite(writer_all()).await}))
        .await?;
    author_all(b.n(), a.n()).await;
    let _ = a.n().share(&lists[0], &[b.n().node_id().await]).await;
    let _ = b.n().sync().await;

    // 1. Concurrent BEGINs on one session.
    let n = b.n().clone();
    let mut tasks = Vec::new();
    for t in 0..8 {
        let n = n.clone();
        tasks.push(tokio::spawn(async move {
            n.add_note(&format!("concurrent{t}"), "c").await
        }));
    }
    let mut outcomes = Vec::new();
    for t in tasks {
        outcomes.push(
            t.await?
                .map(|_| "ok".to_owned())
                .unwrap_or_else(|e| short(&e)),
        );
    }
    println!("  8 concurrent transactions on one session: {outcomes:?}");
    let have = b
        .n()
        .dump("SELECT key FROM notes WHERE key LIKE 'concurrent%'")
        .await
        .unwrap_or_default();
    println!("  rows present: {}", have.len());
    if have.len() != outcomes.iter().filter(|o| *o == "ok").count() {
        finding(format!(
            "interleaved transactions on one session: {} reported ok, {} rows exist",
            outcomes.iter().filter(|o| *o == "ok").count(),
            have.len()
        ));
    }
    let _ = b.n().sql("ROLLBACK", json!([])).await;

    // 2. Kill with an open, uncommitted multi-row transaction.
    b.n().sql("BEGIN", json!([])).await.ok();
    for i in 0..5 {
        let id = b.n().call(json!({"type":"next_id","table":"notes"})).await;
        if let Ok(id) = id {
            let _ = b.n().sql("INSERT INTO notes(id,key,body) VALUES(?,?,?)", json!([{"type":"integer","value":id["id"]},{"type":"text","value":format!("uncommitted{i}")},{"type":"text","value":"u"}])).await;
        }
    }
    b.kill().await;
    b.resume().await?;
    let leaked = b
        .n()
        .dump("SELECT key FROM notes WHERE key LIKE 'uncommitted%'")
        .await;
    println!("  uncommitted rows after kill: {}", short(&leaked));
    if leaked.as_ref().is_ok_and(|r| !r.is_empty()) {
        finding("rows of an uncommitted transaction survive SIGKILL");
    }
    // 3. Kill racing COMMIT, many times; every committed row must be
    // durable once COMMIT replied, and the replica must still sync.
    let mut acknowledged = Vec::new();
    for round in 0..12 {
        let n = b.n().clone();
        let key = format!("racing{round}");
        let k = key.clone();
        let task = tokio::spawn(async move { n.add_note(&k, "r").await });
        tokio::time::sleep(Duration::from_millis(round as u64 % 4)).await;
        b.kill().await;
        if let Ok(Ok(Ok(_))) = tokio::time::timeout(Duration::from_secs(1), task).await {
            acknowledged.push(key);
        }
        if let Err(e) = b.resume().await {
            finding(format!("reopen after SIGKILL racing COMMIT fails: {e}"));
            return Ok(());
        }
    }
    let present = b
        .n()
        .dump("SELECT key FROM notes WHERE key LIKE 'racing%'")
        .await
        .unwrap_or_default();
    let lost: Vec<_> = acknowledged
        .iter()
        .filter(|k| !present.iter().any(|r| r.contains(&format!("\"{k}\""))))
        .collect();
    println!(
        "  acknowledged before kill: {acknowledged:?}; present: {}",
        present.len()
    );
    if !lost.is_empty() {
        finding(format!(
            "COMMIT acknowledged, row lost after SIGKILL (buffered durability): {lost:?}"
        ));
    }
    let ok = heal(&[a.n(), b.n()], &[a.n(), b.n()], 5).await;
    if !ok {
        finding("after kills racing COMMIT, replicas never converge");
        report_divergence(&[a.n(), b.n()]).await;
    }
    b.close().await?;
    a.close().await?;
    println!("  b integrity: {}", integrity(&root.join("b")));
    Ok(())
}

/// A partial replica evicts a list it wrote to, SIGKILLed mid-eviction,
/// then resubscribes and writes again.
pub async fn evict_kill() -> Result<(), Failure> {
    let root = root("evict-kill");
    let mut a = Slot::new("a", &root);
    a.start(json!({"mode":"create"})).await?;
    author_all(a.n(), a.n()).await;
    let lists = populate(a.n(), "a", 5, 10, 2, 300).await;
    let mut p = Slot::new("p", &root);
    p.start(json!({"mode":"join","invitation": a.n().invite(writer_all()).await}))
        .await?;
    println!("  role: {}", short(&p.n().role(true).await));
    author_all(p.n(), a.n()).await;
    let pid = p.n().node_id().await;
    let _ = a.n().share(&lists[0], std::slice::from_ref(&pid)).await;
    crate::cluster::mesh(&[a.n(), p.n()]).await;
    let _ = p.n().sync().await;
    let _ = p.n().sync().await;
    for i in 0..50 {
        let _ = p.n().add_item(&lists[0], &format!("p-item{i}")).await;
    }
    let list = json!([{"type":"integer","value":lists[0]}]);
    // Unsynced own writes: eviction must refuse rather than lose them.
    let early = p
        .n()
        .call(json!({"type":"partition_evict","group":"list","values":list}))
        .await;
    println!("  evict with unsynced own writes: {}", short(&early));
    if early.is_ok() {
        let on_a = a
            .n()
            .dump("SELECT title FROM items WHERE title LIKE 'p-item%'")
            .await
            .unwrap_or_default();
        if on_a.len() < 50 {
            finding(format!(
                "partial evicted a list holding {} unsynced own items; issuer has {}",
                50,
                on_a.len()
            ));
        }
    }
    let _ = p.n().sync().await;
    // Evict, killed at a random moment.
    let n = p.n().clone();
    let l2 = list.clone();
    let task = tokio::spawn(async move {
        n.call(json!({"type":"partition_evict","group":"list","values":l2}))
            .await
    });
    tokio::time::sleep(Duration::from_millis(3)).await;
    p.kill().await;
    println!("  evict raced by kill: {}", short(&task.await.ok()));
    p.resume().await?;
    let status = p
        .n()
        .call(json!({"type":"partition_status","group":"list","values":list}))
        .await;
    println!(
        "  after reopen: {}",
        short(&status.map(|s| s["tables"].clone()))
    );
    let write = p.n().add_item(&lists[0], "after-evict").await;
    println!("  write into evicting/evicted list: {}", short(&write));
    let resub = p
        .n()
        .call(json!({"type":"partition_subscribe","group":"list","values":list}))
        .await;
    println!("  resubscribe: {}", short(&resub));
    for _ in 0..3 {
        let _ = p.n().sync().await;
    }
    let write = p.n().add_item(&lists[0], "after-resubscribe").await;
    println!("  write after resubscribe+sync: {}", short(&write));
    let _ = p.n().sync().await;
    let pa = p
        .n()
        .dump(&format!(
            "SELECT title FROM items WHERE list = {} ORDER BY title",
            lists[0]
        ))
        .await
        .unwrap_or_default();
    let aa = a
        .n()
        .dump(&format!(
            "SELECT title FROM items WHERE list = {} ORDER BY title",
            lists[0]
        ))
        .await
        .unwrap_or_default();
    println!("  list items: partial {} issuer {}", pa.len(), aa.len());
    if pa != aa {
        finding(
            "after an interrupted eviction and resubscribe the partial's list differs from the issuer's",
        );
    }
    let unshared = p
        .n()
        .dump(&format!(
            "SELECT title FROM items WHERE list = {}",
            lists[1]
        ))
        .await;
    println!(
        "  partial rows of unshared list: {}",
        short(&unshared.map(|r| r.len()))
    );
    p.close().await?;
    a.close().await?;
    Ok(())
}

/// The issuer prunes with window 0 after a full node acknowledged; a node
/// that was offline (with unsent writes) then reconnects, and a third joins.
pub async fn prune_then_rejoin() -> Result<(), Failure> {
    let root = root("prune");
    let mut a = Slot::new("a", &root);
    a.start(json!({"mode":"create"})).await?;
    author_all(a.n(), a.n()).await;
    let lists = populate(a.n(), "a", 200, 100, 2, 50).await;
    let mut f = Slot::new("f", &root);
    let mut c = Slot::new("c", &root);
    f.start(json!({"mode":"join","invitation": a.n().invite(writer_all()).await}))
        .await?;
    c.start(json!({"mode":"join","invitation": a.n().invite(writer_all()).await}))
        .await?;
    author_all(f.n(), a.n()).await;
    author_all(c.n(), a.n()).await;
    for node in [f.n(), c.n()] {
        let id = node.node_id().await;
        for list in &lists {
            let _ = a.n().share(list, std::slice::from_ref(&id)).await;
        }
    }
    crate::cluster::mesh(&[a.n(), f.n(), c.n()]).await;
    for n in [f.n(), c.n(), f.n(), c.n()] {
        let _ = n.sync().await;
    }
    // C goes offline with unsent edits, also on rows A will rewrite.
    c.n()
        .call(json!({"type":"networking_control","enabled":false}))
        .await
        .ok();
    for i in 0..30 {
        let _ = c.n().add_note(&format!("c-offline{i}"), "x").await;
        let _ = c
            .n()
            .sql(
                "UPDATE notes SET body = 'c-edit' WHERE key = ?",
                json!([{"type":"text","value":format!("a{i}")}]),
            )
            .await;
        let _ = c.n().add_item(&lists[0], &format!("c-item{i}")).await;
    }
    for i in 0..100 {
        let _ = a
            .n()
            .sql(
                "UPDATE notes SET body = 'a-edit' WHERE key = ?",
                json!([{"type":"text","value":format!("a{i}")}]),
            )
            .await;
        let _ = a.n().add_note(&format!("a-late{i}"), "x").await;
    }
    let _ = f.n().sync().await;
    let policy = json!({"required": [], "behind": [], "copies": 1, "window": 0});
    for table in ["notes", "lists", "items"] {
        let r = a
            .n()
            .call(json!({"type":"retention","collection":table,"policy":policy,"maintain":true}))
            .await;
        println!(
            "  prune {table}: {}",
            short(&r.map(|s| (
                s["covered"].clone(),
                s["pruned_prefix"].as_array().map(|h| h.len()),
                s["error"].clone()
            )))
        );
    }
    // C returns; then is killed in the middle of its rebootstrap.
    c.n()
        .call(json!({"type":"networking_control","enabled":true}))
        .await
        .ok();
    let cn = c.n().clone();
    let sync = tokio::spawn(async move { cn.sync().await });
    tokio::time::sleep(Duration::from_millis(40)).await;
    c.kill().await;
    let _ = sync.await;
    c.resume().await?;
    let mut d = Slot::new("d", &root);
    crate::scenarios::join_full(a.n(), &mut d, &lists).await?;
    let ok = heal(&[a.n(), f.n(), c.n(), d.n()], &[a.n(), f.n(), c.n()], 8).await;
    if !ok {
        finding(
            "after pruning, an offline writer that reconnects (killed mid-rebootstrap) does not converge",
        );
        report_divergence(&[a.n(), f.n(), c.n()]).await;
    }
    let notes = a
        .n()
        .dump("SELECT key FROM notes WHERE key LIKE 'c-offline%'")
        .await
        .unwrap_or_default();
    println!("  c's offline notes on issuer: {}/30", notes.len());
    if notes.len() != 30 {
        finding(format!(
            "offline writes made before the issuer pruned were lost: {}/30 arrived",
            notes.len()
        ));
    }
    let edits = a
        .n()
        .dump("SELECT body FROM notes WHERE key IN ('a0','a1','a2')")
        .await
        .unwrap_or_default();
    println!("  conflicting edit winners on issuer: {edits:?}");
    let d_notes = d.n().dump(NOTES).await.map(|r| r.len());
    let a_notes = a.n().dump(NOTES).await.map(|r| r.len());
    println!("  d notes {d_notes:?} a notes {a_notes:?}");
    for slot in [&mut a, &mut f, &mut c, &mut d] {
        slot.close().await?;
    }
    Ok(())
}

/// Graceful close while sync, reads, writes and subscriptions run.
pub async fn close_during_sync() -> Result<(), Failure> {
    let root = root("close-sync");
    let mut a = Slot::new("a", &root);
    a.start(json!({"mode":"create"})).await?;
    author_all(a.n(), a.n()).await;
    let _ = populate(a.n(), "a", 800, 1000, 1, 10).await;
    for round in 0..5 {
        let name = format!("b{round}");
        let mut b = Slot::new(&name, &root);
        b.start(json!({"mode":"join","invitation": a.n().invite(writer_all()).await}))
            .await?;
        author_all(b.n(), a.n()).await;
        let n = b.n().clone();
        let busy = tokio::spawn(async move {
            let s = n.call(json!({"type":"subscribe","query":{"sql":"SELECT count(*) FROM notes","values":[],"max_rows":1}})).await;
            let _ = n.add_note("busy", "b").await;
            let _ = n.sync().await;
            s
        });
        tokio::time::sleep(Duration::from_millis(20 * round as u64)).await;
        let started = Instant::now();
        let closed = tokio::time::timeout(Duration::from_secs(40), b.close()).await;
        println!(
            "  {name}: close during bootstrap/sync -> {} in {:.1?}",
            short(&closed),
            started.elapsed()
        );
        if closed.is_err() {
            finding(format!("close hangs during an active bootstrap ({name})"));
        }
        let _ = busy.await;
        let mut again = Slot::new(&name, &root);
        match again.resume().await {
            Ok(n) => {
                let data = status_data(&n).await;
                let _ = n.sync().await;
                let notes = n.dump(NOTES).await.map(|r| r.len());
                println!(
                    "  {name} reopened: pending={} notes={notes:?}",
                    data["bootstrap_pending"]
                );
                again.close().await?;
            }
            Err(e) => finding(format!(
                "{name} does not reopen after close during bootstrap: {e}"
            )),
        }
    }
    a.close().await?;
    let _ = (LISTS, ITEMS, OPEN, Value::Null);
    Ok(())
}
