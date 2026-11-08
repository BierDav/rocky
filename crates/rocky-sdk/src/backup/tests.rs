//! A member's backup restored from bytes under a fresh invitation, the
//! same natively and in a browser, and a restore stopped at each step
//! finishing under the identity it first enrolled.
use crate::recovery::fault::{STOPS, Stage};
use crate::{
    Affinity, Column, Configuration, Database, Failure, GrantProfile, Network, Schema, Table,
    params, transport::memory::Hub,
};
use rocky_platform::fs::Scratch;
use std::{path::Path, sync::Arc};

fn schema() -> Result<Schema, Failure> {
    Schema::builder("restore", 1)
        .table(Table::replayed("notes").column(Column::new("body", Affinity::Text).required()))
        .build()
}
async fn write(database: &Database, body: &str) -> Result<(), Failure> {
    let mut tx = database.transaction().await?;
    let id = tx.next_id("notes", vec![]).await?;
    tx.execute("INSERT INTO notes(id,body) VALUES(?,?)", params![id, body])
        .await?;
    tx.commit().await?;
    Ok(())
}
async fn bodies(database: &Database) -> Result<Vec<String>, Failure> {
    let rows = database
        .query("SELECT body FROM notes ORDER BY body", params![])
        .await?
        .collect()
        .await?;
    Ok(rows
        .iter()
        .map(|row| row.get::<String>("body"))
        .collect::<Result<_, _>>()?)
}
async fn invite(owner: &Database) -> Result<String, Failure> {
    let profile = GrantProfile::Writer {
        scopes: ["notes".into()].into(),
    };
    Ok(owner.session().invitation_with(profile).await?.uri)
}
/// Staged archives left in a directory.
fn staged(directory: &Path) -> Result<Vec<String>, Failure> {
    if !rocky_platform::fs::exists(directory) {
        return Ok(Vec::new());
    }
    Ok(rocky_platform::fs::read_dir(directory)?
        .iter()
        .filter_map(|path| path.file_name()?.to_str().map(str::to_owned))
        .filter(|name| name.ends_with(".backup"))
        .collect())
}
fn identity(directory: &Path) -> Result<serde_json::Value, Failure> {
    let saved: serde_json::Value =
        serde_json::from_slice(&rocky_platform::fs::read(directory.join("identity.json"))?)?;
    Ok(saved["identity"].clone())
}
/// An owner and a member that synced "synced" and holds "unsent", which
/// never left it, exported as bytes, with the member's directory deleted.
struct Lost {
    owner: Arc<Database>,
    archive: Vec<u8>,
    identity: serde_json::Value,
}
async fn lose_a_member(hub: &Arc<Hub>, root: &Path) -> Result<Lost, Failure> {
    let owner = Configuration::new(root.join("owner"), &schema()?)?
        .network(Network::Owned(hub.connector("owner")))
        .create()
        .await?
        .database;
    owner.session().set_automatic_sync(false).await?;
    let directory = root.join("member");
    let member = Configuration::new(&directory, &schema()?)?
        .network(Network::Owned(hub.connector("member")))
        .join(&invite(&owner).await?)
        .await?
        .database;
    let session = member.session();
    session.set_automatic_sync(false).await?;
    write(&member, "synced").await?;
    session.sync_once().await?;
    write(&member, "unsent").await?;
    let archive = member.export_backup().await?;
    member.close().await?;
    let identity = identity(&directory)?;
    rocky_platform::fs::remove_dir_all(&directory)?;
    Ok(Lost {
        owner,
        archive,
        identity,
    })
}
/// The restored node holds both writes, sends the unsent one and writes
/// `body` on its own.
async fn restored_works(owner: &Database, restored: &Database, body: &str) -> Result<(), Failure> {
    assert_eq!(bodies(restored).await?, ["synced", "unsent"]);
    write(restored, body).await?;
    let session = restored.session();
    session.set_automatic_sync(false).await?;
    session.sync_once().await?;
    let delivered = bodies(owner).await?;
    for expected in [body, "synced", "unsent"] {
        assert!(
            delivered.iter().any(|seen| seen == expected),
            "{delivered:?}"
        );
    }
    Ok(())
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_member_restored_from_bytes_with_a_fresh_invitation_syncs_and_writes()
-> Result<(), Failure> {
    let root = Scratch::new().await;
    let hub = Arc::new(Hub::default());
    let lost = lose_a_member(&hub, root.path()).await?;

    let directory = root.path().join("restored");
    let config = Configuration::new(&directory, &schema()?)?
        .network(Network::Owned(hub.connector("restored")));
    // Not an archive: refused, and nothing staged stays.
    let invitation = invite(&lost.owner).await?;
    let refused = config.restore_archive(&[0; 64], &invitation).await.err();
    let refused = refused.ok_or("restored junk")?.to_string();
    assert!(refused.contains("not a Rocky backup"), "{refused}");
    assert_eq!(staged(&directory)?, Vec::<String>::new());

    let opened = config
        .restore_archive(&lost.archive, &invite(&lost.owner).await?)
        .await?;
    assert_eq!(staged(&directory)?, Vec::<String>::new());
    assert_ne!(identity(&directory)?, lost.identity, "a fresh identity");
    restored_works(&lost.owner, &opened.database, "restored").await?;
    opened.database.close().await?;
    // Restored once: the directory holds a domain and opens.
    let invitation = invite(&lost.owner).await?;
    let again = config
        .restore_archive(&lost.archive, &invitation)
        .await
        .err();
    let again = again.ok_or("restored twice")?.to_string();
    assert!(again.contains("a domain exists"), "{again}");
    assert_eq!(staged(&directory)?, Vec::<String>::new());
    let reopened = config.open().await?.database;
    assert_eq!(bodies(&reopened).await?, ["restored", "synced", "unsent"]);
    reopened.close().await?;
    lost.owner.close().await
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_restore_stopped_at_any_step_finishes_when_restored_again_under_its_identity()
-> Result<(), Failure> {
    let root = Scratch::new().await;
    let hub = Arc::new(Hub::default());
    let lost = lose_a_member(&hub, root.path()).await?;
    for stage in [Stage::Enrolled, Stage::Rebound, Stage::Attached] {
        let directory = root.path().join(format!("{stage:?}"));
        STOPS
            .lock()
            .map_err(|_| "poisoned")?
            .push((directory.clone(), stage));
        let config = Configuration::new(&directory, &schema()?)?
            .network(Network::Owned(hub.connector(&format!("{stage:?}"))));
        let invitation = invite(&lost.owner).await?;
        let stopped = config.restore_archive(&lost.archive, &invitation).await;
        let stopped = stopped.err().ok_or("restored past the stop")?.to_string();
        assert!(stopped.contains("injected stop"), "{stage:?}: {stopped}");
        assert_eq!(staged(&directory)?, Vec::<String>::new(), "{stage:?}");
        let enrolled = identity(&directory)?;
        if stage == Stage::Enrolled {
            // Nothing placed: opening is refused and names the restore.
            let refused = config.open().await.err().ok_or("opened unplaced")?;
            let refused = refused.to_string();
            assert!(
                refused.contains("a restore is unfinished"),
                "{stage:?}: {refused}"
            );
        }
        // The invitation was spent: the restore goes on as enrolled.
        let opened = config.restore_archive(&lost.archive, &invitation).await?;
        assert_eq!(identity(&directory)?, enrolled, "{stage:?}: enrolled again");
        assert_eq!(staged(&directory)?, Vec::<String>::new(), "{stage:?}");
        restored_works(&lost.owner, &opened.database, &format!("{stage:?}")).await?;
        opened.database.close().await?;
    }
    lost.owner.close().await
}

/// A crash during the open that rebinds a restored file leaves its WAL
/// behind. Restoring again replaces the file and never applies that WAL.
// Needs a native file system and a raw connection to it.
#[cfg(not(target_family = "wasm"))]
#[tokio::test]
async fn a_restore_resumed_after_a_crashed_open_drops_its_stale_wal() -> Result<(), Failure> {
    let root = Scratch::new().await;
    let hub = Arc::new(Hub::default());
    let lost = lose_a_member(&hub, root.path()).await?;
    let directory = root.path().join("crashed");
    STOPS
        .lock()
        .map_err(|_| "poisoned")?
        .push((directory.clone(), Stage::Rebound));
    let config = Configuration::new(&directory, &schema()?)?
        .network(Network::Owned(hub.connector("crashed")));
    let invitation = invite(&lost.owner).await?;
    let stopped = config.restore_archive(&lost.archive, &invitation).await;
    assert!(stopped.is_err(), "restored past the stop");
    // The rebound file and a WAL of its own, as a crash would leave them.
    let database = directory.join("rocky.sqlite");
    let wal = directory.join("rocky.sqlite-wal");
    let crashed = {
        let connection = rusqlite::Connection::open(&database)?;
        connection.pragma_update(None, "wal_autocheckpoint", 0)?;
        connection.execute_batch("CREATE TABLE stale(x); INSERT INTO stale VALUES(1);")?;
        std::fs::read(&wal)?
    };
    std::fs::write(&wal, crashed)?;

    let opened = config.restore_archive(&lost.archive, &invitation).await?;
    restored_works(&lost.owner, &opened.database, "resumed").await?;
    opened.database.close().await?;
    let connection = rusqlite::Connection::open(&database)?;
    let stale: i64 = connection.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE name='stale'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(stale, 0, "the stale WAL applied to the restored file");
    lost.owner.close().await
}
