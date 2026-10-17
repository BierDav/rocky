use super::*;
use rocky_core::{
    DomainId, PartitionSelection, PartitionSelector, PartitionTuple, PartitionValue, RowIdentity,
    SchemaDefinition, SqlValue, TypedRows,
};
use rocky_credentials::{AuthorRange, Authority, PartitionGrant};
use std::path::Path;

use std::time::{Duration, Instant};

pub(crate) mod fixture;
use fixture::Fixture;

async fn database_bytes(replica: &EncryptedReplica<TypedRows>) -> i64 {
    let inner = replica.inner.lock().await;
    let rows = crate::storage::rows(&inner.conn, "PRAGMA page_count")
        .await
        .unwrap();
    let SqlValue::Integer(pages) = rows[0][0] else {
        panic!("page count")
    };
    let rows = crate::storage::rows(&inner.conn, "PRAGMA page_size")
        .await
        .unwrap();
    let SqlValue::Integer(size) = rows[0][0] else {
        panic!("page size")
    };
    pages * size
}

// Same row/existence/pruned_prefix sequence as rocky-sdk::sync::rows, over loopback TCP.
async fn sync(
    writer: &EncryptedReplica<TypedRows>,
    reader: &EncryptedReplica<TypedRows>,
    scopes: &[ResourceScope],
) -> Vec<ResourceScope> {
    let (writer_node, writer_credentials) = {
        let inner = writer.inner.lock().await;
        (inner.config.node.clone(), inner.config.credentials.clone())
    };
    let (reader_node, reader_credentials) = {
        let inner = reader.inner.lock().await;
        (inner.config.node.clone(), inner.config.credentials.clone())
    };
    let mut visited = BTreeSet::new();
    for attempt in 1..=16 {
        let (near, far) = crate::network::link::testing::tcp().await;
        near.set_nodelay(true).unwrap();
        far.set_nodelay(true).unwrap();
        let (session, mut served) = crate::network::link::testing::sessions_over(
            near,
            far,
            (reader, reader_node.clone(), reader_credentials.clone()),
            (writer, writer_node.clone(), writer_credentials.clone()),
        )
        .await
        .unwrap();
        let server = async {
            served.serve(writer).await.unwrap();
            served.close().await.unwrap();
        };
        let client = async {
            let mut session = session;
            let changed = session
                .changed_scopes(reader, scopes)
                .await
                .unwrap()
                .changed;
            let mut exhausted = false;
            for scope in &changed {
                let pulled = session
                    .catch_up(reader, *scope, MAX_SECURE_FRAME / 2)
                    .await
                    .unwrap();
                assert!(!pulled.behind);
                if pulled.exhausted {
                    exhausted = true;
                    break;
                }
                let pushed = session
                    .push(reader, *scope, MAX_SECURE_FRAME / 2)
                    .await
                    .unwrap();
                assert!(!pushed.behind);
                if pushed.exhausted {
                    exhausted = true;
                    break;
                }
            }
            if !exhausted {
                for scope in reader.existence_scopes().await.unwrap() {
                    session.sync_existence(reader, scope).await.unwrap();
                }
            }
            session.close().await.unwrap();
            (changed, exhausted)
        };
        let (_, (changed, exhausted)) = tokio::join!(server, client);
        visited.extend(changed);
        if !exhausted {
            println!("P09 sync_sessions={attempt}");
            return scopes
                .iter()
                .copied()
                .filter(|scope| visited.contains(scope))
                .collect();
        }
        println!("P09 session={attempt} exhausted; resuming");
    }
    panic!("SDK sync session budget exhausted")
}

async fn assert_rows(replica: &EncryptedReplica<TypedRows>, names: &[&str], partitions: i64) {
    let inner = replica.inner.lock().await;
    for name in names {
        let rows = crate::storage::rows(
            &inner.conn,
            &format!("SELECT count(*) FROM {name} WHERE row_kind=0"),
        )
        .await
        .unwrap();
        assert_eq!(rows[0][0], SqlValue::Integer(partitions), "{name}");
    }
}

/// Samples this process while a stage runs, when `ROCKY_PROFILE_DIR` names
/// a directory: macOS `sample` writes `<stage>.txt` there when the stage ends.
struct Profile(Option<std::process::Child>);
fn profile(stage: &str) -> Profile {
    let Ok(directory) = std::env::var("ROCKY_PROFILE_DIR") else {
        return Profile(None);
    };
    let file = Path::new(&directory).join(format!("{stage}.txt"));
    let sampler = std::process::Command::new("sample")
        .args([&std::process::id().to_string(), "3600", "-mayDie", "-file"])
        .arg(file)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    // `sample` attaches before it samples.
    std::thread::sleep(Duration::from_millis(500));
    Profile(Some(sampler))
}
impl Drop for Profile {
    fn drop(&mut self) {
        if let Some(mut sampler) = self.0.take() {
            let _ = std::process::Command::new("kill")
                .args(["-INT", &sampler.id().to_string()])
                .status();
            let _ = sampler.wait();
        }
    }
}

/// Bytes of the frames a catch-up received: headers, encrypted payloads,
/// and the credential chains stored once beside them.
async fn frame_bytes(replica: &EncryptedReplica<TypedRows>, tables: usize) {
    let inner = replica.inner.lock().await;
    let stored = crate::storage::rows(&inner.conn, "SELECT header,body FROM secure_operations")
        .await
        .unwrap();
    let (mut header, mut chain, mut body) = (0, 0usize, 0);
    for record in &stored {
        let [SqlValue::Blob(head), SqlValue::Blob(payload)] = record.as_slice() else {
            panic!("stored operation")
        };
        header += head.len();
        body += payload.len();
    }
    // Chains are stored once, beside the operations that name them.
    for record in crate::storage::rows(&inner.conn, "SELECT LENGTH(chain) FROM secure_chains")
        .await
        .unwrap()
    {
        let [SqlValue::Integer(bytes)] = record.as_slice() else {
            panic!("stored chain")
        };
        chain += *bytes as usize;
    }
    println!(
        "P03 frame_bytes tables={tables} frames={} header={header} chain={chain} payload={body}",
        stored.len()
    );
}

struct Measurement {
    open: Duration,
    register: Duration,
    author: Duration,
    catch_up: Duration,
    idle: Duration,
    checkpoint: Duration,
    reopen: Duration,
    metadata: f64,
}

async fn measure(tables: usize, partitions: i64) -> Measurement {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::new(tables);
    let names: Vec<_> = ["customers", "orders", "order_lines", "attachments"]
        .into_iter()
        .take(tables)
        .collect();
    let path = root.path().join("writer.sqlite");
    let node = PrivateKey::new();
    let started = Instant::now();
    let writer = f
        .open(
            &path,
            node.clone(),
            f.grants(
                None,
                &[
                    Right::Read,
                    Right::Insert,
                    Right::Update,
                    Right::Delete,
                    Right::Custody,
                ],
            ),
            Some((PrivateKey::new(), 1)),
            vec![],
        )
        .await;
    let open = started.elapsed();
    writer
        .install_key(KeyMaterial::new(f.scope("products", 0), 1, [8; 32]))
        .await
        .unwrap();
    let empty = database_bytes(&writer).await;
    let sampled = profile(&format!("register_author_{tables}"));
    let started = Instant::now();
    for partition in 1..=partitions {
        f.keys(&writer, partition, &names).await;
    }
    let register = started.elapsed();
    let metadata = (database_bytes(&writer).await - empty) as f64 / partitions as f64;
    let started = Instant::now();
    writer
        .author_bundle([0; 32], vec![f.write("products", 0)], vec![])
        .await
        .unwrap();
    for partition in 1..=partitions {
        writer
            .author_bundle(
                rocky_core::commitment("erp-bundle", &partition.to_be_bytes()),
                names.iter().map(|name| f.write(name, partition)).collect(),
                vec![],
            )
            .await
            .unwrap();
    }
    writer.flush().await.unwrap();
    let author = started.elapsed();
    drop(sampled);
    let reader = f
        .open(
            &root.path().join("reader.sqlite"),
            PrivateKey::new(),
            f.grants(
                None,
                &[
                    Right::Read,
                    Right::Insert,
                    Right::Update,
                    Right::Delete,
                    Right::Custody,
                ],
            ),
            None,
            vec![],
        )
        .await;
    reader
        .install_key(KeyMaterial::new(f.scope("products", 0), 1, [8; 32]))
        .await
        .unwrap();
    for partition in 1..=partitions {
        f.keys(&reader, partition, &names).await;
    }
    let scopes: Vec<_> = std::iter::once(f.scope("products", 0))
        .chain(
            (1..=partitions)
                .flat_map(|partition| names.iter().map(move |name| (name, partition)))
                .map(|(name, partition)| f.scope(name, partition)),
        )
        .collect();
    let sampled = profile(&format!("catch_up_{tables}"));
    let started = Instant::now();
    assert_eq!(sync(&writer, &reader, &scopes).await.len(), scopes.len());
    let catch_up = started.elapsed();
    drop(sampled);
    let started = Instant::now();
    assert!(sync(&writer, &reader, &scopes).await.is_empty());
    let idle = started.elapsed();
    println!(
        "P09 sync_stage tables={tables} catch_up_ms={} idle_ms={}",
        catch_up.as_millis(),
        idle.as_millis()
    );
    assert_rows(&reader, &names, partitions).await;
    frame_bytes(&reader, tables).await;
    let sampled = profile(&format!("checkpoint_{tables}"));
    let started = Instant::now();
    // One seal; compacting installs every scope's base there.
    seal_and_compact(&writer, &f.publisher, f.domain).await;
    let mut checkpoints = Vec::new();
    for scope in &scopes {
        checkpoints.push(writer.checkpoint(*scope).await.unwrap());
    }
    let checkpoint = started.elapsed();
    drop(sampled);
    // Each record carries the signer's whole chain.
    let chain = crate::encode(&writer.inner.lock().await.config.credentials)
        .unwrap()
        .len();
    let record: usize = checkpoints
        .iter()
        .map(|signed| signed.to_bytes().unwrap().len())
        .sum();
    println!(
        "P03 checkpoint_bytes tables={tables} records={} bytes={record} chain_bytes={}",
        checkpoints.len(),
        chain * checkpoints.len()
    );
    drop(writer);
    let started = Instant::now();
    let keys = std::iter::once(f.key("products", 0))
        .chain(
            (1..=partitions)
                .flat_map(|partition| names.iter().map(move |name| (name, partition)))
                .map(|(name, partition)| f.key(name, partition)),
        )
        .collect();
    let reopened = f
        .open(
            &path,
            node,
            f.grants(
                None,
                &[
                    Right::Read,
                    Right::Insert,
                    Right::Update,
                    Right::Delete,
                    Right::Custody,
                ],
            ),
            None,
            keys,
        )
        .await;
    let reopen = started.elapsed();
    assert_rows(&reopened, &names, partitions).await;
    let retained = crate::storage::rows(
        &reopened.inner.lock().await.conn,
        "SELECT scope,count(*) FROM secure_operations GROUP BY scope",
    )
    .await
    .unwrap();
    let started_audit = Instant::now();
    let reports = reopened.audit_scopes(&scopes).await.unwrap();
    println!(
        "P37 audit scopes={} elapsed_ms={}",
        reports.len(),
        started_audit.elapsed().as_millis()
    );
    assert_eq!(reports.len(), scopes.len());
    for (((scope, report), expected_scope), checkpoint) in
        reports.iter().zip(&scopes).zip(&checkpoints)
    {
        assert_eq!(scope, expected_scope);
        let count = retained
            .iter()
            .find_map(|row| match row.as_slice() {
                [SqlValue::Blob(stored), SqlValue::Integer(count)]
                    if *stored == crate::encode(scope).unwrap() =>
                {
                    Some(*count as u64)
                }
                _ => None,
            })
            .unwrap_or(0);
        assert_eq!(
            count, 1,
            "actual authored and retained history of {scope:?}"
        );
        assert_eq!(report.operations, count);
        assert_eq!(report.rows, 1);
        assert_eq!(report.checkpoint, Some(checkpoint.id().unwrap()));
        assert_eq!(report.replayed, 0);
    }
    let result = Measurement {
        open,
        register,
        author,
        catch_up,
        idle,
        checkpoint,
        reopen,
        metadata,
    };
    println!(
        "P09 partitions={partitions} tables={tables} scopes={} open_ms={} register_ms={} author_ms={} catch_up_ms={} idle_ms={} checkpoint_ms={} reopen_ms={} metadata_bytes_per_partition={metadata:.2}",
        scopes.len(),
        open.as_millis(),
        register.as_millis(),
        author.as_millis(),
        catch_up.as_millis(),
        idle.as_millis(),
        checkpoint.as_millis(),
        reopen.as_millis()
    );
    result
}

#[tokio::test]
async fn erp_group_sync_preserves_all_references_and_reopens() {
    measure(4, 2).await;
}

#[tokio::test]
#[ignore = "phase 9 measurement; run with --ignored --nocapture"]
async fn measure_200_partition_group_sync() {
    let one = measure(1, 200).await;
    let four = measure(4, 200).await;
    for (name, one, four) in [
        ("open", one.open, four.open),
        ("register", one.register, four.register),
        ("author", one.author, four.author),
        ("catch_up", one.catch_up, four.catch_up),
        ("idle", one.idle, four.idle),
        ("checkpoint", one.checkpoint, four.checkpoint),
        ("reopen", one.reopen, four.reopen),
    ] {
        println!(
            "P09 additional_table {name}_ms={:.2}",
            (four.as_secs_f64() - one.as_secs_f64()) * 1000.0 / 3.0
        );
    }
    println!(
        "P09 additional_table metadata_bytes_per_partition={:.2}",
        (four.metadata - one.metadata) / 3.0
    );
}

/// Full-size Phase 10 gate: every idle frontier carries session-bound proofs.
#[tokio::test]
#[ignore = "full-size e2e holding-proof qualification"]
async fn holdings_idle_200_partition_four_table_fixture() {
    let result = measure(4, 200).await;
    println!("P10 four_table_idle_ms={}", result.idle.as_millis());
    assert!(
        result.idle < Duration::from_secs(10),
        "idle with proofs: {:?}",
        result.idle
    );
}
