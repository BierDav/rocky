//! Incoming replication on two domains
//! that share one file writer. Quotas are untouched, so the operation count comes
//! from several rounds on fresh files.
use super::{CALLER, Failure, NODE, futures, installation, line, scope};
use inventory_app::InventoryApp;
use rocky_core::*;
use rocky_runtime::{
    DomainInstallation, Durability, Error, Mutation, Runtime, RuntimeConfig, RuntimeOptions,
};
use std::time::{Duration, Instant};

const PEER: NodeId = NodeId([4; 16]);
const ROUNDS: usize = 3;
const INCOMING: usize = 1700;

async fn open(
    path: std::path::PathBuf,
    node: NodeId,
    options: RuntimeOptions,
) -> Result<Runtime<InventoryApp>, Error> {
    let installations = [1, 2]
        .into_iter()
        .map(|domain| {
            let wide = installation(2, domain);
            DomainInstallation {
                app: InventoryApp,
                policy: wide.policy,
                identity: rocky_runtime::Identity {
                    node,
                    ..wide.identity
                },
                config: RuntimeConfig {
                    authors: vec![(NODE, CALLER), (PEER, CALLER)],
                    delivery_targets: Vec::new(),
                    full_nodes: Vec::new(),
                },
            }
        })
        .collect();
    Runtime::open_with_options(path, installations, options).await
}
type Timings = Vec<Duration>;
async fn round(options: RuntimeOptions) -> Result<Timings, Failure> {
    let dir = tempfile::tempdir()?;
    // The author only supplies operations; its own flushes are not under test.
    let author = RuntimeOptions::default();
    let source = open(dir.path().join("author.db"), PEER, author).await?;
    let runtime = open(dir.path().join("receiver.db"), NODE, options).await?;
    let results = futures(2, |task| {
        let domain = (task % 2) as u8 + 1;
        let (source, runtime) = (&source, &runtime);
        async move {
            let replica = runtime.domain(scope(domain))?;
            let mut timings = Vec::new();
            if task < 2 {
                let from = source.domain(scope(domain))?;
                for index in 0..INCOMING {
                    let operation = from
                        .mutate(Mutation {
                            entity: None,
                            payload: format!("{domain}-{index}").into_bytes(),
                        })?
                        .committed()
                        .await?;
                    let started = Instant::now();
                    replica.receive(operation.encode()?).await?;
                    if index % 32 == 31 {
                        replica.process_pending().await?;
                    }
                    timings.push(started.elapsed());
                }
                replica.process_pending().await?;
            }
            Ok::<_, Error>(timings)
        }
    })
    .await;
    let mut incoming = Vec::new();
    for result in results {
        incoming.extend(result?);
    }
    for domain in [1, 2] {
        let replica = runtime.domain(scope(domain))?;
        let applied = replica.query().await?.rows.len();
        if applied != INCOMING {
            return Err(format!("domain {domain}: {applied} applied").into());
        }
    }
    runtime.close().await?;
    source.close().await?;
    // Reopen replays history against counters and projections.
    open(
        dir.path().join("receiver.db"),
        NODE,
        RuntimeOptions::default(),
    )
    .await?
    .close()
    .await?;
    Ok(incoming)
}
pub async fn run() -> Result<(), Failure> {
    for (profile, durability) in [
        ("buffered", Durability::Buffered),
        ("full", Durability::Full),
    ] {
        let options = RuntimeOptions {
            durability,
            ..RuntimeOptions::default()
        };
        let label = format!("hardening {profile} domains=2");
        let started = Instant::now();
        let mut incoming = Vec::new();
        let mut failure = None;
        for _ in 0..ROUNDS {
            match round(options).await {
                Ok(received) => incoming.extend(received),
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        let elapsed = started.elapsed();
        let total = incoming.len();
        if !incoming.is_empty() {
            line(&format!("{label} incoming"), incoming, elapsed);
        }
        match failure {
            None => println!("{label}: {total} operations, no storage error"),
            Some(error) => println!("{label}: failed after {total} operations: {error}"),
        }
    }
    Ok(())
}
