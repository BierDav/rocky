use crate::common;

use common::{World, titles};
use rocky_sdk::DropReason;
use todo_app::{Failure, Role};

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
        [Role::Admin, Role::Full, Role::Partial]
    );
    assert_eq!(watch.role().await?, Role::Partial);

    let errands = laptop.create_list("Errands").await?.id;
    laptop.add(errands, "Post").await?;
    let groceries = phone.create_list("Groceries").await?.id;
    phone.add(groceries, "Milk").await?;
    world.sync().await?;
    for device in [&laptop, &phone] {
        assert_eq!(
            device.lists().await?.len(),
            2,
            "admin and full devices hold every list"
        );
    }
    assert!(
        watch.lists().await?.is_empty(),
        "a partial device holds no list until one is shared with it"
    );

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

    let tablet = world.join("tablet", Role::Full).await?;
    world.sync().await?;
    assert_eq!(
        tablet.lists().await?.len(),
        2,
        "a full device joining later receives the existing lists"
    );

    let watch_node = watch.node().await?;
    assert!(
        laptop.remove_device(&laptop.node().await?).await.is_err(),
        "a device cannot remove itself"
    );
    laptop.remove_device(&watch_node).await?;
    world
        .devices
        .retain(|device| device.database() != watch.database());
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
        assert!(
            !nodes.contains(&watch_node),
            "a removed device is dropped from every member list"
        );
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

    laptop.move_to(list, ids[2], 1).await?;
    phone.move_to(list, ids[1], 0).await?;
    laptop.raise_priority(list, ids[0]).await?;
    phone.raise_priority(list, ids[0]).await?;
    let laptop_pharmacy = laptop.add(list, "Pharmacy").await?;
    let phone_pharmacy = phone.add(list, "Pharmacy").await?;
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
        Some(2),
        "both offline raises count"
    );
    assert_eq!(
        titles(&laptop, list).await?,
        ["Bank", "Post", "Bakery", "Pharmacy"],
        "each move rewrote only its own todo, so both placements hold"
    );

    let tokens = [laptop_pharmacy.token, phone_pharmacy.token];
    let mut outcomes = Vec::new();
    for device in [&laptop, &phone] {
        let mut here = Vec::new();
        for token in tokens {
            here.push(device.dropped(token).await?);
        }
        outcomes.push(here);
    }
    assert_eq!(
        outcomes[0], outcomes[1],
        "both devices agree on which insert dropped"
    );
    assert_eq!(
        outcomes[0].iter().flatten().collect::<Vec<_>>(),
        [&(DropReason::Constraint, false)],
        "exactly one of the two \"Pharmacy\" inserts dropped"
    );

    laptop.delete_list(list).await?;
    world.sync().await?;
    assert!(
        phone.all_todos().await?.is_empty(),
        "deleting a list cascades to its todos on every device"
    );
    world.close().await
}
