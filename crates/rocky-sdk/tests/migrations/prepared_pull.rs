//! Closed, relocatable input for repeated member-pull measurements.
use super::*;
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    rows: usize,
    schema: String,
}

fn copy_directory(source: &Path, target: &Path) -> Result<(), Failure> {
    fs::create_dir(target)?;
    fs::set_permissions(target, fs::metadata(source)?.permissions())?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let destination = target.join(entry.file_name());
        if kind.is_dir() {
            copy_directory(&entry.path(), &destination)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), destination)?;
        } else {
            return Err("prepared fixtures must contain only directories and regular files".into());
        }
    }
    Ok(())
}

async fn retain(fixture: MemberFixture, destination: &Path) -> Result<(), Failure> {
    let MemberFixture {
        root,
        large,
        multi_step,
        fill,
        hub,
        a,
    } = fixture;
    assert!(multi_step);
    a.close().await;
    drop(a);
    drop(hub);
    // Close/checkpoint before copying, including when SQLite retained an empty WAL.
    for role in ["a", "b"] {
        let db = rusqlite::Connection::open(root.path().join(role).join("rocky.sqlite"))?;
        let busy: i64 = db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
        if busy != 0 {
            return Err("prepared fixture still has an active SQLite handle".into());
        }
    }
    fs::create_dir(destination)?;
    fs::set_permissions(destination, fs::metadata(root.path())?.permissions())?;
    for role in ["a", "b"] {
        copy_directory(&root.path().join(role), &destination.join(role))?;
    }
    // Written last: a partial preparation is never accepted for replay.
    fs::write(
        destination.join("fixture.json"),
        serde_json::to_vec(&Manifest {
            version: 1,
            rows: large,
            schema: fill,
        })?,
    )?;
    Ok(())
}

fn restore(source: &Path) -> Result<MemberFixture, Failure> {
    let manifest: Manifest = serde_json::from_slice(&fs::read(source.join("fixture.json"))?)?;
    if manifest.version != 1
        || ![8, PULL_ROWS].contains(&manifest.rows)
        || manifest.schema != FILLED.replace("100000", &manifest.rows.to_string())
    {
        return Err("unsupported prepared member-pull fixture".into());
    }
    let root = common::identity::small_directory()?;
    // File secrets are relative to each installation directory. Copy all of them;
    // reopening validates the retained identity against those relocated keys.
    for role in ["a", "b"] {
        copy_directory(&source.join(role), &root.path().join(role))?;
    }
    let hub = Arc::new(Hub::default());
    let a = Device::new(Database::open(open(
        &root.path().join("a"),
        &manifest.schema,
        Some(Network::Owned(hub.connector("a"))),
    ))?)?;
    Ok(MemberFixture {
        root,
        large: manifest.rows,
        multi_step: true,
        fill: manifest.schema,
        hub,
        a,
    })
}

/// Rows of the features_13 sync non-regression member pull.
const PULL_ROWS: usize = 100_000;

/// A member pulls the issuer's 100k-row migration, three times from one
/// prepared fixture. Diagnostic; `cargo test --release -p rocky-sdk --test
/// migrations prepared_pull::member_pull -- --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "measurement"]
async fn member_pull_of_100000_rows() -> Result<(), Failure> {
    if cfg!(debug_assertions) {
        return Err("run measurements with --release".into());
    }
    let cache = common::identity::small_directory()?;
    let fixture = cache.path().join("prepared");
    retain(prepare_member_fixture(PULL_ROWS, 0).await?, &fixture).await?;
    for _ in 0..3 {
        pull_member_fixture(restore(&fixture)?).await?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn relocated_fixture_can_be_pulled_twice() -> Result<(), Failure> {
    let cache = common::identity::small_directory()?;
    let fixture = cache.path().join("prepared");
    retain(prepare_member_fixture(8, 0).await?, &fixture).await?;
    // Retain dropped the original directories; both trials must work independently.
    for _ in 0..2 {
        pull_member_fixture(restore(&fixture)?).await?;
    }
    Ok(())
}
