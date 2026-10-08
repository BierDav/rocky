//! Bounded adapter measurements; run the compiled binary under `/usr/bin/time -l`.
use inventory_app::{InventoryApp, Policy, RESERVE};
use rocky_coordinator::MemoryCoordinator;
use rocky_core::{DomainId, MeshId, NodeId, PrincipalId, Scope};
use rocky_runtime::{
    BoundQuery, CommandHandle, DomainInstallation, Identity, Mutation, ReadOptions, Replica,
    Runtime, RuntimeConfig,
};
use std::time::Instant;

fn scope(domain: u8) -> Scope {
    Scope {
        mesh: MeshId([11; 16]),
        app: inventory_app::app_id(),
        domain: DomainId([domain; 16]),
    }
}
async fn drafts(
    replica: &Replica<InventoryApp>,
    label: &str,
) -> Result<Vec<u128>, rocky_runtime::Error> {
    let mut timings = Vec::new();
    for index in 0..64 {
        let started = Instant::now();
        replica
            .mutate(Mutation {
                entity: None,
                payload: format!("{label}-{index}").into_bytes(),
            })?
            .committed()
            .await?;
        timings.push(started.elapsed().as_micros());
    }
    Ok(timings)
}
async fn commands(
    replica: &Replica<InventoryApp>,
    label: u8,
    coordinator: &MemoryCoordinator,
) -> Result<Vec<u128>, rocky_runtime::Error> {
    let mut timings = Vec::new();
    for index in 0..8 {
        let started = Instant::now();
        let mut handle = [label; 32];
        handle[0] = index;
        replica
            .submit_command_with_arguments(
                CommandHandle(handle),
                RESERVE,
                1u64.to_le_bytes().to_vec(),
                [9; 32],
            )?
            .committed()
            .await?;
        replica.execute_pending(coordinator).await?;
        timings.push(started.elapsed().as_micros());
    }
    Ok(timings)
}
fn report(workload: &str, phase: &str, mut timings: Vec<u128>, elapsed: std::time::Duration) {
    timings.sort();
    println!(
        "{workload} {phase}: n={} throughput={:.1}/s p50_us={} p99_us={}",
        timings.len(),
        timings.len() as f64 / elapsed.as_secs_f64(),
        timings[timings.len() / 2],
        timings[(timings.len() * 99 / 100).min(timings.len() - 1)]
    );
}
async fn workload(independent: bool) -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let caller = PrincipalId([7; 16]);
    let node = NodeId([3; 16]);
    let runtime = Runtime::open(
        directory.path().join("runtime.db"),
        [1, 2]
            .into_iter()
            .map(|domain| DomainInstallation {
                app: InventoryApp,
                policy: Policy {
                    scope: scope(domain),
                    caller,
                    reference: [9; 32],
                    note_prefix: None,
                },
                identity: Identity {
                    scope: scope(domain),
                    node,
                    caller,
                },
                config: RuntimeConfig {
                    authors: vec![(node, caller)],
                    delivery_targets: Vec::new(),
                    full_nodes: Vec::new(),
                },
            })
            .collect(),
    )
    .await?;
    let coordinator = MemoryCoordinator::default();
    for domain in [1, 2] {
        coordinator.register(scope(domain), caller, [6; 16], vec![node], 1, 1)?;
        coordinator.acquire(scope(domain), caller, node)?;
    }
    let a = runtime.domain(scope(1))?;
    let b = runtime.domain(scope(if independent { 2 } else { 1 }))?;
    let name = if independent {
        "independent"
    } else {
        "conflicting"
    };
    let initial_bytes = file_bytes(directory.path())?;
    let started = Instant::now();
    let (left, right) = tokio::join!(drafts(&a, "a"), drafts(&b, "b"));
    report(
        name,
        "mutation",
        [left?, right?].concat(),
        started.elapsed(),
    );
    let started = Instant::now();
    let (left, right) = tokio::join!(commands(&a, 1, &coordinator), commands(&b, 2, &coordinator));
    report(name, "strong", [left?, right?].concat(), started.elapsed());
    let mut reads = Vec::new();
    let started = Instant::now();
    for _ in 0..32 {
        let snapshot_started = Instant::now();
        a.read(
            BoundQuery {
                sql: "SELECT note FROM drafts WHERE note != ? ORDER BY sequence".into(),
                values: vec![rocky_core::SqlValue::Text("absent".into())],
            },
            ReadOptions::new(scope(1), 256),
        )
        .await?;
        reads.push(snapshot_started.elapsed().as_micros());
    }
    report(name, "snapshot", reads, started.elapsed());
    let mut operation_bytes = 0;
    let mut operations = 0;
    for domain in [1, 2] {
        let replica = runtime.domain(scope(domain))?;
        let mut cursor = None;
        loop {
            let page = replica.operation_page(cursor).await?;
            operations += page.items.len();
            operation_bytes += page.items.iter().map(Vec::len).sum::<usize>();
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
        }
    }
    let active_bytes = file_bytes(directory.path())?;
    runtime.close().await?;
    println!(
        "{name} retention: operations={operations} operation_bytes={operation_bytes} initial_file_bytes={initial_bytes} active_file_bytes={active_bytes} closed_file_bytes={}",
        file_bytes(directory.path())?
    );
    Ok(())
}
fn file_bytes(path: &std::path::Path) -> std::io::Result<u64> {
    std::fs::read_dir(path)?.try_fold(0, |sum, entry| Ok(sum + entry?.metadata()?.len()))
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    workload(true).await?;
    workload(false).await
}
