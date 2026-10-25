//! Schedules the sweep found and shrank, kept as fixed regressions.
use super::{
    driver,
    schedule::{Action::*, Scenario},
};
use crate::Failure;

const SIZE: Scenario = Scenario {
    nodes: 4,
    steps: 0,
    rounds: 4,
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

/// Seeds 10187 and 10096: a write or a refused opening on a branch sealed
/// after compacting at the seal it opened from.
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
/// Seed 10041: leaving a branch sealed and compacted after compacting
/// its base.
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

/// Leaving a compacted branch for a line with an unsent write loses the
/// compaction, yet scopes whose base stays on the line still rebuild.
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

/// Seed 10095: a member that joined while the owner followed a branch,
/// which the owner then sealed and compacted, audits on `main`.
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
