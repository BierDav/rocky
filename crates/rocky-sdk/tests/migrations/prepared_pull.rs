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
        || ![8, LARGE].contains(&manifest.rows)
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

#[cfg(feature = "qualification")]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "explicit benchmark preparation; use scripts/measure-prepared-pull"]
async fn prepare() -> Result<(), Failure> {
    let destination = std::env::var_os("ROCKY_PULL_FIXTURE").ok_or("missing ROCKY_PULL_FIXTURE")?;
    retain(
        prepare_member_fixture(LARGE, 0, true).await?,
        Path::new(&destination),
    )
    .await
}

#[cfg(feature = "qualification")]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "explicit benchmark trial; use scripts/measure-prepared-pull"]
async fn replay() -> Result<(), Failure> {
    let source = std::env::var_os("ROCKY_PULL_FIXTURE").ok_or("missing ROCKY_PULL_FIXTURE")?;
    pull_member_fixture(restore(Path::new(&source))?).await
}

#[tokio::test(flavor = "multi_thread")]
async fn relocated_fixture_can_be_pulled_twice() -> Result<(), Failure> {
    let cache = common::identity::small_directory()?;
    let fixture = cache.path().join("prepared");
    retain(prepare_member_fixture(8, 0, false).await?, &fixture).await?;
    // Retain dropped the original directories; both trials must work independently.
    for _ in 0..2 {
        pull_member_fixture(restore(&fixture)?).await?;
    }
    Ok(())
}
