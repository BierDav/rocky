use super::*;

const RED: &str = "INSERT INTO teams(name,cap,id) VALUES('red',2,#id)";

fn split_records(
    c: &Constrained,
    frames: Vec<EncryptedFrame>,
) -> (Vec<EncryptedFrame>, Vec<EncryptedFrame>) {
    frames
        .into_iter()
        .partition(|frame| c.payload(frame).outcome().is_none())
}

fn position(frame: &EncryptedFrame) -> (u64, Option<rocky_core::WriterId>) {
    let header = &frame.operation_ref().unwrap().header;
    (
        header.timestamp,
        header.extensions.author(&header.public_key),
    )
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_outcome_record_names_no_frame_of_another_author() {
    let c = Constrained::new();
    let root = rocky_platform::fs::Scratch::new().await;
    let (mut devices, ids, _) = devices(&c, root.path(), 2, &[RED]).await;
    let red = ids[0];
    devices[0]
        .commit(&c, &[&format!("UPDATE teams SET cap=3 WHERE id={red}")])
        .await
        .unwrap();
    let raise = devices[0].fresh().await;
    devices[1].receive(&raise).await;
    devices[1]
        .commit(
            &c,
            &[
                &format!("UPDATE teams SET name='blue' WHERE id={red}"),
                &format!("INSERT INTO members(team,email,id) VALUES({red},'p@example',#id)"),
            ],
        )
        .await
        .unwrap();
    let (intents, records) = split_records(&c, devices[1].fresh().await);
    let names_another_author =
        |frame: &EncryptedFrame| !frame.operation_ref().unwrap().header.previous.is_empty();
    assert!(
        intents.iter().any(names_another_author),
        "the intents build on the other author's raise"
    );
    assert!(!records.is_empty());
    assert!(
        !records.iter().any(names_another_author),
        "a device that joins after compaction pruned the raise still applies the records"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_frame_stamped_between_a_transactions_members_replays_after_it_in_either_arrival_order() {
    let c = Constrained::new();
    let root = rocky_platform::fs::Scratch::new().await;
    let (mut devices, ids, _) = devices(&c, root.path(), 4, &[RED]).await;
    let red = ids[0];
    devices[0]
        .commit(
            &c,
            &[&format!(
                "INSERT INTO members(team,email,id) VALUES({red},'q@example',#id)"
            )],
        )
        .await
        .unwrap();
    let (mut joined, _) = split_records(&c, devices[0].fresh().await);
    let joined = joined.pop().unwrap();
    devices[1]
        .commit(
            &c,
            &[
                &format!("UPDATE teams SET cap=1 WHERE id={red}"),
                &format!("INSERT INTO members(team,email,id) VALUES({red},'p@example',#id)"),
            ],
        )
        .await
        .unwrap();
    let (shrink, _) = split_records(&c, devices[1].fresh().await);
    let first = shrink.iter().min_by_key(|frame| position(frame)).unwrap();
    let last = shrink.iter().max_by_key(|frame| position(frame)).unwrap();
    assert_eq!(first.scope().unwrap(), c.scope("members"));
    assert_eq!(last.scope().unwrap(), c.scope("teams"));
    let clock = match position(&joined).1 < position(first).1 {
        true => position(last).0,
        false => position(first).0,
    };
    let between = restamped(&joined, &devices[0].node, clock);
    assert!(position(first) < position(&between) && position(&between) < position(last));

    let (earlier, later) = (&devices[2], &devices[3]);
    earlier.receive(std::slice::from_ref(&between)).await;
    earlier.receive(&shrink).await;
    later.receive(&shrink).await;
    later.receive(std::slice::from_ref(&between)).await;
    for device in [earlier, later] {
        assert_eq!(
            device
                .replica
                .transaction_outcome(c.transaction(&shrink))
                .await
                .unwrap(),
            Some(TransactionOutcome::Applied { sealed: false }),
            "the shrink's first member precedes the second member's join"
        );
        assert!(matches!(
            device
                .replica
                .transaction_outcome(c.transaction(std::slice::from_ref(&between)))
                .await
                .unwrap(),
            Some(TransactionOutcome::Dropped {
                reason: DropReason::Constraint,
                ..
            })
        ));
    }
    assert_eq!(earlier.rows(&c).await, later.rows(&c).await);
    let (_, earlier_records) = split_records(&c, devices[2].fresh().await);
    let (_, later_records) = split_records(&c, devices[3].fresh().await);
    devices[2].receive(&later_records).await;
    devices[3].receive(&earlier_records).await;
    for device in &devices[2..] {
        assert!(
            device
                .replica
                .outcome_divergences()
                .await
                .unwrap()
                .is_empty()
        );
    }
}
