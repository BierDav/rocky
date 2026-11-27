use crate::common::{Device, Failure};
use rocky_sdk::{
    Access, Affinity, Column, Database, DenyReason, ErrorKind, Network, Right, Role, RoleName,
    Schema, Table, TransactionOutcome, TransactionToken, params, transport::memory::Hub,
};
use std::{path::Path, sync::Arc};

const SCANNER: RoleName = RoleName::from_static("scanner");
const PORTER: RoleName = RoleName::from_static("porter");

fn venue() -> Result<Schema, Failure> {
    Ok(Schema::builder("venue", 1)
        .table(
            Table::replayed("check_ins")
                .column(Column::new("code", Affinity::Text).required())
                .column(Column::new("gate", Affinity::Text).required())
                .column(Column::new("note", Affinity::Text)),
        )
        .role(Role::new(SCANNER).append("check_ins"))
        .role(Role::new(PORTER).update_columns("check_ins", ["gate"]))
        .build()?)
}

fn configuration(directory: &Path, schema: &Schema, hub: &Arc<Hub>) -> rocky_sdk::Configuration {
    crate::common::configuration(
        directory,
        schema,
        None,
        None,
        Some(Network::Owned(
            hub.connector(&directory.display().to_string()),
        )),
    )
}

async fn venue_with(root: &Path, role: RoleName) -> Result<(Device, Device), Failure> {
    let hub = Arc::new(Hub::default());
    let schema = venue()?;
    let admin = Device::new(
        configuration(&root.join("admin"), &schema, &hub)
            .create()
            .await?,
    )?;
    admin
        .database
        .write(async |tx| {
            tx.execute(
                "INSERT INTO check_ins(code, gate) VALUES('T1', 'north')",
                Vec::new(),
            )
            .await
        })
        .await?;
    synced(&admin).await?;
    let invitation = admin
        .database
        .invite(rocky_sdk::Invite::new(Access::Role(role)))
        .await?;
    let member = Device::new(
        configuration(&root.join("member"), &schema, &hub)
            .fixture_person()
            .join(&invitation.code)
            .await?,
    )?;
    synced(&member).await?;
    Ok((admin, member))
}

async fn synced(device: &Device) -> Result<(), Failure> {
    crate::support::peerless_ok(device.sync_now().await)?;
    Ok(())
}

async fn refused_write(
    database: &Database,
    sql: &str,
) -> Result<(rocky_sdk::Error, TransactionToken), Failure> {
    let mut token = None;
    let refused = database
        .write(async |tx| {
            token = Some(tx.token());
            tx.execute(sql, Vec::new()).await
        })
        .await
        .err()
        .ok_or_else(|| format!("{sql} was allowed"))?;
    Ok((refused, token.ok_or("the write body never ran")?))
}

fn denied(error: &rocky_sdk::Error) -> Option<(&str, &Right)> {
    match error.kind() {
        ErrorKind::Denied {
            table: Some(table),
            need,
            reason: DenyReason::NotGranted,
        } => Some((table.as_str(), need)),
        _ => None,
    }
}

async fn gates(database: &Database) -> Result<Vec<String>, Failure> {
    database
        .query("SELECT gate FROM check_ins ORDER BY code", Vec::new())
        .await?
        .collect()
        .await?
        .iter()
        .map(|row| Ok(row.get_at::<String>(0)?))
        .collect()
}

#[tokio::test]
async fn an_append_only_role_is_refused_at_the_statement_and_records_nothing() -> Result<(), Failure>
{
    let root = tempfile::tempdir()?;
    let (admin, scanner) = venue_with(root.path(), SCANNER).await?;
    let gate = Right::Update("gate".into());
    for (sql, need) in [
        ("UPDATE check_ins SET gate = 'south'", &gate),
        (
            "UPDATE check_ins SET gate = 'south' WHERE code = 'absent'",
            &gate,
        ),
        ("DELETE FROM check_ins WHERE code = 'absent'", &Right::Write),
        ("DELETE FROM check_ins", &Right::Write),
        (
            "INSERT INTO check_ins(id, code, gate) VALUES(1, 'T2', 'east') \
             ON CONFLICT(id) DO UPDATE SET gate = excluded.gate",
            &gate,
        ),
        (
            "INSERT OR REPLACE INTO check_ins(code, gate) VALUES('T2', 'east')",
            &Right::Write,
        ),
    ] {
        let (refused, token) = refused_write(&scanner.database, sql).await?;
        assert_eq!(
            denied(&refused),
            Some(("check_ins", need)),
            "{sql} is refused when it runs, not at commit: {refused}"
        );
        assert_eq!(
            scanner.database.transaction_outcome(token).await?,
            None,
            "a refused statement records no intent: {sql}"
        );
    }
    let appended = scanner
        .database
        .write(async |tx| {
            tx.execute(
                "INSERT INTO check_ins(code, gate) VALUES(?, ?)",
                params!["T2", "east"],
            )
            .await?;
            Ok::<_, rocky_sdk::Error>(tx.token())
        })
        .await?;
    synced(&scanner).await?;
    synced(&admin).await?;
    assert_eq!(gates(&admin.database).await?, ["north", "east"]);
    assert!(
        matches!(
            admin.database.transaction_outcome(appended).await?,
            Some(TransactionOutcome::Applied { .. }) | None
        ),
        "the admin replays the append it was sent"
    );
    assert!(
        matches!(
            scanner.database.transaction_outcome(appended).await?,
            Some(TransactionOutcome::Applied { .. })
        ),
        "an allowed write commits and is never dropped for admission"
    );
    scanner.close().await;
    admin.close().await;
    Ok(())
}

#[tokio::test]
async fn an_update_of_a_column_without_update_rights_is_refused_at_the_statement()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let (admin, porter) = venue_with(root.path(), PORTER).await?;
    let (refused, token) = refused_write(
        &porter.database,
        "UPDATE check_ins SET gate = 'south', note = 'late' WHERE code = 'T1'",
    )
    .await?;
    assert_eq!(
        denied(&refused),
        Some(("check_ins", &Right::Update("note".into()))),
        "{refused}"
    );
    assert_eq!(porter.database.transaction_outcome(token).await?, None);
    let (refused, token) = refused_write(
        &porter.database,
        "UPDATE check_ins SET note = 'late' WHERE code = 'absent'",
    )
    .await?;
    assert_eq!(
        denied(&refused),
        Some(("check_ins", &Right::Update("note".into()))),
        "a blind update of a column the role may not change is refused though it matches no row"
    );
    assert_eq!(porter.database.transaction_outcome(token).await?, None);
    let moved = porter
        .database
        .write(async |tx| {
            tx.execute(
                "UPDATE check_ins SET gate = 'south' WHERE code = 'T1'",
                Vec::new(),
            )
            .await?;
            Ok::<_, rocky_sdk::Error>(tx.token())
        })
        .await?;
    assert!(matches!(
        porter.database.transaction_outcome(moved).await?,
        Some(TransactionOutcome::Applied { .. })
    ));
    let (refused, _) = refused_write(
        &porter.database,
        "INSERT INTO check_ins(code, gate) VALUES('T3', 'west')",
    )
    .await?;
    assert_eq!(denied(&refused), Some(("check_ins", &Right::Append)));
    synced(&porter).await?;
    synced(&admin).await?;
    assert_eq!(gates(&admin.database).await?, ["south"]);
    porter.close().await;
    admin.close().await;
    Ok(())
}

#[tokio::test]
async fn reads_observations_and_status_never_wait_for_an_open_write() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let (admin, scanner) = venue_with(root.path(), SCANNER).await?;
    let database = &scanner.database;
    database.status().await?;
    let observed = database
        .write(async |tx| {
            tx.execute(
                "INSERT INTO check_ins(code, gate) VALUES('T2', 'east')",
                Vec::new(),
            )
            .await?;
            let refused = tx
                .execute("DELETE FROM check_ins WHERE code = 'T1'", Vec::new())
                .await
                .err()
                .ok_or("a scanner deleted a check-in")?;
            assert_eq!(denied(&refused), Some(("check_ins", &Right::Write)));
            let beside = gates(database).await?;
            let events = database.observe(&["check_ins"]).await?;
            database.status().await?;
            Ok::<_, Failure>((beside, events))
        })
        .await?;
    let (beside, mut events) = observed;
    assert_eq!(
        beside,
        ["north"],
        "a read beside the open write sees the last committed state"
    );
    assert!(
        events
            .next()
            .await?
            .tables
            .iter()
            .any(|table| table == "check_ins"),
        "the observation registered during the write sees its commit"
    );
    assert_eq!(gates(database).await?, ["north", "east"]);
    scanner.close().await;
    admin.close().await;
    Ok(())
}
