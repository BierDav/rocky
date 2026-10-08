//! Grouped incoming pages, merged strong steps and the awaiting submit; see
//! `docs/runtime/write-concurrency.md`.
use super::*;
use crate::scaling_option_tests::{NODE, PEER, grouped, note, open_node, pause, scope};
use crate::worker::{Fault, GroupLog};
use inventory_app::InventoryApp;
use rocky_core::*;
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
pub(super) fn coordinator() -> rocky_coordinator::MemoryCoordinator {
    let coordinator = rocky_coordinator::MemoryCoordinator::default();
    let owner = PrincipalId([7; 16]);
    coordinator
        .register(scope(1), owner, [6; 16], vec![NODE], 1, 1)
        .unwrap();
    coordinator.acquire(scope(1), owner, NODE).unwrap();
    coordinator
}
/// A published request, its prepared package and the coordinator's receipt.
pub(super) async fn confirmed(
    replica: &Replica<InventoryApp>,
    coordinator: &rocky_coordinator::MemoryCoordinator,
) -> (StrongPackage, StrongReceipt) {
    let request = replica
        .submit_command_with_arguments(
            CommandHandle([1; 32]),
            inventory_app::RESERVE,
            1u64.to_le_bytes().to_vec(),
            [9; 32],
        )
        .unwrap()
        .committed()
        .await
        .unwrap();
    let head = coordinator.head(scope(1)).unwrap();
    let prepared = replica
        .strong_request(strong::Request::Prepare(Box::new(request), head))
        .await
        .unwrap();
    let strong::Response::Package(package) = prepared else {
        panic!("package");
    };
    let receipt = coordinator
        .confirm(package.confirmation().unwrap())
        .unwrap();
    (package, receipt)
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

/// Store, verify and apply are one commit: when it fails, none of them
/// happened and the command is still an unknown outcome to look up.
#[tokio::test]
async fn a_failed_commit_of_a_received_result_leaves_none_of_its_steps() {
    let dir = TempDir::new().unwrap();
    let runtime = open_node(&dir.path().join("node.db"), NODE, grouped()).await;
    let replica = runtime.domain(scope(1)).unwrap();
    let coordinator = coordinator();
    let (package, receipt) = confirmed(&replica, &coordinator).await;
    let before = replica.query().await.unwrap();
    crate::tests::fault(&replica, Fault::FailCommit).await;
    assert!(
        replica
            .receive_result(package.encode().unwrap(), receipt.encode(), &coordinator)
            .await
            .is_err()
    );
    let status = replica.strong_status().await.unwrap();
    assert!(status.received.is_empty() && status.verification.is_empty());
    assert_eq!(status.sequence, 0);
    assert_eq!(status.commands[0].state, CommandState::OutcomeUnknown);
    assert_eq!(replica.query().await.unwrap(), before);
    replica.execute_pending(&coordinator).await.unwrap();
    assert_eq!(replica.confirmed_publications().await.unwrap().len(), 1);
    assert_eq!(replica.strong_status().await.unwrap().sequence, 1);
    assert_eq!(coordinator.head(scope(1)).unwrap().sequence, 1);
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
