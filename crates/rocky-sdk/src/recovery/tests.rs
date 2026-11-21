use super::fault::{STOPS, Stage};
use crate::{Affinity, Column, Configuration, Failure, Schema, Table, params};
use rocky_platform::fs::Scratch;
use serde_json::Value;
use std::path::Path;

fn schema() -> Result<Schema, Failure> {
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
async fn grant(database: &crate::Database, directory: &Path) -> Result<(), Failure> {
    let device = crate::Database::node_keys_of(
        directory,
        None,
        Some(std::sync::Arc::new(crate::IdentityKey::generate())),
    )?;
    let session = database.session();
    session
        .grant(
            device,
            crate::GrantProfile::Reader {
                scopes: ["notes".into()].into(),
            },
            crate::GrantTerms::default(),
        )
        .await?;
    session.close().await;
    Ok(())
}
fn slot(directory: &Path) -> Result<Option<Value>, Failure> {
    let path = directory.join("rocky.sqlite");
    if !crate::enrollment::Settings::exists(&path)? {
        return Ok(None);
    }
    let saved = crate::enrollment::Settings::load(&path)?;
    Ok(Some(serde_json::to_value(saved.identity)?))
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
    let lost = slot(&creator)?.ok_or("no creator")?;
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
        let refused = config.open().await.err();
        let refused = refused.ok_or("opened half-recovered")?.to_string();
        let expected = if saved.is_some() {
            "a recovery is unfinished"
        } else {
            "no domain in this directory"
        };
        assert!(refused.contains(expected), "{stage:?}: {refused}");

        let opened = config.recover(&archive, &key).await?;
        assert_eq!(
            serde_json::to_value(&opened.revoked)?,
            Value::Array(vec![lost.clone()]),
            "{stage:?}: named in full, also what an earlier attempt revoked"
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
        let settings = crate::enrollment::Settings::load(&directory.join("rocky.sqlite"))?;
        let kept = settings
            .revocations
            .recovery()
            .map(|recovery| recovery.keep);
        assert_eq!(
            kept,
            Some(settings.identity),
            "{stage:?}: finished only once the root revoked what came before it"
        );
        let reopened = config.open().await?;
        assert_eq!(rows(&reopened.database).await?.len(), 3, "{stage:?}");
        reopened.database.close().await?;
        let again = config.recover(&archive, &key).await.err();
        let again = again.ok_or("recovered twice")?.to_string();
        assert!(
            again.contains("a domain exists"),
            "{stage:?}: a finished recovery is no longer recovered: {again}"
        );
    }
    Ok(())
}

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
    assert!(archive.starts_with(b"rocky.backup.v4\n"));
    created.database.close().await?;
    let files = |directory: &Path| -> Result<Vec<String>, Failure> {
        Ok(rocky_platform::fs::read_dir(directory)?
            .iter()
            .filter_map(|path| path.file_name()?.to_str().map(str::to_owned))
            .filter(|name| name.ends_with(".backup") || name.ends_with(".partial"))
            .collect())
    };
    assert_eq!(files(&creator)?, Vec::<String>::new());
    let lost = slot(&creator)?.ok_or("no creator")?;
    rocky_platform::fs::remove_dir_all(&creator)?;

    let directory = root.path().join("recovered");
    let config = Configuration::new(&directory, &schema)?;
    let refused = config.recover_archive(&[0; 64], &key).await.err();
    let refused = refused.ok_or("recovered junk")?.to_string();
    assert!(refused.contains("not a Rocky backup"), "{refused}");
    assert_eq!(
        files(&directory)?,
        Vec::<String>::new(),
        "a refused non-archive leaves nothing staged"
    );
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
