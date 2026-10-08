use rocky_core::{Envelope, OperationId};
use rocky_runtime::Mutation;
use tempfile::TempDir;
use todo_app::Action;
use todo_desktop::{
    Demo,
    transport::{Delivery, Message, SimulatedTransport},
};

async fn mutate(
    demo: &Demo,
    index: usize,
    entity: Option<OperationId>,
    action: Action,
) -> Envelope {
    demo.replicas[index]
        .mutate(Mutation {
            entity,
            payload: action.encode().unwrap(),
        })
        .unwrap()
        .committed()
        .await
        .unwrap()
}
async fn create(demo: &Demo, index: usize, title: &str) -> Envelope {
    mutate(
        demo,
        index,
        None,
        Action::CreateTodo {
            title: title.into(),
            completed: false,
        },
    )
    .await
}
async fn settle(transport: &mut SimulatedTransport, demo: &Demo) {
    for _ in 0..1000 {
        transport.pump(&demo.replicas).await.unwrap();
        transport.reconcile(&demo.replicas).await.unwrap();
        if transport.queued().is_empty() {
            return;
        }
    }
    panic!("transport did not drain");
}

#[tokio::test]
async fn bounded_queue_duplicate_reverse_delivery_and_slow_observer_converge() {
    let dir = TempDir::new().unwrap();
    let demo = Demo::open(dir.path()).await.unwrap();
    let mut transport = SimulatedTransport::new(3).unwrap();
    let mut observer = demo.replicas[1].observe().unwrap();
    observer.next().await.unwrap();
    for n in 0..40 {
        create(&demo, n % 2, &format!("todo {n}")).await;
    }
    transport.reconcile(&demo.replicas).await.unwrap();
    assert_eq!(transport.queued().len(), 3);
    assert!(matches!(
        transport.duplicate(0),
        Err(todo_desktop::transport::Error::QueueFull)
    ));
    // Successors arrive before predecessors, with durable pending receipts.
    transport.deliver(2, &demo.replicas).await.unwrap();
    transport.duplicate(0).unwrap();
    while !transport.queued().is_empty() {
        transport
            .deliver(transport.queued().len() - 1, &demo.replicas)
            .await
            .unwrap();
    }
    settle(&mut transport, &demo).await;
    let left = demo.replicas[0].query().await.unwrap();
    let right = observer.next().await.unwrap();
    assert_eq!(left.rows, right.rows);
    assert_eq!(left.rows.len(), 40);
    assert_eq!(
        demo.replicas[0].sync_state().await.unwrap(),
        demo.replicas[1].sync_state().await.unwrap()
    );
    let state = demo.replicas[0].sync_state().await.unwrap();
    for (index, peer) in [rocky_core::fixtures::PHONE, rocky_core::fixtures::LAPTOP]
        .into_iter()
        .enumerate()
    {
        let progress = demo.replicas[index].peer_progress(peer).await.unwrap();
        assert_eq!(progress.pending, 0);
        assert_eq!(progress.received, 20);
        assert_eq!(progress.applied, 20);
        assert_eq!(progress.frontier, state.applied);
    }
    let revisions = [left.revision, right.revision];
    for _ in 0..3 {
        transport.pump(&demo.replicas).await.unwrap();
    }
    assert!(transport.queued().is_empty());
    for (index, revision) in revisions.into_iter().enumerate() {
        assert_eq!(
            demo.replicas[index].query().await.unwrap().revision,
            revision
        );
    }
    demo.close().await.unwrap();
    let demo = Demo::open(dir.path()).await.unwrap();
    assert_eq!(demo.replicas[0].sync_state().await.unwrap(), state);
    assert_eq!(demo.replicas[1].sync_state().await.unwrap(), state);
    demo.close().await.unwrap();
}

#[tokio::test]
async fn lost_ack_sender_restart_cleared_outbox_and_queue_loss_reconcile() {
    let dir = TempDir::new().unwrap();
    let demo = Demo::open(dir.path()).await.unwrap();
    let mut transport = SimulatedTransport::new(2).unwrap();
    let first = create(&demo, 0, "retained").await;
    transport.reconcile(&demo.replicas).await.unwrap();
    transport.deliver(0, &demo.replicas).await.unwrap();
    // The acknowledgment is lost; reconciliation reports the phone's held
    // frontier instead, which clears the outgoing work without one.
    transport.clear();
    assert_eq!(
        demo.replicas[0]
            .peer_progress(rocky_core::fixtures::PHONE)
            .await
            .unwrap()
            .pending,
        1
    );
    transport.reconcile(&demo.replicas).await.unwrap();
    assert!(
        !transport
            .queued()
            .iter()
            .any(|d| matches!(d.message, Message::Acknowledgment(_)))
    );
    assert_eq!(
        demo.replicas[0]
            .peer_progress(rocky_core::fixtures::PHONE)
            .await
            .unwrap()
            .pending,
        0
    );
    transport.clear();
    demo.close().await.unwrap();
    let demo = Demo::open(dir.path()).await.unwrap();
    settle(&mut transport, &demo).await;
    assert_eq!(
        demo.replicas[0]
            .peer_progress(rocky_core::fixtures::PHONE)
            .await
            .unwrap()
            .pending,
        0
    );
    // A stale/missing reconciliation set still returns acknowledged retained history.
    assert_eq!(
        demo.replicas[0]
            .missing_operations(vec![])
            .await
            .unwrap()
            .records,
        vec![first.encode().unwrap()]
    );
    transport.set_paused(true);
    mutate(
        &demo,
        0,
        Some(first.id),
        Action::SetTitle("offline title".into()),
    )
    .await;
    mutate(&demo, 1, Some(first.id), Action::SetCompleted(true)).await;
    transport.pump(&demo.replicas).await.unwrap();
    assert!(!demo.replicas[0].query().await.unwrap().rows[0].completed);
    assert_eq!(
        demo.replicas[1].query().await.unwrap().rows[0].title,
        "retained"
    );
    assert_eq!(
        demo.replicas[0]
            .peer_progress(rocky_core::fixtures::PHONE)
            .await
            .unwrap()
            .pending,
        1
    );
    demo.close().await.unwrap();
    let demo = Demo::open(dir.path()).await.unwrap();
    assert_eq!(
        demo.replicas[0].query().await.unwrap().rows[0].title,
        "offline title"
    );
    assert!(demo.replicas[1].query().await.unwrap().rows[0].completed);
    transport.clear();
    transport.set_paused(false);
    settle(&mut transport, &demo).await;
    assert_eq!(
        demo.replicas[0].query().await.unwrap().rows,
        demo.replicas[1].query().await.unwrap().rows
    );
    assert_eq!(
        demo.replicas[0].query().await.unwrap().rows[0].title,
        "offline title"
    );
    assert!(demo.replicas[0].query().await.unwrap().rows[0].completed);
    demo.close().await.unwrap();
}

#[tokio::test]
async fn dropped_operation_retry_and_rejection_are_distinct() {
    let dir = TempDir::new().unwrap();
    let demo = Demo::open(dir.path()).await.unwrap();
    let mut transport = SimulatedTransport::new(1).unwrap();
    create(&demo, 0, "survives drop").await;
    transport.reconcile(&demo.replicas).await.unwrap();
    transport.drop_message(0).unwrap();
    settle(&mut transport, &demo).await;
    assert_eq!(
        demo.replicas[1].query().await.unwrap().rows[0].title,
        "survives drop"
    );
    transport
        .enqueue(Delivery {
            to: 1,
            message: Message::Operation(vec![0]),
        })
        .unwrap();
    assert!(matches!(
        transport.deliver(0, &demo.replicas).await,
        Err(todo_desktop::transport::Error::Rejected(
            rocky_runtime::Error::Protocol(_)
        ))
    ));
    assert!(transport.queued().is_empty());
    demo.close().await.unwrap();
}

/// The phone stays offline while the laptop checkpoints and prunes. The
/// transport then copies the checkpoint; the phone's own edit survives.
#[tokio::test]
async fn a_replica_behind_the_horizon_rebootstraps_through_the_transport() {
    let dir = TempDir::new().unwrap();
    let demo = Demo::open(dir.path()).await.unwrap();
    let mut transport = SimulatedTransport::new(64).unwrap();
    let offline = create(&demo, 1, "written offline").await;
    for n in 0..1100 {
        create(&demo, 0, &format!("todo {n}")).await;
    }
    let laptop = &demo.replicas[0];
    laptop
        .declare_behind(rocky_core::fixtures::PHONE)
        .await
        .unwrap();
    laptop.checkpoint().await.unwrap();
    let horizon = laptop.prune().await.unwrap();
    assert!(!horizon.0.is_empty());
    settle(&mut transport, &demo).await;
    let (ours, theirs) = (
        laptop.query().await.unwrap(),
        demo.replicas[1].query().await.unwrap(),
    );
    assert_eq!(ours.rows, theirs.rows);
    assert_eq!(ours.rows.len(), 1101);
    assert!(ours.rows.iter().any(|row| row.id == offline.id));
    for replica in &demo.replicas {
        assert!(replica.audit().await.unwrap().recomputed);
    }
    demo.close().await.unwrap();
}
