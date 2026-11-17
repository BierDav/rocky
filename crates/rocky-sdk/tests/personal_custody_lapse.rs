//! A keyless custodian whose custody grant lapsed while every owner device
//! was gone still restores a passkey-only device, under the same grant
//! renewed. A binary of its own: it skips the whole process's wall clock.
#![cfg(not(target_family = "wasm"))]
use rocky_sdk::{
    Configuration, Freshness, GrantProfile, Network, PersonalNamespace, Schema, SignInSource,
    VerifiedPasskeyPrf, transport::memory::Hub,
};
use std::{
    path::Path,
    sync::{Arc, atomic::Ordering},
};

#[expect(dead_code, reason = "this suite uses a few of the shared fixtures")]
#[path = "integration/common/mod.rs"]
mod common;
#[expect(dead_code, reason = "this suite uses a few of the shared fixtures")]
#[path = "integration/support/mod.rs"]
mod support;
use common::{Device, Failure};

fn namespace() -> PersonalNamespace {
    PersonalNamespace::new("shopping.example", "account root").expect("namespace")
}
fn prf() -> VerifiedPasskeyPrf {
    let input = namespace().prf_input().expect("input");
    let output = rocky_core::commitment(
        "fixture.passkey-prf",
        &[[1; 32].as_slice(), input.as_bytes()].concat(),
    );
    VerifiedPasskeyPrf::new(&input, output)
}
fn config(directory: &Path, hub: &Arc<Hub>, name: &str) -> Result<Configuration, Failure> {
    Ok(
        Configuration::new(directory, &Schema::parse(common::SCHEMA)?)?
            .network(Network::Owned(hub.connector(name))),
    )
}
/// The custody link of the custodian's saved chain: its identity link.
fn custody_link(directory: &Path) -> Result<rocky_credentials::SignedCapability, Failure> {
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.join("identity.json"))?)?;
    let chain: rocky_credentials::CredentialChain =
        serde_json::from_value(saved["credentials"].clone())?;
    let links = chain.capabilities();
    Ok(links.0.last().ok_or("custody link")?.clone())
}
fn now() -> Result<u64, Failure> {
    Ok(rocky_platform::time::since_epoch()?.as_secs())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lapsed_custodian_alone_restores_under_its_same_renewed_grant() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    hub.discovery.store(true, Ordering::SeqCst);
    let owner_path = directory.path().join("owner");
    let owner = Device::new(
        config(&owner_path, &hub, "owner")?
            .create_personal(prf(), namespace())
            .await?
            .opened,
    )?;
    let invitation = owner
        .session
        .invitation_with(GrantProfile::Custody {
            scopes: ["items".to_owned()].into(),
        })
        .await?
        .uri;
    let custodian_path = directory.path().join("custodian");
    let custodian = Device::new(
        config(&custodian_path, &hub, "custodian")?
            .join(&invitation)
            .await?,
    )?;
    common::introduce(&owner, &custodian).await;
    owner.add(1, "kept by custody").await;
    owner.sync().await.map_err(|error| error.to_string())?;
    let original = custody_link(&custodian_path)?;
    assert!(
        original
            .capability
            .grants
            .iter()
            .all(|grant| grant.rights == [rocky_core::Right::Custody].into())
    );
    owner.close().await;
    std::fs::remove_dir_all(&owner_path)?;
    let an_hour_past_the_custody_grant = original.capability.expires - now()? + 3600;
    rocky_platform::entropy::simulation::skip(std::time::Duration::from_secs(
        an_hour_past_the_custody_grant,
    ));
    assert!(
        custody_link(&custodian_path)?.capability.expires <= now()?,
        "with every full node gone, the custody grant lapses while the custodian runs"
    );

    let path = directory.path().join("phone");
    let personal = config(&path, &hub, "phone")?
        .sign_in(prf(), namespace(), SignInSource::Discover)
        .await
        .map_err(|e| format!("sign in: {e}"))?;
    assert_eq!(personal.freshness, Freshness::Unwitnessed);
    assert!(personal.restored.complete(), "{:?}", personal.restored);
    let phone = Device::new(personal.opened)?;
    phone
        .sync()
        .await
        .map_err(|error| format!("sync: {error}"))?;
    assert_eq!(phone.titles().await, ["kept by custody"]);

    let renewed = custody_link(&custodian_path)?;
    assert!(renewed.capability.expires > now()?);
    assert_ne!(renewed.id()?, original.id()?);
    assert_eq!(renewed.capability.subject, original.capability.subject);
    assert_eq!(renewed.capability.kind, original.capability.kind);
    assert_eq!(
        renewed.capability.grants, original.capability.grants,
        "the renewed custody grant widened nothing"
    );
    assert_eq!(
        renewed.capability.delegations,
        original.capability.delegations
    );
    assert_eq!(
        renewed.capability.extended()?,
        original.capability.extended()?
    );
    assert!(!custodian.session.status().await?.authority.expired);
    phone.add(2, "after renewal").await;
    phone
        .sync()
        .await
        .map_err(|error| format!("the renewed grant takes the phone's writes: {error}"))?;
    phone.close().await;
    custodian.close().await;
    Ok(())
}
