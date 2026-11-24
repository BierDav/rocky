use crate::common;
mod schema_changes;
use crate::support::snapshot as snap;
use common::{Device, Failure, introduce};
use rocky_sdk::{Network, transport::memory::Hub};
use serde_json::{Value, json};
use std::sync::Arc;

fn message(error: Value) -> String {
    error["message"].as_str().unwrap_or_default().to_owned()
}
fn seal_of(inspection: &Value, tip: &str) -> Value {
    inspection["seals"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|seal| seal["tip"] == tip && seal["canonical"] == true)
        .cloned()
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_continue_seal_moves_the_bookmark_and_keeps_every_row() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let a = Device::create(root.path(), None).await?;
    let before = snap::ok(a.database.tip_status()).await;
    assert_eq!(before["bookmark"], "main");
    let genesis = before["tip"].clone();
    a.add(1, "milk").await;
    a.add(2, "eggs").await;
    let written = snap::ok(a.database.tip_status()).await;
    assert_eq!(written["bookmark"], "main");
    assert_eq!(written["operations_since_seal"], 2);
    assert_eq!(written["evidence_releasable"], Value::Null);

    let sealed = snap::ok(a.database.seal()).await;
    assert_ne!(sealed["tip"], genesis);
    assert_eq!(sealed["bookmark"], "main");
    assert_eq!(sealed["operations_since_seal"], 0);
    assert_eq!(a.titles().await, ["eggs", "milk"]);
    a.add(3, "bread").await;
    assert_eq!(
        snap::ok(a.database.tip_status()).await["operations_since_seal"],
        1
    );

    let inspection = snap::ok(a.database.inspect_tips()).await;
    assert_eq!(inspection["genesis"], genesis);
    let seal = seal_of(&inspection, genesis.as_str().ok_or("tip")?);
    assert_eq!(seal["action"], "continue");
    assert_eq!(seal["id"], sealed["tip"]);
    assert_eq!(seal["frontier"].as_array().ok_or("frontier")?.len(), 1);
    let main = &inspection["bookmarks"][0];
    assert_eq!(
        (main["name"].clone(), main["tip"].clone()),
        (json!("main"), sealed["tip"].clone())
    );
    assert_eq!(main["moves"].as_array().ok_or("moves")?.len(), 2);
    let local = &inspection["nodes"][0];
    assert_eq!(local["followed_tip"], sealed["tip"]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn compact_keeps_the_requested_seals_and_every_row_across_reopen() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let a = Device::create(root.path(), None).await?;
    let none = snap::ok(a.database.compaction()).await;
    assert_eq!(
        (none["compacted"].clone(), none["pruned"].clone()),
        (Value::Null, json!(0))
    );
    a.add(1, "milk").await;
    let eggs = a.add(2, "eggs").await;
    a.set(3, &eggs, "completed", json!(true)).await;
    let first = snap::ok(a.database.seal()).await["tip"].clone();
    a.add(4, "bread").await;
    let second = snap::ok(a.database.seal()).await["tip"].clone();

    assert_eq!(
        snap::ok(a.database.compaction()).await["compacted"],
        Value::Null
    );
    let kept = snap::ok(a.database.compact(1)).await;
    assert_eq!(kept["finalized"], second);
    assert_eq!(kept["compacted"], first);
    assert_eq!(kept["uncompacted"], json!([second]));
    assert_eq!(kept["rebootstrap"], Value::Null);
    let all = snap::ok(a.database.compact(0)).await;
    assert_eq!(all["compacted"], second);
    assert_eq!(all["uncompacted"], json!([]));
    let inspected = snap::ok(a.database.inspect_tips()).await;
    let checkpoints_beside_seal = |seal: &Value| -> Vec<Value> {
        inspected["seals"]
            .as_array()
            .unwrap()
            .iter()
            .find(|candidate| candidate["id"] == *seal)
            .unwrap()["checkpoints"]
            .as_array()
            .unwrap()
            .clone()
    };
    let at_first = checkpoints_beside_seal(&first);
    assert_eq!(at_first.len(), 2);
    let mut rows: Vec<_> = at_first
        .iter()
        .map(|record| record["rows"].as_u64().unwrap())
        .collect();
    rows.sort_unstable();
    assert_eq!(rows, [0, 2]);
    assert_ne!(at_first[0]["scope"], at_first[1]["scope"]);
    let at_second = checkpoints_beside_seal(&second);
    assert_eq!(at_second.len(), 1);
    assert_eq!(at_second[0]["rows"], 3);
    for record in at_first.iter().chain(&at_second) {
        assert_eq!(record["authority"], "trusted");
        assert!(
            record["signer"]
                .as_str()
                .is_some_and(|signer| signer.len() == 64)
        );
        let scope: rocky_core::ResourceScope = serde_json::from_value(record["scope"].clone())?;
        assert_eq!(scope.domain, a.database.scope());
    }
    let empty_registry = at_first.iter().find(|record| record["rows"] == 0).unwrap();
    let changed = at_first.iter().find(|record| record["rows"] == 2).unwrap();
    assert_eq!(at_second[0]["scope"], changed["scope"]);
    assert_ne!(
        at_second[0]["scope"], empty_registry["scope"],
        "the unchanged empty registry keeps its signed base at the first seal"
    );
    assert_ne!(at_second[0]["checkpoint"], changed["checkpoint"]);
    let items = ["bread/0", "eggs/1", "milk/0"];
    assert_eq!(a.items().await, items);
    a.close().await;

    let a = Device::open(root.path(), None).await?;
    assert_eq!(a.items().await, items);
    let reopened = snap::ok(a.database.compaction()).await;
    assert_eq!(reopened["compacted"], second);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn only_trusted_devices_and_issuers_seal() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = Device::create(
        &root.path().join("a"),
        Some(Network::Owned(hub.connector("a"))),
    )
    .await?;
    let b = Device::join(
        &root.path().join("b"),
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )
    .await?;
    introduce(&a, &b).await;
    let sealed = snap::ok(a.database.seal()).await;
    b.sync().await.expect("sync");
    assert_eq!(
        snap::ok(b.database.tip_status()).await["tip"],
        sealed["tip"]
    );
    let refused = message(snap::observe(b.database.seal()).await.unwrap_err());
    assert!(
        refused.contains("only a trusted identity or an issuer"),
        "a writer never seals: {refused}"
    );

    let issuer = Device::join(
        &root.path().join("c"),
        &a.invite_as(rocky_sdk::Access::Full).await,
        Network::Owned(hub.connector("c")),
    )
    .await?;
    a.admit(&issuer).await;
    introduce(&a, &issuer).await;
    issuer.sync().await.expect("issuer sync");
    issuer.add(1, "issued").await;
    let resealed = snap::ok(issuer.database.seal()).await;
    assert_ne!(
        resealed["tip"], sealed["tip"],
        "an issuer seals and moves main forward"
    );
    assert_eq!(
        snap::ok(issuer.database.tip_status()).await["tip"],
        resealed["tip"]
    );
    Ok(())
}

mod trusted {
    use super::*;

    struct Household {
        _root: tempfile::TempDir,
        owner: Device,
        devices: Vec<Device>,
    }
    async fn household(count: usize) -> Result<Household, Failure> {
        let root = tempfile::tempdir()?;
        let hub = Arc::new(Hub::default());
        let owner = Device::create(
            &root.path().join("owner"),
            Some(Network::Owned(hub.connector("owner"))),
        )
        .await?;
        let mut devices = Vec::new();
        for index in 0..count {
            let name = format!("d{index}");
            let device = Device::join(
                &root.path().join(&name),
                &owner.invite().await,
                Network::Owned(hub.connector(&name)),
            )
            .await?;
            introduce(&owner, &device).await;
            for other in &devices {
                introduce(other, &device).await;
            }
            device.sync().await.expect("join sync");
            devices.push(device);
        }
        owner.add(1, "before").await;
        snap::ok(owner.database.seal()).await;
        let all: Vec<&Device> = std::iter::once(&owner).chain(&devices).collect();
        settle(&all).await;
        for device in &devices {
            assert_eq!(device.titles().await, ["before"]);
        }
        Ok(Household {
            _root: root,
            owner,
            devices,
        })
    }
    async fn identity(device: &Device) -> String {
        snap::ok(snap::device(&device.database)).await["principal"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }
    async fn trust(owner: &Device, device: &Device) -> Value {
        let person = identity(device).await;
        snap::ok(owner.database.make_admin(
            rocky_sdk::PersonId::new(snap::public_key(&person)),
            owner.root().clone(),
        ))
        .await;
        let granted = json!(person);
        snap::ok(device.database.renew()).await;
        settle(&[owner, device]).await;
        granted
    }
    async fn settle(devices: &[&Device]) {
        for _ in 0..3 {
            for device in devices {
                let _ = device.sync().await;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revoking_a_trusted_node_ends_its_tip_authority() -> Result<(), Failure> {
        let house = household(1).await?;
        let owner = &house.owner;
        let t = &house.devices[0];
        let granted = trust(owner, t).await;
        assert!(
            snap::observe(t.database.make_admin(
                rocky_sdk::PersonId::new(snap::public_key(&(identity(owner).await))),
                std::sync::Arc::new(rocky_sdk::RootKey::generate())
            ))
            .await
            .is_err(),
            "only the root grants trust"
        );
        snap::ok(owner.database.revoke_admin(
            rocky_sdk::PersonId::new(snap::public_key(&snap::text(&granted))),
            rocky_sdk::Removal::Compromised,
            (owner.root()).clone(),
        ))
        .await;
        settle(&[owner, t]).await;
        snap::observe(t.database.seal())
            .await
            .expect_err("the revoked trusted node learned it and seals nothing more");
        let status = snap::ok(t.database.status()).await;
        assert!(
            status["authority"]["verified_revocation"].is_object(),
            "{status}"
        );
        Ok(())
    }
}

mod races {
    use super::*;

    struct Household {
        _root: tempfile::TempDir,
        hub: Arc<Hub>,
        owner: Device,
        trusted: [Device; 2],
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
        let mut joined = Vec::new();
        for name in ["t0", "t1", "m"] {
            let invitation = if name == "m" {
                owner.invite().await
            } else {
                owner.invite_as(rocky_sdk::Access::Full).await
            };
            let device = Device::join(
                &root.path().join(name),
                &invitation,
                Network::Owned(hub.connector(name)),
            )
            .await?;
            if name != "m" {
                owner.admit(&device).await;
            }
            introduce(&owner, &device).await;
            for other in &joined {
                introduce(other, &device).await;
            }
            device.sync().await.expect("join sync");
            joined.push(device);
        }
        owner.add(1, "before").await;
        snap::ok(owner.database.seal()).await;
        let mut joined = joined.into_iter();
        let (t0, t1, member) = (
            joined.next().ok_or("t0")?,
            joined.next().ok_or("t1")?,
            joined.next().ok_or("m")?,
        );
        let house = Household {
            _root: root,
            hub,
            owner,
            trusted: [t0, t1],
            member,
        };
        let tip = snap::ok(house.owner.database.tip_status()).await["tip"].clone();
        settle_on(&house.all(), &tip, &["before"]).await;
        for device in house.all() {
            assert_eq!(device.titles().await, ["before"]);
        }
        Ok(house)
    }
    impl Household {
        fn all(&self) -> Vec<&Device> {
            vec![
                &self.owner,
                &self.trusted[0],
                &self.trusted[1],
                &self.member,
            ]
        }
    }
    async fn settle(devices: &[&Device]) {
        for _ in 0..3 {
            for device in devices {
                let _ = device.sync().await;
            }
        }
    }
    async fn settle_on(devices: &[&Device], tip: &Value, titles: &[&str]) {
        for _ in 0..3 {
            for device in devices {
                let _ = device.sync().await;
            }
            let mut ready = true;
            for device in devices {
                if snap::ok(device.database.tip_status()).await["tip"] != *tip {
                    ready = false;
                    break;
                }
                let page = snap::observe(snap::read(
                    &device.database,
                    "SELECT title FROM items ORDER BY title",
                    Vec::new(),
                ))
                .await;
                let rows = page.as_ref().ok().and_then(|page| page["rows"].as_array());
                let found: Option<Vec<&str>> = rows.map(|rows| {
                    rows.iter()
                        .filter_map(|row| row[0]["value"].as_str())
                        .collect()
                });
                if found.as_deref() != Some(titles) {
                    ready = false;
                    break;
                }
            }
            if ready {
                return;
            }
        }
        panic!("devices did not converge on {tip} with {titles:?}");
    }
    fn seal_by_id<'a>(inspection: &'a Value, id: &Value) -> &'a Value {
        inspection["seals"]
            .as_array()
            .and_then(|seals| seals.iter().find(|seal| seal["id"] == *id))
            .unwrap_or(&Value::Null)
    }

    async fn alerts(device: &Device, kind: &str) -> Vec<Value> {
        snap::ok(device.database.status()).await["alerts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|alert| alert["kind"]["type"] == kind)
            .cloned()
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revoking_the_creator_keeps_what_it_sealed_and_main_moves_on_through_another_trusted_node()
    -> Result<(), Failure> {
        let house = household().await?;
        let (owner, member) = (&house.owner, &house.member);
        owner.add(2, "sealed by the creator").await;
        let sealed = snap::ok(owner.database.seal()).await["tip"].clone();
        let titles = ["before", "sealed by the creator"];
        settle_on(&house.all(), &sealed, &titles).await;

        let creator = snap::ok(snap::device(&owner.database)).await["principal"].clone();
        let (t0, t1) = (&house.trusted[0], &house.trusted[1]);
        snap::ok(t0.database.revoke_admin(
            rocky_sdk::PersonId::new(snap::public_key(&(creator))),
            rocky_sdk::Removal::Compromised,
            (owner.root()).clone(),
        ))
        .await;
        let rest = [t0, t1];
        settle(&rest).await;
        let followed = snap::ok(t0.database.tip_status()).await["tip"].clone();
        settle_on(&rest, &followed, &titles).await;
        for device in rest {
            let inspection = snap::ok(device.database.inspect_tips()).await;
            let kept = seal_by_id(&inspection, &sealed);
            assert_eq!(
                (kept["revoked"].clone(), kept["canonical"].clone()),
                (json!(false), json!(true)),
                "{inspection}"
            );
            assert!(alerts(device, "tip_statements_past_cut").await.is_empty());
        }
        let ended = member
            .sync()
            .await
            .expect_err("the member ends with the creator");
        assert!(ended.to_string().contains("revoked"), "{ended}");
        t0.add(3, "after").await;
        settle(&rest).await;
        let next = snap::ok(t1.database.seal()).await["tip"].clone();
        assert_ne!(
            next, followed,
            "main keeps moving through the other trusted node"
        );
        settle_on(&rest, &next, &["after", "before", "sealed by the creator"]).await;
        for device in house.all() {
            device.close().await;
        }
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_statement_the_revoked_creator_signed_past_the_cut_is_deleted_and_surfaced()
    -> Result<(), Failure> {
        let house = household().await?;
        let (owner, member) = (&house.owner, &house.member);
        let (t0, t1) = (&house.trusted[0], &house.trusted[1]);
        isolate_t0(&house.hub, true);
        let late = snap::ok(owner.database.seal()).await["tip"].clone();
        settle(&[owner, t1, member]).await;
        assert!(!seal_by_id(&snap::ok(t1.database.inspect_tips()).await, &late).is_null());
        let creator = snap::ok(snap::device(&owner.database)).await["principal"].clone();
        snap::ok(t0.database.revoke_admin(
            rocky_sdk::PersonId::new(snap::public_key(&(creator))),
            rocky_sdk::Removal::Compromised,
            (owner.root()).clone(),
        ))
        .await;
        isolate_t0(&house.hub, false);
        settle(&[t0, t1]).await;
        let past = alerts(t1, "tip_statements_past_cut").await;
        assert_eq!(
            past.len(),
            1,
            "the device that held the statement past the cut deletes it and says so: {past:?}"
        );
        assert_eq!(
            past[0]["kind"]["statements"], 2,
            "the late seal and its move of main: {past:?}"
        );
        assert_eq!(past[0]["kind"]["signer"], creator, "{past:?}");
        for device in [t0, t1] {
            let inspection = snap::ok(device.database.inspect_tips()).await;
            assert!(seal_by_id(&inspection, &late).is_null(), "{inspection}");
        }
        for device in house.all() {
            device.close().await;
        }
        Ok(())
    }
    fn isolate_t0(hub: &Hub, down: bool) {
        for other in ["owner", "t1", "m"] {
            hub.set_down("t0", other, down);
        }
    }
}
