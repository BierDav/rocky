use crate::common::{self, Device, Failure, introduce};
use crate::support::snapshot as snap;
use rocky_sdk::{
    Configuration, IdentityKey, IdentityProvider, Invite, Network, StoreOnlyRefusal,
    transport::memory::Hub,
};
use std::{path::Path, sync::Arc};

fn store_only() -> rocky_sdk::Access {
    rocky_sdk::Access::Store
}
fn relay_configuration(
    directory: &Path,
    hub: &Arc<Hub>,
    person: &Arc<dyn IdentityProvider>,
) -> Configuration {
    Configuration::store_only(directory)
        .network(Network::Owned(hub.connector("relay")))
        .identity(person.clone())
}
async fn invited_relay(
    issuer: &Device,
    directory: &Path,
    hub: &Arc<Hub>,
) -> Result<(Device, Arc<dyn IdentityProvider>), Failure> {
    let person: Arc<dyn IdentityProvider> = Arc::new(IdentityKey::generate());
    let invitation = issuer
        .database
        .invite(Invite::new(store_only()).for_identity(person.public_key()?.into()))
        .await?;
    let relay = relay_configuration(directory, hub, &person)
        .join(&invitation.code)
        .await?;
    Ok((Device::new(relay)?.with_identity(person.clone()), person))
}
fn assert_keyless(directory: &Path) -> Result<(), Failure> {
    let identity = common::authority_json(directory, "identity")?;
    assert_eq!(identity["keys"], serde_json::json!({}));
    assert_eq!(identity["authority"], serde_json::json!(null));
    assert_eq!(identity["access"], "store");
    for file in std::fs::read_dir(directory.join("secrets"))? {
        let name = file?.file_name().to_string_lossy().into_owned();
        assert!(
            !name.contains("reader") && !name.contains("publisher"),
            "a store-only relay holds no data key: {name}"
        );
    }
    Ok(())
}
async fn reads_nothing(relay: &Device) -> bool {
    snap::observe(snap::read(
        &relay.database,
        "SELECT * FROM items",
        Vec::new(),
    ))
    .await
    .is_err()
}
fn refusal(opened: Result<rocky_sdk::Database, rocky_sdk::Error>) -> Option<StoreOnlyRefusal> {
    match opened.err()?.kind {
        rocky_sdk::ErrorKind::StoreOnly(refusal) => Some(refusal),
        _ => None,
    }
}

#[tokio::test]
async fn devices_never_online_together_converge_through_a_store_only_relay() -> Result<(), Failure>
{
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let net = |name| Network::Owned(hub.connector(name));
    let issuer = Device::create(&root.path().join("issuer"), Some(net("issuer"))).await?;
    let (relay, person) = invited_relay(&issuer, &root.path().join("relay"), &hub).await?;
    assert_keyless(&root.path().join("relay"))?;
    let a = Device::join(&root.path().join("a"), &issuer.invite().await, net("a")).await?;
    let b = Device::join(&root.path().join("b"), &issuer.invite().await, net("b")).await?;
    introduce(&a, &relay).await;
    introduce(&b, &relay).await;
    issuer.close().await;
    snap::ok(b.database.set_sync(rocky_sdk::SyncMode::Off)).await;
    a.add(1, "Milk").await;
    a.sync().await.expect("upload");
    assert!(reads_nothing(&relay).await);
    relay.close().await;
    let relay = Device::new(
        relay_configuration(&root.path().join("relay"), &hub, &person)
            .open()
            .await?,
    )?;
    snap::ok(a.database.set_sync(rocky_sdk::SyncMode::Off)).await;
    snap::ok(b.database.set_sync(rocky_sdk::SyncMode::Manual)).await;
    b.sync().await.expect("catch up");
    assert_eq!(b.titles().await, ["Milk"]);
    b.add(2, "Eggs").await;
    b.sync().await.expect("upload");
    snap::ok(b.database.set_sync(rocky_sdk::SyncMode::Off)).await;
    snap::ok(a.database.set_sync(rocky_sdk::SyncMode::Manual)).await;
    a.sync().await.expect("catch up");
    assert_eq!(a.titles().await, ["Eggs", "Milk"]);
    assert!(reads_nothing(&relay).await);
    assert_keyless(&root.path().join("relay"))?;
    assert!(
        relay
            .database
            .invite(Invite::new(store_only()))
            .await
            .is_err(),
        "a store-only relay cannot invite"
    );
    a.close().await;
    b.close().await;
    relay.close().await;
    Ok(())
}

#[tokio::test]
async fn a_new_device_bootstraps_from_the_relay_while_every_other_device_is_offline()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let issuer = Device::create(
        &root.path().join("issuer"),
        Some(Network::Owned(hub.connector("issuer"))),
    )
    .await?;
    let (relay, _) = invited_relay(&issuer, &root.path().join("relay"), &hub).await?;
    introduce(&issuer, &relay).await;
    let device = Device::join(
        &root.path().join("device"),
        &issuer.invite().await,
        Network::Owned(hub.connector("device")),
    )
    .await?;
    issuer.add(1, "Milk").await;
    issuer.sync().await.expect("upload");
    issuer.close().await;
    introduce(&device, &relay).await;
    device.sync().await.expect("bootstrap from the relay");
    assert_eq!(device.titles().await, ["Milk"]);
    assert!(reads_nothing(&relay).await);
    device.close().await;
    relay.close().await;
    Ok(())
}

#[tokio::test]
async fn removing_the_relay_stops_it_carrying_frames() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let net = |name| Network::Owned(hub.connector(name));
    let issuer = Device::create(&root.path().join("issuer"), Some(net("issuer"))).await?;
    let (relay, person) = invited_relay(&issuer, &root.path().join("relay"), &hub).await?;
    let a = Device::join(&root.path().join("a"), &issuer.invite().await, net("a")).await?;
    introduce(&a, &relay).await;
    introduce(&issuer, &relay).await;
    a.sync().await.expect("reach the relay");
    let (node, contact) = (relay.node().await, relay.contact().await);
    snap::ok(issuer.database.revoke_identity(person.public_key()?)).await;
    a.sync().await.expect("learn the revocation");
    assert_eq!(
        a.peer(&node).await,
        serde_json::Value::Null,
        "removing the relay forgets its contact"
    );
    assert!(a.add_peer(&contact).await.is_err());
    assert!(
        common::exchange_failed(&relay.sync().await),
        "members online neither take frames from a removed relay nor give it any"
    );
    issuer.close().await;
    a.close().await;
    relay.close().await;
    Ok(())
}

#[tokio::test]
async fn a_store_only_configuration_refuses_readable_access_and_creating() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let issuer = Device::create(
        &root.path().join("issuer"),
        Some(Network::Owned(hub.connector("issuer"))),
    )
    .await?;
    let person: Arc<dyn IdentityProvider> = Arc::new(IdentityKey::generate());
    let writer = issuer
        .database
        .invite(Invite::new(rocky_sdk::Access::Full).for_identity(person.public_key()?.into()))
        .await?;
    assert_eq!(
        refusal(
            relay_configuration(&root.path().join("writer"), &hub, &person)
                .join(&writer.code)
                .await
                .map(|opened| opened.database)
        ),
        Some(StoreOnlyRefusal::ReadableAccess)
    );
    assert_eq!(
        refusal(
            Configuration::store_only(root.path().join("bearer"))
                .network(Network::Owned(hub.connector("bearer")))
                .join(&issuer.invite().await)
                .await
                .map(|opened| opened.database)
        ),
        Some(StoreOnlyRefusal::ReadableAccess)
    );
    assert!(!root.path().join("bearer").join("rocky.sqlite").exists());
    assert_eq!(
        refusal(
            Configuration::store_only(root.path().join("created"))
                .create()
                .await
                .map(|created| created.database)
        ),
        Some(StoreOnlyRefusal::NeedsSchema)
    );
    issuer.close().await;
    assert_eq!(
        refusal(
            Configuration::store_only(root.path().join("issuer"))
                .open()
                .await
        ),
        Some(StoreOnlyRefusal::ReadableAccess),
    );
    Ok(())
}
