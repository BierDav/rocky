//! Phase 1 gate: strong commands beside incoming replication on two domains
//! that share one file writer. Quotas are untouched, so the operation count comes
//! from several rounds on fresh files.
use super::{CALLER, Failure, NODE, futures, installation, line, scope};
use inventory_app::InventoryApp;
use rocky_core::*;
use rocky_runtime::{
    CommandHandle, DomainInstallation, Durability, Error, Mutation, Runtime, RuntimeConfig,
    RuntimeOptions,
};
use std::time::{Duration, Instant};

const PEER: NodeId = NodeId([4; 16]);
const ROUNDS: usize = 3;
const INCOMING: usize = 1700;
const COMMANDS: usize = 40;

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
type Timings = (Vec<Duration>, Vec<Duration>);
async fn round(options: RuntimeOptions) -> Result<Timings, Failure> {
    let dir = tempfile::tempdir()?;
    // The author only supplies operations; its own flushes are not under test.
    let author = RuntimeOptions::default();
    let source = open(dir.path().join("author.db"), PEER, author).await?;
    let runtime = open(dir.path().join("receiver.db"), NODE, options).await?;
    let coordinator = rocky_coordinator::MemoryCoordinator::default();
    for domain in [1, 2] {
        coordinator.register(scope(domain), CALLER, [6; 16], vec![NODE], 1, 1)?;
        coordinator.acquire(scope(domain), CALLER, NODE)?;
    }
    // Tasks 0 and 1 replicate into the two domains; tasks 2 and 3 run commands.
    let results = futures(4, |task| {
        let domain = (task % 2) as u8 + 1;
        let (source, runtime, coordinator) = (&source, &runtime, &coordinator);
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
            } else {
                for index in 0..COMMANDS {
                    let started = Instant::now();
                    let mut handle = [domain; 32];
                    handle[..8].copy_from_slice(&(index as u64).to_le_bytes());
                    replica
                        .submit_command_with_arguments(
                            CommandHandle(handle),
                            inventory_app::RESERVE,
                            1u64.to_le_bytes().to_vec(),
                            [9; 32],
                        )?
                        .committed()
                        .await?;
                    replica.execute_pending(coordinator).await?;
                    timings.push(started.elapsed());
                }
            }
            Ok::<_, Error>(timings)
        }
    })
    .await;
    let (mut incoming, mut commands) = (Vec::new(), Vec::new());
    for (task, result) in results.into_iter().enumerate() {
        if task < 2 {
            incoming.extend(result?);
        } else {
            commands.extend(result?);
        }
    }
    for domain in [1, 2] {
        let replica = runtime.domain(scope(domain))?;
        let applied = replica.query().await?.rows.len();
        let confirmed = replica.strong_status().await?.sequence;
        if (applied, confirmed) != (INCOMING, COMMANDS as u64) {
            return Err(
                format!("domain {domain}: {applied} applied, {confirmed} confirmed").into(),
            );
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
    Ok((incoming, commands))
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
        let (mut incoming, mut commands) = (Vec::new(), Vec::new());
        let mut failure = None;
        for _ in 0..ROUNDS {
            match round(options).await {
                Ok((received, confirmed)) => {
                    incoming.extend(received);
                    commands.extend(confirmed);
                }
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        let elapsed = started.elapsed();
        let total = incoming.len() + commands.len();
        if !incoming.is_empty() {
            line(&format!("{label} incoming"), incoming, elapsed);
        }
        if !commands.is_empty() {
            line(&format!("{label} strong"), commands, elapsed);
        }
        match failure {
            None => println!("{label}: {total} operations, no storage error"),
            Some(error) => println!("{label}: failed after {total} operations: {error}"),
        }
    }
    Ok(())
}
