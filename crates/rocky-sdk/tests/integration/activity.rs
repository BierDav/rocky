use rocky_sdk::{
    ActivityKind, Affinity, Column, Configuration, Failure, Partition, Schema, ServiceError,
    SqlValue, Table,
};

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn closing_the_database_ends_an_activity_wait() -> Result<(), Failure> {
    let directory = rocky_platform::fs::Scratch::new().await;
    let schema = Schema::builder("typed_activity", 1)
        .table(Table::replayed("notes").column(Column::new("body", Affinity::Text)))
        .table(
            Table::replayed("pages")
                .column(Column::new("book", Affinity::Text).required())
                .column(Column::new("body", Affinity::Text))
                .primary_key(["book"])
                .partition_by(["book"]),
        )
        .build()?;
    let database = Configuration::new(directory.path(), &schema)?
        .create()
        .await?
        .database;
    let writer = database.clone();
    let observer = database.clone();
    let after = observer.activity(0).await?.token;
    let pages = Partition::new("pages", vec![SqlValue::Text("A".into())]);
    let (page, changed) =
        rocky_platform::time::timeout(std::time::Duration::from_secs(30), async {
            tokio::join!(
                observer.next_activity(after),
                writer.create_partition(&pages)
            )
        })
        .await?;
    changed?;
    let page = page?;
    assert!(page.token > after);
    assert!(
        page.events
            .iter()
            .any(|event| matches!(event.detail, ActivityKind::PartitionCreated { .. }))
    );
    let (ended, closed) =
        rocky_platform::time::timeout(std::time::Duration::from_secs(30), async {
            tokio::join!(observer.next_activity(page.token), writer.close())
        })
        .await?;
    closed?;
    assert_eq!(
        ended.unwrap_err().kind,
        rocky_sdk::ErrorKind::Service(ServiceError::Closed("database"))
    );
    Ok(())
}
