use crate::common::{self, Device, Failure};
use rocky_sdk::{Network, PersonId, SqlValue, transport::memory::Hub};
use std::sync::Arc;

async fn commit(device: &Device, title: &str) -> Result<(), rocky_sdk::Error> {
    device
        .database
        .write(async |transaction| {
            transaction
                .execute(
                    "INSERT INTO items(key,title,completed) VALUES(?,?,0)",
                    vec![SqlValue::Text(title.into()), SqlValue::Text(title.into())],
                )
                .await?;
            Ok(())
        })
        .await
}

async fn read(device: &Device) -> Result<Vec<rocky_sdk::Row>, rocky_sdk::Error> {
    device
        .database
        .query("SELECT title FROM items", Vec::new())
        .await?
        .collect()
        .await
}

fn revoked(result: Result<impl std::fmt::Debug, rocky_sdk::Error>) -> Result<(), Failure> {
    let error = result.err().ok_or("a removed device used its data")?;
    match error.kind() {
        rocky_sdk::ErrorKind::Removed => Ok(()),
        _ => Err(format!("expected a revoked refusal, got {error:?}").into()),
    }
}

struct Household {
    root: tempfile::TempDir,
    hub: Arc<Hub>,
    owner: Device,
    member: Device,
}

async fn household() -> Result<Household, Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let owner = Device::create(
        &root.path().join("owner"),
        Some(Network::Owned(hub.connector("owner"))),
    )
    .await?;
    let member = Device::join(
        &root.path().join("member"),
        &owner.invite().await,
        Network::Owned(hub.connector("member")),
    )
    .await?;
    common::introduce(&owner, &member).await;
    owner.add(1, "Milk").await;
    member.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(member.titles().await, ["Milk"]);
    Ok(Household {
        root,
        hub,
        owner,
        member,
    })
}

async fn remove(household: &Household) -> Result<(), Failure> {
    let person = household.member.database.me().await?.person;
    household
        .owner
        .database
        .remove(PersonId::new(person.key()))
        .await?;
    Ok(())
}

async fn learns_its_removal(member: &Device) -> Result<(), Failure> {
    let refused = member.sync().await.err().ok_or("a removed device synced")?;
    assert_eq!(refused["code"], "removed", "{refused}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_removed_device_that_learns_its_removal_purges_its_data_and_refuses_commits()
-> Result<(), Failure> {
    let household = household().await?;
    remove(&household).await?;
    learns_its_removal(&household.member).await?;
    revoked(read(&household.member).await)?;
    revoked(commit(&household.member, "Bread").await)?;
    household.member.close().await;
    let reopened = Device::open(&household.root.path().join("member"), None).await?;
    revoked(read(&reopened).await)?;
    revoked(commit(&reopened, "Butter").await)?;
    reopened.close().await;
    household.owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_offline_removed_device_commits_until_it_syncs_then_purges() -> Result<(), Failure> {
    let household = household().await?;
    household.member.database.detach().await?;
    remove(&household).await?;
    commit(&household.member, "Bread").await?;
    assert_eq!(household.member.titles().await, ["Bread", "Milk"]);
    household
        .member
        .database
        .attach(Network::Owned(household.hub.connector("member")))
        .await?;
    learns_its_removal(&household.member).await?;
    revoked(read(&household.member).await)?;
    revoked(commit(&household.member, "Butter").await)?;
    let titles = household
        .owner
        .database
        .query("SELECT title FROM items ORDER BY title", Vec::new())
        .await?
        .collect()
        .await?
        .len();
    assert_eq!(
        titles, 1,
        "the removed device's offline commit never reaches the owner"
    );
    household.member.close().await;
    household.owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_removed_device_gives_one_explanation_on_sync_read_and_commit() -> Result<(), Failure> {
    let household = household().await?;
    remove(&household).await?;
    let synced = household
        .member
        .database
        .sync_now()
        .await
        .err()
        .ok_or("a removed device synced")?;
    let read = read(&household.member)
        .await
        .err()
        .ok_or("a removed device read")?;
    let committed = commit(&household.member, "Bread")
        .await
        .err()
        .ok_or("a removed device committed")?;
    for error in [&synced, &read, &committed] {
        assert_eq!(error.kind(), &rocky_sdk::ErrorKind::Removed, "{error}");
        assert_eq!(error.to_string(), synced.to_string());
    }
    assert!(
        synced.to_string().contains("logout(Logout::PurgeOnClose)"),
        "the message names the purging logout that deletes the rest: {synced}"
    );
    household.member.close().await;
    household.owner.close().await;
    Ok(())
}
