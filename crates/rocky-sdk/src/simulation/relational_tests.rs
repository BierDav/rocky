use super::{
    driver::{self, Report},
    schedule::{Action, Fixture, Invites, Scenario, Wall, plan},
};
use crate::Failure;
use std::collections::BTreeMap;

const INTEGRATION: Scenario = Scenario {
    nodes: 3,
    steps: 24,
    rounds: 4,
    fixture: Fixture::Relational,
    wall: Wall::Moving,
    invites: Invites::Full,
};
fn requested(seed: u64) -> Scenario {
    match std::env::var_os("ROCKY_SIM_INTEGRATION") {
        Some(_) => INTEGRATION,
        None => soak_size(seed),
    }
}
fn soak_size(seed: u64) -> Scenario {
    let wall = match std::env::var("ROCKY_SIM_WALL").as_deref() {
        Ok("frozen") => Wall::Frozen,
        _ => Wall::Moving,
    };
    let invites = match std::env::var_os("ROCKY_SIM_PARTIAL") {
        Some(_) => Invites::Partial,
        None => Invites::Full,
    };
    Scenario {
        invites,
        ..soak_at(seed, wall)
    }
}
fn soak_at(seed: u64, wall: Wall) -> Scenario {
    Scenario {
        nodes: 3 + (seed % 2) as usize,
        steps: 40,
        rounds: 5,
        fixture: Fixture::Relational,
        wall,
        invites: Invites::Full,
    }
}

async fn simulate(seed: u64, scenario: Scenario, steps: Vec<Action>) -> Result<Report, Failure> {
    driver::run_isolated(seed, scenario, steps).await
}
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
async fn check(seed: u64, scenario: Scenario) -> Result<Report, Failure> {
    let report = simulate(seed, scenario, plan(seed, scenario)).await?;
    assert!(failure(&report).is_none(), "{}", report.dump());
    Ok(report)
}
macro_rules! seeds {
    ($($name:ident = $seed:expr, $wall:ident),* $(,)?) => {$(
        #[tokio::test(start_paused = true)]
        async fn $name() -> Result<(), Failure> {
            check($seed, soak_at($seed, Wall::$wall)).await.map(drop)
        }
    )*};
}
seeds!(
    seed_20080 = 20080,
    Moving,
    seed_20060 = 20060,
    Moving,
    seed_20084 = 20084,
    Moving,
    seed_20089 = 20089,
    Moving,
    seed_20091 = 20091,
    Moving,
    seed_20050 = 20050,
    Moving,
    seed_20101 = 20101,
    Moving,
    seed_20080_frozen = 20080,
    Frozen,
);

async fn replays_identically(seed: u64, scenario: Scenario) -> Result<(), Failure> {
    let first = simulate(seed, scenario, plan(seed, scenario)).await?;
    let second = simulate(seed, scenario, plan(seed, scenario)).await?;
    assert!(!first.states.is_empty());
    assert_eq!(first.dump(), second.dump());
    assert_eq!(first.coverage, second.coverage);
    assert_eq!(first.states, second.states, "seed {seed}");
    assert_eq!(first.digest(), second.digest());
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn one_seed_replays_identically() -> Result<(), Failure> {
    replays_identically(3, INTEGRATION).await
}
#[tokio::test(start_paused = true)]
async fn a_soak_seed_replays_identically() -> Result<(), Failure> {
    replays_identically(20245, soak_at(20245, Wall::Moving)).await
}

#[tokio::test(start_paused = true)]
#[ignore = "replays ROCKY_SIM_SEED with its steps, at the integration size with ROCKY_SIM_INTEGRATION=1: ROCKY_SIM_SEED=<seed> cargo test -p rocky-sdk --lib simulation::relational_tests::replay -- --ignored --nocapture"]
async fn replay() -> Result<(), Failure> {
    let seed = std::env::var("ROCKY_SIM_SEED")?.parse()?;
    let scenario = requested(seed);
    let report = simulate(seed, scenario, plan(seed, scenario)).await?;
    println!("{}\ncoverage {:?}", report.dump(), report.coverage);
    assert!(failure(&report).is_none());
    Ok(())
}

#[tokio::test(start_paused = true)]
#[ignore = "shrinks ROCKY_SIM_SEED's failure, at the integration size with ROCKY_SIM_INTEGRATION=1: ROCKY_SIM_SEED=<seed> cargo test -p rocky-sdk --lib simulation::relational_tests::shrink -- --ignored --nocapture"]
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

#[tokio::test(start_paused = true)]
#[ignore = "the relational soak of ROCKY_SIM_SEEDS seeds (200) from ROCKY_SIM_FIRST, ROCKY_SIM_WALL=frozen stopping the wall clock and ROCKY_SIM_PARTIAL=1 drawing per-list invites and grants: ROCKY_SIM_SEEDS=300 cargo test --release -p rocky-sdk --lib simulation::relational_tests::relational_soak -- --ignored --nocapture"]
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
        let steps = plan(seed, scenario);
        let kinds: std::collections::BTreeSet<String> = steps
            .iter()
            .map(|step| {
                format!("{step:?}")
                    .split('(')
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect();
        let report = simulate(seed, scenario, steps).await?;
        for (what, times) in &report.coverage {
            *coverage.entry(what.clone()).or_default() += times;
        }
        println!("seed {seed} replay {}", report.digest());
        println!(
            "seed {seed} covers {kinds:?} {:?}",
            report.coverage.keys().collect::<Vec<_>>()
        );
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

mod regressions {
    use super::super::schedule::{Action::*, Change};
    use super::*;

    async fn holds(seed: u64, steps: Vec<Action>) -> Result<(), Failure> {
        holds_at(seed, INTEGRATION, steps).await
    }
    async fn holds_at(seed: u64, scenario: Scenario, steps: Vec<Action>) -> Result<(), Failure> {
        let report = simulate(seed, scenario, steps).await?;
        assert!(failure(&report).is_none(), "{}", report.dump());
        Ok(())
    }
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

    async fn keeps_outcomes(
        seed: u64,
        scenario: Scenario,
        steps: Vec<Action>,
    ) -> Result<Report, Failure> {
        let report = simulate(seed, scenario, steps).await?;
        assert!(failure(&report).is_none(), "{}", report.dump());
        assert_eq!(
            report.coverage.get("outcome under a base"),
            None,
            "{}",
            report.dump()
        );
        Ok(report)
    }
    #[tokio::test(start_paused = true)]
    async fn a_listed_checkpoint_waits_for_placements() -> Result<(), Failure> {
        keeps_outcomes(
            20013,
            soak_size(20013),
            vec![
                Change(
                    0,
                    Change::Board {
                        list: "l0".into(),
                        code: 1,
                        cap: 8,
                    },
                ),
                Invite(0, 3),
            ],
        )
        .await
        .map(drop)
    }

    #[tokio::test(start_paused = true)]
    async fn a_seal_rebootstrap_waits_for_placements() -> Result<(), Failure> {
        keeps_outcomes(
            20033,
            soak_size(20033),
            vec![
                Change(
                    0,
                    Change::Board {
                        list: "l1".into(),
                        code: 3,
                        cap: 8,
                    },
                ),
                Partition("l8".into()),
                Invite(0, 2),
                Seal(0),
            ],
        )
        .await
        .map(drop)
    }

    #[tokio::test(start_paused = true)]
    async fn another_authors_outcome_may_go_with_a_covering_base() -> Result<(), Failure> {
        let report = simulate(
            20067,
            soak_size(20067),
            vec![
                Invite(0, 2),
                Change(
                    0,
                    Change::Board {
                        list: "l0".into(),
                        code: 3,
                        cap: 9,
                    },
                ),
                Revoke(2),
            ],
        )
        .await?;
        assert!(failure(&report).is_none(), "{}", report.dump());
        assert_eq!(
            report.coverage.get("outcome under a base"),
            Some(&1),
            "{}",
            report.dump()
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn a_member_holding_one_list_finalizes_seals_over_another() -> Result<(), Failure> {
        keeps_outcomes(
            20000,
            INTEGRATION,
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
            ],
        )
        .await
        .map(drop)
    }

    #[tokio::test(start_paused = true)]
    async fn a_superseded_head_of_a_retained_frame_leaves_a_stub() -> Result<(), Failure> {
        holds_at(
            20229,
            soak_size(20229),
            vec![
                Change(
                    1,
                    Change::Pair {
                        list: "l0".into(),
                        code: 0,
                        name: 4,
                        points: 6,
                    },
                ),
                Sync(1),
                Change(
                    0,
                    Change::Recode {
                        list: "l0".into(),
                        board: 5,
                        code: 1,
                    },
                ),
                Change(
                    0,
                    Change::Board {
                        list: "l0".into(),
                        code: 0,
                        cap: 12,
                    },
                ),
                Seal(0),
                Invite(0, 2),
            ],
        )
        .await
    }

    #[tokio::test(start_paused = true)]
    async fn an_outcome_waits_for_its_authors_pending_frames() -> Result<(), Failure> {
        holds_at(
            20290,
            soak_size(20290),
            vec![
                Invite(0, 2),
                Sync(0),
                Seal(0),
                Change(
                    2,
                    Change::Pair {
                        list: "l1".into(),
                        code: 3,
                        name: 1,
                        points: 5,
                    },
                ),
            ],
        )
        .await
    }

    #[tokio::test(start_paused = true)]
    async fn a_head_whose_author_is_revoked_resolves_at_a_peer_that_never_saw_it()
    -> Result<(), Failure> {
        holds_at(
            20276,
            soak_size(20276),
            vec![
                Invite(0, 2),
                Sync(0),
                Restart(2),
                Change(
                    2,
                    Change::Pair {
                        list: "l0".into(),
                        code: 1,
                        name: 0,
                        points: 4,
                    },
                ),
                Sever(0, 2),
                Sync(2),
                Revoke(1),
                Change(
                    2,
                    Change::Board {
                        list: "l0".into(),
                        code: 2,
                        cap: 4,
                    },
                ),
            ],
        )
        .await
    }

    fn a_pair_sealed_and_compacted_in_l0() -> Vec<Action> {
        vec![
            Change(
                0,
                Change::Board {
                    list: "l0".into(),
                    code: 3,
                    cap: 4,
                },
            ),
            Change(
                0,
                Change::Pair {
                    list: "l0".into(),
                    code: 2,
                    name: 0,
                    points: 2,
                },
            ),
            Seal(0),
            Compact(0),
        ]
    }
    #[tokio::test(start_paused = true)]
    async fn a_member_granted_a_compacted_list_replays_it() -> Result<(), Failure> {
        let mut steps = a_pair_sealed_and_compacted_in_l0();
        steps.extend([InviteTo(0, 2, "l1".into()), Grant("l0".into(), 2), Sync(2)]);
        holds_at(178201, soak_at(178201, Wall::Moving), steps).await
    }
    #[tokio::test(start_paused = true)]
    async fn a_member_invited_to_a_compacted_list_replays_it() -> Result<(), Failure> {
        let mut steps = a_pair_sealed_and_compacted_in_l0();
        steps.extend([InviteTo(0, 2, "l0".into()), Sync(2)]);
        holds_at(178201, soak_at(178201, Wall::Moving), steps).await
    }

    #[tokio::test(start_paused = true)]
    async fn a_joiner_rebootstrapping_a_list_publishes_the_outcome_its_peers_reached()
    -> Result<(), Failure> {
        holds_at(
            20311,
            soak_at(20311, Wall::Moving),
            vec![
                Change(
                    0,
                    Change::Raise {
                        list: "l0".into(),
                        card: 5,
                        by: -1,
                    },
                ),
                Sync(1),
                Change(
                    1,
                    Change::Pair {
                        list: "l1".into(),
                        code: 0,
                        name: 1,
                        points: 4,
                    },
                ),
                Seal(0),
                Change(
                    1,
                    Change::Card {
                        list: "l0".into(),
                        board: 1,
                        pinned: Some(3),
                        name: 3,
                        points: 1,
                    },
                ),
                Change(
                    0,
                    Change::Card {
                        list: "l0".into(),
                        board: 6,
                        pinned: None,
                        name: 1,
                        points: 1,
                    },
                ),
                Sync(0),
                Sync(1),
                Invite(0, 2),
                Online(0),
                Sever(2, 0),
                Change(
                    1,
                    Change::DropBoard {
                        list: "l1".into(),
                        board: 7,
                    },
                ),
                Sync(1),
                Change(
                    1,
                    Change::Raise {
                        list: "l1".into(),
                        card: 3,
                        by: -1,
                    },
                ),
                Change(
                    0,
                    Change::DropBoard {
                        list: "l1".into(),
                        board: 3,
                    },
                ),
                Compact(1),
                Invite(0, 3),
                Compact(0),
            ],
        )
        .await
    }

    #[tokio::test(start_paused = true)]
    async fn members_finalize_the_seal_before_a_revocation() -> Result<(), Failure> {
        holds_at(
            20025,
            soak_at(20025, Wall::Moving),
            vec![
                Invite(0, 2),
                Invite(0, 3),
                Partition("l16".into()),
                Seal(0),
                Change(
                    0,
                    Change::Pair {
                        list: "l16".into(),
                        code: 1,
                        name: 0,
                        points: 3,
                    },
                ),
                Sync(0),
                Sync(0),
                Revoke(2),
            ],
        )
        .await
    }

    #[tokio::test(start_paused = true)]
    async fn the_oracle_audits_the_full_replica_of_the_creator() -> Result<(), Failure> {
        let board = Change::Board {
            list: "l0".into(),
            code: 1,
            cap: 5,
        };
        let report = simulate(2, INTEGRATION, vec![Change(0, board)]).await?;
        assert!(failure(&report).is_none(), "{}", report.dump());
        assert_eq!(
            report.coverage.get("lists held"),
            Some(&4),
            "a and b each hold both lists: {}",
            report.dump()
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn a_joiner_writing_while_it_imports_two_lists_does_not_stall() -> Result<(), Failure> {
        let board = |node, list: &str, code, cap| {
            Change(
                node,
                Change::Board {
                    list: list.into(),
                    code,
                    cap,
                },
            )
        };
        holds_at(
            20050,
            soak_at(20050, Wall::Moving),
            vec![
                board(0, "l0", 0, 12),
                Sync(0),
                board(0, "l1", 3, 8),
                Change(
                    1,
                    Change::Recode {
                        list: "l0".into(),
                        board: 6,
                        code: 2,
                    },
                ),
                Sync(1),
                Seal(0),
                Sync(0),
                Invite(0, 2),
                board(2, "l0", 3, 10),
            ],
        )
        .await
    }

    #[tokio::test(start_paused = true)]
    async fn a_seal_leaves_out_a_revoked_members_position_past_an_unsent_one() -> Result<(), Failure>
    {
        holds_at(
            20138,
            soak_at(20138, Wall::Moving),
            vec![
                Seal(0),
                Compact(0),
                Change(
                    0,
                    Change::Pair {
                        list: "l0".into(),
                        code: 3,
                        name: 1,
                        points: 3,
                    },
                ),
                Invite(0, 2),
                Change(
                    1,
                    Change::DropCard {
                        list: "l1".into(),
                        card: 4,
                    },
                ),
                Seal(0),
                Change(
                    1,
                    Change::Raise {
                        list: "l1".into(),
                        card: 3,
                        by: 0,
                    },
                ),
                Revoke(2),
            ],
        )
        .await
    }

    #[tokio::test(start_paused = true)]
    async fn a_rewound_board_keeps_the_card_a_member_holds_only_in_its_base() -> Result<(), Failure>
    {
        holds_at(
            20307,
            soak_at(20307, Wall::Moving),
            vec![
                Seal(0),
                Invite(0, 2),
                Change(
                    2,
                    Change::Board {
                        list: "l0".into(),
                        code: 2,
                        cap: 8,
                    },
                ),
                InviteTo(0, 3, "l0".into()),
                Change(
                    2,
                    Change::Card {
                        list: "l0".into(),
                        board: 2,
                        pinned: Some(4),
                        name: 3,
                        points: 4,
                    },
                ),
                Change(
                    3,
                    Change::Card {
                        list: "l0".into(),
                        board: 6,
                        pinned: None,
                        name: 4,
                        points: 0,
                    },
                ),
                Sync(2),
                Revoke(3),
                Seal(0),
            ],
        )
        .await
    }

    #[tokio::test(start_paused = true)]
    async fn a_joiner_replays_a_card_whose_board_a_later_drop_removed() -> Result<(), Failure> {
        holds_at(
            20327,
            soak_at(20327, Wall::Frozen),
            vec![
                Change(
                    1,
                    Change::Board {
                        list: "l0".into(),
                        code: 2,
                        cap: 9,
                    },
                ),
                Change(
                    0,
                    Change::Board {
                        list: "l1".into(),
                        code: 1,
                        cap: 4,
                    },
                ),
                Change(
                    1,
                    Change::Card {
                        list: "l0".into(),
                        board: 3,
                        pinned: None,
                        name: 0,
                        points: 3,
                    },
                ),
                Sync(1),
                Change(
                    1,
                    Change::Card {
                        list: "l1".into(),
                        board: 2,
                        pinned: Some(4),
                        name: 1,
                        points: 0,
                    },
                ),
                Change(
                    1,
                    Change::DropBoard {
                        list: "l0".into(),
                        board: 0,
                    },
                ),
                Invite(0, 2),
                SyncPair(0, 1),
                Seal(0),
                Compact(0),
                Invite(0, 3),
                Change(
                    1,
                    Change::Board {
                        list: "l1".into(),
                        code: 2,
                        cap: 3,
                    },
                ),
                Sync(2),
            ],
        )
        .await
    }

    #[tokio::test(start_paused = true)]
    async fn a_joiner_never_shows_a_card_of_a_compacted_list_before_its_board()
    -> Result<(), Failure> {
        let board = |node, code, cap| {
            Change(
                node,
                Change::Board {
                    list: "l0".into(),
                    code,
                    cap,
                },
            )
        };
        holds_at(
            20044,
            soak_at(20044, Wall::Moving),
            vec![
                Partition("l0".into()),
                board(1, 2, 4),
                Sync(0),
                board(0, 3, 11),
                Change(
                    0,
                    Change::Card {
                        list: "l0".into(),
                        board: 2,
                        pinned: None,
                        name: 4,
                        points: 2,
                    },
                ),
                Seal(0),
                Compact(0),
                Invite(0, 2),
            ],
        )
        .await
    }

    #[tokio::test(start_paused = true)]
    async fn a_joiner_never_shows_a_pin_of_a_compacted_list_before_its_board() -> Result<(), Failure>
    {
        holds_at(
            20365,
            soak_at(20365, Wall::Moving),
            vec![
                Seal(1),
                SyncPair(2, 3),
                Advance(std::time::Duration::from_secs(60)),
                Sync(1),
                Seal(0),
                Change(
                    0,
                    Change::Raise {
                        list: "l0".into(),
                        card: 4,
                        by: 2,
                    },
                ),
                SyncPair(1, 0),
                Sync(1),
                Change(
                    1,
                    Change::MoveCard {
                        from: "l0".into(),
                        to: "l0".into(),
                        card: 0,
                        board: 4,
                    },
                ),
                Compact(0),
                SyncPair(3, 1),
                Online(0),
                Change(
                    1,
                    Change::MoveBoard {
                        from: "l0".into(),
                        to: "l0".into(),
                        board: 2,
                    },
                ),
                Change(
                    1,
                    Change::MoveCard {
                        from: "l1".into(),
                        to: "l0".into(),
                        card: 6,
                        board: 0,
                    },
                ),
                Sync(0),
                Change(
                    1,
                    Change::Recode {
                        list: "l0".into(),
                        board: 2,
                        code: 1,
                    },
                ),
                Advance(std::time::Duration::from_secs(60)),
                Sync(0),
                Change(
                    0,
                    Change::Recode {
                        list: "l1".into(),
                        board: 6,
                        code: 3,
                    },
                ),
                Crash(1, Some(rocky_secure::CrashPoint::GroupAfterCommit)),
                Crash(0, Some(rocky_secure::CrashPoint::Redo)),
                Change(
                    0,
                    Change::Pair {
                        list: "l1".into(),
                        code: 1,
                        name: 0,
                        points: 4,
                    },
                ),
                Seal(0),
                Compact(0),
                Invite(0, 2),
            ],
        )
        .await
    }

    #[tokio::test(start_paused = true)]
    async fn a_member_granted_a_list_after_its_seal_publishes_the_outcome_its_peers_reached()
    -> Result<(), Failure> {
        holds_at(
            20109,
            soak_at(20109, Wall::Moving),
            vec![
                InviteTo(0, 2, "l1".into()),
                Invite(0, 3),
                Change(
                    1,
                    Change::DropCard {
                        list: "l1".into(),
                        card: 1,
                    },
                ),
                Change(
                    3,
                    Change::Pair {
                        list: "l0".into(),
                        code: 1,
                        name: 1,
                        points: 0,
                    },
                ),
                Change(
                    0,
                    Change::Board {
                        list: "l0".into(),
                        code: 1,
                        cap: 8,
                    },
                ),
                Seal(0),
                Sync(2),
                Grant("l0".into(), 2),
            ],
        )
        .await
    }
}
