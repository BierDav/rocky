use super::*;
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    rows: usize,
    schema: Schema,
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
    for role in ["a", "b"] {
        truncate_wal(&root.path().join(role).join("rocky.sqlite"))?;
    }
    fs::create_dir(destination)?;
    fs::set_permissions(destination, fs::metadata(root.path())?.permissions())?;
    for role in ["a", "b"] {
        copy_directory(&root.path().join(role), &destination.join(role))?;
    }
    write_manifest(
        destination,
        &Manifest {
            version: 1,
            rows: large,
            schema: fill,
        },
    )
}

fn truncate_wal(database: &Path) -> Result<(), Failure> {
    let db = rusqlite::Connection::open(database)?;
    let busy: i64 = db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
    if busy != 0 {
        return Err("prepared fixture still has an active SQLite handle".into());
    }
    Ok(())
}

fn write_manifest(destination: &Path, manifest: &Manifest) -> Result<(), Failure> {
    fs::write(
        destination.join("fixture.json"),
        serde_json::to_vec(manifest)?,
    )?;
    Ok(())
}

async fn restore(source: &Path) -> Result<MemberFixture, Failure> {
    let manifest: Manifest = serde_json::from_slice(&fs::read(source.join("fixture.json"))?)?;
    if manifest.version != 1 || manifest.rows != 8 || manifest.schema != filled(manifest.rows) {
        return Err("unsupported prepared member-pull fixture".into());
    }
    let root = crate::support::small_directory()?;
    for role in ["a", "b"] {
        copy_directory(&source.join(role), &root.path().join(role))?;
    }
    let hub = Arc::new(Hub::default());
    let a = Device::new(
        open(
            &root.path().join("a"),
            &manifest.schema,
            Some(Network::Owned(hub.connector("a"))),
        )
        .open()
        .await?,
    )?;
    Ok(MemberFixture {
        root,
        large: manifest.rows,
        multi_step: true,
        fill: manifest.schema,
        hub,
        a,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn relocated_fixture_can_be_pulled_twice() -> Result<(), Failure> {
    let cache = crate::support::small_directory()?;
    let fixture = cache.path().join("prepared");
    retain(prepare_member_fixture(8, 0).await?, &fixture).await?;
    let independent_trials_after_the_originals_were_dropped = 2;
    for _ in 0..independent_trials_after_the_originals_were_dropped {
        pull_member_fixture(restore(&fixture).await?).await?;
    }
    Ok(())
}
