//! Trusted, full and partial devices over an in-process link: lists reach
//! the devices their roles promise, and offline edits converge.
use rocky_sdk::{DropReason, Failure, Network, transport::memory::Hub};
use std::{path::Path, sync::Arc};
use todo_app::{Role, Todos, configuration, roles};

struct World {
    root: tempfile::TempDir,
    hub: Arc<Hub>,
    devices: Vec<Todos>,
}
impl World {
    async fn new() -> Result<Self, Failure> {
        let root = tempfile::tempdir()?;
        let hub = Arc::new(Hub::default());
        let laptop = roles::create(&Self::config(root.path(), &hub, "laptop")?, "Laptop").await?;
        Ok(Self {
            root,
            hub,
            devices: vec![laptop],
        })
    }
    fn config(
        root: &Path,
        hub: &Arc<Hub>,
        name: &str,
    ) -> Result<rocky_sdk::Configuration, Failure> {
        Ok(configuration(root.join(name))?.network(Network::Owned(hub.connector(name))))
    }
    /// Joins a device invited by the first and meshes it with every other.
    async fn join(&mut self, name: &str, role: Role) -> Result<Todos, Failure> {
        let code = self.devices[0].invite(role).await?;
        let joined = roles::join(
            &Self::config(self.root.path(), &self.hub, name)?,
            &code,
            name,
        )
        .await?;
        for other in &self.devices {
            joined
                .database()
                .session()
                .add_peer(&other.database().session().contact().await?)
                .await?;
            other
                .database()
                .session()
                .add_peer(&joined.database().session().contact().await?)
                .await?;
        }
        self.devices.push(joined.clone());
        Ok(joined)
    }
    async fn sync(&self) -> Result<(), Failure> {
        for _ in 0..2 {
            for device in &self.devices {
                device.database().session().sync_once().await?;
            }
        }
        Ok(())
    }
    async fn close(self) -> Result<(), Failure> {
        for device in &self.devices {
            device.database().close().await?;
        }
        Ok(())
    }
}
async fn titles(device: &Todos, list: i64) -> Result<Vec<String>, Failure> {
    Ok(device
        .todos(list)
        .await?
        .into_iter()
        .map(|todo| todo.title)
        .collect())
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_reach_devices_by_role() -> Result<(), Failure> {
    let mut world = World::new().await?;
    let phone = world.join("phone", Role::Full).await?;
    let watch = world.join("watch", Role::Partial).await?;
    world.sync().await?;
    let laptop = world.devices[0].clone();
    assert_eq!(
        laptop
            .members()
            .await?
            .iter()
            .map(|member| member.role)
            .collect::<Vec<_>>(),
        [Role::Trusted, Role::Full, Role::Partial]
    );
    assert_eq!(watch.role().await?, Some(Role::Partial));

    let errands = laptop.create_list("Errands").await?.id;
    laptop.add(errands, "Post").await?;
    let groceries = phone.create_list("Groceries").await?.id;
    phone.add(groceries, "Milk").await?;
    world.sync().await?;
    // Full and trusted devices hold every list; the partial one none yet.
    for device in [&laptop, &phone] {
        assert_eq!(device.lists().await?.len(), 2);
    }
    assert!(watch.lists().await?.is_empty());

    laptop.share(errands, &[watch.node().await?]).await?;
    world.sync().await?;
    let held: Vec<_> = watch
        .lists()
        .await?
        .into_iter()
        .map(|list| list.title)
        .collect();
    assert_eq!(held, ["Errands"]);
    watch.add(errands, "Bank").await?;
    world.sync().await?;
    assert_eq!(titles(&laptop, errands).await?, ["Post", "Bank"]);

    // A full device joining later receives the existing lists.
    let tablet = world.join("tablet", Role::Full).await?;
    world.sync().await?;
    laptop.share_lists_with_members().await?;
    world.sync().await?;
    assert_eq!(tablet.lists().await?.len(), 2);

    // Removing a device revokes it and drops it from every member list;
    // a device cannot remove itself.
    let watch_node = watch.node().await?;
    assert!(laptop.remove_device(&laptop.node().await?).await.is_err());
    laptop.remove_device(&watch_node).await?;
    world
        .devices
        .retain(|device| !Arc::ptr_eq(device.database(), watch.database()));
    watch.database().close().await?;
    world.sync().await?;
    for device in [&laptop, &phone, &tablet] {
        let nodes: Vec<_> = device
            .members()
            .await?
            .into_iter()
            .map(|member| member.node)
            .collect();
        assert_eq!(nodes.len(), 3);
        assert!(!nodes.contains(&watch_node));
    }
    world.close().await
}

#[tokio::test(flavor = "multi_thread")]
async fn offline_edits_converge_in_one_order() -> Result<(), Failure> {
    let mut world = World::new().await?;
    let phone = world.join("phone", Role::Full).await?;
    world.sync().await?;
    let laptop = world.devices[0].clone();
    let list = laptop.create_list("Errands").await?.id;
    let mut ids = Vec::new();
    for title in ["Post", "Bank", "Bakery"] {
        ids.push(laptop.add(list, title).await?.id);
    }
    world.sync().await?;
    assert_eq!(titles(&phone, list).await?, ["Post", "Bank", "Bakery"]);

    // Offline from each other: both move a todo into the first gap, both
    // raise one priority, and both add "Pharmacy".
    laptop.move_to(list, ids[2], 1).await?;
    phone.move_to(list, ids[1], 0).await?;
    laptop.raise_priority(list, ids[0]).await?;
    phone.raise_priority(list, ids[0]).await?;
    let first = laptop.add(list, "Pharmacy").await?;
    let second = phone.add(list, "Pharmacy").await?;
    assert_eq!(
        titles(&phone, list).await?,
        ["Bank", "Post", "Bakery", "Pharmacy"]
    );
    world.sync().await?;

    let converged = laptop.all_todos().await?;
    assert_eq!(phone.all_todos().await?, converged);
    assert_eq!(
        converged
            .iter()
            .find(|todo| todo.id == ids[0])
            .map(|todo| todo.priority),
        Some(2)
    );
    // Each move rewrote only its own todo, so both placements hold.
    assert_eq!(
        titles(&laptop, list).await?,
        ["Bank", "Post", "Bakery", "Pharmacy"]
    );

    // Exactly one of the two "Pharmacy" inserts dropped, the same on both.
    let tokens = [
        first.token.ok_or("published")?,
        second.token.ok_or("published")?,
    ];
    let mut outcomes = Vec::new();
    for device in [&laptop, &phone] {
        let mut here = Vec::new();
        for token in tokens {
            here.push(device.dropped(token).await?);
        }
        outcomes.push(here);
    }
    assert_eq!(outcomes[0], outcomes[1]);
    assert_eq!(
        outcomes[0].iter().flatten().collect::<Vec<_>>(),
        [&(DropReason::Constraint, false)]
    );

    // Deleting the list cascades on both.
    laptop.delete_list(list).await?;
    world.sync().await?;
    assert!(phone.all_todos().await?.is_empty());
    world.close().await
}
