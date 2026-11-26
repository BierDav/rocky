use rocky_sdk::{Configuration, Database, IdentityKey, IdentityProvider, PersonId};

type Failure = Box<dyn std::error::Error + Send + Sync>;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Failure> {
    let Ok(phrase) = std::env::var("ROCKY_RELAY_PHRASE") else {
        let identity = IdentityKey::generate();
        println!(
            "Invite relay identity {} with store-only access (Invite::new(Access::Store).for_identity(..)),",
            PersonId::of(&identity)?
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
            .network(rocky_iroh::Options::default().into())
            .identity(identity.clone());
        let database = match invitation {
            Some(invitation) => configuration.join(&invitation.parse()?).await?.database,
            None => configuration.open().await?,
        };
        println!("{directory}: {}", database.contact().await?.uri);
        databases.push(database);
    }
    Ok(())
}
