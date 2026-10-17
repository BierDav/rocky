//! Phase 7 gate: one full replica node and two partial replicas over the
//! native transport. See docs/runtime/scaling-limits.md.
use super::{Failure, lifetime, line, resident_mib};
use employees_app::Employees;
use rocky_core::*;
use rocky_secure::network::{NativeSession, link::Link};
use rocky_secure::*;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};

type Replica = EncryptedReplica<Employees>;
const SCOPES: u8 = 20;
const QUOTA: usize = MAX_SECURE_FRAME / 2;
const ROWS_PER_OPERATION: usize = 32;

struct World {
    publisher: PrivateKey,
    manifest: SignedManifest,
    app: Employees,
    scopes: Vec<ResourceScope>,
}
#[derive(Clone)]
struct Node {
    key: PrivateKey,
    credentials: CredentialChain,
    replica: Replica,
}
impl World {
    fn new() -> Result<Self, Failure> {
        let publisher = PrivateKey::new();
        let app = Employees::new(*publisher.public_key().as_bytes());
        let body = app.definition();
        let public = app.scope(MeshId([1; 16]), false);
        Ok(Self {
            manifest: SignedManifest::sign(&body, &publisher)?,
            scopes: (0..SCOPES)
                .map(|index| ResourceScope {
                    encryption: commitment("roles.department.v1", &[index]),
                    ..public
                })
                .collect(),
            app,
            publisher,
        })
    }
    fn credentials(&self, node: &PrivateKey) -> Result<CredentialChain, Failure> {
        self.credentials_for(node, &self.scopes)
    }
    /// Every frame carries its author's chain, so its size follows the grants.
    fn credentials_for(
        &self,
        node: &PrivateKey,
        scopes: &[ResourceScope],
    ) -> Result<CredentialChain, Failure> {
        let grants: Vec<_> = scopes
            .iter()
            .map(|scope| ResourceGrant {
                scope: *scope,
                rows: RowSelection::All,
                rights: [
                    Right::Read,
                    Right::Insert,
                    Right::Update,
                    Right::Delete,
                    Right::Custody,
                ]
                .into(),
                fields: [self.app.field("display_name")].into_iter().collect(),
            })
            .collect();
        let user = PrivateKey::new();
        let deployment = PrivateKey::new();
        let extension = credentials::Extension::new(credentials::Extended::default())?;
        let capability = |subject, kind, delegations| Capability {
            subject,
            kind,
            grants: grants.clone(),
            not_before: lifetime().0,
            expires: lifetime().1,
            delegations,
            extension: extension.clone(),
        };
        Ok(CredentialChain(vec![
            SignedCapability::sign(
                capability(user.public_key(), PrincipalKind::User, 2),
                &self.publisher,
            )?,
            SignedCapability::sign(
                capability(deployment.public_key(), PrincipalKind::Deployment, 1),
                &user,
            )?,
            SignedCapability::sign(
                capability(node.public_key(), PrincipalKind::Node, 0),
                &deployment,
            )?,
        ]))
    }
    async fn open(
        &self,
        path: &Path,
        key: &PrivateKey,
        credentials: &CredentialChain,
    ) -> Result<Node, Failure> {
        self.open_scopes(path, key, credentials, &self.scopes).await
    }
    async fn open_scopes(
        &self,
        path: &Path,
        key: &PrivateKey,
        credentials: &CredentialChain,
        scopes: &[ResourceScope],
    ) -> Result<Node, Failure> {
        let config = SecureConfig {
            publisher: self.publisher.public_key(),
            manifest: self.manifest.clone(),
            revocations: RevocationSet::new(),
            node: key.clone(),
            credentials: credentials.clone(),
            keys: scopes
                .iter()
                .map(|scope| {
                    KeyMaterial::new(*scope, 1, commitment("roles.key.v1", &scope.encryption))
                })
                .collect(),
        };
        let replica = EncryptedReplica::open(
            path,
            Employees::new(*self.publisher.public_key().as_bytes()),
            config,
        )
        .await?;
        // A full node keeps everything it was not told it may drop.
        replica
            .set_retention_limits(RetentionLimits {
                hard_bytes: 4 << 30,
                ..RetentionLimits::default()
            })
            .await?;
        Ok(Node {
            key: key.clone(),
            credentials: credentials.clone(),
            replica,
        })
    }
    fn row(scope: u8, index: usize) -> Commitment {
        let mut bytes = vec![scope];
        bytes.extend((index as u64).to_le_bytes());
        commitment("roles.row.v1", &bytes)
    }
    fn change(&self, scope: u8, index: usize, before: Option<&str>, after: &str) -> RowChange {
        RowChange {
            scope: self.scopes[scope as usize],
            row: Self::row(scope, index),
            before: before.map(|name| self.app.name(name)),
            after: Some(self.app.name(after)),
        }
    }
}
fn handle(node: &Node, label: &str, index: usize) -> Commitment {
    let mut bytes = node.key.public_key().as_bytes().to_vec();
    bytes.extend(label.as_bytes());
    bytes.extend((index as u64).to_le_bytes());
    commitment("roles.handle.v1", &bytes)
}
/// A request session of `client` on a loopback link to `server`, and the
/// task answering it until the client hangs up or the task is aborted.
async fn connect(
    client: &Node,
    server: &Node,
) -> Result<(NativeSession, tokio::task::JoinHandle<()>), Failure> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (accepted, dialed) = tokio::join!(
        async {
            let (stream, _) = listener.accept().await?;
            let key = server.key.clone();
            Ok::<_, Failure>(
                Link::open(
                    stream,
                    &server.replica,
                    key,
                    server.credentials.clone(),
                    None,
                )
                .await?,
            )
        },
        async {
            Ok::<_, Failure>(
                Link::open(
                    TcpStream::connect(address).await?,
                    &client.replica,
                    client.key.clone(),
                    client.credentials.clone(),
                    Some(server.key.public_key()),
                )
                .await?,
            )
        },
    );
    let (mut accepted, mut dialed) = (accepted?, dialed?);
    let (near, far) = tokio::join!(dialed.ready(false), accepted.ready(false));
    near?;
    far?;
    let handle = dialed.channels.handle.clone();
    let (served, mut incoming) = tokio::sync::mpsc::channel::<NativeSession>(1);
    let node = server.clone();
    let task = tokio::spawn(async move {
        let serve = async {
            let Some(mut session) = incoming.recv().await else {
                return;
            };
            loop {
                match session.serve_once(&node.replica).await {
                    Ok(()) => {}
                    // The peer hung up.
                    Err(rocky_runtime::Error::Io(_)) => return,
                    Err(error) => {
                        eprintln!("roles server: {error}");
                        return;
                    }
                }
            }
        };
        // Ending the exchange drops both ends of the link.
        tokio::select! {
            () = serve => {}
            _ = accepted.drive(served) => {}
            _ = dialed.drive(tokio::sync::mpsc::channel(1).0) => {}
        }
    });
    Ok((handle.open().await?, task))
}
async fn finish(session: NativeSession, task: tokio::task::JoinHandle<()>) -> Result<(), Failure> {
    drop(session);
    task.await?;
    Ok(())
}
async fn rows(
    replica: &Replica,
    scope: ResourceScope,
) -> Result<BTreeMap<Commitment, RowFields>, Failure> {
    let mut rows = BTreeMap::new();
    let mut cursor = None;
    loop {
        let page = replica
            .snapshot_page(scope, cursor, MAX_SNAPSHOT_ROWS)
            .await?;
        rows.extend(page.rows);
        cursor = page.next;
        if cursor.is_none() {
            return Ok(rows);
        }
    }
}
async fn same(a: &Replica, b: &Replica, scope: ResourceScope) -> Result<bool, Failure> {
    Ok(a.frontier(scope)
        .await?
        .logs
        .iter()
        .map(|log| (log.author, log.applied))
        .collect::<Vec<_>>()
        == b.frontier(scope)
            .await?
            .logs
            .iter()
            .map(|log| (log.author, log.applied))
            .collect::<Vec<_>>()
        && rows(a, scope).await? == rows(b, scope).await?)
}
fn check(condition: bool, what: &str) -> Result<(), Failure> {
    if condition {
        println!("roles check: {what}: ok");
        Ok(())
    } else {
        Err(format!("roles check failed: {what}").into())
    }
}
/// Syncs `scope`, on new sessions while one runs out of its byte budget.
async fn sync(
    client: &Node,
    server: &Node,
    scope: ResourceScope,
) -> Result<network::ScopeSync, Failure> {
    let mut total: Option<network::ScopeSync> = None;
    loop {
        let (mut session, task) = connect(client, server).await?;
        let synced = session.sync_scope(&client.replica, scope, QUOTA).await?;
        finish(session, task).await?;
        let exhausted = synced.catch_up.exhausted;
        total = Some(match total {
            None => synced,
            Some(mut total) => {
                total.rebootstrap = total.rebootstrap.or(synced.rebootstrap);
                total.catch_up.frames += synced.catch_up.frames;
                total.catch_up.pages += synced.catch_up.pages;
                total.catch_up.bytes += synced.catch_up.bytes;
                total
            }
        });
        if !exhausted {
            return total.ok_or_else(|| "no sync".into());
        }
    }
}
async fn push(client: &Node, server: &Node, scope: ResourceScope) -> Result<usize, Failure> {
    let mut frames = 0;
    loop {
        let (mut session, task) = connect(client, server).await?;
        let pushed = session.push(&client.replica, scope, QUOTA).await?;
        finish(session, task).await?;
        frames += pushed.frames;
        if pushed.behind {
            return Err("the full node is behind this replica's pruned_prefix".into());
        }
        if !pushed.exhausted {
            return Ok(frames);
        }
    }
}
fn partial(full: &Node) -> RoleConfig {
    RoleConfig {
        role: ReplicaRole::Partial,
        full_nodes: [rocky_secure::PeerId::of(full.key.public_key())].into(),
        custody: 1,
    }
}
/// Authors `count` single-row edits of fresh rows in `scope`, timing each.
async fn writes(
    world: &World,
    node: &Node,
    scope: u8,
    label: &str,
    stop: &std::sync::atomic::AtomicBool,
) -> Result<Vec<Duration>, Failure> {
    let mut times = Vec::new();
    let mut index = 0;
    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        let start = Instant::now();
        node.replica
            .author_buffered(
                handle(node, label, index),
                vec![world.change(scope, 1_000_000 + index, None, label)],
                vec![],
            )
            .await?;
        times.push(start.elapsed());
        index += 1;
        // Paced like an interactive client, not a bulk loader.
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    Ok(times)
}

pub async fn run(total: usize, behind: usize) -> Result<(), Failure> {
    let world = World::new()?;
    let dir = tempfile::tempdir()?;
    let full_key = PrivateKey::new();
    let full_credentials = world.credentials(&full_key)?;
    let mut full = world
        .open(&dir.path().join("full.db"), &full_key, &full_credentials)
        .await?;
    let open = |name: &str| {
        let key = PrivateKey::new();
        let path = dir.path().join(name);
        let world = &world;
        async move {
            let credentials = world.credentials(&key)?;
            world.open(&path, &key, &credentials).await
        }
    };
    let a = open("a.db").await?;
    let b = open("b.db").await?;
    for node in [&a, &b] {
        node.replica.set_role(partial(&full)).await?;
    }
    let per_scope = total / SCOPES as usize;
    println!("roles rows={total} scopes={SCOPES} rows_per_scope={per_scope} behind={behind}");

    // Devices author in their own scope and push to the full node.
    let start = Instant::now();
    let mut writers = Vec::new();
    for scope in 0..SCOPES {
        let key = PrivateKey::new();
        let scopes = [world.scopes[scope as usize]];
        let credentials = world.credentials_for(&key, &scopes)?;
        let writer = world
            .open_scopes(
                &dir.path().join(format!("writer{scope}.db")),
                &key,
                &credentials,
                &scopes,
            )
            .await?;
        for (batch, first) in (0..per_scope).step_by(ROWS_PER_OPERATION).enumerate() {
            let changes = (first..(first + ROWS_PER_OPERATION).min(per_scope))
                .map(|index| world.change(scope, index, None, &format!("row {index}")))
                .collect();
            writer
                .replica
                .author_buffered(handle(&writer, "load", batch), changes, vec![])
                .await?;
        }
        push(&writer, &full, scopes[0]).await?;
        writers.push(writer);
    }
    full.replica.flush().await?;
    let elapsed = start.elapsed();
    println!(
        "roles load (author and push): rows={total} ms={} rows_per_s={:.0} resident_mib={}",
        elapsed.as_millis(),
        total as f64 / elapsed.as_secs_f64(),
        resident_mib()
    );

    // 1. A partial replica holding 5% converges on its subset.
    a.replica.subscribe(world.scopes[0]).await?;
    let start = Instant::now();
    let synced = sync(&a, &full, world.scopes[0]).await?;
    println!(
        "roles subset-sync: rows={per_scope} frames={} pages={} frame_bytes_mean={} ms={}",
        synced.catch_up.frames,
        synced.catch_up.pages,
        synced.catch_up.bytes / synced.catch_up.frames.max(1),
        start.elapsed().as_millis()
    );
    check(
        same(&a.replica, &full.replica, world.scopes[0]).await?,
        "subset converges",
    )?;
    let status = a.replica.replica_status().await?;
    check(
        status
            .scopes
            .iter()
            .all(|scope| scope.scope == world.scopes[0] || scope.rows == 0),
        "no stored rows outside the subscription",
    )?;
    check(
        !a.replica
            .snapshot_page(world.scopes[1], None, 1)
            .await?
            .complete,
        "an unsubscribed scope is not complete",
    )?;

    // 2. A replica `behind` operations back catches up while both sides write.
    b.replica.subscribe(world.scopes[1]).await?;
    b.replica.subscribe(world.scopes[2]).await?;
    sync(&b, &full, world.scopes[1]).await?;
    for index in 0..behind {
        writers[1]
            .replica
            .author_buffered(
                handle(&writers[1], "behind", index),
                vec![world.change(1, 3_000_000 + index, None, "behind")],
                vec![],
            )
            .await?;
    }
    push(&writers[1], &full, world.scopes[1]).await?;
    let stop = std::sync::atomic::AtomicBool::new(false);
    let start = Instant::now();
    let catch_up = async {
        let result = sync(&b, &full, world.scopes[1]).await;
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        result.map(|synced| synced.catch_up)
    };
    let (caught, local, remote) = tokio::join!(
        catch_up,
        writes(&world, &b, 2, "b-local", &stop),
        writes(&world, &full, 3, "full-local", &stop),
    );
    let caught = caught?;
    let elapsed = start.elapsed();
    println!(
        "roles catch-up: operations={behind} frames={} pages={} ms={} ops_per_s={:.0} resident_mib={}",
        caught.frames,
        caught.pages,
        elapsed.as_millis(),
        caught.frames as f64 / elapsed.as_secs_f64(),
        resident_mib()
    );
    let (local, remote) = (local?, remote?);
    line("roles catch-up partial local writes", local, elapsed);
    line("roles catch-up full-node local writes", remote, elapsed);
    check(
        same(&b.replica, &full.replica, world.scopes[1]).await?,
        "caught-up scope converges",
    )?;

    // 3. Behind the pruned prefix: offline edits are applied or reported.
    a.replica.subscribe(world.scopes[1]).await?;
    for index in 0..10 {
        let current = rows(&b.replica, world.scopes[1]).await?[&World::row(1, index)].clone();
        let current = String::from_utf8(current[&world.app.field("display_name")].clone())?;
        b.replica
            .author_buffered(
                handle(&b, "offline", index),
                vec![world.change(1, index, Some(&current), &format!("offline {index}"))],
                vec![],
            )
            .await?;
    }
    for index in 0..200 {
        full.replica
            .author_buffered(
                handle(&full, "later", index),
                vec![world.change(1, 2_000_000 + index, None, "later")],
                vec![],
            )
            .await?;
    }
    sync(&a, &full, world.scopes[1]).await?;
    full.replica
        .set_retention_policy(
            world.scopes[1],
            RetentionPolicy {
                required: Default::default(),
                behind: [rocky_secure::PeerId::of(b.key.public_key())].into(),
                copies: 1,
                window: 0,
            },
        )
        .await?;
    full.replica.checkpoint(world.scopes[1]).await?;
    let pruned = full.replica.prune(world.scopes[1]).await?;
    println!(
        "roles prune: pruned={} blocked={:?}",
        pruned.pruned, pruned.blocked
    );
    let start = Instant::now();
    let synced = sync(&b, &full, world.scopes[1]).await?;
    let report = synced.rebootstrap.ok_or("expected a rebootstrap")?;
    println!(
        "roles rebootstrap: ms={} superseded={} reapplied={} rejected={} pending={}",
        start.elapsed().as_millis(),
        report.superseded,
        report.reapplied.len(),
        report.rejected.len(),
        report.pending.len()
    );
    check(
        report.reapplied.len() + report.rejected.len() == 10 && report.pending.is_empty(),
        "every offline edit is reapplied or rejected",
    )?;
    push(&b, &full, world.scopes[1]).await?;
    check(
        same(&b.replica, &full.replica, world.scopes[1]).await?,
        "rebootstrapped scope converges",
    )?;

    // 4. Eviction needs custody; evicted and resubscribed converges.
    for index in 0..5 {
        a.replica
            .author_buffered(
                handle(&a, "evict", index),
                vec![world.change(0, index, Some(&format!("row {index}")), "local")],
                vec![],
            )
            .await?;
    }
    check(
        a.replica.evict(world.scopes[0]).await.is_err(),
        "eviction without custody is refused",
    )?;
    push(&a, &full, world.scopes[0]).await?;
    let before = rows(&a.replica, world.scopes[0]).await?;
    let start = Instant::now();
    let evicted = a.replica.evict(world.scopes[0]).await?;
    println!(
        "roles evict: rows={} operations={} ms={}",
        evicted.rows,
        evicted.operations,
        start.elapsed().as_millis()
    );
    a.replica.subscribe(world.scopes[0]).await?;
    let start = Instant::now();
    sync(&a, &full, world.scopes[0]).await?;
    println!("roles resubscribe: ms={}", start.elapsed().as_millis());
    check(
        rows(&a.replica, world.scopes[0]).await? == before,
        "resubscribed rows equal the evicted ones",
    )?;
    check(
        same(&a.replica, &full.replica, world.scopes[0]).await?,
        "resubscribed scope converges",
    )?;

    // 5. The full node dies mid-transfer and mid-checkpoint-serve.
    let c = open("c.db").await?;
    c.replica.set_role(partial(&full)).await?;
    c.replica.subscribe(world.scopes[1]).await?;
    c.replica.subscribe(world.scopes[4]).await?;
    let (mut session, task) = connect(&c, &full).await?;
    let interrupted = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        task.abort();
    };
    let (result, ()) = tokio::join!(
        session.catch_up(&c.replica, world.scopes[4], QUOTA),
        interrupted
    );
    println!(
        "roles kill mid-transfer: catch-up result ok={}",
        result.is_ok()
    );
    drop(session);
    let signed = full
        .replica
        .checkpoint_offer(c.key.public_key(), &c.credentials, world.scopes[1])
        .await?
        .ok_or("no checkpoint")?;
    c.replica.begin_rebootstrap(signed.clone()).await?;
    let page = full
        .replica
        .checkpoint_page(
            c.key.public_key(),
            &c.credentials,
            world.scopes[1],
            signed.id()?,
            None,
            QUOTA - 4096,
        )
        .await?;
    c.replica.stage_checkpoint_page(page).await?;
    // Every handle to the full node goes away, as in a process exit.
    let full_path = dir.path().join("full.db");
    drop(full);
    tokio::time::sleep(Duration::from_millis(100)).await;
    for scope in [world.scopes[1], world.scopes[4]] {
        c.replica.audit(scope).await?;
    }
    full = world.open(&full_path, &full_key, &full_credentials).await?;
    let start = Instant::now();
    for scope in [world.scopes[1], world.scopes[4]] {
        sync(&c, &full, scope).await?;
        check(
            same(&c.replica, &full.replica, scope).await?,
            "a replica interrupted by the full node's exit converges",
        )?;
    }
    println!(
        "roles resume after restart: ms={}",
        start.elapsed().as_millis()
    );

    // 6. A replacement full node starts from the existing one.
    let replacement = open("replacement.db").await?;
    let start = Instant::now();
    let (mut session, task) = connect(&replacement, &full).await?;
    let (offered, _) = session.peer_scopes(None).await?;
    finish(session, task).await?;
    for scope in &offered {
        sync(&replacement, &full, *scope).await?;
    }
    println!(
        "roles replacement: scopes={} ms={} resident_mib={}",
        offered.len(),
        start.elapsed().as_millis(),
        resident_mib()
    );
    let mut equal = offered.len() == SCOPES as usize;
    for scope in &world.scopes {
        equal &= same(&replacement.replica, &full.replica, *scope).await?;
    }
    check(equal, "the replacement full node reaches the same state")?;
    for node in [&a, &b] {
        node.replica.set_role(partial(&replacement)).await?;
    }
    for (node, scope) in [(&a, 0), (&b, 1)] {
        sync(node, &replacement, world.scopes[scope]).await?;
        check(
            same(&node.replica, &replacement.replica, world.scopes[scope]).await?,
            "partials converge with the replacement",
        )?;
    }
    Ok(())
}
