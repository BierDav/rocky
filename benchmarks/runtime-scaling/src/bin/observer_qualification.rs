//! Same-file independent-domain observer contention evidence; not an engine benchmark.
use inventory_app::{InventoryApp, Policy};
use rocky_core::{DomainId, MeshId, NodeId, PrincipalId, Scope, SqlValue};
use rocky_runtime::{
    BoundQuery, DomainInstallation, Identity, Mutation, ReadOptions, Replica, Runtime,
    RuntimeConfig,
};
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

const WRITES: usize = 128;
fn scope(domain: u8) -> Scope {
    Scope {
        mesh: MeshId([11; 16]),
        app: inventory_app::app_id(),
        domain: DomainId([domain; 16]),
    }
}
fn query() -> BoundQuery {
    BoundQuery {
        sql: "SELECT COUNT(*) FROM drafts".into(),
        values: vec![],
    }
}
async fn workload(
    replica: Replica<InventoryApp>,
    scope: Scope,
) -> Result<(Vec<u128>, Vec<u128>), rocky_runtime::Error> {
    let starts = Arc::new(Mutex::new(vec![None; WRITES]));
    let mut observer = replica.observe_read(query(), ReadOptions::new(scope, 1))?;
    observer.next().await?;
    let writer = async {
        let mut timings = Vec::new();
        for index in 0..WRITES {
            let started = Instant::now();
            starts.lock().map_err(|_| rocky_runtime::Error::Closed)?[index] = Some(started);
            replica
                .mutate(Mutation {
                    entity: None,
                    payload: format!("draft-{index}").into_bytes(),
                })?
                .committed()
                .await?;
            timings.push(started.elapsed().as_micros());
        }
        Ok::<_, rocky_runtime::Error>(timings)
    };
    let reader = async {
        let mut latencies = Vec::new();
        loop {
            let snapshot = observer.next().await?;
            let Some([SqlValue::Integer(count)]) = snapshot.rows.first().map(Vec::as_slice) else {
                return Err(rocky_runtime::Error::Corrupt("observer count".into()));
            };
            if *count > 0 {
                let start = starts.lock().map_err(|_| rocky_runtime::Error::Closed)?
                    [*count as usize - 1]
                    .ok_or(rocky_runtime::Error::Closed)?;
                latencies.push(start.elapsed().as_micros());
            }
            if *count as usize == WRITES {
                break;
            }
        }
        Ok::<_, rocky_runtime::Error>(latencies)
    };
    let noise = async {
        for _ in 0..WRITES {
            replica.read(query(), ReadOptions::new(scope, 1)).await?;
        }
        Ok::<_, rocky_runtime::Error>(())
    };
    let (writes, observations, ()) = tokio::try_join!(writer, reader, noise)?;
    Ok((writes, observations))
}
fn report(label: &str, mut latencies: Vec<u128>) {
    latencies.sort();
    println!(
        "{label}: n={} p50_us={} p99_us={} max_us={}",
        latencies.len(),
        latencies[latencies.len() / 2],
        latencies[(latencies.len() * 99 / 100).min(latencies.len() - 1)],
        latencies.last().unwrap_or(&0)
    );
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let runtime = Runtime::open(
        dir.path().join("domains.db"),
        [1, 2]
            .into_iter()
            .map(|domain| DomainInstallation {
                app: InventoryApp,
                policy: Policy {
                    scope: scope(domain),
                    caller: PrincipalId([7; 16]),
                    reference: [9; 32],
                    note_prefix: None,
                },
                identity: Identity {
                    scope: scope(domain),
                    node: NodeId([3; 16]),
                    caller: PrincipalId([7; 16]),
                },
                config: RuntimeConfig {
                    authors: vec![(NodeId([3; 16]), PrincipalId([7; 16]))],
                    delivery_targets: vec![],
                    full_nodes: Vec::new(),
                },
            })
            .collect(),
    )
    .await?;
    let started = Instant::now();
    let (one, two) = tokio::try_join!(
        workload(runtime.domain(scope(1))?, scope(1)),
        workload(runtime.domain(scope(2))?, scope(2))
    )?;
    report("domain-1 writes", one.0);
    report("domain-1 admission-to-observer", one.1);
    report("domain-2 writes", two.0);
    report("domain-2 admission-to-observer", two.1);
    println!(
        "same-file domains=2 writes={} noisy_queries={} elapsed_ms={}",
        WRITES * 2,
        WRITES * 2,
        started.elapsed().as_millis()
    );
    runtime.close().await?;
    Ok(())
}
