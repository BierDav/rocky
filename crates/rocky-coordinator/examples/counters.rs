use rocky_coordinator::{Claim, Claimed, Counters, Name, Tenant, TrustBundle};
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
    let transport = rocky_iroh::Options::isolated("127.0.0.1:0")
        .client()
        .await
        .map_err(|error| -> Failure { error.to_string().into() })?
        .transport();
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
            Table::replayed("admissions")
                .column(Column::new("ticket", Affinity::Text).required())
                .column(Column::new("number", Affinity::Integer).required())
                .primary_key(["ticket"]),
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

    let door = Name::new("main-door")?;
    let east = Claim::new(b"ticket-1");
    let west = Claim::new(b"ticket-1");
    let admitted = counters.claim(&door, &east).await?;
    let rescanned = counters.claim(&door, &west).await?;
    let retried = counters.claim(&door, &east).await?;
    println!("east door: {admitted:?}; west door: {rescanned:?}; east retry: {retried:?}");
    if !matches!(admitted, Claimed::First(_))
        || !matches!(rescanned, Claimed::Taken(_))
        || retried != admitted
    {
        return Err("a ticket was admitted twice or a retry lost its admission".into());
    }
    created.database.close().await?;
    Ok(())
}
