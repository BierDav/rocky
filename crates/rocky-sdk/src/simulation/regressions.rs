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

async fn safe(seed: u64, steps: Vec<super::schedule::Action>) -> Result<(), Failure> {
    let report = tokio::task::LocalSet::new()
        .run_until(driver::run_steps(seed, SIZE, steps))
        .await?;
    assert!(report.safety.is_none(), "{}", report.dump());
    Ok(())
}
fn branch(name: &str) -> super::schedule::Action {
    Branch(0, name.into())
}

#[tokio::test(start_paused = true)]
async fn a_write_on_a_branch_sealed_after_compaction_keeps_the_ledger() -> Result<(), Failure> {
    safe(
        10187,
        vec![
            Seal(0),
            Write(1),
            Sync(0),
            Compact(0),
            branch("dev"),
            Seal(0),
            Write(0),
        ],
    )
    .await
}
#[tokio::test(start_paused = true)]
async fn a_refused_opening_on_a_sealed_branch_keeps_the_ledger() -> Result<(), Failure> {
    safe(
        10096,
        vec![
            Write(1),
            Seal(0),
            Sync(0),
            Compact(0),
            branch("dev"),
            Seal(0),
            branch("next"),
        ],
    )
    .await
}
#[tokio::test(start_paused = true)]
async fn leaving_a_branch_compacted_after_its_base_keeps_the_projection() -> Result<(), Failure> {
    safe(
        10041,
        vec![
            Seal(0),
            Invite(0, 2),
            Compact(0),
            branch("dev"),
            Seal(0),
            Compact(0),
        ],
    )
    .await
}

#[tokio::test(start_paused = true)]
async fn leaving_a_compacted_branch_rebuilds_scopes_on_the_line() -> Result<(), Failure> {
    safe(
        1,
        vec![
            Seal(0),
            Write(0),
            Compact(0),
            branch("dev"),
            Seal(0),
            Compact(0),
            Follow(0, "main".into()),
        ],
    )
    .await
}

#[tokio::test(start_paused = true)]
async fn a_member_joined_on_a_compacted_branch_audits_on_main() -> Result<(), Failure> {
    safe(
        10095,
        vec![
            Write(0),
            Seal(0),
            branch("dev"),
            Invite(0, 2),
            Revoke(1),
            Seal(0),
            Compact(0),
        ],
    )
    .await
}

async fn healed(seed: u64, steps: Vec<super::schedule::Action>) -> Result<(), Failure> {
    let report = tokio::task::LocalSet::new()
        .run_until(driver::run_steps(seed, SIZE, steps))
        .await?;
    assert!(report.safety.is_none(), "{}", report.dump());
    assert!(report.liveness.is_none(), "{}", report.dump());
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn leaving_an_imported_base_rebuilds_without_stale_seal_bounds() -> Result<(), Failure> {
    use rocky_secure::CrashPoint;
    healed(
        10335,
        vec![
            Seal(0),
            branch("dev"),
            Seal(0),
            Invite(0, 2),
            Crash(1, Some(CrashPoint::GroupAfterCommit)),
            Write(2),
            Invite(0, 3),
            Compact(0),
            Sync(3),
            Write(0),
        ],
    )
    .await
}

#[tokio::test(start_paused = true)]
async fn a_member_on_another_branch_recovers_from_listed_bases() -> Result<(), Failure> {
    use rocky_secure::CrashPoint;
    healed(
        10961,
        vec![
            Crash(0, Some(CrashPoint::GroupAfterCommit)),
            Seal(0),
            Invite(0, 2),
            Write(0),
            Compact(0),
            branch("dev"),
            Revoke(2),
            Invite(0, 3),
        ],
    )
    .await
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
async fn a_lone_signer_recovers_after_leaving_a_compacted_branch() -> Result<(), Failure> {
    healed(
        10957,
        vec![
            Write(1),
            Seal(0),
            branch("dev"),
            Compact(0),
            Write(0),
            Revoke(1),
            branch("next"),
        ],
    )
    .await
}

#[tokio::test(start_paused = true)]
async fn a_late_empty_partition_gets_a_verified_replay_base() -> Result<(), Failure> {
    healed(
        10532,
        vec![
            Write(0),
            Seal(0),
            Compact(0),
            Partition("late".into()),
            Share("late".into(), 1),
        ],
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
                Follow(1, "main".into()),
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
            Share("empty".into(), 1),
        ],
    )
    .await
}

#[tokio::test(start_paused = true)]
async fn importing_on_a_branch_preserves_an_admitted_write_of_main() -> Result<(), Failure> {
    healed(
        10834,
        vec![
            Seal(0),
            branch("dev"),
            Write(0),
            Write(0),
            Write(1),
            Sync(0),
            Follow(1, "dev".into()),
            Seal(0),
            Write(0),
            Seal(0),
            Write(1),
            Sync(1),
            Write(1),
            Sync(0),
            Seal(0),
            Invite(0, 2),
        ],
    )
    .await
}
