use rocky_coordinator::{
    Claim, Claimed, ControlReceipt, CounterError, Counters, Name, Tenant, TrustBundle,
};
use rocky_sdk::{Affinity, Column, Configuration, Database, Schema, Table, Timestamp, params};
use std::{path::Path, time::SystemTime};

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
    let schema = Schema::builder("till", 1)
        .table(
            Table::replayed("sales")
                .column(Column::new("sale", Affinity::Text).required())
                .column(Column::new("created", Affinity::Integer).required())
                .column(Column::new("invoice", Affinity::Integer))
                .primary_key(["sale"]),
        )
        .build()?;
    let created = Configuration::new(Path::new(directory), &schema)?
        .create()
        .await?;
    let database = created.database;

    let counters = Counters::connect(database.clone(), &bundle).await?;
    let tenant = Tenant::generate();
    let receipt = counters
        .register(&created.root, &tenant, CONTROL_LIFETIME_SECONDS)
        .await?;
    let kept = (tenant.secret(), receipt.to_bytes());
    let receipt = ControlReceipt::from_bytes(&kept.1)?;
    counters
        .renew(
            &Tenant::from_secret(kept.0),
            &receipt,
            CONTROL_LIFETIME_SECONDS,
        )
        .await?;

    let invoices = Name::new("invoices")?;
    record_sale(&database, "sale-1").await?;
    let first = invoice(&database, &counters, &invoices, "sale-1").await?;
    let retried = invoice(&database, &counters, &invoices, "sale-1").await?;
    println!("sale-1 is invoice {first}; its retry is invoice {retried}");
    if retried != first {
        return Err("a retry took a second number".into());
    }

    counters.close().await;
    record_sale(&database, "sale-2").await?;
    match invoice(&database, &counters, &invoices, "sale-2").await {
        Err(closed) if matches!(closed.downcast_ref(), Some(CounterError::Closed)) => {}
        other => return Err(format!("closed counters answered {other:?}").into()),
    }
    database.close().await?;
    Ok(())
}

async fn record_sale(database: &Database, sale: &str) -> Result<(), Failure> {
    let created = Timestamp::try_from(SystemTime::now())?;
    database
        .write(async |tx| {
            tx.execute(
                "INSERT INTO sales(sale, created) VALUES(?, ?)",
                params![sale, created],
            )
            .await
        })
        .await?;
    database.flush().await?;
    Ok(())
}

async fn invoice(
    database: &Database,
    counters: &Counters,
    invoices: &Name,
    sale: &str,
) -> Result<i64, Failure> {
    let rows = database
        .query("SELECT created FROM sales WHERE sale = ?", params![sale])
        .await?;
    let created: Timestamp = rows
        .collect()
        .await?
        .first()
        .ok_or("no sale")?
        .get("created")?;
    let claim = Claim::new(sale, sale, created.system_time().ok_or("sale time")?);
    let Claimed::First(number) = counters.claim(invoices, &claim).await? else {
        return Err("another attempt holds this sale's number".into());
    };
    database
        .write(async |tx| {
            tx.execute(
                "UPDATE sales SET invoice = ? WHERE sale = ?",
                params![number, sale],
            )
            .await
        })
        .await?;
    Ok(number)
}
