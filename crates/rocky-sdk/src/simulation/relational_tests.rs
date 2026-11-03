//! Seeds of the relational fixture: boards and cards under keys, checks
//! and an assertion, with lifecycle events around the writes, checked at
//! quiescence against the from-scratch reference in `relational`.
//!
//! Replay one seed with its steps, or shrink a failing one:
//! `ROCKY_SIM_SEED=<seed> cargo test -p rocky-sdk --lib simulation::relational_tests::replay -- --ignored --nocapture`
//! `ROCKY_SIM_SEED=<seed> cargo test -p rocky-sdk --lib simulation::relational_tests::shrink -- --ignored --nocapture`
//! Set `ROCKY_SIM_INTEGRATION=1` for an integration seed's size.
//! The soak runs `ROCKY_SIM_SEEDS` seeds (default 200) from `ROCKY_SIM_FIRST`:
//! `ROCKY_SIM_SEEDS=300 cargo test --release -p rocky-sdk --lib simulation::relational_tests::relational_soak -- --ignored --nocapture`
use super::{
    driver::{self, Report},
    schedule::{Action, Fixture, Scenario, plan},
};
use crate::Failure;
use std::collections::BTreeMap;

/// The integration tier's size.
const INTEGRATION: Scenario = Scenario {
    nodes: 3,
    steps: 24,
    rounds: 4,
    fixture: Fixture::Relational,
};
/// The size `replay` and `shrink` use: the integration tier's with
/// `ROCKY_SIM_INTEGRATION` set, the soak's otherwise.
fn requested(seed: u64) -> Scenario {
    match std::env::var_os("ROCKY_SIM_INTEGRATION") {
        Some(_) => INTEGRATION,
        None => soak_size(seed),
    }
}
/// The soak's size for `seed`: three or four nodes, longer schedules.
fn soak_size(seed: u64) -> Scenario {
    Scenario {
        nodes: 3 + (seed % 2) as usize,
        steps: 40,
        rounds: 5,
        fixture: Fixture::Relational,
    }
}

async fn simulate(seed: u64, scenario: Scenario, steps: Vec<Action>) -> Result<Report, Failure> {
    tokio::task::LocalSet::new()
        .run_until(driver::run_steps(seed, scenario, steps))
        .await
}
/// A failure's text, without where it happened: what shrinking keeps.
fn failure(report: &Report) -> Option<String> {
    match (&report.safety, &report.liveness) {
        (Some(safety), _) => Some(
            safety
                .split_once(": ")
                .map_or(safety.as_str(), |(_, what)| what)
                .split(' ')
                .take(3)
                .collect::<Vec<_>>()
                .join(" "),
        ),
        (None, Some(stuck)) => Some(format!("liveness {:?}", stuck.classes)),
        (None, None) => None,
    }
}
/// Fails with the seed and its steps on any safety or liveness failure.
async fn check(seed: u64, scenario: Scenario) -> Result<Report, Failure> {
    let report = simulate(seed, scenario, plan(seed, scenario)).await?;
    assert!(failure(&report).is_none(), "{}", report.dump());
    Ok(report)
}
macro_rules! seeds {
    ($($name:ident = $seed:expr),* $(,)?) => {$(
        #[tokio::test(start_paused = true)]
        async fn $name() -> Result<(), Failure> {
            check($seed, INTEGRATION).await.map(drop)
        }
    )*};
}
seeds!(seed_1 = 1, seed_2 = 2, seed_3 = 3, seed_4 = 4);

#[tokio::test(start_paused = true)]
#[ignore = "replays one seed on request"]
async fn replay() -> Result<(), Failure> {
    let seed = std::env::var("ROCKY_SIM_SEED")?.parse()?;
    let scenario = requested(seed);
    let report = simulate(seed, scenario, plan(seed, scenario)).await?;
    println!("{}\ncoverage {:?}", report.dump(), report.coverage);
    assert!(failure(&report).is_none());
    Ok(())
}

/// Drops steps one at a time while the same failure still happens.
#[tokio::test(start_paused = true)]
#[ignore = "shrinks one failing seed on request"]
async fn shrink() -> Result<(), Failure> {
    let seed = std::env::var("ROCKY_SIM_SEED")?.parse()?;
    let scenario = requested(seed);
    let mut steps = plan(seed, scenario);
    let first = simulate(seed, scenario, steps.clone()).await?;
    let wanted = failure(&first).ok_or("the seed passes")?;
    let mut shrunk = true;
    while shrunk {
        shrunk = false;
        for index in (0..steps.len()).rev() {
            let mut fewer = steps.clone();
            fewer.remove(index);
            let report = simulate(seed, scenario, fewer.clone()).await?;
            if failure(&report).as_ref() == Some(&wanted) {
                steps = fewer;
                shrunk = true;
            }
        }
    }
    let last = simulate(seed, scenario, steps.clone()).await?;
    println!("{wanted}\n{steps:#?}\n{}", last.dump());
    Ok(())
}

/// `ROCKY_SIM_SEEDS` seeds from `ROCKY_SIM_FIRST` (default 20_000):
/// prints every failure with its steps and the summed coverage.
#[tokio::test(start_paused = true)]
#[ignore = "the relational soak"]
async fn relational_soak() -> Result<(), Failure> {
    let read = |name: &str, default: u64| {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    };
    let first = read("ROCKY_SIM_FIRST", 20_000);
    let count = read("ROCKY_SIM_SEEDS", 200);
    let mut failed = BTreeMap::<String, Vec<u64>>::new();
    let mut coverage = BTreeMap::<String, usize>::new();
    let started = std::time::Instant::now();
    for seed in first..first + count {
        let scenario = soak_size(seed);
        let report = simulate(seed, scenario, plan(seed, scenario)).await?;
        for (what, times) in &report.coverage {
            *coverage.entry(what.clone()).or_default() += times;
        }
        if let Some(failure) = failure(&report) {
            println!("{}", report.dump());
            failed.entry(failure).or_default().push(seed);
        }
    }
    println!("{count} seeds from {first} in {:?}", started.elapsed());
    println!("coverage {coverage:#?}");
    for (failure, seeds) in &failed {
        println!("{} failed with {failure}: {seeds:?}", seeds.len());
    }
    assert!(failed.is_empty(), "failed: {failed:?}");
    Ok(())
}

/// Schedules the relational fuzz found and shrank, kept as regressions.
mod regressions {
    use super::super::schedule::{Action::*, Change};
    use super::*;

    async fn holds(seed: u64, steps: Vec<Action>) -> Result<(), Failure> {
        let report = simulate(seed, INTEGRATION, steps).await?;
        assert!(failure(&report).is_none(), "{}", report.dump());
        Ok(())
    }

    /// Seed 2: b's board collides with a's code, so it drops where it
    /// replays after a's seal, and b's card on it drops with it.
    #[tokio::test(start_paused = true)]
    async fn a_card_on_a_board_that_lost_its_code_after_a_seal_audits() -> Result<(), Failure> {
        let board = |node| {
            Change(
                node,
                Change::Board {
                    list: "l0".into(),
                    code: 1,
                    cap: 5,
                },
            )
        };
        holds(
            2,
            vec![
                board(0),
                board(1),
                Change(
                    1,
                    Change::Card {
                        list: "l0".into(),
                        board: 0,
                        pinned: Some(0),
                        name: 1,
                        points: 3,
                    },
                ),
                Seal(0),
            ],
        )
        .await
    }

    /// Seed 20008: b deletes a board, cascading its card, while a seals
    /// without the delete; the delete replays after the seal, a seals it
    /// and compacts. Compaction kept `cards`' base from the first seal,
    /// since the second covers no `cards` member, and signed `boards` at
    /// the second, so a rebuild brought the card back without its board.
    #[tokio::test(start_paused = true)]
    async fn a_cascade_replayed_after_a_seal_survives_the_next_seal_and_compaction()
    -> Result<(), Failure> {
        holds(
            20008,
            vec![
                Change(
                    0,
                    Change::Board {
                        list: "l0".into(),
                        code: 3,
                        cap: 11,
                    },
                ),
                Change(
                    0,
                    Change::Card {
                        list: "l0".into(),
                        board: 0,
                        pinned: None,
                        name: 0,
                        points: 0,
                    },
                ),
                Sync(0),
                Change(
                    1,
                    Change::DropBoard {
                        list: "l0".into(),
                        board: 0,
                    },
                ),
                Seal(0),
                Sync(0),
                Seal(0),
                Compact(0),
            ],
        )
        .await
    }

    /// Seed 20004: a partial member evicts a list right after the owner
    /// sealed a board in it. Its full rebuild restored the evicted scope
    /// from the rows the seal took, which eviction left behind.
    #[tokio::test(start_paused = true)]
    async fn an_eviction_after_a_seal_audits() -> Result<(), Failure> {
        holds(
            20004,
            vec![
                Change(
                    0,
                    Change::Board {
                        list: "l0".into(),
                        code: 1,
                        cap: 3,
                    },
                ),
                Seal(0),
                Evict(1, "l0".into()),
            ],
        )
        .await
    }

    /// Seed 20004, shrunk again once the first fix held: b evicts `l0`
    /// before a's board reaches it. b still finalizes the owner's seals:
    /// it places a's `l0` position like one of a scope it never held.
    #[tokio::test(start_paused = true)]
    async fn an_eviction_before_the_list_arrives_finalizes() -> Result<(), Failure> {
        holds(
            20004,
            vec![
                Change(
                    0,
                    Change::Board {
                        list: "l0".into(),
                        code: 1,
                        cap: 3,
                    },
                ),
                Evict(1, "l0".into()),
            ],
        )
        .await
    }

    /// Seed 20033: b, offline, is shared `l8` and evicts it before the
    /// share reaches it. The eviction failed on the partition tuple b never
    /// learned ("replayed scope") and left the scope evicting; a board a
    /// wrote there later made b's identity ledger differ from its rebuild.
    /// A partition whose tuple never arrived holds no rows to clear.
    #[tokio::test(start_paused = true)]
    async fn evicting_a_list_whose_share_never_arrived_succeeds() -> Result<(), Failure> {
        let report = simulate(
            20033,
            INTEGRATION,
            vec![
                Offline(1),
                Partition("l8".into()),
                Share("l8".into(), 1),
                Evict(1, "l8".into()),
                Seal(0),
            ],
        )
        .await?;
        assert!(failure(&report).is_none(), "{}", report.dump());
        assert_eq!(
            report.coverage.get("evicted"),
            Some(&1),
            "{}",
            report.dump()
        );
        Ok(())
    }

    /// Seed 20042: a writes a board and a card on it in `l1`, and b evicts
    /// `l1`.
    #[tokio::test(start_paused = true)]
    async fn evicting_a_list_with_a_board_and_its_card_audits() -> Result<(), Failure> {
        let report = simulate(
            20042,
            INTEGRATION,
            vec![
                Change(
                    0,
                    Change::Pair {
                        list: "l1".into(),
                        code: 0,
                        name: 3,
                        points: 1,
                    },
                ),
                Evict(1, "l1".into()),
            ],
        )
        .await?;
        assert!(failure(&report).is_none(), "{}", report.dump());
        assert_eq!(
            report.coverage.get("evicted"),
            Some(&1),
            "{}",
            report.dump()
        );
        Ok(())
    }

    /// Seed 20064: b evicts `l1` after a sealed a board and its card there,
    /// then holds it again. Its seal bounds outlived the eviction, so it
    /// placed the sealed transaction after the seal: era 1, not 0.
    #[tokio::test(start_paused = true)]
    async fn a_list_held_again_after_eviction_keeps_sealed_eras() -> Result<(), Failure> {
        holds(
            20064,
            vec![
                Change(
                    0,
                    Change::Pair {
                        list: "l1".into(),
                        code: 2,
                        name: 4,
                        points: 5,
                    },
                ),
                Seal(0),
                Evict(1, "l1".into()),
                Hold(1, "l1".into()),
            ],
        )
        .await
    }

    /// Seed 20000: c joins holding only `l1` after the owner wrote in `l0`.
    /// Open: c never finalizes the final seal, waiting for "missing frames
    /// of author 1 through the seal's frontier", the owner's `l0` frames
    /// c holds no key for.
    #[tokio::test(start_paused = true)]
    #[ignore = "open defect: a member holding one list waits for another's positions"]
    async fn a_member_holding_one_list_finalizes_seals_over_another() -> Result<(), Failure> {
        holds(
            20000,
            vec![
                Change(
                    0,
                    Change::Board {
                        list: "l0".into(),
                        code: 1,
                        cap: 7,
                    },
                ),
                Invite(0, 2),
                Share("l1".into(), 2),
            ],
        )
        .await
    }
}
