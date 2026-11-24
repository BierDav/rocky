use super::*;
use rocky_core::{MAX_CLOCK, MAX_CLOCK_AHEAD};

const RED: &str = "INSERT INTO teams(name,cap,id) VALUES('red',1,#id)";

async fn status(device: &Device, frame: &EncryptedFrame) -> String {
    let inner = device
        .replica
        .inner
        .lock(crate::replica::Class::Background)
        .await;
    let stored = crate::storage::collect_rows(
        inner
            .conn
            .query(
                "SELECT status FROM secure_operations WHERE hash=?",
                [frame.id().unwrap().as_bytes().to_vec()],
            )
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    match stored.as_slice() {
        [row] => match row.as_slice() {
            [SqlValue::Text(status)] => status.clone(),
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
}
fn ahead_blockers(blockers: &[Blocker]) -> Vec<(u64, u64)> {
    blockers
        .iter()
        .filter_map(|blocker| match &blocker.kind {
            BlockerKind::ClockAhead { count, seconds, .. } => {
                assert_eq!(blocker.clears_by, ClearsBy::Clock);
                Some((*count, *seconds))
            }
            _ => None,
        })
        .collect()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_stamp_beyond_the_clock_bound_is_refused_and_authoring_stops_before_it() {
    let c = Constrained::new();
    let root = rocky_platform::fs::Scratch::new().await;
    let (mut devices, _, _) = devices(&c, root.path(), 2, &[]).await;
    devices[0].commit(&c, &[RED]).await.unwrap();
    let frames = devices[0].fresh().await;
    let node = devices[0].node.clone();
    let beyond: Vec<_> = frames
        .iter()
        .map(|frame| restamped(frame, &node, MAX_CLOCK + 1))
        .collect();
    for result in devices[1].replica.receive_all(beyond.clone()).await {
        let error = result.unwrap_err();
        assert!(
            error.to_string().contains("invalid operation metadata"),
            "{error}"
        );
    }
    assert!(beyond.iter().all(|frame| frame.id().is_err()));
    let stored = "SELECT header,body FROM secure_operations";
    assert!(stored_frames(&devices[1].replica, stored).await.is_empty());
    let last = frames.len() as u64 - 1;
    let at_the_bound: Vec<_> = (0..)
        .zip(&frames)
        .map(|(index, frame)| restamped(frame, &node, MAX_CLOCK - last + index))
        .collect();
    devices[1].receive(&at_the_bound).await;
    for frame in &at_the_bound {
        assert_eq!(
            status(&devices[1], frame).await,
            "pending",
            "a stamp at the bound is valid and, being far ahead, waits"
        );
    }
    assert!(devices[1].rows(&c).await["teams"].is_empty());
    let writer = rocky_core::WriterId::from_bytes([1; 32]);
    let row_stamp = |clock: u64| encode(&(clock, writer, 0_u64)).unwrap();
    assert!(decode::<rocky_core::SecureStamp>(&row_stamp(MAX_CLOCK)).is_ok());
    assert!(
        decode::<rocky_core::SecureStamp>(&row_stamp(MAX_CLOCK + 1)).is_err(),
        "decoded row stamps carry the clock bound"
    );
    let position = |clock: u64| {
        encode(&crate::LogPosition {
            author: node.public_key(),
            sequence: 0,
            hash: frames[0].id().unwrap(),
            clock,
        })
        .unwrap()
    };
    assert!(decode::<crate::LogPosition>(&position(MAX_CLOCK)).is_ok());
    assert!(
        decode::<crate::LogPosition>(&position(MAX_CLOCK + 1)).is_err(),
        "decoded checkpoint positions carry the clock bound"
    );
    assert_eq!(crate::next_clock(MAX_CLOCK - 1).unwrap(), MAX_CLOCK);
    assert!(
        crate::next_clock(MAX_CLOCK).is_err() && crate::next_clock(u64::MAX).is_err(),
        "authoring refuses a next stamp beyond the bound instead of wrapping"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn frames_from_a_clock_ahead_wait_until_this_clock_is_near_them() {
    let c = Constrained::new();
    let root = rocky_platform::fs::Scratch::new().await;
    let (mut devices, _, mut history) = devices(&c, root.path(), 3, &[]).await;
    let receiver = devices.pop().unwrap();
    crate::CLOCK_SKEW.with(|skew| skew.set(3600));
    let first = devices[0].commit(&c, &[RED]).await;
    let second = devices[0]
        .commit(&c, &["INSERT INTO teams(name,cap,id) VALUES('blue',2,#id)"])
        .await;
    crate::CLOCK_SKEW.with(|skew| skew.set(0));
    let red = first.unwrap()[0];
    second.unwrap();
    let ahead = devices[0].fresh().await;
    devices[1].receive(&ahead).await;
    for frame in &ahead {
        assert_eq!(status(&devices[1], frame).await, "pending");
    }
    assert!(devices[1].rows(&c).await["teams"].is_empty());
    let blockers = ahead_blockers(&devices[1].replica.blockers().await.unwrap());
    assert_eq!(blockers.len(), 1, "{blockers:?}");
    let (count, seconds) = blockers[0];
    assert_eq!(count, ahead.len() as u64);
    assert!((3590..=3601).contains(&seconds), "{seconds}");
    let wait = devices[1].replica.clock_wait().await.unwrap().unwrap();
    let expected = std::time::Duration::from_secs(3600) - MAX_CLOCK_AHEAD;
    assert!(wait <= expected && wait + std::time::Duration::from_secs(10) > expected);
    devices[1]
        .commit(
            &c,
            &["INSERT INTO teams(name,cap,id) VALUES('green',3,#id)"],
        )
        .await
        .unwrap();
    let own = devices[1].fresh().await;
    let clock = |frame: &EncryptedFrame| frame.operation_ref().unwrap().header.timestamp;
    assert!(
        own.iter().map(clock).max() < ahead.iter().map(clock).min(),
        "the receiver's own write does not pass the waiting stamps"
    );
    let still_early = 3600 - MAX_CLOCK_AHEAD.as_secs() - 60;
    crate::CLOCK_SKEW.with(|skew| skew.set(still_early));
    devices[1].replica.recover_locally().await.unwrap();
    assert!(devices[1].replica.clock_wait().await.unwrap().is_some());
    assert_eq!(
        devices[1].rows(&c).await["teams"].len(),
        1,
        "still early: only the receiver's own write applies"
    );
    let due = 3600 - MAX_CLOCK_AHEAD.as_secs() + 60;
    crate::CLOCK_SKEW.with(|skew| skew.set(due));
    devices[1].replica.recover_locally().await.unwrap();
    for frame in &ahead {
        assert_eq!(
            status(&devices[1], frame).await,
            "applied",
            "once due, waiting frames apply with no other frame arriving"
        );
    }
    assert_eq!(devices[1].replica.clock_wait().await.unwrap(), None);
    assert!(ahead_blockers(&devices[1].replica.blockers().await.unwrap()).is_empty());
    history.extend(ahead);
    history.extend(own);
    let expected = exchange(&c, &mut devices, &receiver, &mut history).await;
    crate::CLOCK_SKEW.with(|skew| skew.set(0));
    assert_eq!(expected.rows["teams"].len(), 3);
    assert!(
        expected.rows["teams"]
            .iter()
            .any(|row| row.contains(&SqlValue::from(red)))
    );
    for device in devices.iter().chain([&receiver]) {
        device
            .replica
            .rebuild_projection(c.scope("teams"))
            .await
            .unwrap();
        assert_eq!(device.state(&c, &expected).await, expected);
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn frames_within_the_bound_ahead_apply_at_once() {
    let c = Constrained::new();
    let root = rocky_platform::fs::Scratch::new().await;
    let (mut devices, _, _) = devices(&c, root.path(), 2, &[]).await;
    crate::CLOCK_SKEW.with(|skew| skew.set(MAX_CLOCK_AHEAD.as_secs() - 30));
    let committed = devices[0].commit(&c, &[RED]).await;
    crate::CLOCK_SKEW.with(|skew| skew.set(0));
    committed.unwrap();
    let frames = devices[0].fresh().await;
    devices[1].receive(&frames).await;
    for frame in &frames {
        assert_eq!(status(&devices[1], frame).await, "applied");
    }
    assert_eq!(devices[1].rows(&c).await["teams"].len(), 1);
    assert_eq!(devices[1].replica.clock_wait().await.unwrap(), None);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_authored_transaction_is_stamped_after_every_replayed_one() {
    let c = Constrained::new();
    let root = rocky_platform::fs::Scratch::new().await;
    let (mut devices, _, _) = devices(&c, root.path(), 2, &[]).await;
    crate::CLOCK_SKEW.with(|skew| skew.set(MAX_CLOCK_AHEAD.as_secs() - 30));
    let committed = devices[0].commit(&c, &[RED]).await;
    crate::CLOCK_SKEW.with(|skew| skew.set(0));
    let red = committed.unwrap()[0];
    let ahead = devices[0].fresh().await;
    devices[1].receive(&ahead).await;
    let latest_ahead = ahead
        .iter()
        .map(|frame| frame.operation_ref().unwrap().header.timestamp)
        .max()
        .unwrap();
    devices[1]
        .commit(
            &c,
            &[&format!(
                "INSERT INTO members(team,email,id) VALUES({red},'x@example',#id)"
            )],
        )
        .await
        .unwrap();
    let reader = devices[1].fresh().await;
    assert!(!reader.is_empty());
    for frame in &reader {
        assert!(
            frame.operation_ref().unwrap().header.timestamp > latest_ahead,
            "a reader's transaction is stamped past the row it read, though that row's clock is ahead of the reader's"
        );
    }
}
