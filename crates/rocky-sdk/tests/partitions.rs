use rocky_sdk::{Database, Network, Open, Session, transport::memory::Hub};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
const SCHEMA: &str = r#"{"format":5,"application":"partitionsdk","version":1,"tables":[{"name":"items","class":"mergeable","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true},{"name":"amount","type":"integer","not_null":true}],"primary_key":["list","key"],"partition_by":["list"]}],"views":[{"name":"partial_items","create_sql":"CREATE VIEW partial_items AS SELECT * FROM items WHERE row_kind=0"}]}"#;
fn open<'a>(directory: &'a Path, hub: &Arc<Hub>) -> Open<'a> {
    let name = directory.file_name().unwrap().to_str().unwrap();
    Open {
        directory,
        schema: SCHEMA,
        secrets: None,
        network: Some(Network::Owned(hub.connector(name))),
        commands: None,
    }
}
async fn reply(session: &Session, scope: &Value, request: Value) -> Value {
    serde_json::from_str(
        &session
            .call(json!({"version":1,"scope":scope,"request":request}).to_string())
            .await
            .unwrap(),
    )
    .unwrap()
}
async fn call(session: &Session, scope: &Value, request: Value) -> Value {
    let reply = reply(session, scope, request).await;
    assert_eq!(reply["status"], "ok", "{reply}");
    reply["value"].clone()
}
fn values(name: &str) -> Value {
    json!([{"type":"text","value":name}])
}
#[tokio::test]
async fn public_partition_schema_keys_sql_identity_and_reopen() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("writer");
    let hub = Arc::new(Hub::default());
    let db = Database::create(open(&path, &hub), None).unwrap().database;
    let scope: Value = serde_json::from_str(&db.scope()).unwrap();
    let session = db.session();
    for name in ["A", "B"] {
        call(
            &session,
            &scope,
            json!({"type":"partition_create","table":"items","values":values(name)}),
        )
        .await;
    }
    let requested = call(
        &session,
        &scope,
        json!({"type":"pseudonym_request","collection":"items"}),
    )
    .await;
    let assigned = call(
        &session,
        &scope,
        json!({"type":"pseudonym_assign","request":requested["request"]}),
    )
    .await;
    call(
        &session,
        &scope,
        json!({"type":"pseudonym_install","pseudonym":assigned["pseudonym"]}),
    )
    .await;
    let mut ids = Vec::new();
    for (name, amount) in [("A", "10"), ("B", "20")] {
        call(&session, &scope, json!({"type":"sql","sql":"BEGIN"})).await;
        let id = call(
            &session,
            &scope,
            json!({"type":"next_id","table":"items","partition_values":values(name)}),
        )
        .await["id"]
            .clone();
        ids.push(id.clone());
        call(&session,&scope,json!({"type":"sql","sql":"INSERT INTO items(list,key,amount,id) VALUES(?,?,?,?)","values":[{"type":"text","value":name},{"type":"text","value":"key"},{"type":"integer","value":amount},{"type":"integer","value":id}]})).await;
        call(&session, &scope, json!({"type":"sql","sql":"COMMIT"})).await;
    }
    assert_ne!(ids[0], ids[1], "one creation log per author and table");
    let driver = db
        .connect(rocky_sdk::ConnectionOptions {
            read_only: false,
            profile: None,
            transaction_timeout: None,
        })
        .await
        .unwrap();
    driver
        .execute(driver.statement(), "BEGIN".into(), vec![])
        .await
        .unwrap();
    let a_id = driver
        .execute(
            driver.statement(),
            "SELECT rocky_next_id('items', ?)".into(),
            vec![rocky_sdk::SqlValue::Text("A".into())],
        )
        .await
        .unwrap();
    let b_id = driver
        .execute(
            driver.statement(),
            "SELECT rocky_next_id('items', ?)".into(),
            vec![rocky_sdk::SqlValue::Text("B".into())],
        )
        .await
        .unwrap();
    assert_ne!(a_id.rows, b_id.rows);
    driver
        .execute(driver.statement(), "ROLLBACK".into(), vec![])
        .await
        .unwrap();

    let data=call(&session,&scope,json!({"type":"read","query":{"sql":"SELECT SUM(amount) FROM partial_items","values":[],"max_rows":1}})).await;
    assert_eq!(data["rows"][0][0]["value"], "30");
    let address = call(
        &session,
        &scope,
        json!({"type":"partition_address","table":"items","values":values("A")}),
    )
    .await;
    let parts = address["tuple"].as_array().unwrap();
    let selector = json!([[parts[0][0],{"Only":[parts[0][1]]}]]);
    let reader_path = root.path().join("reader");
    let device = Database::device(&reader_path, None).unwrap();
    let supplied=call(&session,&scope,json!({"type":"partition_grant","device":device,"partitions":[{"domain":scope,"table":address["scope"]["table"],"selector":selector,"rows":"All","rights":["Read"],"fields":[]}]})).await;
    let reader = Database::install(
        open(&reader_path, &hub),
        serde_json::from_value(supplied["supplied"].clone()).unwrap(),
    )
    .unwrap()
    .database;
    let reader_session = reader.session();
    call(&session,&scope,json!({"type":"partition_share","table":"items","values":values("A"),"recipients":[device.node]})).await;
    // The reader receives the envelope by syncing with its sender.
    let contact = call(&session, &scope, json!({"type":"contact"})).await;
    call(
        &reader_session,
        &scope,
        json!({"type":"peer_add","contact":contact["uri"]}),
    )
    .await;
    call(&reader_session, &scope, json!({"type":"sync"})).await;
    assert_eq!(
        call(
            &reader_session,
            &scope,
            json!({"type":"partition_status","table":"items","values":values("A")})
        )
        .await["epochs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_ne!(
        reply(
            &reader_session,
            &scope,
            json!({"type":"partition_register","table":"items","values":values("B")})
        )
        .await["status"],
        "ok"
    );
    assert!(
        !call(
            &reader_session,
            &scope,
            json!({"type":"partition_complete","table":"items","values":values("A")})
        )
        .await["complete"]
            .as_bool()
            .unwrap()
    );
    call(
        &reader_session,
        &scope,
        json!({"type":"install","supplied":supplied["supplied"]}),
    )
    .await;
    reader.close().await.unwrap();
    Database::open(open(&reader_path, &hub))
        .unwrap()
        .database
        .close()
        .await
        .unwrap();
    db.close().await.unwrap();
    let db = Database::open(open(&path, &hub)).unwrap().database;
    let session = db.session();
    call(&session, &scope, json!({"type":"sql","sql":"BEGIN"})).await;
    let id = call(
        &session,
        &scope,
        json!({"type":"next_id","table":"items","partition_values":values("A")}),
    )
    .await;
    assert_eq!(
        id["id"].as_str().unwrap().parse::<i64>().unwrap(),
        ids[0].as_str().unwrap().parse::<i64>().unwrap() + 2
    );
    call(&session, &scope, json!({"type":"sql","sql":"ROLLBACK"})).await;
    call(
        &session,
        &scope,
        json!({"type":"partition_create","table":"items","values":values("C")}),
    )
    .await;
    let request = json!({"type":"move_row","table":"items","id":ids[0],"from":values("A"),"to":values("C"),"handle":vec![89u8;32]});
    let moved = call(&session, &scope, request.clone()).await;
    assert_eq!(moved["committed"]["frames"], 2);
    assert_eq!(call(&session, &scope, request).await, moved);
    let moved_rows = call(&session, &scope, json!({"type":"read","query":{"sql":"SELECT list,id FROM items WHERE list='C' AND row_kind=0","values":[],"max_rows":2}})).await;
    assert_eq!(moved_rows["rows"][0][1]["value"], ids[0]);
    let revoked = call(
        &session,
        &scope,
        json!({"type":"revoke","node":device.node.to_string()}),
    )
    .await;
    assert_eq!(revoked["revocations"]["statements"], 1);
    call(&session, &scope, json!({"type":"sql","sql":"BEGIN"})).await;
    let id = call(
        &session,
        &scope,
        json!({"type":"next_id","table":"items","partition_values":values("A")}),
    )
    .await;
    call(&session,&scope,json!({"type":"sql","sql":"INSERT INTO items(list,key,amount,id) VALUES('A','next',7,?)","values":[{"type":"integer","value":id["id"]}]})).await;
    call(&session, &scope, json!({"type":"sql","sql":"COMMIT"})).await;
    db.close().await.unwrap();
    Database::open(open(&path, &hub))
        .unwrap()
        .database
        .close()
        .await
        .unwrap();
}

#[tokio::test]
async fn own_id_partitions_continue_the_table_log() {
    let root = tempfile::tempdir().unwrap();
    let schema = r#"{"format":5,"application":"ownid","version":1,"tables":[{"name":"lists","class":"mergeable","columns":[{"name":"title","type":"text","not_null":true}],"primary_key":["id"],"partition_by":["id"]}]}"#;
    let options = || Open {
        directory: root.path(),
        schema,
        secrets: None,
        network: None,
        commands: None,
    };
    let mut db = Database::create(options(), None).unwrap().database;
    let scope: Value = serde_json::from_str(&db.scope()).unwrap();
    let session = db.session();
    let requested = call(
        &session,
        &scope,
        json!({"type":"pseudonym_request","collection":"lists"}),
    )
    .await;
    let assigned = call(
        &session,
        &scope,
        json!({"type":"pseudonym_assign","request":requested["request"]}),
    )
    .await;
    call(
        &session,
        &scope,
        json!({"type":"pseudonym_install","pseudonym":assigned["pseudonym"]}),
    )
    .await;
    let mut previous = 0;
    for index in 0..3 {
        if index == 2 {
            db.close().await.unwrap();
            db = Database::open(options()).unwrap().database;
        }
        let session = db.session();
        let prepared = call(
            &session,
            &scope,
            json!({"type":"partition_next","table":"lists"}),
        )
        .await;
        call(&session, &scope, json!({"type":"sql","sql":"BEGIN"})).await;
        let allocated = call(&session, &scope, json!({"type":"next_id","table":"lists"})).await;
        assert_eq!(allocated["id"], prepared["id"]);
        let id = allocated["id"].as_str().unwrap().parse::<i64>().unwrap();
        if previous != 0 {
            assert_eq!(id, previous + 1);
        }
        previous = id;
        call(&session,&scope,json!({"type":"sql","sql":"INSERT INTO lists(id,title) VALUES(?,?)","values":[{"type":"integer","value":id.to_string()},{"type":"text","value":"list"}]})).await;
        call(&session, &scope, json!({"type":"sql","sql":"COMMIT"})).await;
    }
    db.close().await.unwrap();
}

#[tokio::test]
async fn moves_keep_historical_birth_proofs_after_an_application_generation() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("writer");
    let hub = Arc::new(Hub::default());
    let db = Database::create(open(&path, &hub), None).unwrap().database;
    let scope: Value = serde_json::from_str(&db.scope()).unwrap();
    let session = db.session();
    for name in ["A", "B"] {
        call(
            &session,
            &scope,
            json!({"type":"partition_create","table":"items","values":values(name)}),
        )
        .await;
    }
    let requested = call(
        &session,
        &scope,
        json!({"type":"pseudonym_request","collection":"items"}),
    )
    .await;
    let assigned = call(
        &session,
        &scope,
        json!({"type":"pseudonym_assign","request":requested["request"]}),
    )
    .await;
    call(
        &session,
        &scope,
        json!({"type":"pseudonym_install","pseudonym":assigned["pseudonym"]}),
    )
    .await;
    call(&session, &scope, json!({"type":"sql","sql":"BEGIN"})).await;
    let id = call(
        &session,
        &scope,
        json!({"type":"next_id","table":"items","partition_values":values("A")}),
    )
    .await["id"]
        .clone();
    call(&session, &scope, json!({"type":"sql","sql":"INSERT INTO items(list,key,amount,id) VALUES('A','kept',10,?)","values":[{"type":"integer","value":id}]})).await;
    call(&session, &scope, json!({"type":"sql","sql":"COMMIT"})).await;
    db.close().await.unwrap();
    let mut definition: Value = serde_json::from_str(SCHEMA).unwrap();
    definition["version"] = json!(2);
    definition["tables"][0]["columns"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"note","type":"text","not_null":false}));
    let schema = definition.to_string();
    let db = Database::open(Open {
        schema: &schema,
        ..open(&path, &hub)
    })
    .unwrap()
    .database;
    let session = db.session();
    let requested = call(
        &session,
        &scope,
        json!({"type":"pseudonym_request","collection":"items"}),
    )
    .await;
    let assigned = call(
        &session,
        &scope,
        json!({"type":"pseudonym_assign","request":requested["request"]}),
    )
    .await;
    call(
        &session,
        &scope,
        json!({"type":"pseudonym_install","pseudonym":assigned["pseudonym"]}),
    )
    .await;

    call(&session, &scope, json!({"type":"move_row","handle":vec![92u8;32],"table":"items","id":id,"from":values("A"),"to":values("B")})).await;
    let rows = call(&session, &scope, json!({"type":"read","query":{"sql":"SELECT id,note FROM items WHERE list='B' AND row_kind=0","values":[],"max_rows":2}})).await;
    assert_eq!(rows["rows"][0][0]["value"], id);
    assert_eq!(rows["rows"][0][1]["type"], "null");
    db.close().await.unwrap();
}
