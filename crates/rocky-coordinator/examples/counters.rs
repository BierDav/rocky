use rocky_coordinator::{
    Counters,
    certified::{TrustBundle, now},
    counters::{Claim, Name},
    tenancy::Tenant,
};
use rocky_sdk::{Affinity, Column, Configuration, Schema, Table, transport::Transport};
use std::{path::Path, sync::Arc};

type Failure = Box<dyn std::error::Error + Send + Sync>;

const USAGE: &str = "usage: counters BUNDLE.json EMPTY_DIRECTORY";
const CONTROL_LIFETIME_SECONDS: u64 = 86_400;

#[tokio::main]
async fn main() -> Result<(), Failure> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let [bundle, directory] = arguments.as_slice() else {
        return Err(USAGE.into());
    };
    let bundle = TrustBundle::from_json(&std::fs::read_to_string(bundle)?)?;
    let transport = transport().await?;
    let claimed = run(&bundle, Path::new(directory), transport.clone()).await;
    transport.close().await;
    claimed
}

async fn run(
    bundle: &TrustBundle,
    directory: &Path,
    transport: Arc<dyn Transport>,
) -> Result<(), Failure> {
    let schema = Schema::builder("tickets", 1)
        .table(
            Table::replayed("tickets")
                .column(Column::new("order_key", Affinity::Text).required())
                .column(Column::new("number", Affinity::Integer).required())
                .primary_key(["order_key"]),
        )
        .build()?;
    let created = Configuration::new(directory, &schema)?.create().await?;
    let counters = Counters::connect(created.database.clone(), bundle, transport)?;

    let tenant = Tenant::generate();
    let control = counters
        .register(&created.root, &tenant, CONTROL_LIFETIME_SECONDS)
        .await?;
    println!(
        "registered control revision {} until {}",
        control.revision, control.expires
    );

    let tickets = Name::new("tickets")?;
    let order = Claim::keyed(b"order-1", now()?);
    let number = counters.claim(&tickets, order).await?;
    let retried = counters.claim(&tickets, order).await?;
    println!("order-1 holds ticket {number}; its retry answered {retried}");
    if retried != number {
        return Err("a keyed retry answered another number".into());
    }
    created.database.close().await?;
    Ok(())
}

async fn transport() -> Result<Arc<dyn Transport>, Failure> {
    let options = rocky_iroh::Options {
        relay: Some("disabled".into()),
        lookup: Some("disabled".into()),
        topic: false,
        local: false,
        bind: Some("127.0.0.1:0".into()),
    };
    let endpoint = options
        .bind(rocky_platform::entropy::random())
        .await
        .map_err(|error| -> Failure { error.to_string().into() })?;
    Ok(rocky_iroh::Attachment::serving(endpoint, Vec::new()).transport())
}
