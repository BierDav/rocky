use rocky_sdk::{
    ActivityKind, Affinity, Column, Configuration, Failure, GrantProfile, GrantTerms, IdentityKey,
    NodeKeys, ReplicaRoleConfig, Schema, ServiceError, Table,
};

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn closing_an_activity_waiter_preserves_the_writer_and_binary_identity_labels()
-> Result<(), Failure> {
    let directory = rocky_platform::fs::Scratch::new().await;
    let schema = Schema::builder("typed_activity", 1)
        .table(Table::replayed("notes").column(Column::new("body", Affinity::Text)))
        .build()?;
    let database = Configuration::new(directory.path(), &schema)?
        .create()
        .await?
        .database;
    let writer = database.session();
    let observer = database.session();
    assert_eq!(writer.domain().await?.scope, database.scope());
    let identity = writer.info().await?.principal;
    let labeled = NodeKeys {
        identity: IdentityKey::generate().public_key(),
        node: rocky_sdk::credentials::PrivateKey::new().public_key(),
    };
    writer
        .grant(
            labeled,
            GrantProfile::Reader {
                scopes: ["notes".into()].into(),
            },
            GrantTerms::default(),
        )
        .await?;
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
    let (ended, ()) = rocky_platform::time::timeout(std::time::Duration::from_secs(30), async {
        tokio::join!(observer.next_activity(page.token), observer.close())
    })
    .await?;
    assert_eq!(
        ended.unwrap_err().downcast_ref::<ServiceError>(),
        Some(&ServiceError::Closed("session"))
    );
    assert_eq!(writer.info().await?.principal, identity);
    database.close().await
}
