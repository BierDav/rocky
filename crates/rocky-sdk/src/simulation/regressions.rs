use super::{
    driver,
    schedule::{Action::*, Fixture, Scenario, Wall},
};
use crate::Failure;

const SIZE: Scenario = Scenario {
    nodes: 4,
    steps: 0,
    rounds: 4,
    fixture: Fixture::Items,
    wall: Wall::Moving,
};

async fn healed(seed: u64, steps: Vec<super::schedule::Action>) -> Result<(), Failure> {
    let report = tokio::task::LocalSet::new()
        .run_until(driver::run_steps(seed, SIZE, steps))
        .await?;
    assert!(report.safety.is_none(), "{}", report.dump());
    assert!(report.liveness.is_none(), "{}", report.dump());
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_member_recovers_earlier_epoch_writes_from_a_newer_base() -> Result<(), Failure> {
    use rocky_secure::CrashPoint;
    healed(
        10123,
        vec![
            Invite(0, 2),
            Revoke(1),
            Crash(2, Some(CrashPoint::GroupBeforeCommit)),
            Sync(0),
            Seal(0),
            Invite(0, 3),
            Write(0),
        ],
    )
    .await
}

#[tokio::test(start_paused = true)]
async fn a_late_empty_partition_gets_a_verified_replay_base() -> Result<(), Failure> {
    healed(
        10532,
        vec![Write(0), Seal(0), Compact(0), Partition("late".into())],
    )
    .await
}

#[tokio::test(start_paused = true)]
async fn an_interrupted_checkpoint_switch_resumes_without_another_write() -> Result<(), Failure> {
    use rocky_secure::CrashPoint;
    let report = tokio::task::LocalSet::new()
        .run_until(driver::run_steps(
            10730,
            SIZE,
            vec![
                Write(0),
                Write(1),
                Seal(0),
                Invite(0, 2),
                Write(2),
                Sync(0),
                Seal(0),
                Compact(0),
                Revoke(2),
                Crash(1, Some(CrashPoint::Switch)),
            ],
        ))
        .await?;
    assert!(
        report
            .steps
            .iter()
            .any(|step| step.contains("Switch fired")),
        "{}",
        report.dump()
    );
    assert!(report.safety.is_none(), "{}", report.dump());
    assert!(report.liveness.is_none(), "{}", report.dump());
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn an_empty_partition_base_advances_to_cover_a_new_author() -> Result<(), Failure> {
    healed(
        10355,
        vec![
            Partition("empty".into()),
            Write(0),
            Seal(0),
            Invite(0, 2),
            Write(2),
            Sync(0),
            Seal(0),
            Compact(0),
        ],
    )
    .await
}
