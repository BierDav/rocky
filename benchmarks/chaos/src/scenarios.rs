//! Scripted reproductions. Each scenario prints what it observed; a finding is
//! printed with `FINDING:` and never aborts the remaining scenarios.
use crate::cluster::{Node, mesh};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU16, Ordering},
    },
    time::{Duration, Instant},
};

pub type Failure = Box<dyn std::error::Error + Send + Sync>;
static PORT: AtomicU16 = AtomicU16::new(0);

pub fn port() -> u16 {
    if PORT.load(Ordering::SeqCst) == 0 {
        PORT.store(
            40000 + (std::process::id() % 400) as u16 * 50,
            Ordering::SeqCst,
        );
    }
    PORT.fetch_add(1, Ordering::SeqCst)
}
pub fn root(name: &str) -> PathBuf {
    let base = std::env::var("CHAOS_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("rocky-chaos"));
    let dir = base.join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
pub const OPEN: Duration = Duration::from_secs(90);
pub const TABLES: [&str; 3] = ["lists", "items", "notes"];
pub fn writer_all() -> Value {
    json!({"kind":"writer","scopes":TABLES})
}
pub fn finding(text: impl std::fmt::Display) {
    println!("FINDING: {text}");
}
pub fn short(value: &impl std::fmt::Debug) -> String {
    let text = format!("{value:?}");
    text.chars().take(400).collect()
}

/// A node with its name, port and directory root kept for restarts.
pub struct Slot {
    pub name: String,
    pub port: u16,
    pub root: PathBuf,
    pub node: Option<Arc<Node>>,
}
impl Slot {
    pub fn new(name: &str, root: &Path) -> Self {
        Self {
            name: name.into(),
            port: port(),
            root: root.into(),
            node: None,
        }
    }
    pub async fn start(&mut self, mode: Value) -> Result<Arc<Node>, String> {
        let node = Node::spawn(&self.name, &self.root, self.port, mode, OPEN).await?;
        self.node = Some(node.clone());
        Ok(node)
    }
    pub async fn resume(&mut self) -> Result<Arc<Node>, String> {
        self.start(json!({"mode":"resume"})).await
    }
    pub fn n(&self) -> &Arc<Node> {
        self.node.as_ref().expect("node running")
    }
    pub async fn kill(&mut self) {
        if let Some(node) = self.node.take() {
            node.kill().await;
        }
    }
    pub async fn close(&mut self) -> Result<(), String> {
        match self.node.take() {
            Some(node) => node.close().await,
            None => Ok(()),
        }
    }
}

pub async fn author_all(node: &Node, issuer: &Node) {
    for table in TABLES {
        if let Err(error) = node.author(issuer, table).await {
            println!("  author {} {table}: {error}", node.name);
        }
    }
}
/// `notes` rows of `len` bytes and `lists` lists of `items` items on `a`.
pub async fn populate(
    a: &Node,
    prefix: &str,
    notes: usize,
    len: usize,
    lists: usize,
    items: usize,
) -> Vec<String> {
    for start in (0..notes).step_by(100) {
        a.add_notes_batch(prefix, start, 100.min(notes - start), len)
            .await
            .unwrap_or_else(|e| panic!("populate notes: {e}"));
    }
    let mut ids = Vec::new();
    for l in 0..lists {
        let id = a
            .add_list(&format!("{prefix}list{l}"))
            .await
            .unwrap_or_else(|e| panic!("populate list: {e}"));
        for i in 0..items {
            a.add_item(&id, &format!("{prefix}item{l}.{i}"))
                .await
                .unwrap_or_else(|e| panic!("populate item: {e}"));
        }
        ids.push(id);
    }
    ids
}

/// Joins `slot` as a full writer: authored, and every list shared with it.
pub async fn join_full(a: &Node, slot: &mut Slot, lists: &[String]) -> Result<Arc<Node>, Failure> {
    let node = slot
        .start(json!({"mode":"join","invitation": a.invite(writer_all()).await}))
        .await?;
    author_all(&node, a).await;
    let id = node.node_id().await;
    for list in lists {
        let _ = a.share(list, std::slice::from_ref(&id)).await;
    }
    Ok(node)
}

pub async fn status_data(node: &Node) -> Value {
    node.status()
        .await
        .map(|s| s["data"].clone())
        .unwrap_or_else(|e| json!({"error": e}))
}
/// Polls until the node has imported at least `rows` checkpoint rows, or `limit`.
pub async fn until_imported(node: &Node, rows: u64, limit: Duration) -> Value {
    let started = Instant::now();
    loop {
        let data = status_data(node).await;
        let done = data["bootstrap"]["rows_done"].as_u64().unwrap_or(0);
        if done >= rows || started.elapsed() > limit {
            return data;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

pub const NOTES: &str = "SELECT key, body FROM notes ORDER BY key";
pub const LISTS: &str = "SELECT id, title FROM lists ORDER BY id";
pub const ITEMS: &str = "SELECT list, id, title, done FROM items ORDER BY list, id";

/// Online, meshed and synced until `full` agree on every table and no
/// bootstrap remains, or `rounds` passes elapse. Returns whether they agreed.
pub async fn heal(nodes: &[&Node], full: &[&Node], rounds: usize) -> bool {
    for node in nodes {
        let _ = node
            .call(json!({"type":"networking_control","enabled":true}))
            .await;
    }
    mesh(nodes).await;
    for round in 0..rounds {
        for node in nodes {
            let _ = node.sync().await;
        }
        let mut views = Vec::new();
        for node in full {
            views.push((
                node.dump(NOTES).await,
                node.dump(LISTS).await,
                node.dump(ITEMS).await,
            ));
        }
        let mut pending = false;
        for node in nodes {
            pending |= status_data(node).await["bootstrap_pending"] == true;
        }
        if views.windows(2).all(|w| w[0] == w[1]) && !pending {
            println!("  healed after {} rounds", round + 1);
            return true;
        }
    }
    false
}
pub async fn report_divergence(full: &[&Node]) {
    for (label, sql) in [("notes", NOTES), ("lists", LISTS), ("items", ITEMS)] {
        let mut counts = Vec::new();
        for node in full {
            counts.push(format!(
                "{}={}",
                node.name,
                node.dump(sql)
                    .await
                    .map(|r| r.len().to_string())
                    .unwrap_or_else(|e| format!("err {e}"))
            ));
        }
        println!("  {label}: {}", counts.join(" "));
    }
    for node in full {
        println!(
            "  {} data status: {}",
            node.name,
            short(&status_data(node).await)
        );
    }
}
/// `PRAGMA integrity_check` and foreign keys on a closed node's file.
pub fn integrity(dir: &Path) -> String {
    let file = dir.join("rocky.sqlite");
    let out = std::process::Command::new("sqlite3")
        .arg(&file)
        .arg("PRAGMA integrity_check; PRAGMA foreign_key_check;")
        .output();
    match out {
        Ok(out) => {
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
                + &String::from_utf8_lossy(&out.stderr)
        }
        Err(e) => format!("sqlite3 unavailable: {e}"),
    }
}

pub async fn run(names: &[String]) -> Result<(), Failure> {
    let all = [
        "kill_joiner_during_bootstrap",
        "kill_source_during_bootstrap",
        "writes_during_bootstrap",
        "network_cuts_during_bootstrap",
        "kill_during_enrollment",
        "enrollment_window",
        "partial_role_race",
        "partial_holds_shared",
        "revoke_during_bootstrap",
        "app_sql_abuse",
        "kill_in_transaction",
        "evict_kill",
        "evict_with_full_node",
        "evict_resubscribe",
        "prune_then_rejoin",
        "close_during_sync",
        "foreground_rebind",
        "pending_forever",
        "horizon_freeze",
        "bootstrap_from_partial",
        "revoked_via_stale_peer",
        "double_open",
        "file_damage",
        "request_fuzz",
        "half_bootstrapped_source",
        "cross_issuer_share",
        "orphaned_second_issuer",
        // Slow (about 12 minutes): "envelope_kill".
    ];
    let names: Vec<String> = if names.iter().any(|n| n == "all") {
        all.iter().map(|s| s.to_string()).collect()
    } else {
        names.to_vec()
    };
    for name in &names {
        println!("=== {name}");
        let started = Instant::now();
        let result = match name.as_str() {
            "kill_joiner_during_bootstrap" => kill_joiner_during_bootstrap().await,
            "kill_source_during_bootstrap" => kill_source_during_bootstrap().await,
            "writes_during_bootstrap" => writes_during_bootstrap().await,
            "network_cuts_during_bootstrap" => network_cuts_during_bootstrap().await,
            "kill_during_enrollment" => kill_during_enrollment().await,
            "partial_role_race" => partial_role_race().await,
            "revoke_during_bootstrap" => revoke_during_bootstrap().await,
            "app_sql_abuse" => crate::abuse::app_sql_abuse().await,
            "kill_in_transaction" => crate::abuse::kill_in_transaction().await,
            "evict_kill" => crate::abuse::evict_kill().await,
            "prune_then_rejoin" => crate::abuse::prune_then_rejoin().await,
            "close_during_sync" => crate::abuse::close_during_sync().await,
            "foreground_rebind" => crate::probes::foreground_rebind().await,
            "enrollment_window" => crate::probes::enrollment_window().await,
            "partial_holds_shared" => crate::probes::partial_holds_shared().await,
            "pending_forever" => crate::probes::pending_forever().await,
            "horizon_freeze" => crate::probes::horizon_freeze().await,
            "evict_with_full_node" => crate::probes::evict_with_full_node().await,
            "evict_resubscribe" => crate::probes::evict_resubscribe().await,
            "bootstrap_from_partial" => crate::extra::bootstrap_from_partial().await,
            "revoked_via_stale_peer" => crate::extra::revoked_via_stale_peer().await,
            "double_open" => crate::extra::double_open().await,
            "file_damage" => crate::extra::file_damage().await,
            "request_fuzz" => crate::extra::request_fuzz().await,
            "half_bootstrapped_source" => crate::extra::half_bootstrapped_source().await,
            "cross_issuer_share" => crate::extra::cross_issuer_share().await,
            "envelope_kill" => crate::extra::envelope_kill().await,
            "orphaned_second_issuer" => crate::extra::orphaned_second_issuer().await,
            other => Err(format!("unknown scenario {other}").into()),
        };
        match result {
            Ok(()) => println!(
                "=== {name}: completed in {:.1}s",
                started.elapsed().as_secs_f64()
            ),
            Err(e) => println!(
                "=== {name}: harness error {e} after {:.1}s",
                started.elapsed().as_secs_f64()
            ),
        }
    }
    Ok(())
}

/// A trusted issuer with a bootstrap big enough to interrupt.
async fn seeded(root: &Path, notes: usize) -> Result<(Slot, Vec<String>), Failure> {
    let mut a = Slot::new("a", root);
    a.start(json!({"mode":"create"})).await?;
    author_all(a.n(), a.n()).await;
    let lists = populate(a.n(), "a", notes, 2000, 3, 20).await;
    Ok((a, lists))
}

async fn kill_joiner_during_bootstrap() -> Result<(), Failure> {
    let root = root("kill-joiner");
    let (mut a, lists) = seeded(&root, 1500).await?;
    let mut b = Slot::new("b", &root);
    let invitation = a.n().invite(writer_all()).await;
    let started = Instant::now();
    b.start(json!({"mode":"join","invitation":invitation}))
        .await?;
    println!("  join returned after {:?}", started.elapsed());
    let id = b.n().node_id().await;
    for list in &lists {
        let _ = a.n().share(list, std::slice::from_ref(&id)).await;
    }
    // Kill at increasing import progress, then resume each time.
    for (round, at) in [1u64, 200, 700, 1200].into_iter().enumerate() {
        let data = until_imported(b.n(), at, Duration::from_secs(30)).await;
        println!("  round {round}: kill at {}", short(&data["bootstrap"]));
        b.kill().await;
        if let Err(e) = b.resume().await {
            finding(format!(
                "joiner cannot reopen after SIGKILL during bootstrap (round {round}): {e}"
            ));
            return Ok(());
        }
    }
    let ok = heal(&[a.n(), b.n()], &[a.n(), b.n()], 6).await;
    if !ok {
        finding("joiner killed during bootstrap never converged with its source");
        report_divergence(&[a.n(), b.n()]).await;
    }
    b.close().await?;
    a.close().await?;
    println!("  b integrity: {}", integrity(&root.join("b")));
    Ok(())
}

async fn kill_source_during_bootstrap() -> Result<(), Failure> {
    let root = root("kill-source");
    let (mut a, lists) = seeded(&root, 1500).await?;
    // A second full node C already synced: B may switch signer to it.
    let mut c = Slot::new("c", &root);
    join_full(a.n(), &mut c, &lists).await?;
    let _ = c.n().sync().await;
    let _ = c.n().sync().await;
    let mut b = Slot::new("b", &root);
    join_full(a.n(), &mut b, &lists).await?;
    let data = until_imported(b.n(), 300, Duration::from_secs(30)).await;
    println!("  killing source a at {}", short(&data["bootstrap"]));
    a.kill().await;
    mesh(&[b.n(), c.n()]).await;
    let started = Instant::now();
    let mut switched = false;
    while started.elapsed() < Duration::from_secs(20) {
        let _ = b.n().sync().await;
        if status_data(b.n()).await["bootstrap_pending"] == false {
            switched = true;
            break;
        }
    }
    println!(
        "  with signer a gone, b completed from c: {switched} after {:?}: {}",
        started.elapsed(),
        short(&status_data(b.n()).await["bootstrap"])
    );
    if !switched {
        println!(
            "  (documented: another signer takes over only after unlink or 3 min without progress)"
        );
    }
    // B writes while the signer is away.
    for i in 0..20 {
        if let Err(e) = b.n().add_note(&format!("b-offline{i}"), "x").await {
            println!("  b write during stalled import: {e}");
            break;
        }
    }
    a.resume().await?;
    let ok = heal(&[a.n(), b.n(), c.n()], &[a.n(), b.n(), c.n()], 8).await;
    if !ok {
        finding("source killed mid-bootstrap: the three nodes do not converge after it returns");
        report_divergence(&[a.n(), b.n(), c.n()]).await;
    }
    for slot in [&mut a, &mut b, &mut c] {
        slot.close().await?;
    }
    Ok(())
}

async fn writes_during_bootstrap() -> Result<(), Failure> {
    let root = root("writes-bootstrap");
    let (mut a, lists) = seeded(&root, 1200).await?;
    let mut b = Slot::new("b", &root);
    b.start(json!({"mode":"join","invitation": a.n().invite(writer_all()).await}))
        .await?;
    let id = b.n().node_id().await;
    for list in &lists {
        let _ = a.n().share(list, std::slice::from_ref(&id)).await;
    }
    // Both sides write while B imports: B into notes and into a list whose
    // key it may not hold yet, A into the scopes being imported.
    let bn = b.n().clone();
    let an = a.n().clone();
    let list = lists[0].clone();
    let writer_b = tokio::spawn(async move {
        let mut log = Vec::new();
        for i in 0..60 {
            if i == 0 {
                for t in TABLES {
                    log.push(format!("author {t}: {:?}", bn.author(&an, t).await.is_ok()));
                }
            }
            let r = bn.add_note(&format!("b-during{i}"), "during").await;
            let r2 = bn.add_item(&list, &format!("b-item{i}")).await;
            log.push(format!("{i}: note {} item {}", r.is_ok(), short(&r2.err())));
        }
        log
    });
    let an = a.n().clone();
    let list = lists[1].clone();
    let writer_a = tokio::spawn(async move {
        for i in 0..60 {
            let _ = an.add_note(&format!("a-during{i}"), "during").await;
            let _ = an.add_item(&list, &format!("a-item{i}")).await;
            // Rewrite rows being imported: update and delete.
            let _ = an
                .sql(
                    "UPDATE notes SET body = 'updated' WHERE key = ?",
                    json!([{"type":"text","value":format!("a{i}")}]),
                )
                .await;
            let _ = an
                .sql(
                    "DELETE FROM notes WHERE key = ?",
                    json!([{"type":"text","value":format!("a{}", 100 + i)}]),
                )
                .await;
        }
    });
    let log = writer_b.await?;
    writer_a.await?;
    let refused: Vec<_> = log.iter().filter(|l| l.contains("Some(")).take(3).collect();
    println!("  b writes during import, first refusals: {refused:?}");
    let ok = heal(&[a.n(), b.n()], &[a.n(), b.n()], 6).await;
    if !ok {
        finding("rows written on both sides during a bootstrap do not converge");
        report_divergence(&[a.n(), b.n()]).await;
    }
    // Every B write that reported success must exist on A.
    let notes_a = a.n().dump(NOTES).await.unwrap_or_default();
    let lost: Vec<_> = (0..60)
        .filter(|i| log.get(*i + 3).is_some_and(|l| l.contains("note true")))
        .filter(|i| {
            !notes_a
                .iter()
                .any(|r| r.contains(&format!("b-during{i}\"")))
        })
        .collect();
    if !lost.is_empty() {
        finding(format!(
            "acknowledged writes made during bootstrap missing on the source: {lost:?}"
        ));
    }
    b.close().await?;
    a.close().await?;
    Ok(())
}

async fn network_cuts_during_bootstrap() -> Result<(), Failure> {
    let root = root("cuts-bootstrap");
    let (mut a, lists) = seeded(&root, 1500).await?;
    let mut b = Slot::new("b", &root);
    // A fixed port cannot rebind after a socket fault (separate finding).
    if std::env::var("CHAOS_FIXED_PORT").is_err() {
        b.port = 0;
    }
    join_full(a.n(), &mut b, &lists).await?;
    // Alternate socket faults, networking toggles and background/foreground.
    for round in 0..30 {
        let data = status_data(b.n()).await;
        if data["bootstrap_pending"] == false {
            println!("  import finished at round {round}");
            break;
        }
        let call = match round % 4 {
            0 => json!({"type":"network_fault_close"}),
            1 => json!({"type":"networking_control","enabled":false}),
            2 => json!({"type":"network_background"}),
            _ => json!({"type":"sync_start","enabled":false}),
        };
        let r = b.n().call_for(call.clone(), Duration::from_secs(20)).await;
        if let Err(e) = &r {
            println!("  {call} -> {e}");
        }
        // The source drops its links too; it keeps its fixed address.
        let _ = a
            .n()
            .call_for(
                json!({"type":"networking_control","enabled":false}),
                Duration::from_secs(20),
            )
            .await;
        tokio::time::sleep(Duration::from_millis(30)).await;
        let _ = a
            .n()
            .call_for(
                json!({"type":"networking_control","enabled":true}),
                Duration::from_secs(20),
            )
            .await;
        for call in [
            json!({"type":"networking_control","enabled":true}),
            json!({"type":"network_foreground"}),
            json!({"type":"sync_start","enabled":true}),
        ] {
            if let Err(e) = b.n().call_for(call.clone(), Duration::from_secs(20)).await {
                finding(format!("restoring network with {call} failed: {e}"));
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let ok = heal(&[a.n(), b.n()], &[a.n(), b.n()], 6).await;
    if !ok {
        finding("repeated network faults during bootstrap leave the joiner unconverged");
        report_divergence(&[a.n(), b.n()]).await;
    }
    b.close().await?;
    a.close().await?;
    Ok(())
}

/// SIGKILL while `Database::join` is enrolling, at many offsets: can the
/// directory be resumed or joined again with a fresh invitation?
async fn kill_during_enrollment() -> Result<(), Failure> {
    let root = root("kill-enroll");
    let (mut a, _lists) = seeded(&root, 300).await?;
    let mut stuck = 0;
    for (attempt, delay_ms) in [1u64, 5, 15, 30, 60, 120, 250, 500].into_iter().enumerate() {
        let name = format!("j{attempt}");
        let mut j = Slot::new(&name, &root);
        let invitation = a.n().invite(writer_all()).await;
        let options = crate::node::options(
            root.join(&name).to_str().unwrap(),
            j.port,
            json!({"mode":"join","invitation":invitation}),
        );
        let mut child = tokio::process::Command::new(crate::cluster::exe())
            .arg("node")
            .arg(options.to_string())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        let _ = child.start_kill();
        let _ = child.wait().await;
        let files: Vec<_> = std::fs::read_dir(root.join(&name))
            .map(|d| {
                d.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        let resumed = j.resume().await;
        let outcome = match resumed {
            Ok(node) => {
                let ok = node.sync().await.is_ok();
                let data = status_data(&node).await;
                let _ = j.close().await;
                format!(
                    "resume ok, sync ok={ok}, pending={}",
                    data["bootstrap_pending"]
                )
            }
            Err(resume_error) => {
                // Can a fresh invitation repair the directory?
                let fresh = a.n().invite(writer_all()).await;
                match j.start(json!({"mode":"join","invitation":fresh})).await {
                    Ok(_) => {
                        let _ = j.close().await;
                        format!("resume failed ({resume_error}); fresh join ok")
                    }
                    Err(join_error) => {
                        stuck += 1;
                        format!(
                            "STUCK: resume failed ({resume_error}); fresh join failed ({join_error})"
                        )
                    }
                }
            }
        };
        println!("  kill after {delay_ms}ms, files {files:?}: {outcome}");
    }
    if stuck > 0 {
        finding(format!(
            "{stuck} directories killed during enrollment can neither resume nor join again"
        ));
    }
    a.close().await?;
    Ok(())
}

/// The shopping app joins first and sets the Partial role afterwards; the
/// join's background bootstrap runs in between. Is a list never shared with
/// the partial device still received and held?
async fn partial_role_race() -> Result<(), Failure> {
    let root = root("partial-race");
    let (mut a, lists) = seeded(&root, 50).await?;
    let mut f = Slot::new("f", &root);
    f.start(json!({"mode":"join","invitation": a.n().invite(writer_all()).await}))
        .await?;
    let fid = f.n().node_id().await;
    for list in &lists {
        let _ = a.n().share(list, std::slice::from_ref(&fid)).await;
    }
    let _ = f.n().sync().await;

    let mut p = Slot::new("p", &root);
    p.start(json!({"mode":"join","invitation": a.n().invite(writer_all()).await}))
        .await?;
    // Exactly as ShoppingNode.join: role after join returns.
    let role = p.n().role(true).await;
    println!("  role after join: {}", short(&role));
    let pid = p.n().node_id().await;
    let _ = a.n().share(&lists[0], std::slice::from_ref(&pid)).await;
    mesh(&[a.n(), f.n(), p.n()]).await;
    for _ in 0..3 {
        for n in [a.n(), f.n(), p.n()] {
            let _ = n.sync().await;
        }
    }
    let held = p.n().dump(LISTS).await.unwrap_or_default();
    println!("  partial holds lists {held:?} (shared only {})", lists[0]);
    if held.len() > 1 {
        finding("a partial device holds lists that were never shared with it");
    }
    // Second variant: SIGKILL between join and role. The app persisted
    // its kind 'Partial' before calling role; resume never re-applies it.
    let mut q = Slot::new("q", &root);
    q.start(json!({"mode":"join","invitation": a.n().invite(writer_all()).await}))
        .await?;
    q.kill().await;
    q.resume().await?;
    let status = q.n().call(json!({"type":"partition_status","group":"list","values":[{"type":"integer","value":lists[1]}]})).await;
    println!(
        "  q (killed before role) partition status of unshared list: {}",
        short(&status)
    );
    for slot in [&mut a, &mut f, &mut p, &mut q] {
        slot.close().await?;
    }
    Ok(())
}

async fn revoke_during_bootstrap() -> Result<(), Failure> {
    let root = root("revoke-bootstrap");
    let (mut a, lists) = seeded(&root, 1200).await?;
    let mut b = Slot::new("b", &root);
    join_full(a.n(), &mut b, &lists).await?;
    let mut c = Slot::new("c", &root);
    join_full(a.n(), &mut c, &lists).await?;
    let _ = c.n().sync().await;
    // C writes unsynced rows, then both get revoked: B mid-import.
    for i in 0..10 {
        let _ = c.n().add_note(&format!("c-unsent{i}"), "x").await;
    }
    let _ = until_imported(b.n(), 200, Duration::from_secs(30)).await;
    let bid = b.n().node_id().await;
    let cid = c.n().node_id().await;
    println!(
        "  revoke b: {}",
        short(&a.n().call(json!({"type":"revoke","node":bid})).await)
    );
    println!(
        "  revoke c: {}",
        short(&a.n().call(json!({"type":"revoke","node":cid})).await)
    );
    mesh(&[a.n(), b.n(), c.n()]).await;
    for n in [b.n(), c.n(), b.n()] {
        let r = n.sync().await;
        println!(
            "  {} sync after revoke: {}",
            n.name,
            short(&r.map(|r| r["peers"].as_array().map(|p| p.len())))
        );
    }
    for n in [b.n(), c.n()] {
        let s = n.status().await.unwrap_or_default();
        println!(
            "  {} authority {}",
            n.name,
            short(&s["authority"]["verified_revocation"])
        );
        // A revoked device must still open, read and close.
        let read = n.dump(NOTES).await.map(|r| r.len());
        println!("  {} local read after revocation: {read:?}", n.name);
    }
    // The issuer stays healthy: writes and a new member bootstraps.
    let _ = a.n().add_note("after-revoke", "x").await;
    let mut d = Slot::new("d", &root);
    join_full(a.n(), &mut d, &lists).await?;
    let ok = heal(&[a.n(), d.n()], &[a.n(), d.n()], 5).await;
    if !ok {
        finding(
            "after revoking a device mid-bootstrap, a fresh member does not converge with the issuer",
        );
        report_divergence(&[a.n(), d.n()]).await;
    }
    let a_notes = a.n().dump(NOTES).await.unwrap_or_default();
    let unsent = a_notes.iter().filter(|r| r.contains("c-unsent")).count();
    println!("  c's unsynced rows on issuer after c's revocation: {unsent} (expected 0)");
    for slot in [&mut a, &mut b, &mut c, &mut d] {
        let r = slot.close().await;
        if let Err(e) = r {
            println!("  close {}: {e}", slot.name);
        }
    }
    for name in ["b", "c"] {
        let mut s = Slot::new(name, &root);
        match s.resume().await {
            Ok(n) => {
                println!(
                    "  {name} reopens after revocation; status {}",
                    short(&n.status().await.map(|s| s["authority"].clone()))
                );
                let _ = s.close().await;
            }
            Err(e) => println!("  {name} reopen after revocation: {e}"),
        }
    }
    Ok(())
}
