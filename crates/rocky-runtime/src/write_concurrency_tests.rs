//! Grouped incoming pages and the awaiting submit; see
//! `docs/runtime/write-concurrency.md`.
use super::*;
use crate::scaling_option_tests::{NODE, PEER, grouped, note, open_node, pause, scope};
use crate::worker::{Fault, GroupLog};
use inventory_app::InventoryApp;
use tempfile::TempDir;

/// Encoded operations of `PEER` in domain 1, in author order.
pub(super) async fn peer_operations(dir: &Path, count: u8) -> Vec<Vec<u8>> {
    let source = open_node(&dir.join("peer.db"), PEER, RuntimeOptions::default()).await;
    let replica = source.domain(scope(1)).unwrap();
    let mut operations = Vec::new();
    for index in 0..count {
        let operation = note(&replica, index, &format!("remote-{index}"));
        operations.push(operation.committed().await.unwrap().encode().unwrap());
    }
    source.close().await.unwrap();
    operations
}
async fn rows(replica: &Replica<InventoryApp>) -> usize {
    replica.query().await.unwrap().rows.len()
}

#[tokio::test]
async fn a_refused_or_pending_record_leaves_the_rest_of_its_page_committed() {
    let dir = TempDir::new().unwrap();
    let operations = peer_operations(dir.path(), 4).await;
    let runtime = open_node(&dir.path().join("node.db"), NODE, grouped()).await;
    let replica = runtime.domain(scope(1)).unwrap();
    let log = GroupLog::default();
    crate::tests::fault(&replica, Fault::RecordGroups(log.clone())).await;
    let id = |index: usize| Envelope::decode(&operations[index]).unwrap().id;
    // Accepted, undecodable, waiting for its predecessor, and a duplicate.
    let page = vec![
        operations[0].clone(),
        vec![1, 2, 3],
        operations[2].clone(),
        operations[0].clone(),
    ];
    let results = replica.receive_page(page).await.unwrap();
    let applied = |index: usize, applied: bool| Acknowledgment {
        id: id(index),
        applied,
    };
    assert_eq!(results[0].as_ref().unwrap(), &applied(0, true));
    assert!(matches!(results[1], Err(Error::Protocol(_))));
    assert_eq!(results[2].as_ref().unwrap(), &applied(2, false));
    assert_eq!(results[3].as_ref().unwrap(), &applied(0, true));
    assert_eq!(rows(&replica).await, 1);
    // A receipt that fails inside its savepoint rolls back alone.
    crate::tests::fault(&replica, Fault::FailBeforeCommit).await;
    let results = replica
        .receive_page(vec![operations[1].clone(), operations[3].clone()])
        .await
        .unwrap();
    assert!(matches!(results[0], Err(Error::InjectedFailure)));
    assert_eq!(results[1].as_ref().unwrap(), &applied(3, false));
    assert_eq!(rows(&replica).await, 1);
    let state = replica.sync_state().await.unwrap();
    assert_eq!(state.received, vec![id(2), id(3)]);
    assert!(state.holds(id(0)) && !state.holds(id(1)));
    // The missing predecessor releases everything that waited for it.
    let results = replica
        .receive_page(vec![operations[1].clone()])
        .await
        .unwrap();
    assert_eq!(results[0].as_ref().unwrap(), &applied(1, true));
    assert_eq!(rows(&replica).await, 4);
    // Each page was one step of one group: one commit and one flush.
    let groups = log.lock().unwrap().clone();
    assert_eq!(groups.len(), 3);
    assert!(groups.iter().all(|group| group.len() == 1));
    runtime.close().await.unwrap();
    let runtime = open_node(&dir.path().join("node.db"), NODE, grouped()).await;
    assert_eq!(rows(&runtime.domain(scope(1)).unwrap()).await, 4);
    runtime.close().await.unwrap();
}

#[tokio::test]
async fn a_page_is_split_into_bounded_batches_in_input_order() {
    let dir = TempDir::new().unwrap();
    let count = 2 * crate::replication::TRANSFER_PAGE_SIZE as u8 + 3;
    let mut page = peer_operations(dir.path(), count).await;
    page.insert(5, vec![0; MAX_FRAME_BYTES]);
    let runtime = open_node(&dir.path().join("node.db"), NODE, grouped()).await;
    let replica = runtime.domain(scope(1)).unwrap();
    let log = GroupLog::default();
    crate::tests::fault(&replica, Fault::RecordGroups(log.clone())).await;
    let results = replica.receive_page(page).await.unwrap();
    assert_eq!(results.len(), usize::from(count) + 1);
    for (index, result) in results.iter().enumerate() {
        match result {
            Ok(ack) => assert!(ack.applied && index != 5),
            Err(error) => assert!(index == 5 && matches!(error, Error::Protocol(_))),
        }
    }
    assert_eq!(rows(&replica).await, usize::from(count));
    assert_eq!(log.lock().unwrap().len(), 3);
    runtime.close().await.unwrap();
}

#[tokio::test]
async fn submit_waits_for_queue_space_instead_of_reporting_backpressure() {
    let dir = TempDir::new().unwrap();
    let runtime = open_node(&dir.path().join("node.db"), NODE, grouped()).await;
    let replica = runtime.domain(scope(1)).unwrap();
    assert_eq!(replica.queue_depth().commands, 0);
    let (first, resume) = pause(&replica).await;
    let mutation = |index: usize| Mutation {
        entity: None,
        payload: format!("queued-{index}").into_bytes(),
    };
    let mut queued = Vec::new();
    loop {
        match replica.mutate(mutation(queued.len())) {
            Ok(receipt) => queued.push(receipt),
            Err(Error::Backpressure) => break,
            Err(error) => panic!("{error}"),
        }
    }
    let depth = replica.queue_depth();
    assert_eq!(depth.commands, depth.command_capacity);
    assert_eq!(queued.len(), depth.command_capacity);
    assert!(depth.bytes > 0 && depth.bytes < depth.byte_capacity);
    let waiting = tokio::spawn({
        let replica = replica.clone();
        async move {
            let receipt = replica.submit(mutation(1000)).await.unwrap();
            receipt.committed().await.unwrap()
        }
    });
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    assert!(!waiting.is_finished());
    resume.send(()).unwrap();
    first.committed().await.unwrap();
    let count = queued.len() as u64;
    for receipt in queued {
        receipt.committed().await.unwrap();
    }
    // Admitted after everything that was already queued.
    assert_eq!(waiting.await.unwrap().id.sequence.get(), count + 2);
    assert_eq!(replica.queue_depth().commands, 0);
    let stats = replica.writer_stats();
    assert!(stats.steps >= count + 2 && stats.groups >= 2 && stats.groups < stats.steps);
    runtime.close().await.unwrap();
}
