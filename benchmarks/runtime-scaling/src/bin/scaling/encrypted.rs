//! Baseline for the `EncryptedReplica` path. It has its own storage code, one
//! mutex around all work and its own retained-history bounds, which
//! checkpoints and pruning keep a scope inside.
use super::{Failure, futures, lifetime, line, resident_mib};
use employees_app::Employees;
use rocky_core::{
    MeshId, PrincipalKind, ResourceGrant, ResourceScope, Right, RowChange, RowSelection,
};
use rocky_runtime::Error;
use rocky_secure::{
    Capability, CredentialChain, EncryptedFrame, EncryptedReplica, KeyMaterial, MAX_SECURE_FRAME,
    MAX_SNAPSHOT_ROWS, PrivateKey, RetentionLimits, RetentionPolicy, RevocationSet, SecureConfig,
    SignedCapability, SignedManifest,
};
use std::time::Instant;

struct Fixture {
    publisher: PrivateKey,
    scope: ResourceScope,
}
impl Fixture {
    fn app(&self) -> Employees {
        Employees::new(*self.publisher.public_key().as_bytes())
    }
    /// Publisher, user, deployment and node: the full credential chain depth.
    fn config(&self, node: &PrivateKey) -> Result<SecureConfig, Error> {
        let manifest = self.app().definition();
        let (user, deployment) = (PrivateKey::new(), PrivateKey::new());
        let extension = rocky_secure::credentials::Extension::new(
            rocky_secure::credentials::Extended::default(),
        )
        .map_err(signing)?;
        let capability = |subject, kind, delegations| Capability {
            manifest: manifest.hash(),
            subject,
            kind,
            grants: vec![ResourceGrant {
                scope: self.scope,
                rows: RowSelection::All,
                rights: [
                    Right::Read,
                    Right::Insert,
                    Right::Update,
                    Right::Delete,
                    Right::Custody,
                ]
                .into(),
                fields: [self.app().field("display_name")].into(),
            }],
            not_before: lifetime().0,
            expires: lifetime().1,
            delegations,
            extension: extension.clone(),
        };
        Ok(SecureConfig {
            publisher: self.publisher.public_key(),
            manifest: SignedManifest::sign(&manifest, &self.publisher).map_err(signing)?,
            revocations: RevocationSet::new(),
            node: node.clone(),
            credentials: CredentialChain(vec![
                SignedCapability::sign(
                    capability(user.public_key(), PrincipalKind::User, 2),
                    &self.publisher,
                )
                .map_err(signing)?,
                SignedCapability::sign(
                    capability(deployment.public_key(), PrincipalKind::Deployment, 1),
                    &user,
                )
                .map_err(signing)?,
                SignedCapability::sign(
                    capability(node.public_key(), PrincipalKind::Node, 0),
                    &deployment,
                )
                .map_err(signing)?,
            ]),
            keys: vec![KeyMaterial {
                scope: self.scope,
                generation: 1,
                key: [11; 32],
            }],
        })
    }
    /// The `version`th edit of a row; version 0 inserts it.
    fn edit(&self, index: usize, version: usize) -> ([u8; 32], Vec<RowChange>) {
        let name = |version: usize| self.app().name(&format!("employee-{index}-{version}"));
        let change = RowChange {
            scope: self.scope,
            row: rocky_core::commitment("rocky.probe.row", &(index as u64).to_le_bytes()),
            before: version.checked_sub(1).map(name),
            after: Some(name(version)),
        };
        let handle = rocky_core::commitment(
            "rocky.probe.edit",
            &[(index as u64).to_le_bytes(), (version as u64).to_le_bytes()].concat(),
        );
        (handle, vec![change])
    }
    fn insert(&self, index: usize) -> ([u8; 32], Vec<RowChange>) {
        let id = rocky_core::commitment("rocky.probe.row", &(index as u64).to_le_bytes());
        let change = RowChange {
            scope: self.scope,
            row: id,
            before: None,
            after: Some(self.app().name(&format!("employee-{index}"))),
        };
        (id, vec![change])
    }
}
/// Prints the outcome of one measured step; a refusal is a result.
fn report(label: &str, started: Instant, result: Result<String, Error>) {
    match result {
        Ok(detail) => println!("{label}: {} ms{detail}", started.elapsed().as_millis()),
        Err(error) => println!("{label}: refused: {error}"),
    }
}
pub async fn run() -> Result<(), Failure> {
    let publisher = PrivateKey::new();
    let scope = Employees::new(*publisher.public_key().as_bytes()).scope(MeshId([1; 16]), false);
    let fixture = Fixture { publisher, scope };
    let dir = tempfile::tempdir()?;
    let (author_key, receiver_key) = (PrivateKey::new(), PrivateKey::new());
    let receiver_config = fixture.config(&receiver_key)?;
    let receiver_credentials = receiver_config.credentials.clone();
    let started = Instant::now();
    let mut source = EncryptedReplica::open(
        dir.path().join("author.db"),
        fixture.app(),
        fixture.config(&author_key)?,
    )
    .await?;
    println!(
        "encrypted install: {} ms, resident {} MiB",
        started.elapsed().as_millis(),
        resident_mib()
    );
    let target = EncryptedReplica::open(
        dir.path().join("receiver.db"),
        fixture.app(),
        receiver_config,
    )
    .await?;
    let mut frames: Vec<EncryptedFrame> = Vec::new();
    let mut delivered = 0;
    for checkpoint in [500usize, 1000, 2000, 4000] {
        let from = frames.len();
        let started = Instant::now();
        let mut timings = Vec::new();
        for index in from..checkpoint {
            let (handle, changes) = fixture.insert(index);
            let step = Instant::now();
            frames.push(source.author(handle, changes, vec![]).await?);
            timings.push(step.elapsed());
        }
        line(
            &format!("encrypted author rows {from}..{checkpoint}"),
            timings,
            started.elapsed(),
        );
        let started = Instant::now();
        let mut timings = Vec::new();
        for frame in &frames[delivered..] {
            let step = Instant::now();
            target.receive(frame.clone()).await?;
            timings.push(step.elapsed());
        }
        delivered = frames.len();
        line(
            &format!("encrypted incoming rows {from}..{checkpoint}"),
            timings,
            started.elapsed(),
        );
        let rows = format!("encrypted rows={checkpoint}");
        let started = Instant::now();
        let mut timings = Vec::new();
        for _ in 0..20 {
            let step = Instant::now();
            let (mut cursor, mut read) = (None, 0);
            loop {
                let page = target
                    .snapshot_page(scope, cursor, MAX_SNAPSHOT_ROWS)
                    .await?;
                read += page.rows.len();
                cursor = page.next;
                if cursor.is_none() {
                    break;
                }
            }
            assert_eq!(read, checkpoint);
            timings.push(step.elapsed());
        }
        line(
            &format!("{rows} paged pass of {MAX_SNAPSHOT_ROWS}"),
            timings,
            started.elapsed(),
        );
        let middle = fixture.insert(checkpoint / 2).0;
        let started = Instant::now();
        let mut timings = Vec::new();
        for _ in 0..200 {
            let step = Instant::now();
            assert_eq!(
                target
                    .snapshot_page(scope, Some(middle), 10)
                    .await?
                    .rows
                    .len(),
                10
            );
            timings.push(step.elapsed());
        }
        line(&format!("{rows} page of 10"), timings, started.elapsed());
        let started = Instant::now();
        let mut timings = Vec::new();
        for _ in 0..200 {
            let step = Instant::now();
            target.companion(scope, middle).await?;
            timings.push(step.elapsed());
        }
        line(&format!("{rows} point read"), timings, started.elapsed());
        // One complete catch-up pass, as a peer with nothing would request it.
        let started = Instant::now();
        let pass = async {
            let (mut cursor, mut pages, mut transferred) = (None, 0, 0);
            loop {
                let page = source
                    .transfer_page(
                        receiver_key.public_key(),
                        &receiver_credentials,
                        &[scope],
                        cursor,
                        MAX_SECURE_FRAME,
                    )
                    .await?;
                pages += 1;
                transferred += page.frames.len();
                cursor = page.next;
                if cursor.is_none() {
                    return Ok(format!(", {pages} pages, {transferred} frames"));
                }
            }
        }
        .await;
        report(&format!("{rows} transfer pass"), started, pass);
        let started = Instant::now();
        let rebuilt = source.rebuild_projection(scope).await;
        report(
            &format!("{rows} rebuild"),
            started,
            rebuilt.map(|()| String::new()),
        );
        drop(source);
        let started = Instant::now();
        source = EncryptedReplica::open(
            dir.path().join("author.db"),
            fixture.app(),
            fixture.config(&author_key)?,
        )
        .await?;
        println!(
            "{rows} reopen: {} ms, resident {} MiB",
            started.elapsed().as_millis(),
            resident_mib()
        );
    }
    // The single mutex: concurrent submitters queue, and reads wait behind writes.
    let contention = EncryptedReplica::open(
        dir.path().join("contention.db"),
        fixture.app(),
        fixture.config(&PrivateKey::new())?,
    )
    .await?;
    let mut next = 0;
    for clients in [1usize, 8] {
        let base = next;
        next += 240;
        let started = Instant::now();
        let results = futures(clients, |client| {
            let (contention, fixture) = (&contention, &fixture);
            async move {
                let mut timings = Vec::new();
                for index in (client..240).step_by(clients) {
                    let (handle, changes) = fixture.insert(base + index);
                    let step = Instant::now();
                    contention.author(handle, changes, vec![]).await?;
                    timings.push(step.elapsed());
                }
                Ok::<_, Error>(timings)
            }
        })
        .await;
        let mut timings = Vec::new();
        for result in results {
            timings.extend(result?);
        }
        line(
            &format!("encrypted one-replica author clients={clients}"),
            timings,
            started.elapsed(),
        );
    }
    let started = Instant::now();
    let (writes, reads) = tokio::join!(
        async {
            for index in 0..120 {
                let (handle, changes) = fixture.insert(next + index);
                contention.author(handle, changes, vec![]).await?;
            }
            Ok::<_, Error>(())
        },
        async {
            let mut timings = Vec::new();
            for _ in 0..120 {
                let step = Instant::now();
                contention.snapshot_page(scope, None, 100).await?;
                timings.push(step.elapsed());
                tokio::task::yield_now().await;
            }
            Ok::<_, Error>(timings)
        }
    );
    writes?;
    line(
        "encrypted page of 100 beside one writer rows=480..600",
        reads?,
        started.elapsed(),
    );
    let accepted = frames.len();
    let refusal = loop {
        let (handle, changes) = fixture.insert(frames.len());
        match source.author(handle, changes, vec![]).await {
            Ok(frame) => frames.push(frame),
            Err(error) => break error,
        }
    };
    println!(
        "encrypted bound without a retention policy: {} more rows accepted, then refused at {} records: {refusal}",
        frames.len() - accepted,
        frames.len()
    );
    println!(
        "encrypted files: author_bytes={}",
        std::fs::read_dir(dir.path())?.try_fold(0, |sum, entry| {
            let entry = entry?;
            let name = entry.file_name();
            let size = if name.to_string_lossy().starts_with("author") {
                entry.metadata()?.len()
            } else {
                0
            };
            Ok::<_, std::io::Error>(sum + size)
        })?
    );
    cycles(&fixture, dir.path()).await
}

fn file_bytes(directory: &std::path::Path, prefix: &str) -> Result<u64, std::io::Error> {
    std::fs::read_dir(directory)?.try_fold(0, |sum, entry| {
        let entry = entry?;
        let size = if entry.file_name().to_string_lossy().starts_with(prefix) {
            entry.metadata()?.len()
        } else {
            0
        };
        Ok(sum + size)
    })
}

/// One scope taken well past the former 4,096-record quota: 2,000 rows are
/// inserted and then edited, 1,000 operations per cycle. A second replica
/// holds custody and acknowledges; every cycle ends with a checkpoint, a
/// prune and a reopen. The handle window is 2,000 so that the file reaches its
/// steady state within the run.
async fn cycles(fixture: &Fixture, directory: &std::path::Path) -> Result<(), Failure> {
    const ROWS: usize = 2000;
    const BATCH: usize = 1000;
    let (author_key, custodian_key) = (PrivateKey::new(), PrivateKey::new());
    let custodian_config = fixture.config(&custodian_key)?;
    let credentials = custodian_config.credentials.clone();
    let path = directory.join("cycle.db");
    let mut source =
        EncryptedReplica::open(&path, fixture.app(), fixture.config(&author_key)?).await?;
    let custodian = EncryptedReplica::open(
        directory.join("custodian.db"),
        fixture.app(),
        custodian_config,
    )
    .await?;
    source
        .set_retention_policy(
            fixture.scope,
            RetentionPolicy {
                required: [custodian_key.public_key()].into(),
                behind: Default::default(),
                copies: 1,
                window: 256,
            },
        )
        .await?;
    let limits = RetentionLimits {
        handles: ROWS,
        ..RetentionLimits::default()
    };
    source.set_retention_limits(limits).await?;
    for cycle in 0..14 {
        let started = Instant::now();
        let mut timings = Vec::new();
        let mut frames = Vec::new();
        for step in 0..BATCH {
            let operation = cycle * BATCH + step;
            let (handle, changes) = fixture.edit(operation % ROWS, operation / ROWS);
            let begun = Instant::now();
            frames.push(source.author(handle, changes, vec![]).await?);
            timings.push(begun.elapsed());
        }
        let label = format!(
            "encrypted cycle {} operations={}",
            cycle + 1,
            (cycle + 1) * BATCH
        );
        line(&format!("{label} author"), timings, started.elapsed());
        let started = Instant::now();
        for page in frames.chunks(32) {
            for status in custodian.receive_all(page.to_vec()).await {
                status?;
            }
            for frame in page {
                source
                    .acknowledge(custodian_key.public_key(), &credentials, frame.id()?, true)
                    .await?;
            }
        }
        let delivered = started.elapsed().as_millis();
        let started = Instant::now();
        let statement = source.checkpoint(fixture.scope).await?.statement;
        let checkpointed = started.elapsed().as_millis();
        let started = Instant::now();
        let pruned = source.prune(fixture.scope).await?;
        let pruning = started.elapsed().as_millis();
        let status = source.checkpoint_status(fixture.scope).await?;
        drop(source);
        let started = Instant::now();
        source = EncryptedReplica::open(&path, fixture.app(), fixture.config(&author_key)?).await?;
        source.set_retention_limits(limits).await?;
        let reopened = started.elapsed().as_millis();
        println!(
            "{label}: deliver+acknowledge {delivered} ms, checkpoint {checkpointed} ms (sequence {}, rows {}), prune {pruning} ms ({} pruned, blocked {:?}), retained {} operations {} bytes, files {} bytes, reopen {reopened} ms, resident {} MiB",
            statement.sequence,
            statement.rows.count,
            pruned.pruned,
            pruned.blocked,
            status.covered + status.uncovered,
            status.retained_bytes,
            file_bytes(directory, "cycle")?,
            resident_mib()
        );
    }
    let started = Instant::now();
    let report = source.audit(fixture.scope).await?;
    println!(
        "encrypted audit after 14 cycles: {} ms, {} operations, {} rows, {} replayed",
        started.elapsed().as_millis(),
        report.operations,
        report.rows,
        report.replayed
    );
    let started = Instant::now();
    let (mut cursor, mut read) = (None, 0);
    loop {
        let page = source
            .snapshot_page(fixture.scope, cursor, MAX_SNAPSHOT_ROWS)
            .await?;
        read += page.rows.len();
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    println!(
        "encrypted rows after 14 cycles: {read} read in {} ms",
        started.elapsed().as_millis()
    );
    Ok(())
}

/// A signing failure as the runtime reports it.
fn signing(error: rocky_secure::credentials::Error) -> Error {
    Error::Rejected(error.to_string())
}
