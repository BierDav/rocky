//! A node whose clock runs ahead: its peer holds its frames until its own
//! clock is near them, woken by local recovery's timer alone.
use super::{
    driver,
    schedule::{Action::*, Fixture, Scenario, Wall},
};
use crate::Failure;
use rocky_core::MAX_CLOCK_AHEAD;
use std::time::Duration;

#[tokio::test(start_paused = true)]
async fn frames_from_a_clock_ahead_apply_when_this_clock_is_near_them() -> Result<(), Failure> {
    let hour = Duration::from_secs(3600);
    let early = hour - MAX_CLOCK_AHEAD - Duration::from_secs(60);
    let steps = vec![
        Write(0),
        Sync(1),
        Sever(0, 1),
        WriteAhead(0, hour),
        Write(0),
        Heal(0, 1),
        Sync(1),
        Ahead(1),
        // Nothing arrives from here on: only b's own timer applies them.
        Sever(0, 1),
        Advance(early),
        Ahead(1),
        Advance(Duration::from_secs(120)),
        Ahead(1),
    ];
    let scenario = Scenario {
        nodes: 2,
        steps: 0,
        rounds: 4,
        fixture: Fixture::Items,
        wall: Wall::Moving,
    };
    let report = tokio::task::LocalSet::new()
        .run_until(driver::run_steps(1, scenario, steps))
        .await?;
    assert!(report.safety.is_none(), "{}", report.dump());
    assert!(report.liveness.is_none(), "{}", report.dump());
    let outcomes: Vec<&str> = report
        .steps
        .iter()
        .filter(|step| step.contains(" ahead b "))
        .filter_map(|step| step.split(" -> ").nth(1))
        .collect();
    // Two writes of two items each; the second follows the first's clock.
    assert_eq!(
        outcomes,
        ["2 ahead, 2 keys", "2 ahead, 2 keys", "0 ahead, 6 keys"],
        "{}",
        report.dump()
    );
    Ok(())
}
