//! The fixed seed set of the integration tier, the e2e sweep and replay.
//!
//! Replay one seed with its steps:
//! `ROCKY_SIM_SEED=<seed> cargo test -p rocky-sdk --lib simulation::tests::replay -- --ignored --nocapture`
use super::{
    driver::{self, Report},
    schedule::Scenario,
};
use crate::Failure;
use std::sync::Arc;

/// The integration tier's size: inside its 2 s per-test budget.
const INTEGRATION: Scenario = Scenario {
    nodes: 3,
    steps: 24,
    rounds: 4,
};
/// The sweep's size for `seed`: three or four nodes. The integration seeds
/// already cover the longer scenario; the e2e sweep adds independent seeds.
fn sweep(seed: u64) -> Scenario {
    Scenario {
        nodes: 3 + (seed % 2) as usize,
        steps: 24,
        rounds: 4,
    }
}
/// Seeds that do not converge, with the classes of what blocks them.
/// Recovery drives itself, so it stays empty; safety failures are never
/// listed.
const KNOWN_LIVENESS: &[(u64, &[&str])] = &[];

async fn simulate(seed: u64, scenario: Scenario) -> Result<Report, Failure> {
    tokio::task::LocalSet::new()
        .run_until(driver::run(seed, scenario))
        .await
}
fn classes(report: &Report) -> Vec<String> {
    report
        .liveness
        .iter()
        .flat_map(|stuck| stuck.classes.iter().cloned())
        .collect()
}
/// The seed's entry in `KNOWN_LIVENESS`; empty for one that converges.
fn known(seed: u64) -> Vec<String> {
    KNOWN_LIVENESS
        .iter()
        .find(|(known, _)| *known == seed)
        .map(|(_, classes)| classes.iter().map(|class| class.to_string()).collect())
        .unwrap_or_default()
}
/// Fails on a safety violation, and on a liveness result that differs from
/// the seed's entry in `KNOWN_LIVENESS`, so the list stays exact.
async fn integration(seed: u64) -> Result<(), Failure> {
    let report = simulate(seed, INTEGRATION).await?;
    assert!(report.safety.is_none(), "{}", report.dump());
    assert_eq!(
        classes(&report),
        known(seed),
        "liveness changed for {}",
        report.dump()
    );
    Ok(())
}
macro_rules! seeds {
    ($($name:ident = $seed:expr),* $(,)?) => {$(
        #[tokio::test(start_paused = true)]
        async fn $name() -> Result<(), Failure> {
            integration($seed).await
        }
    )*};
}
seeds!(
    seed_1 = 1,
    seed_2 = 2,
    seed_3 = 3,
    seed_4 = 4,
    seed_5 = 5,
    seed_6 = 6,
    seed_7 = 7,
    seed_8 = 8,
);

/// Runs `ROCKY_SIM_SEED` at the sweep's size and prints its steps.
#[tokio::test(start_paused = true)]
#[ignore = "replays one seed on request"]
async fn replay() -> Result<(), Failure> {
    let seed = std::env::var("ROCKY_SIM_SEED")?.parse()?;
    let report = simulate(seed, sweep(seed)).await?;
    println!("{}", report.dump());
    assert!(report.safety.is_none());
    Ok(())
}

/// How many seeds the e2e sweep runs, and where it starts; the shards
/// split them.
#[cfg(feature = "iroh")]
fn sweep_range() -> std::ops::Range<u64> {
    let read = |name: &str, default: u64| {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    };
    // The full sweep is for exploratory validation. Keep the correctness tier
    // representative but bounded; larger sweeps remain opt-in through the
    // environment when investigating a seed range.
    let first = read("ROCKY_SIM_FIRST", 10_000);
    first..first + read("ROCKY_SIM_SEEDS", 8)
}
#[cfg(feature = "iroh")]
const SHARDS: u64 = 16;
/// One shard of the sweep: fails on any safety violation and on a seed
/// whose healing result differs from `KNOWN_LIVENESS`, listing each with
/// what blocks it.
#[cfg(feature = "iroh")]
async fn shard(shard: u64) -> Result<(), Failure> {
    let mut unsafe_seeds = Vec::new();
    let mut stuck = std::collections::BTreeMap::<Vec<String>, Vec<u64>>::new();
    let (mut ran, mut scope_local) = (0, Vec::new());
    let started = std::time::Instant::now();
    for seed in sweep_range().filter(|seed| seed % SHARDS == shard) {
        let report = simulate(seed, sweep(seed)).await?;
        ran += 1;
        if report.scope_local > 0 {
            scope_local.push((seed, report.scope_local));
        }
        if report.safety.is_some() {
            println!("{}", report.dump());
            unsafe_seeds.push(seed);
        } else if classes(&report) != known(seed) {
            stuck.entry(classes(&report)).or_default().push(seed);
        }
    }
    println!("shard {shard}: {ran} seeds in {:?}", started.elapsed());
    println!("  writes a scope-local gate accepted (seed, writes): {scope_local:?}");
    for (classes, seeds) in &stuck {
        println!("  {} not converged {classes:?}: {seeds:?}", seeds.len());
    }
    assert!(
        unsafe_seeds.is_empty(),
        "safety failed for seeds {unsafe_seeds:?}"
    );
    assert!(stuck.is_empty(), "liveness changed for seeds {stuck:?}");
    Ok(())
}
macro_rules! shards {
    ($($name:ident = $shard:expr),* $(,)?) => {$(
        // Once, in the workspace build: the build without iroh runs the
        // same simulation.
        #[cfg(feature = "iroh")]
        #[tokio::test(start_paused = true)]
        #[ignore = "the e2e sweep"]
        async fn $name() -> Result<(), Failure> {
            shard($shard).await
        }
    )*};
}
shards!(
    sweep_0 = 0,
    sweep_1 = 1,
    sweep_2 = 2,
    sweep_3 = 3,
    sweep_4 = 4,
    sweep_5 = 5,
    sweep_6 = 6,
    sweep_7 = 7,
    sweep_8 = 8,
    sweep_9 = 9,
    sweep_10 = 10,
    sweep_11 = 11,
    sweep_12 = 12,
    sweep_13 = 13,
    sweep_14 = 14,
    sweep_15 = 15,
);

/// Twelve streams of fixed bytes through every kind of seeded fault: what
/// crossed and what the hub injected, in order.
async fn hub_transcript(seed: u64) -> Result<(Vec<u8>, Vec<String>), Failure> {
    use crate::transport::{
        Connector, Protocol,
        memory::{Hub, LinkFaults},
    };
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let hub = Arc::new(Hub::seeded(seed));
    hub.record("x");
    hub.set_faults(
        "x",
        "y",
        LinkFaults {
            latency: Duration::from_millis(20),
            jitter: Duration::from_millis(30),
            bandwidth: Some(64),
            stalls: 100,
            stall: Duration::from_secs(3),
            cuts: 300,
            cut_within: 6_000,
            refusals: 200,
        },
    );
    let x = hub.connector("x").start(false, [1; 32]).await?;
    let y = hub.connector("y").start(true, [2; 32]).await?;
    y.listen(true);
    let echo = tokio::task::spawn_local(async move {
        while let Some((_, mut stream)) = y.accept().await {
            tokio::task::spawn_local(async move {
                let mut buffer = [0; 1024];
                while let Ok(read) = stream.read(&mut buffer).await {
                    if read == 0 || stream.write_all(&buffer[..read]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    for stream in 0..12u8 {
        if stream == 6 {
            hub.set_down("x", "y", true);
            assert!(x.connect("y", Protocol::Link).await.is_err());
            tokio::time::sleep(Duration::from_secs(1)).await;
            hub.set_down("x", "y", false);
        }
        let Ok(link) = x.connect("y", Protocol::Link).await else {
            continue;
        };
        let (mut reader, mut writer) = tokio::io::split(link);
        let payload: Vec<u8> = (0..4_000u32).map(|byte| (byte as u8) ^ stream).collect();
        let mut echoed = vec![0; payload.len()];
        let _ = tokio::join!(writer.write_all(&payload), reader.read_exact(&mut echoed));
    }
    echo.abort();
    Ok((hub.transcript("x"), hub.events()))
}

#[tokio::test(start_paused = true)]
async fn the_same_seed_yields_byte_identical_hub_transcripts() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(async {
            let first = hub_transcript(7).await?;
            assert_eq!(first, hub_transcript(7).await?);
            let other = hub_transcript(8).await?;
            assert_ne!(first.1, other.1, "the seed chose nothing");
            for kind in ["open", "refuse", "cut", "stall", "down", "unreachable"] {
                assert!(
                    first.1.iter().any(|event| event.starts_with(kind)),
                    "no {kind} in {:#?}",
                    first.1
                );
            }
            Ok(())
        })
        .await
}

/// A violation without where it happened or the run's random scope labels.
fn masked(safety: &str) -> String {
    let what = safety.split_once(": ").map_or(safety, |(_, what)| what);
    what.split(' ')
        .map(|word| match word.trim_end_matches(':') {
            label if label.len() == 12 && label.bytes().all(|b| b.is_ascii_hexdigit()) => "*",
            _ => word,
        })
        .collect::<Vec<_>>()
        .join(" ")
}
/// Shrinks the schedule of `ROCKY_SIM_SEED`'s failure, a safety violation
/// or what keeps it from converging: drops steps one at a time while the
/// same failure still happens, then prints it.
#[tokio::test(start_paused = true)]
#[ignore = "shrinks one failing seed on request"]
async fn shrink() -> Result<(), Failure> {
    let seed = std::env::var("ROCKY_SIM_SEED")?.parse()?;
    let scenario = sweep(seed);
    let run = |steps: Vec<super::schedule::Action>| async move {
        let report = tokio::task::LocalSet::new()
            .run_until(driver::run_steps(seed, scenario, steps))
            .await?;
        Ok::<_, Failure>(match &report.safety {
            Some(safety) => Some(masked(safety)),
            None => report
                .liveness
                .as_ref()
                .map(|_| format!("liveness {:?}", classes(&report))),
        })
    };
    let mut steps = super::schedule::plan(seed, scenario);
    let violation = run(steps.clone()).await?.ok_or("the seed converges")?;
    let mut shrunk = true;
    while shrunk {
        shrunk = false;
        for index in (0..steps.len()).rev() {
            let mut fewer = steps.clone();
            fewer.remove(index);
            if run(fewer.clone()).await?.as_ref() == Some(&violation) {
                steps = fewer;
                shrunk = true;
            }
        }
    }
    println!("{violation}");
    for step in &steps {
        println!("  {step}");
    }
    Ok(())
}
