use rocky_core::fixtures;
use rocky_runtime::{CommandState, Mutation};
use tempfile::TempDir;
use todo_app::{Action, decode_snapshot};
use todo_desktop::{
    Demo,
    transport::{Delivery, Message, SimulatedTransport},
};
async fn create(demo: &Demo, node: usize, title: &str) {
    demo.replicas[node]
        .mutate(Mutation {
            entity: None,
            payload: Action::CreateTodo {
                title: title.into(),
                completed: false,
            }
            .encode()
            .unwrap(),
        })
        .unwrap()
        .committed()
        .await
        .unwrap();
}
async fn settle(demo: &Demo, transport: &mut SimulatedTransport) {
    for _ in 0..40 {
        transport.pump(&demo.replicas).await.unwrap();
        demo.pump_coordination(transport).await.unwrap();
    }
}
#[tokio::test]
async fn phone_publish_outage_takeover_and_full_restart_ledger_loss() {
    let dir = TempDir::new().unwrap();
    let demo = Demo::open(dir.path()).await.unwrap();
    let mut transport = SimulatedTransport::new(3).unwrap();
    create(&demo, 0, "original").await;
    settle(&demo, &mut transport).await;
    demo.replicas[1]
        .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    settle(&demo, &mut transport).await;
    for replica in &demo.replicas {
        let p = replica.confirmed_publications().await.unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(
            decode_snapshot(&p[0].package.rows).unwrap()[0].title,
            "original"
        );
        assert_eq!(
            replica.strong_status().await.unwrap().commands[0].state,
            CommandState::PeerApplied
        );
    }
    demo.coordinator.set_available(false).unwrap();
    demo.replicas[1]
        .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    create(&demo, 1, "after sealed cut").await;
    for _ in 0..8 {
        transport.pump(&demo.replicas).await.unwrap();
        assert!(demo.pump_coordination(&mut transport).await.is_err());
    }
    assert_eq!(demo.replicas[0].query().await.unwrap().rows.len(), 2);
    assert_eq!(
        demo.replicas[0]
            .confirmed_publications()
            .await
            .unwrap()
            .len(),
        1
    );
    demo.coordinator.set_available(true).unwrap();
    settle(&demo, &mut transport).await;
    let p = demo.replicas[1].confirmed_publications().await.unwrap();
    assert_eq!(p.len(), 2);
    assert_eq!(decode_snapshot(&p[1].package.rows).unwrap().len(), 1);
    demo.takeover_phone().await.unwrap();
    assert_eq!(
        demo.coordinator.head(fixtures::SCOPE).unwrap().executor,
        fixtures::PHONE
    );
    demo.replicas[0]
        .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    settle(&demo, &mut transport).await;
    assert_eq!(
        demo.replicas[0]
            .confirmed_publications()
            .await
            .unwrap()
            .len(),
        3
    );
    demo.close().await.unwrap();
    let reopened = Demo::open(dir.path()).await.unwrap();
    assert!(
        reopened.replicas[0]
            .execute_pending(reopened.coordinator.as_ref())
            .await
            .unwrap_err()
            .to_string()
            .contains("CoordinationStateLost")
    );
    create(&reopened, 0, "ordinary edits after ledger loss").await;
    assert_eq!(
        reopened.replicas[0]
            .confirmed_publications()
            .await
            .unwrap()
            .len(),
        3
    );
    reopened.close().await.unwrap();
}
#[tokio::test]
async fn strong_results_use_bounded_queue_duplicate_reordering_and_retained_history() {
    let dir = TempDir::new().unwrap();
    let demo = Demo::open(dir.path()).await.unwrap();
    let mut transport = SimulatedTransport::new(4).unwrap();
    for _ in 0..2 {
        demo.replicas[0]
            .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
            .await
            .unwrap();
        demo.replicas[0]
            .execute_pending(demo.coordinator.as_ref())
            .await
            .unwrap();
    }
    let results = demo.replicas[0].strong_results().await.unwrap();
    for result in results.iter().rev() {
        transport
            .enqueue(Delivery {
                to: 1,
                message: Message::StrongResult {
                    package: result.package.encode().unwrap(),
                    receipt: result.receipt.encode(),
                },
            })
            .unwrap();
    }
    transport.duplicate(0).unwrap();
    while transport
        .deliver_strong(0, &demo.replicas, demo.coordinator.as_ref())
        .await
        .unwrap()
    {}
    assert_eq!(
        demo.replicas[1]
            .confirmed_publications()
            .await
            .unwrap()
            .len(),
        2
    );
    settle(&demo, &mut transport).await;
    transport.clear();
    settle(&demo, &mut transport).await;
    assert_eq!(
        demo.replicas[1]
            .confirmed_publications()
            .await
            .unwrap()
            .len(),
        2
    );
    demo.close().await.unwrap();
}

#[tokio::test]
async fn ordinary_sync_progresses_behind_a_blocked_strong_frame() {
    let dir = TempDir::new().unwrap();
    let demo = Demo::open(dir.path()).await.unwrap();
    let mut transport = SimulatedTransport::new(4).unwrap();
    demo.replicas[0]
        .request_command(todo_app::PUBLISH_LIST_SNAPSHOT)
        .await
        .unwrap();
    demo.replicas[0]
        .execute_pending(demo.coordinator.as_ref())
        .await
        .unwrap();
    let result = demo.replicas[0].strong_results().await.unwrap().remove(0);
    transport
        .enqueue(Delivery {
            to: 1,
            message: Message::StrongResult {
                package: result.package.encode().unwrap(),
                receipt: result.receipt.encode(),
            },
        })
        .unwrap();
    create(&demo, 0, "edit behind strong frame").await;
    // The ordinary pump has no coordinator port and must still move edits.
    transport.pump(&demo.replicas).await.unwrap();
    assert_eq!(demo.replicas[1].query().await.unwrap().rows.len(), 1);
    assert_eq!(transport.queued().len(), 1);
    demo.close().await.unwrap();
}
