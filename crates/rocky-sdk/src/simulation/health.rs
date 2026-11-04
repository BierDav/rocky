//! Mesh health: coordination failures as a device's alerts, and each
//! device's report on the domain's trusted nodes.
use super::{
    driver,
    schedule::{Action, Action::*, Fixture, Scenario, Wall},
};
use crate::Failure;
use std::time::Duration;

async fn run(nodes: usize, steps: Vec<Action>, kinds: &[&str]) -> Result<Vec<String>, Failure> {
    let scenario = Scenario {
        nodes,
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
    Ok(report
        .steps
        .iter()
        .filter(|step| kinds.iter().any(|kind| step.contains(kind)))
        .filter_map(|step| step.split(" -> ").nth(1))
        .filter(|outcome| *outcome != "watching")
        .map(str::to_owned)
        .collect())
}

/// A coordination service that refuses this device's clock raises an
/// alert, delivered to a `status_after` wait, that repeated refusals keep
/// and the next accepted call clears, again through `status_after`.
#[tokio::test(start_paused = true)]
async fn a_coordinator_clock_skew_is_an_alert_until_a_call_is_accepted() -> Result<(), Failure> {
    let steps = vec![
        Watch(1),
        Coordinated(1, Some(3600)),
        Woken(1),
        Coordinated(1, Some(3600)),
        Alerts(1),
        Alerts(0),
        Watch(1),
        Coordinated(1, None),
        Woken(1),
    ];
    let skewed = r#"alerts ["coordinator skew 60 min ahead false"]"#;
    assert_eq!(
        run(2, steps, &[" woken ", " alerts "]).await?,
        [skewed, skewed, "alerts []", "alerts []"]
    );
    Ok(())
}

/// The owner sees a partial device's alert and blocker once it syncs; an
/// hour without a link shows it silent; its next sync shows it heard and
/// both cleared, each change through `status_after`.
#[tokio::test(start_paused = true)]
async fn a_trusted_node_sees_a_partial_devices_alert_and_blocker_clear() -> Result<(), Failure> {
    let hour = Duration::from_secs(3600);
    // Severing the pair ends its link: the next sync makes a new one.
    let relink = [Sever(1, 2), Heal(1, 2)];
    let mut steps = vec![Invite(0, 2), Partial(1), Sync(0), Sync(1), Sync(2), Mesh(0)];
    // c's clock runs an hour ahead and only b hears from it.
    steps.extend([Sever(0, 2), Clock(2, hour)]);
    steps.extend(relink.clone());
    steps.extend([
        Write(2),
        Sync(1),
        Ahead(1),
        WatchMesh(0, 1),
        Sync(1),
        WokenMesh(0),
    ]);
    steps.extend([
        Clock(2, Duration::ZERO),
        Advance(hour),
        Ahead(1),
        // b's last link to a closed an hour ago.
        Mesh(0),
        WatchMesh(0, 1),
    ]);
    steps.extend(relink);
    steps.extend([Sync(1), WokenMesh(0), Mesh(0)]);
    assert_eq!(
        run(3, steps, &[" mesh ", " ahead b"]).await?,
        [
            r#"mesh ["b: alerts []", "c: alerts []"]"#,
            "1 ahead, 0 keys",
            r#"mesh ["b: alerts [\"c ahead 60 min\"], clock_ahead 1", "c: alerts []"]"#,
            "0 ahead, 2 keys",
            r#"mesh ["b: alerts [\"c ahead 60 min\"], clock_ahead 1, silent", "c: alerts [], silent"]"#,
            // Heard again once its link opens, then its report arrives.
            r#"mesh ["b: alerts [\"c ahead 60 min\"], clock_ahead 1", "c: alerts [], silent"]"#,
            r#"mesh ["b: alerts []", "c: alerts [], silent"]"#,
        ]
    );
    Ok(())
}

/// A device that goes offline shows silent on the owner once
/// `SILENT_AFTER` passes, which wakes a pending `status_after` by itself.
#[tokio::test(start_paused = true)]
async fn a_device_that_stops_reporting_shows_silent() -> Result<(), Failure> {
    let steps = vec![Sync(1), Mesh(0), WatchMesh(0, 1), Offline(1), WokenMesh(0)];
    assert_eq!(
        run(2, steps, &[" mesh "]).await?,
        [
            r#"mesh ["b: alerts []"]"#,
            r#"mesh ["b: alerts [], silent"]"#
        ]
    );
    Ok(())
}

/// Devices that are not trusted hold and receive no report of another
/// device, though they link to each other; the owner holds both.
#[tokio::test(start_paused = true)]
async fn only_trusted_nodes_receive_reports() -> Result<(), Failure> {
    let mut steps = vec![Invite(0, 2)];
    for _ in 0..2 {
        steps.extend([Sync(0), Sync(1), Sync(2)]);
    }
    steps.extend([
        Received(1),
        Received(2),
        Received(0),
        Mesh(1),
        Mesh(2),
        Mesh(0),
    ]);
    assert_eq!(
        run(3, steps, &[" mesh ", " received "]).await?,
        [
            "no reports",
            "no reports",
            "some reports",
            "mesh none",
            "mesh none",
            r#"mesh ["b: alerts []", "c: alerts []"]"#,
        ]
    );
    Ok(())
}

/// A trusted node that never links to a device learns its report from
/// another trusted node, the creating device here.
#[tokio::test(start_paused = true)]
async fn trusted_nodes_share_the_reports_they_receive() -> Result<(), Failure> {
    let steps = vec![Trust(2), Sever(1, 2), Sync(1), Sync(2), Mesh(2), Mesh(1)];
    assert_eq!(
        run(3, steps, &[" mesh ", " trust "]).await?,
        [
            "trusted",
            r#"mesh ["a: alerts []", "b: alerts [], via a"]"#,
            "mesh none"
        ]
    );
    Ok(())
}
