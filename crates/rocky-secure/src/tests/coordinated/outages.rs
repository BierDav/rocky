use super::*;

#[derive(Default)]
pub(super) struct Faults {
    pub appends: Vec<Append>,
    pub unavailable: usize,
    pub lost: usize,
    pub ack_failures: usize,
    pub polls: usize,
    pub skew: bool,
    pub append_wait: Option<Arc<tokio::sync::Notify>>,
}

#[tokio::test]
async fn lost_assignment_reply_retries_the_same_append_without_eviction() {
    let f = Fixture::new(2).await;
    let slot = f.slot(&f.alice).await.unwrap();
    let frames = f.stage(&f.alice, slot, "survives outage").await.unwrap();
    let faults = Arc::new(StdMutex::new(Faults {
        unavailable: 1,
        lost: 1,
        ..Faults::default()
    }));
    let mut client = f.mesh.client(&f.alice.0);
    client.faults = Some(faults.clone());
    let committed = f
        .alice
        .1
        .propose(
            tip_log(&f.lineage),
            &client,
            &f.mesh.peers(&f.lineage, &f.alice.0),
        )
        .await
        .unwrap();
    let calls = faults.lock().unwrap();
    assert_eq!(calls.appends.len(), 3);
    assert!(
        calls
            .appends
            .iter()
            .all(|append| *append == calls.appends[0])
    );
    assert_eq!(
        frame_ids(&committed.frames).unwrap(),
        frame_ids(&frames).unwrap()
    );
    let head = f.mesh.log.head(tip_log(&f.lineage)).unwrap();
    assert_eq!(
        (head.commit, head.members.len(), head.hashes.len()),
        (1, 2, 1)
    );
}

#[tokio::test]
async fn acknowledged_members_receive_no_frames_during_repeated_status_polls() {
    let f = Fixture::new(2).await;
    let slot = f.slot(&f.alice).await.unwrap();
    f.stage(&f.alice, slot, "wait for proposer ack")
        .await
        .unwrap();
    let faults = Arc::new(StdMutex::new(Faults {
        ack_failures: 3,
        ..Faults::default()
    }));
    let mut client = f.mesh.client(&f.alice.0);
    client.faults = Some(faults.clone());
    f.alice
        .1
        .propose(
            tip_log(&f.lineage),
            &client,
            &f.mesh.peers(&f.lineage, &f.alice.0),
        )
        .await
        .unwrap();
    assert_eq!(faults.lock().unwrap().polls, 4);
    assert_eq!(
        *f.mesh.sent.lock().unwrap(),
        vec![*f.bob.0.public_key().as_bytes()]
    );
}

#[tokio::test]
async fn clock_skew_keeps_the_write_retryable_after_clock_correction() {
    let f = Fixture::new(2).await;
    let slot = f.slot(&f.alice).await.unwrap();
    let frames = f.stage(&f.alice, slot, "correct the clock").await.unwrap();
    let faults = Arc::new(StdMutex::new(Faults {
        skew: true,
        ..Faults::default()
    }));
    let mut client = f.mesh.client(&f.alice.0);
    client.faults = Some(faults.clone());
    let peers = f.mesh.peers(&f.lineage, &f.alice.0);
    assert!(matches!(
        f.alice
            .1
            .propose(tip_log(&f.lineage), &client, &peers)
            .await,
        Err(CoordinatedError::ClockSkew {
            coordinator_time: 123
        })
    ));
    assert!(f.mesh.sent.lock().unwrap().is_empty());
    faults.lock().unwrap().skew = false;
    let committed = f
        .alice
        .1
        .propose(tip_log(&f.lineage), &client, &peers)
        .await
        .unwrap();
    assert_eq!(
        frame_ids(&committed.frames).unwrap(),
        frame_ids(&frames).unwrap()
    );
    let calls = faults.lock().unwrap();
    assert_eq!(calls.appends[0], calls.appends[1]);
}

fn frame_ids(frames: &[EncryptedFrame]) -> Result<Vec<Hash>, Error> {
    frames.iter().map(EncryptedFrame::id).collect()
}

#[tokio::test]
async fn canceling_after_assignment_allows_background_recovery_of_the_same_write() {
    let f = Fixture::new(2).await;
    let slot = f.slot(&f.alice).await.unwrap();
    let frames = f.stage(&f.alice, slot, "caller left").await.unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let faults = Arc::new(StdMutex::new(Faults {
        append_wait: Some(entered.clone()),
        ..Faults::default()
    }));
    let mut client = f.mesh.client(&f.alice.0);
    client.faults = Some(faults.clone());
    let log = tip_log(&f.lineage);
    let peers = f.mesh.peers(&f.lineage, &f.alice.0);
    {
        let proposed = f.alice.1.propose(log, &client, &peers);
        tokio::pin!(proposed);
        tokio::select! {
            () = entered.notified() => {},
            result = &mut proposed => panic!("append should wait: {result:?}"),
        }
    }
    faults.lock().unwrap().append_wait = None;
    let status = f
        .alice
        .1
        .resolve_coordinated(log, &client, &peers)
        .await
        .unwrap();
    assert_eq!((status.position, status.undecided), (1, 0));
    assert_eq!(faults.lock().unwrap().appends.len(), 2);
    {
        let calls = faults.lock().unwrap();
        assert_eq!(calls.appends[0], calls.appends[1]);
    }
    assert_eq!(f.mesh.log.head(log).unwrap().members.len(), 2);
    assert!(
        held(&f.alice.1)
            .await
            .is_superset(&frame_ids(&frames).unwrap().into_iter().collect())
    );
}
