//! Range reconciliation and full-node custody on the trusted path.
use super::*;
use crate::tests::{create, mutation};
use rocky_core::fixtures;
use tempfile::TempDir;
use todo_app::{Action, StaticPolicy, TodoApp};

const SERVER: NodeId = NodeId([0x77; 16]);

fn config(node: NodeId, full_nodes: &[NodeId]) -> RuntimeConfig {
    let nodes = [fixtures::LAPTOP, fixtures::PHONE, SERVER];
    RuntimeConfig {
        authors: nodes.iter().map(|node| (*node, fixtures::OWNER)).collect(),
        delivery_targets: nodes.into_iter().filter(|other| *other != node).collect(),
        full_nodes: full_nodes.to_vec(),
    }
}
fn manual(custody: usize) -> RuntimeOptions {
    RuntimeOptions {
        retention: RetentionPolicy {
            window: 0,
            automatic: false,
            custody,
            ..RetentionPolicy::default()
        },
        ..RuntimeOptions::default()
    }
}
async fn open(
    dir: &TempDir,
    node: NodeId,
    full_nodes: &[NodeId],
    custody: usize,
) -> Replica<TodoApp> {
    Replica::open_with_options(
        dir.path().join(format!("{:02x}.db", node.0[0])),
        TodoApp,
        StaticPolicy::default(),
        Identity {
            node,
            ..crate::tests::identity()
        },
        config(node, full_nodes),
        manual(custody),
    )
    .await
    .unwrap()
}
/// One range pass from `from` to `to`; returns the pages and records sent,
/// or `None` when `to` is behind the pruned_prefix.
async fn pass(
    from: &Replica<TodoApp>,
    to: &Replica<TodoApp>,
    peer: NodeId,
) -> Option<(usize, usize)> {
    let state = to.sync_state().await.unwrap();
    let (mut pages, mut records) = (0, 0);
    let mut after = None;
    loop {
        let page = match from
            .catch_up_after(Some(peer), state.clone(), after)
            .await
            .unwrap()
        {
            Catchup::Operations(page) => page,
            Catchup::BehindPruned(_) => return None,
        };
        pages += 1;
        records += page.records.len();
        assert!(page.records.len() <= TRANSFER_PAGE_SIZE);
        for result in to.receive_page(page.records).await.unwrap() {
            result.unwrap();
        }
        to.process_pending().await.unwrap();
        after = page.next;
        if after.is_none() {
            // Report the applied frontier, which acknowledges the pass.
            from.catch_up(Some(peer), to.sync_state().await.unwrap())
                .await
                .unwrap();
            return Some((pages, records));
        }
    }
}
fn at(frontier: &rocky_core::AppliedFrontier, node: NodeId) -> u64 {
    frontier
        .0
        .iter()
        .find(|(author, _)| *author == node)
        .map_or(0, |(_, sequence)| sequence.get())
}

#[tokio::test]
async fn range_reconciliation_exchanges_frontiers_not_receipt_sets() {
    let dir = TempDir::new().unwrap();
    let laptop = open(&dir, fixtures::LAPTOP, &[], 0).await;
    let phone = open(&dir, fixtures::PHONE, &[], 0).await;
    let server = open(&dir, SERVER, &[], 0).await;
    let mut created = Vec::new();
    for batch in (0..300).step_by(8) {
        let pending: Vec<_> = (batch..(batch + 8).min(300))
            .map(|index| {
                laptop
                    .mutate(mutation(
                        None,
                        Action::CreateTodo {
                            title: format!("todo {index}"),
                            completed: false,
                        },
                    ))
                    .unwrap()
            })
            .collect();
        for receipt in pending {
            created.push(receipt.committed().await.unwrap());
        }
    }
    assert_eq!(
        pass(&laptop, &phone, fixtures::PHONE).await,
        Some((10, 300))
    );
    // The state stays small however long the history is.
    let state = phone.sync_state().await.unwrap();
    assert_eq!(at(&state.held, fixtures::LAPTOP), 300);
    assert!(state.received.is_empty());
    assert_eq!(pass(&laptop, &phone, fixtures::PHONE).await, Some((1, 0)));
    // The frontier counts as receipt: the outbox is cleared for that peer.
    let progress = laptop.peer_progress(fixtures::PHONE).await.unwrap();
    assert_eq!((progress.received, progress.applied), (300, 300));

    // Out of order: more held operations above a gap than a state lists.
    let bytes = |id: OperationId| {
        let laptop = laptop.clone();
        async move {
            laptop
                .operation(rocky_core::ScopedOperationId {
                    scope: fixtures::SCOPE,
                    operation: id,
                })
                .await
                .unwrap()
                .unwrap()
        }
    };
    for batch in created[1..].chunks(32) {
        let mut records = Vec::new();
        for envelope in batch {
            records.push(bytes(envelope.id).await);
        }
        for result in server.receive_page(records).await.unwrap() {
            result.unwrap();
        }
    }
    let state = server.sync_state().await.unwrap();
    assert_eq!(at(&state.held, fixtures::LAPTOP), 0);
    assert_eq!(state.received.len(), MAX_SYNC_EXCEPTIONS);
    // The pass still ends, resending only what the state could not list.
    let (_, records) = pass(&laptop, &server, SERVER).await.unwrap();
    assert_eq!(records, 1 + 299 - MAX_SYNC_EXCEPTIONS);
    let state = server.sync_state().await.unwrap();
    assert_eq!(at(&state.applied, fixtures::LAPTOP), 300);
    assert_eq!(
        server.query().await.unwrap().rows,
        laptop.query().await.unwrap().rows
    );
}

#[tokio::test]
async fn full_node_custody_moves_the_pruned_prefix_without_waiting_for_devices() {
    let dir = TempDir::new().unwrap();
    let laptop = open(&dir, fixtures::LAPTOP, &[SERVER], 1).await;
    let phone = open(&dir, fixtures::PHONE, &[SERVER], 1).await;
    let server = open(&dir, SERVER, &[], 0).await;
    let first = create(&laptop, "first").await;
    for index in 0..40 {
        create(&laptop, &format!("todo {index}")).await;
    }
    laptop.checkpoint().await.unwrap();
    // Receipt is not custody, and the phone is no full node.
    for envelope in [&first] {
        let bytes = laptop
            .operation(rocky_core::ScopedOperationId {
                scope: fixtures::SCOPE,
                operation: envelope.id,
            })
            .await
            .unwrap()
            .unwrap();
        let mut ack = server.receive(bytes).await.unwrap();
        ack.applied = false;
        laptop.acknowledge(SERVER, ack).await.unwrap();
    }
    assert_eq!(pass(&laptop, &phone, fixtures::PHONE).await, Some((2, 41)));
    assert_eq!(at(&laptop.prune().await.unwrap(), fixtures::LAPTOP), 0);
    // The full node applied everything: the pruned prefix follows it alone.
    assert!(pass(&laptop, &server, SERVER).await.is_some());
    let edit = laptop
        .mutate(mutation(Some(first.id), Action::SetCompleted(true)))
        .unwrap()
        .committed()
        .await
        .unwrap();
    laptop.checkpoint().await.unwrap();
    assert_eq!(at(&laptop.prune().await.unwrap(), fixtures::LAPTOP), 41);
    let status = laptop.retention_status().await.unwrap();
    let full = status
        .peers
        .iter()
        .find(|peer| peer.peer == SERVER)
        .unwrap();
    assert!(full.full_node && at(&full.applied, fixtures::LAPTOP) == 41);
    assert!(
        status
            .peers
            .iter()
            .any(|peer| peer.peer == fixtures::PHONE && !peer.full_node)
    );

    // A device that fell behind takes the full node's checkpoint.
    let late = Replica::open_with_options(
        dir.path().join("late.db"),
        TodoApp,
        StaticPolicy::default(),
        Identity {
            node: fixtures::PHONE,
            ..crate::tests::identity()
        },
        config(fixtures::PHONE, &[SERVER]),
        manual(1),
    )
    .await
    .unwrap();
    assert_eq!(pass(&laptop, &late, fixtures::PHONE).await, None);
    assert!(pass(&laptop, &server, SERVER).await.is_some());
    server.checkpoint().await.unwrap();
    let mut cursor = None;
    loop {
        let page = server.checkpoint_page(cursor).await.unwrap();
        cursor = page.next;
        if late.import_checkpoint(page).await.unwrap().is_some() {
            break;
        }
    }
    assert!(pass(&laptop, &late, fixtures::PHONE).await.is_some());
    assert_eq!(
        late.query().await.unwrap().rows,
        laptop.query().await.unwrap().rows
    );
    assert!(
        late.query()
            .await
            .unwrap()
            .frontier
            .0
            .contains(&(fixtures::LAPTOP, edit.id.sequence))
    );
}

#[tokio::test]
async fn custody_needs_configured_full_nodes() {
    let dir = TempDir::new().unwrap();
    let result = Replica::open_with_options(
        dir.path().join("a.db"),
        TodoApp,
        StaticPolicy::default(),
        crate::tests::identity(),
        config(fixtures::LAPTOP, &[]),
        manual(1),
    )
    .await;
    assert!(matches!(result, Err(Error::Incompatible(_))));
    let mut stray = config(fixtures::LAPTOP, &[]);
    stray.full_nodes = vec![NodeId([0x99; 16])];
    let result = Replica::open_with_options(
        dir.path().join("b.db"),
        TodoApp,
        StaticPolicy::default(),
        crate::tests::identity(),
        stray,
        manual(0),
    )
    .await;
    assert!(matches!(result, Err(Error::Incompatible(_))));
}
