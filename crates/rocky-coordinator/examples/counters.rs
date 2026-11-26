use rocky_coordinator::{Claim, Claimed, Counters, Name, Tenant, TrustBundle};
use rocky_sdk::{Affinity, Column, Configuration, Schema, Table};
use std::path::Path;

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
    let schema = Schema::builder("tickets", 1)
        .table(
            Table::replayed("admissions")
                .column(Column::new("ticket", Affinity::Text).required())
                .column(Column::new("number", Affinity::Integer).required())
                .primary_key(["ticket"]),
        )
        .build()?;
    let created = Configuration::new(Path::new(directory), &schema)?
        .create()
        .await?;

    let counters = Counters::connect(created.database.clone(), &bundle).await?;
    counters
        .register(&created.root, &Tenant::generate(), CONTROL_LIFETIME_SECONDS)
        .await?;
    let admissions = Name::new("admissions")?;
    let east = counters
        .claim(&admissions, &Claim::new("ticket-1", "east-scan-1"))
        .await?;
    admitted_once(&counters, &admissions, east).await?;

    counters.close().await;
    let offline = counters
        .claim(&admissions, &Claim::new("ticket-2", "east-scan-2"))
        .await;
    if offline.is_ok() {
        return Err("closed counters still took a number".into());
    }
    created.database.close().await?;
    Ok(())
}

async fn admitted_once(
    counters: &Counters,
    admissions: &Name,
    east: Claimed,
) -> Result<(), Failure> {
    let west = counters
        .claim(admissions, &Claim::new("ticket-1", "west-scan-1"))
        .await?;
    let east_retry = counters
        .claim(admissions, &Claim::new("ticket-1", "east-scan-1"))
        .await?;
    println!("east door: {east:?}; west door: {west:?}; east retry: {east_retry:?}");
    if !matches!(east, Claimed::First(_))
        || !matches!(west, Claimed::Taken(_))
        || east_retry != east
    {
        return Err("a ticket was admitted twice or a retry lost its admission".into());
    }
    Ok(())
}
