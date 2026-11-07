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

/// A device whose clock runs an hour ahead learns from its next link that
/// its changes wait on its peer, and the peer learns why they will; both
/// alerts clear with the next link after the clock is right again, and a
/// skew within `MAX_CLOCK_AHEAD` raises none.
#[tokio::test(start_paused = true)]
async fn a_link_tells_both_devices_when_one_clock_runs_ahead() -> Result<(), Failure> {
    let hour = Duration::from_secs(3600);
    // Severing the pair ends its link: the next sync makes a new one.
    let relink = [Sever(0, 1), Heal(0, 1), Sync(0)];
    let mut steps = vec![Watch(0), Watch(1), Clock(0, hour)];
    steps.extend(relink.clone());
    steps.extend([Woken(0), Woken(1), Write(0), Sync(1), Ahead(1)]);
    steps.extend([Watch(0), Watch(1), Clock(0, Duration::ZERO)]);
    steps.extend(relink.clone());
    steps.extend([
        Woken(0),
        Woken(1),
        Clock(0, MAX_CLOCK_AHEAD - Duration::from_secs(60)),
    ]);
    steps.extend(relink);
    steps.extend([Alerts(0), Alerts(1), Clock(0, Duration::ZERO)]);
    // b's own clock reaches a's held frames.
    steps.push(Advance(hour));
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
        .filter(|step| {
            [" woken ", " alerts ", " ahead b "]
                .iter()
                .any(|kind| step.contains(kind))
        })
        .filter_map(|step| step.split(" -> ").nth(1))
        .collect();
    assert_eq!(
        outcomes,
        [
            r#"alerts ["ahead of b 60 min", "a linked"]"#,
            r#"alerts ["a ahead 60 min"]"#,
            "1 ahead, 0 keys",
            r#"alerts ["a linked"]"#,
            "alerts []",
            r#"alerts ["a linked"]"#,
            "alerts []",
        ],
        "{}",
        report.dump()
    );
    Ok(())
}
