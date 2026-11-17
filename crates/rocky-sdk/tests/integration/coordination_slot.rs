use rocky_coordinator::coordinated::MemoryLog;
use rocky_platform::fs::Scratch;
use rocky_sdk::{
    Affinity, Column, Configuration, CoordinatorNotConfigured, Failure, Schema, Table, counters,
    credentials::PrivateKey, params,
};
use std::sync::Arc;

fn schema() -> Result<Schema, Failure> {
    Schema::builder("coordination_slot", 1)
        .table(Table::replayed("notes").column(Column::new("body", Affinity::Text).required()))
        .build()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn missing_coordinator_refuses_claims_while_local_storage_keeps_working()
-> Result<(), Failure> {
    let directory = Scratch::new().await;
    let db = Configuration::new(directory.path(), &schema()?)?
        .create()
        .await?
        .database;
    let counter = counters::Name::new("invoices")?;
    let claim = counters::Claim::fresh()?;
    for error in [
        db.claim(counter.clone(), claim).await.unwrap_err(),
        db.counter(counter, Some(claim)).await.unwrap_err(),
    ] {
        assert_eq!(
            error.downcast_ref::<CoordinatorNotConfigured>(),
            Some(&CoordinatorNotConfigured)
        );
    }
    let mut tx = db.transaction().await?;
    let id = tx.next_id("notes", params![]).await?;
    tx.execute(
        "INSERT INTO notes(id,body) VALUES(?,?)",
        params![id, "local"],
    )
    .await?;
    tx.commit().await?;
    let rows = db
        .query("SELECT body FROM notes", params![])
        .await?
        .collect()
        .await?;
    assert_eq!(rows[0].get::<String>("body")?, "local");
    db.close().await
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_application_coordinator_keeps_claim_identity_across_database_reopen()
-> Result<(), Failure> {
    let directory = Scratch::new().await;
    let logs = MemoryLog::new();
    let coordinator = Arc::new(logs.client(PrivateKey::new().public_key()));
    let config = Configuration::new(directory.path(), &schema()?)?.coordination(coordinator);
    let opened = config.create().await?;
    let counter = counters::Name::new("invoices")?;
    let claim = counters::Claim::fresh()?;
    assert_eq!(opened.database.claim(counter.clone(), claim).await?, 1);
    opened.database.close().await?;
    let db = config.open().await?.database;
    assert_eq!(db.claim(counter.clone(), claim).await?, 1);
    assert_eq!(
        db.claim(counter.clone(), counters::Claim::fresh()?).await?,
        2
    );
    assert_eq!(
        db.counter(counter, Some(claim)).await?.claim,
        Some(counters::Lookup::Claimed(1))
    );
    db.close().await
}
