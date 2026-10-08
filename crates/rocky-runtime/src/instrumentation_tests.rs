//! Subphase attribution, checkpoint-interval recording and the bounded log.
use super::*;
use crate::scaling_option_tests::{NODE, note, open_node, scope};
use crate::worker::Fault;
use std::time::Instant;

fn instrumented() -> RuntimeOptions {
    RuntimeOptions {
        files: 1,
        instrument: true,
        ..RuntimeOptions::default()
    }
}

#[tokio::test]
async fn subphases_are_zero_by_default_and_recorded_when_instrumented() {
    // Default options leave instrumentation off: the subphase counters stay
    // zero even though steps ran.
    let dir = tempfile::TempDir::new().unwrap();
    let runtime = open_node(&dir.path().join("off.db"), NODE, RuntimeOptions::default()).await;
    let replica = runtime.domain(scope(1)).unwrap();
    for index in 0..8u8 {
        note(&replica, index, "off").committed().await.unwrap();
    }
    let stats = replica.writer_stats();
    assert_eq!(stats.admission_micros, 0);
    assert_eq!(stats.reduction_micros, 0);
    assert_eq!(stats.encoding_micros, 0);
    assert_eq!(stats.sql_micros, 0);
    assert_eq!(replica.checkpoint_intervals().0.len(), 0);
    runtime.close().await.unwrap();

    let runtime = open_node(&dir.path().join("on.db"), NODE, instrumented()).await;
    let replica = runtime.domain(scope(1)).unwrap();
    for index in 0..8u8 {
        note(&replica, index, "on").committed().await.unwrap();
    }
    let stats = replica.writer_stats();
    assert!(stats.admission_micros > 0, "{stats:?}");
    assert!(stats.reduction_micros > 0, "{stats:?}");
    assert!(stats.encoding_micros > 0, "{stats:?}");
    assert!(stats.sql_micros > 0, "{stats:?}");
    // The subphases stay within the total step time (not counting commit).
    assert!(
        stats.admission_micros + stats.reduction_micros + stats.encoding_micros + stats.sql_micros
            <= stats.step_micros,
        "{stats:?}"
    );
    runtime.close().await.unwrap();
}

#[tokio::test]
async fn checkpoint_intervals_are_recorded_in_monotonic_order_when_instrumented() {
    let dir = tempfile::TempDir::new().unwrap();
    let runtime = open_node(&dir.path().join("ckpt.db"), NODE, instrumented()).await;
    let replica = runtime.domain(scope(1)).unwrap();
    // A tiny threshold so a short write burst crosses it several times.
    crate::tests::fault(&replica, Fault::CheckpointFrames(10)).await;
    for index in 0..400u16 {
        replica
            .mutate(Mutation {
                entity: None,
                payload: format!("burst-{index}").into_bytes(),
            })
            .unwrap()
            .committed()
            .await
            .unwrap();
    }
    let (intervals, dropped) = replica.checkpoint_intervals();
    assert!(!intervals.is_empty(), "no checkpoint interval recorded");
    for window in intervals.windows(2) {
        assert!(window[0].start <= window[1].start, "{window:?}");
        assert!(window[0].start <= window[0].end, "{window:?}");
    }
    // Truncating resets are the second kind; both kinds carry a copy/flush time.
    assert!(
        intervals
            .iter()
            .all(|interval| interval.kind == CheckpointKind::Passive
                || interval.kind == CheckpointKind::Truncate)
    );
    // A passive checkpoint measured its page copy; the flush can be zero only
    // if the copy failed, which would have been surfaced as an error.
    assert!(
        intervals
            .iter()
            .all(|interval| interval.copy_micros > 0 || interval.kind == CheckpointKind::Truncate),
        "{intervals:?}"
    );
    assert_eq!(dropped, 0);
    runtime.close().await.unwrap();
}

#[tokio::test]
async fn writer_steps_are_attributed_by_kind_only_when_instrumented() {
    // Default options leave the attribution counters at zero even though
    // local mutations ran.
    let dir = tempfile::TempDir::new().unwrap();
    let runtime = open_node(
        &dir.path().join("kind-off.db"),
        NODE,
        RuntimeOptions::default(),
    )
    .await;
    let replica = runtime.domain(scope(1)).unwrap();
    for index in 0..8u8 {
        note(&replica, index, "off").committed().await.unwrap();
    }
    let stats = replica.writer_stats();
    assert_eq!(stats.authoring_steps, 0);
    assert_eq!(stats.authoring_step_micros, 0);
    runtime.close().await.unwrap();

    let runtime = open_node(&dir.path().join("kind-on.db"), NODE, instrumented()).await;
    let replica = runtime.domain(scope(1)).unwrap();
    for index in 0..8u8 {
        note(&replica, index, "on").committed().await.unwrap();
    }
    let stats = replica.writer_stats();
    assert!(stats.authoring_steps > 0, "{stats:?}");
    assert!(stats.authoring_step_micros > 0, "{stats:?}");
    // Attributed authoring time is part of the group's total step time.
    assert!(
        stats.authoring_step_micros <= stats.step_micros,
        "{stats:?}"
    );
    runtime.close().await.unwrap();
}

#[test]
fn the_checkpoint_log_is_bounded_and_counts_dropped_intervals() {
    let metrics = crate::writer::Metrics::default();
    for index in 0..(crate::writer::MAX_CHECKPOINT_INTERVALS + 7) {
        let start = Instant::now() + std::time::Duration::from_micros(index as u64);
        metrics.record_checkpoint(CheckpointInterval {
            start,
            end: start + std::time::Duration::from_micros(1),
            kind: CheckpointKind::Passive,
            copy_micros: 1,
            flush_micros: 1,
        });
    }
    let (intervals, dropped) = metrics.checkpoint_intervals();
    assert_eq!(intervals.len(), crate::writer::MAX_CHECKPOINT_INTERVALS);
    assert_eq!(dropped, 7);
    // The oldest intervals were dropped, so the log starts at the 8th record.
    assert_eq!(intervals[0].kind, CheckpointKind::Passive);
}
