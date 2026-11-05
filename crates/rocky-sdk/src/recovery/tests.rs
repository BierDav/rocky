//! A recovery stopped at each step: opening the directory is refused, and
//! recovering again finishes it under the identity, and so the author slot,
//! it first saved.
use super::fault::{STOPS, Stage};
use crate::{Affinity, Column, Configuration, Failure, Schema, Table, params};
use rocky_platform::fs::Scratch;
use serde_json::Value;
use std::path::Path;

fn schema() -> Result<Schema, Failure> {
    // Two collections, two keys: a stop after the first leaves one missing.
    Schema::builder("recovery_faults", 1)
        .table(Table::replayed("notes").column(Column::new("body", Affinity::Text).required()))
        .table(Table::replayed("tasks").column(Column::new("body", Affinity::Text).required()))
        .build()
}
async fn write(database: &crate::Database, table: &str, body: &str) -> Result<(), Failure> {
    let mut tx = database.transaction().await?;
    let id = tx.next_id(table, vec![]).await?;
    tx.execute(
        &format!("INSERT INTO {table}(id,body) VALUES(?,?)"),
        params![id, body],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
async fn rows(database: &crate::Database) -> Result<Vec<String>, Failure> {
    let rows = database
        .query(
            "SELECT 'note/' || body AS row FROM notes UNION ALL \
             SELECT 'task/' || body FROM tasks ORDER BY 1",
            params![],
        )
        .await?
        .collect()
        .await?;
    Ok(rows
        .iter()
        .map(|row| row.get::<String>("row"))
        .collect::<Result<_, _>>()?)
}
/// Grants a reader device, which records the creator's own trusted grant
/// in its registry, the record a recovery revokes it by.
async fn grant(database: &crate::Database, directory: &Path) -> Result<(), Failure> {
    let device = crate::Database::device(directory, None)?;
    let session = database.session();
    let call = serde_json::json!({"version": 1,
        "scope": serde_json::from_str::<Value>(&database.scope())?,
        "request": {"type": "grant", "device": device,
            "profile": {"kind": "reader", "scopes": ["notes"]}}});
    let reply: Value = serde_json::from_str(&session.call(call.to_string()).await?)?;
    session.close().await;
    if reply["status"] != "ok" {
        return Err(reply.to_string().into());
    }
    Ok(())
}
/// The recovered identity and its author numbers.
fn slot(directory: &Path) -> Result<Option<(Value, Value)>, Failure> {
    let path = directory.join("identity.json");
    if !rocky_platform::fs::exists(&path) {
        return Ok(None);
    }
    let saved: Value = serde_json::from_slice(&rocky_platform::fs::read(path)?)?;
    Ok(Some((
        saved["identity"].clone(),
        saved["minting"]["range"].clone(),
    )))
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_recovery_stopped_at_any_step_finishes_when_recovered_again_in_one_slot()
-> Result<(), Failure> {
    let root = Scratch::new().await;
    let schema = schema()?;
    let creator = root.path().join("creator");
    let created = Configuration::new(&creator, &schema)?.create().await?;
    let key = created.root.ok_or("root")?;
    write(&created.database, "notes", "before").await?;
    write(&created.database, "tasks", "before").await?;
    grant(&created.database, &root.path().join("reader")).await?;
    let archive = root.path().join("creator.backup");
    created.database.backup(&archive).await?;
    created.database.close().await?;
    let (lost, _) = slot(&creator)?.ok_or("no creator")?;
    rocky_platform::fs::remove_dir_all(&creator)?;

    for stage in [
        Stage::Marked,
        Stage::Saved,
        Stage::Moved,
        Stage::Placed,
        Stage::Rebound,
        Stage::Attached,
        Stage::Keyed,
        Stage::Revoking,
        Stage::Named,
    ] {
        let directory = root.path().join(format!("{stage:?}"));
        STOPS
            .lock()
            .map_err(|_| "poisoned")?
            .push((directory.clone(), stage));
        let config = Configuration::new(&directory, &schema)?;
        let stopped = config.recover(&archive, &key).await.err();
        let stopped = stopped.ok_or("recovered past the stop")?.to_string();
        assert!(stopped.contains("injected stop"), "{stage:?}: {stopped}");
        let saved = slot(&directory)?;
        // Nothing opens half-recovered.
        let refused = config.open().await.err();
        let refused = refused.ok_or("opened half-recovered")?.to_string();
        let expected = if saved.is_some() {
            "a recovery is unfinished"
        } else {
            "no domain in this directory"
        };
        assert!(refused.contains(expected), "{stage:?}: {refused}");

        let opened = config.recover(&archive, &key).await?;
        // Named in full, also what an earlier attempt revoked.
        assert_eq!(
            serde_json::to_value(&opened.revoked)?,
            Value::Array(vec![lost.clone()]),
            "{stage:?}"
        );
        assert_eq!(
            rows(&opened.database).await?,
            ["note/before", "task/before"],
            "{stage:?}"
        );
        if let Some(saved) = saved {
            assert_eq!(slot(&directory)?, Some(saved), "{stage:?}: another slot");
        }
        write(&opened.database, "notes", "recovered").await?;
        opened.database.close().await?;
        // Finished only once the root revoked what came before it.
        let settings = crate::enrollment::Settings::load(&directory.join("identity.json"))?;
        let kept = settings
            .revocations
            .recovery()
            .map(|recovery| recovery.keep);
        assert_eq!(kept, Some(settings.identity), "{stage:?}");
        // Finished: it opens, and is no longer recovered.
        let reopened = config.open().await?;
        assert_eq!(rows(&reopened.database).await?.len(), 3, "{stage:?}");
        reopened.database.close().await?;
        let again = config.recover(&archive, &key).await.err();
        let again = again.ok_or("recovered twice")?.to_string();
        assert!(again.contains("a domain exists"), "{stage:?}: {again}");
    }
    Ok(())
}

/// An archive goes out and comes back as bytes, the same way in a browser
/// and natively, leaving no file behind on either side.
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_backup_exported_as_bytes_recovers_the_domain() -> Result<(), Failure> {
    let root = Scratch::new().await;
    let schema = schema()?;
    let creator = root.path().join("creator");
    let created = Configuration::new(&creator, &schema)?.create().await?;
    let key = created.root.ok_or("root")?;
    write(&created.database, "notes", "before").await?;
    grant(&created.database, &root.path().join("reader")).await?;
    let archive = created.database.export_backup().await?;
    assert!(archive.starts_with(b"rocky.backup.v2\n"));
    created.database.close().await?;
    let files = |directory: &Path| -> Result<Vec<String>, Failure> {
        Ok(rocky_platform::fs::read_dir(directory)?
            .iter()
            .filter_map(|path| path.file_name()?.to_str().map(str::to_owned))
            .filter(|name| name.ends_with(".backup") || name.ends_with(".partial"))
            .collect())
    };
    assert_eq!(files(&creator)?, Vec::<String>::new());
    let (lost, _) = slot(&creator)?.ok_or("no creator")?;
    rocky_platform::fs::remove_dir_all(&creator)?;

    let directory = root.path().join("recovered");
    let config = Configuration::new(&directory, &schema)?;
    // Not an archive: refused, and nothing staged stays.
    let refused = config.recover_archive(&[0; 64], &key).await.err();
    let refused = refused.ok_or("recovered junk")?.to_string();
    assert!(refused.contains("not a Rocky backup"), "{refused}");
    assert_eq!(files(&directory)?, Vec::<String>::new());
    let opened = config.recover_archive(&archive, &key).await?;
    assert_eq!(
        serde_json::to_value(&opened.revoked)?,
        Value::Array(vec![lost])
    );
    assert_eq!(rows(&opened.database).await?, ["note/before"]);
    assert_eq!(files(&directory)?, Vec::<String>::new());
    write(&opened.database, "notes", "recovered").await?;
    opened.database.close().await?;
    Ok(())
}
