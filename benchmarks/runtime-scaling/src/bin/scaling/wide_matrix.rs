//! Phase 6 install/open and steady-state sweep. Files are retained for inspection.
use super::{
    CALLER, Failure, NODE, line, resident_mib, wide,
    wide_schema::{Policy, WideSchema, domain_id},
};
use rocky_core::*;
use rocky_runtime::*;
use std::{path::PathBuf, time::Instant};

fn installations(app: WideSchema, count: u16) -> Vec<DomainInstallation<WideSchema>> {
    (1..=count)
        .map(|domain| {
            let scope = Scope {
                domain: domain_id(domain),
                ..wide::scope()
            };
            DomainInstallation {
                app,
                policy: Policy {
                    scope,
                    caller: CALLER,
                },
                identity: Identity {
                    scope,
                    node: NODE,
                    caller: CALLER,
                },
                config: RuntimeConfig {
                    authors: vec![(NODE, CALLER)],
                    delivery_targets: vec![],
                    full_nodes: Vec::new(),
                },
            }
        })
        .collect()
}
pub async fn run(
    directory: PathBuf,
    table_filter: Option<u16>,
    domain_filter: Option<u16>,
) -> Result<(), Failure> {
    if table_filter.is_some_and(|tables| ![10, 100, 500, 2000].contains(&tables))
        || domain_filter.is_some_and(|domains| ![1, 8, 64].contains(&domains))
    {
        return Err("wide-matrix supports tables 10/100/500/2000 and domains 1/8/64".into());
    }
    std::fs::create_dir_all(&directory)?;
    for tables in [10, 100, 500, 2000]
        .into_iter()
        .filter(|tables| table_filter.is_none_or(|selected| selected == *tables))
    {
        for domains in [1, 8, 64]
            .into_iter()
            .filter(|domains| domain_filter.is_none_or(|selected| selected == *domains))
        {
            let app = WideSchema::new(tables).with_domains(domains);
            let start = Instant::now();
            let manifest = app.manifest().ok_or("missing manifest")?;
            manifest.validate()?;
            let validate = start.elapsed().as_micros();
            let start = Instant::now();
            let hash = manifest.hash();
            std::hint::black_box(hash);
            let hashing = start.elapsed().as_micros();
            let start = Instant::now();
            std::hint::black_box(installed_definition(&app));
            let fingerprint = start.elapsed().as_micros();
            println!(
                "tables={tables} domains={domains} manifest_bytes={} validate_us={validate} hash_us={hashing} fingerprint_us={fingerprint}",
                manifest.canonical_body().len()
            );
            let path = directory.join(format!("t{tables}-d{domains}.db"));
            let files = if domains == 64 {
                if tables == 2000 { 32 } else { 16 }
            } else if tables == 2000 && domains == 8 {
                4
            } else {
                1
            };
            let options = RuntimeOptions {
                files,
                ..RuntimeOptions::default()
            };
            let start = Instant::now();
            let runtime = match Runtime::open_placed(
                &path,
                installations(app, domains),
                (1..=domains)
                    .map(|domain| DomainPlacement {
                        scope: Scope {
                            domain: domain_id(domain),
                            ..wide::scope()
                        },
                        file: (domain as usize - 1) % files,
                    })
                    .collect(),
                options,
            )
            .await
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    println!("tables={tables} domains={domains} files={files} refused: {error}");
                    continue;
                }
            };
            let install = start.elapsed();
            println!(
                "tables={tables} domains={domains} files={files} installed_ms={}",
                install.as_millis()
            );
            let replica = runtime.domain(wide::scope())?;
            let operation = wide::commit(
                &replica,
                None,
                WideSchema::create(tables - 1, 1, 1, "probe"),
            )
            .await?;
            for index in 0..100 {
                wide::commit(
                    &replica,
                    Some(operation.id),
                    WideSchema::set_a(tables - 1, index),
                )
                .await?;
            }
            let start = Instant::now();
            let mut writes = Vec::new();
            for index in 0..1000 {
                let step = Instant::now();
                wide::commit(
                    &replica,
                    Some(operation.id),
                    WideSchema::set_a(tables - 1, index),
                )
                .await?;
                writes.push(step.elapsed());
            }
            line(
                &format!("tables={tables} domains={domains} warm-write"),
                writes,
                start.elapsed(),
            );
            let write = start.elapsed().as_micros() / 1000;
            let query = BoundQuery {
                sql: format!("SELECT a FROM t{} WHERE node=? AND sequence=?", tables - 1),
                values: vec![
                    SqlValue::Blob(NODE.0.to_vec()),
                    SqlValue::Integer(operation.id.sequence.get() as i64),
                ],
            };
            let start = Instant::now();
            for _ in 0..8 {
                replica
                    .read(query.clone(), ReadOptions::new(wide::scope(), 1))
                    .await?;
            }
            println!(
                "tables={tables} domains={domains} reader_warmup_us={}",
                start.elapsed().as_micros()
            );
            let start = Instant::now();
            let mut reads = Vec::new();
            for _ in 0..1000 {
                let step = Instant::now();
                replica
                    .read(query.clone(), ReadOptions::new(wide::scope(), 1))
                    .await?;
                reads.push(step.elapsed());
            }
            line(
                &format!("tables={tables} domains={domains} warm-read"),
                reads,
                start.elapsed(),
            );
            let read = start.elapsed().as_micros() / 1000;
            let memory = resident_mib();
            runtime.close().await?;
            let start = Instant::now();
            let runtime = Runtime::open_placed(
                &path,
                installations(app, domains),
                (1..=domains)
                    .map(|domain| DomainPlacement {
                        scope: Scope {
                            domain: domain_id(domain),
                            ..wide::scope()
                        },
                        file: (domain as usize - 1) % files,
                    })
                    .collect(),
                options,
            )
            .await?;
            println!(
                "tables={tables} domains={domains} files={files} install_ms={} open_ms={} write_us={write} read_us={read} resident_mib={memory}",
                install.as_millis(),
                start.elapsed().as_millis()
            );
            runtime.close().await?;
        }
    }
    Ok(())
}
