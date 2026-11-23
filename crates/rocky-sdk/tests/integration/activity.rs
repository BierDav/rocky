use rocky_sdk::{
    ActivityKind, Affinity, Column, Configuration, Failure, ReplicaRoleConfig, Schema,
    ServiceError, Table,
};

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn closing_the_database_ends_an_activity_wait_and_keeps_binary_identity_labels()
-> Result<(), Failure> {
    let directory = rocky_platform::fs::Scratch::new().await;
    let schema = Schema::builder("typed_activity", 1)
        .table(Table::replayed("notes").column(Column::new("body", Affinity::Text)))
        .build()?;
    let database = Configuration::new(directory.path(), &schema)?
        .create()
        .await?
        .database;
    let writer = database.clone();
    let observer = database.clone();
    let labeled = writer.provision(rocky_sdk::Access::Read, None).await?;
    let label = vec![0, 255, b'a', 0];
    writer
        .set_identity_label(labeled.identity, label.clone())
        .await?;
    let listed = writer.identities().await?;
    let labeled = listed
        .iter()
        .find(|listed| listed.identity == labeled.identity)
        .ok_or("issued identity")?;
    assert_eq!(labeled.label.as_ref(), Some(&label));

    let after = observer.activity(0).await?.token;
    let (page, changed) =
        rocky_platform::time::timeout(std::time::Duration::from_secs(30), async {
            tokio::join!(
                observer.next_activity(after),
                writer.set_replica_role(ReplicaRoleConfig::default())
            )
        })
        .await?;
    changed?;
    let page = page?;
    assert!(page.token > after);
    assert!(
        page.events
            .iter()
            .any(|event| { matches!(event.detail, ActivityKind::RoleChanged { partial: false }) })
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
