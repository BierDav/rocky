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
