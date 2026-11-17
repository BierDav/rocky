//! Records another release wrote are refused by the format they name, at
//! open and when this node acknowledges, never read or skipped.
use super::seals::{bookmark, follow_main, seal};
use super::*;
use crate::TipStatement;

/// An earlier release's finality record: format 1, acknowledgments of
/// format 2 without this release's fields.
#[derive(serde::Serialize)]
enum Stored {
    Finality(EarlierFinality),
}
#[derive(serde::Serialize)]
struct EarlierFinality {
    format: u32,
    acknowledgments: Vec<EarlierAck>,
}
#[derive(serde::Serialize)]
struct EarlierAck {
    statement: EarlierAckStatement,
}
#[derive(serde::Serialize)]
struct EarlierAckStatement {
    format: u32,
}
fn earlier_ack() -> EarlierAck {
    EarlierAck {
        statement: EarlierAckStatement { format: 2 },
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_store_holding_another_releases_finality_record_refuses_to_open_by_its_format() {
    let Fixture {
        lineage,
        dir,
        alice: (key, alice),
        ..
    } = Fixture::new(1).await;
    let earlier = crate::encode(&Stored::Finality(EarlierFinality {
        format: 1,
        acknowledgments: vec![earlier_ack()],
    }))
    .unwrap();
    alice
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .conn
        .execute(
            "INSERT INTO secure_tip_statements(digest,statement,staged) VALUES(?,?,0)",
            crate::storage::bindings(vec![SqlValue::Blob(vec![7; 32]), SqlValue::Blob(earlier)]),
        )
        .await
        .unwrap();
    drop(alice);
    let Err(refused) = lineage
        .open_trusted(
            &dir.path().join("alice.db"),
            (&PrivateKey::new(), &key),
            3,
            None,
        )
        .await
    else {
        panic!("a store holding another release's finality record opened");
    };
    match &refused {
        Error::Incompatible(message) => assert!(
            message.contains("seal finality record of format 1, written by another release")
                && message.contains("reads only format 2"),
            "{message}"
        ),
        other => panic!("the store is refused as incompatible, by format: {other:?}"),
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_acknowledgment_chain_another_release_wrote_refuses_finality_by_its_format() {
    let fixture = Fixture::new(1).await;
    follow_main(&fixture).await;
    let (lineage, alice) = (&fixture.lineage, &fixture.alice);
    let main = tip_log(lineage);
    alice
        .1
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .conn
        .execute(
            "INSERT INTO secure_ack_chain(lane,ack) VALUES(?,?)",
            crate::storage::bindings(vec![
                SqlValue::Blob(alice.0.public_key().as_bytes().to_vec()),
                SqlValue::Blob(crate::encode(&earlier_ack()).unwrap()),
            ]),
        )
        .await
        .unwrap();
    let committed = seal(lineage, &alice.1, main.tip).await;
    let next = committed.successor().unwrap().unwrap();
    alice
        .1
        .publish_tip_statements(&[
            TipStatement::Seal(committed.clone()),
            bookmark(lineage, "main", Some(main.tip), Some(next)),
        ])
        .await
        .unwrap();
    let refused = alice.1.finalize_seals(lineage.domain).await.unwrap_err();
    assert!(
        matches!(
            &refused,
            Error::Incompatible(message)
                if message.contains("terminal acknowledgment of format 2, written by another release")
        ),
        "{refused}"
    );
    assert!(
        !alice
            .1
            .tip_graph(lineage.domain)
            .await
            .unwrap()
            .is_final(committed.id().unwrap())
            .unwrap(),
        "nothing is acknowledged over a chain this release cannot extend"
    );
}
