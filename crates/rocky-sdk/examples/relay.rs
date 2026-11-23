use rocky_sdk::{Configuration, Database, Failure, IdentityKey, IdentityProvider, Network};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Failure> {
    let Ok(phrase) = std::env::var("ROCKY_RELAY_PHRASE") else {
        let identity = IdentityKey::generate();
        println!(
            "Invite relay identity {} with store-only (custody) access,",
            identity.public_key().to_hex()
        );
        println!("then run with ROCKY_RELAY_PHRASE=\"{}\"", identity.phrase());
        return Ok(());
    };
    let identity: Arc<dyn IdentityProvider> = Arc::new(IdentityKey::from_phrase(&phrase)?);
    let tenants: Vec<String> = std::env::args().skip(1).collect();
    if tenants.is_empty() {
        return Err("usage: relay <tenant directory>[=<invitation>]...".into());
    }
    let mut databases = Vec::new();
    let opened = open_tenants(&tenants, &identity, &mut databases).await;
    if opened.is_ok() {
        tokio::signal::ctrl_c().await?;
    }
    for database in databases {
        database.close().await?;
    }
    opened
}

async fn open_tenants(
    tenants: &[String],
    identity: &Arc<dyn IdentityProvider>,
    databases: &mut Vec<Database>,
) -> Result<(), Failure> {
    for tenant in tenants {
        let (directory, invitation) = match tenant.split_once('=') {
            Some((directory, invitation)) => (directory, Some(invitation)),
            None => (tenant.as_str(), None),
        };
        let configuration = Configuration::store_only(directory)
            .network(Network::Owned(Arc::new(rocky_iroh::Options::default())))
            .identity(identity.clone());
        let database = match invitation {
            Some(invitation) => configuration.accept_invitation(invitation).await?,
            None => configuration.open().await?,
        };
        database.set_sync(rocky_sdk::SyncMode::Foreground).await?;
        println!("{directory}: {}", database.contact().await?.uri);
        databases.push(database);
    }
    Ok(())
}
