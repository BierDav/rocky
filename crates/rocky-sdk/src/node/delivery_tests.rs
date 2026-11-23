use crate::test_snapshot as snap;
use crate::{
    Configuration, Database, SecretStore,
    delivery::{Mailbox, fault},
    transport::memory::Hub,
};
use rocky_credentials::testing::MemoryKeys;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex},
};

type Failure = crate::Failure;

const SCHEMA: &str = r#"{"application":"delivery","version":1,"format":7,"tables":[
 {"name":"notes","class":"replayed","columns":[{"name":"key","type":"text","not_null":true}],"primary_key":["key"]},
 {"name":"parts","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true}],"primary_key":["list","key"],"partition_by":["list"],"partition_group":"list"},
 {"name":"labels","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true}],"primary_key":["list","key"],"partition_by":["list"],"partition_group":"list"}]}"#;
const LIST: &str = "Weekend";

static KEYS: LazyLock<Mutex<HashMap<PathBuf, Arc<MemoryKeys>>>> = LazyLock::new(Default::default);
fn keys(directory: &Path) -> Arc<dyn SecretStore> {
    KEYS.lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .entry(directory.to_path_buf())
        .or_default()
        .clone()
}

struct Device {
    database: Database,
    directory: PathBuf,
    name: String,
    hub: Arc<Hub>,
}
fn parsed_schema() -> &'static crate::Schema {
    static PARSED: std::sync::LazyLock<crate::Schema> =
        std::sync::LazyLock::new(|| crate::Schema::parse(SCHEMA).unwrap());
    &PARSED
}
fn open(directory: &Path, hub: &Arc<Hub>, name: &str) -> Result<Configuration, Failure> {
    Ok(Configuration::new(directory, parsed_schema())?
        .secrets(keys(directory))
        .network(crate::Network::Owned(hub.connector(name))))
}
impl Device {
    fn new(database: Database, directory: PathBuf, name: &str, hub: &Arc<Hub>) -> Self {
        Self {
            database,
            directory,
            name: name.into(),
            hub: hub.clone(),
        }
    }

    async fn node(&self) -> Result<String, Failure> {
        Ok(snap::value(self.database.info()).await?["node"]
            .as_str()
            .ok_or("node")?
            .to_owned())
    }
    async fn identity(&self) -> Result<String, Failure> {
        Ok(snap::value(self.database.info()).await?["principal"]
            .as_str()
            .ok_or("principal")?
            .to_owned())
    }
    async fn online(&self, online: bool) -> Result<(), Failure> {
        snap::value(self.database.set_sync(if online {
            crate::SyncMode::Manual
        } else {
            crate::SyncMode::Off
        }))
        .await
        .map(drop)
    }
    async fn stop(self) -> Result<Stopped, Failure> {
        let Self {
            database,
            directory,
            name,
            hub,
        } = self;
        database.close().await?;
        Ok(Stopped {
            directory,
            name,
            hub,
        })
    }
    async fn restart(self) -> Result<Self, Failure> {
        self.stop().await?.start().await
    }
    async fn invite(&self, profile: crate::Access) -> Result<String, Failure> {
        Ok(self.database.provision(profile, None).await?.uri)
    }
    async fn epochs(&self) -> Result<Vec<Value>, Failure> {
        Ok(snap::value(
            self.database
                .partition_status(rocky_sdk::PartitionTarget::Group {
                    group: "list".to_owned(),
                    values: (values()).clone(),
                }),
        )
        .await?["epochs"]
            .as_array()
            .ok_or("epochs")?
            .iter()
            .map(|epoch| epoch["generation"].clone())
            .collect())
    }
    async fn state(&self, node: &str) -> Result<Value, Failure> {
        let status = snap::value(self.database.partition_status(
            rocky_sdk::PartitionTarget::Group {
                group: "list".to_owned(),
                values: (values()).clone(),
            },
        ))
        .await?;
        Ok(status["recipients"]
            .as_array()
            .ok_or("recipients")?
            .iter()
            .filter(|recipient| recipient["node"] == node)
            .max_by_key(|recipient| recipient["epoch"]["generation"].as_u64())
            .map(|recipient| recipient["state"].clone())
            .unwrap_or_default())
    }
    async fn events(&self, kind: &str) -> Result<Vec<Value>, Failure> {
        Ok(snap::value(self.database.activity(0)).await?["events"]
            .as_array()
            .ok_or("events")?
            .iter()
            .filter(|event| event["kind"] == kind)
            .cloned()
            .collect())
    }
    fn mailbox(&self) -> Result<Mailbox, Failure> {
        Mailbox::open(&self.directory.join("mailbox.json"))
    }
    async fn close(self) -> Result<(), Failure> {
        Ok(self.database.close().await?)
    }
}
struct Stopped {
    directory: PathBuf,
    name: String,
    hub: Arc<Hub>,
}
impl Stopped {
    async fn start(self) -> Result<Device, Failure> {
        let database = open(&self.directory, &self.hub, &self.name)?.open().await?;
        Ok(Device::new(database, self.directory, &self.name, &self.hub))
    }
}
fn values() -> Vec<rocky_sdk::SqlValue> {
    vec![rocky_sdk::SqlValue::Text(snap::text(&(LIST)))]
}
async fn introduce(left: &Device, right: &Device) -> Result<(), Failure> {
    for (from, to) in [(left, right), (right, left)] {
        let contact = snap::value(to.database.contact()).await?;
        snap::value(async {
            let value = from
                .database
                .add_peer(&(snap::text(&(contact["uri"]))))
                .await?;
            Ok::<_, crate::Error>(json!({"node":value}))
        })
        .await?;
    }
    Ok(())
}
struct Mesh {
    root: crate::test_directory::Directory,
    hub: Arc<Hub>,
    a: Device,
    custody: Device,
    b: Device,
}
impl Mesh {
    async fn new() -> Result<Self, Failure> {
        Self::with_root(tempfile::tempdir()?.into()).await
    }
    async fn with_root(root: crate::test_directory::Directory) -> Result<Self, Failure> {
        let mesh = Self::unpartitioned(root).await?;
        create_list(&mesh.a).await?;
        Ok(mesh)
    }
    async fn unpartitioned(root: crate::test_directory::Directory) -> Result<Self, Failure> {
        let hub = Arc::new(Hub::default());
        let directory = root.path().join("a");
        let a = Device::new(
            open(&directory, &hub, "a")?.create().await?.database,
            directory,
            "a",
            &hub,
        );
        let custody = join(&root, &hub, &a, "custody", crate::Access::Store).await?;
        let b = join(&root, &hub, &a, "b", crate::Access::Full).await?;
        introduce(&a, &custody).await?;
        introduce(&b, &custody).await?;
        Ok(Self {
            root,
            hub,
            a,
            custody,
            b,
        })
    }
    async fn member(&self, name: &str) -> Result<Device, Failure> {
        let device = join(&self.root, &self.hub, &self.a, name, crate::Access::Read).await?;
        introduce(&device, &self.custody).await?;
        Ok(device)
    }
    async fn close(self) -> Result<(), Failure> {
        for device in [self.a, self.custody, self.b] {
            device.close().await?;
        }
        Ok(())
    }
}
async fn create_list(issuer: &Device) -> Result<(), Failure> {
    snap::value(
        issuer
            .database
            .create_partition(crate::PartitionTarget::Group {
                group: "list".into(),
                values: values(),
            }),
    )
    .await
    .map(drop)
}
async fn join(
    root: &tempfile::TempDir,
    hub: &Arc<Hub>,
    issuer: &Device,
    name: &str,
    profile: crate::Access,
) -> Result<Device, Failure> {
    let directory = root.path().join(name);
    let invitation = issuer.invite(profile).await?;
    let device = Device::new(
        open(&directory, hub, name)?.join(&invitation).await?,
        directory,
        name,
        hub,
    );
    let mut changes = device.database.status_changes();
    let mut status = snap::value(changes.next()).await?;
    let first_sync_in_flight = |status: &Value| status["data"]["bootstrap_pending"] == true;
    while first_sync_in_flight(&status) {
        status = snap::value(changes.next()).await?;
    }
    Ok(device)
}
async fn relay(from: &Device, to: &Device) -> Result<Value, String> {
    to.online(false).await.map_err(|error| error.to_string())?;
    from.online(true).await.map_err(|error| error.to_string())?;
    snap::observe(from.database.sync_now())
        .await
        .map_err(|error| error.to_string())
}
async fn share(device: &Device, recipients: &[&str]) -> Result<Value, Failure> {
    let recipients = recipients
        .iter()
        .map(|key| key.parse())
        .collect::<Result<_, _>>()?;
    snap::value(device.database.share_partition(
        crate::PartitionTarget::Group {
            group: "list".into(),
            values: values(),
        },
        recipients,
        None,
    ))
    .await
}
async fn delivered(device: &Device, count: &str) -> Result<u64, Failure> {
    Ok(device
        .events("envelopes_delivered")
        .await?
        .iter()
        .filter_map(|event| event[count].as_u64())
        .sum())
}
async fn explicit_syncs_only(devices: &[&Device]) -> Result<(), Failure> {
    for device in devices {
        snap::value(device.database.set_sync(rocky_sdk::SyncMode::Manual)).await?;
    }
    Ok(())
}
fn contains(bytes: &[u8], pattern: &[u8]) -> bool {
    bytes.windows(pattern.len()).any(|window| window == pattern)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_share_and_a_rotation_reach_an_offline_recipient_through_custody_alone()
-> Result<(), Failure> {
    let mesh = Mesh::with_root(super::sync_tests::small_directory()?).await?;
    let third = mesh.member("third").await?;
    let removed = mesh.member("removed").await?;
    let Mesh { a, custody, b, .. } = &mesh;
    mesh.hub.record("third");
    let node = b.node().await?;
    b.online(false).await?;
    let removed_node = removed.node().await?;
    share(a, &[&node, &third.node().await?, &removed_node]).await?;
    snap::value(
        a.database
            .revoke_identity(removed.identity().await?.parse()?),
    )
    .await?;
    assert_eq!(a.state(&node).await?, "pending");
    relay(a, b).await?;
    assert_eq!(a.state(&node).await?, "deposited");
    assert_eq!(
        custody.mailbox()?.len(),
        4,
        "a share and a rotation each for B and the third member"
    );
    let held = std::fs::read(custody.directory.join("mailbox.json"))?;
    let address = snap::value(
        a.database
            .partition_status(rocky_sdk::PartitionTarget::Group {
                group: "list".to_owned(),
                values: (values()).clone(),
            }),
    )
    .await?["address"]
        .as_str()
        .ok_or("address")?
        .to_owned();
    for named in [LIST, "\"list\"", "parts", address.as_str()] {
        assert!(
            !contains(&held, named.as_bytes()),
            "the mailbox names {named}"
        );
    }
    relay(b, a).await?;
    assert_eq!(
        b.epochs().await?,
        [json!(1), json!(2)],
        "B meets only custody, and both envelopes arrive and install"
    );
    snap::value(third.database.sync_now()).await?;
    relay(a, b).await?;
    assert_eq!(
        a.state(&node).await?,
        "delivered",
        "the sender learns of the delivery from custody"
    );
    assert_eq!(custody.mailbox()?.len(), 0, "custody forgets a delivery");
    assert_eq!(
        b.state(&node).await?,
        Value::Null,
        "status is the sharer's alone"
    );

    assert_eq!(
        third.state(&node).await?,
        Value::Null,
        "delivery progress is the sharer's alone"
    );
    third.close().await?;
    removed.close().await?;
    mesh.close().await
}
#[tokio::test(flavor = "multi_thread")]
async fn an_envelope_delivered_directly_and_through_two_mailboxes_installs_once()
-> Result<(), Failure> {
    let mesh = Mesh::with_root(super::sync_tests::small_directory()?).await?;
    let Mesh { a, custody, b, .. } = &mesh;
    let second = join(&mesh.root, &mesh.hub, a, "second", crate::Access::Store).await?;
    introduce(a, &second).await?;
    introduce(b, &second).await?;
    let node = b.node().await?;
    b.online(false).await?;
    share(a, &[&node]).await?;
    relay(a, b).await?;
    assert_eq!(custody.mailbox()?.len(), 1);
    assert_eq!(second.mailbox()?.len(), 1);
    introduce(a, b).await?;
    b.online(true).await?;
    snap::value(b.database.sync_now()).await?;
    assert_eq!(delivered(b, "received").await?, 3);
    assert_eq!(b.events("partition_key_installed").await?.len(), 1);
    assert_eq!(b.epochs().await?, [json!(1)]);
    assert_eq!(a.state(&node).await?, "delivered");
    snap::value(a.database.sync_now()).await?;
    assert_eq!(
        custody.mailbox()?.len() + second.mailbox()?.len(),
        0,
        "both mailboxes keep B's receipt only until A takes it"
    );
    second.close().await?;
    mesh.close().await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mailbox_serves_only_the_recipient_and_refuses_at_its_bound_while_the_sender_keeps_it()
-> Result<(), Failure> {
    let Mesh {
        root,
        hub,
        a,
        custody,
        b,
    } = Mesh::with_root(super::sync_tests::small_directory()?).await?;
    let mesh_member = |name: &'static str| {
        let (root, hub, a, custody) = (&root, &hub, &a, &custody);
        async move {
            let device = join(root, hub, a, name, crate::Access::Read).await?;
            introduce(&device, custody).await?;
            Ok::<_, Failure>(device)
        }
    };
    let other = mesh_member("other").await?;
    let removed = mesh_member("removed").await?;
    explicit_syncs_only(&[&a]).await?;
    let node = b.node().await?;
    b.online(false).await?;
    share(&a, &[&node]).await?;
    relay(&a, &b).await?;
    snap::value(other.database.sync_now()).await?;
    assert_eq!(
        delivered(&other, "received").await?,
        0,
        "another member collects nothing of B's"
    );
    assert_eq!(custody.mailbox()?.len(), 1);

    let recipient: rocky_secure::PublicKey = node.parse()?;
    let custody = custody
        .restart_with(|mailbox| {
            let depositor = rocky_secure::PrivateKey::new().public_key();
            while mailbox.len() < crate::delivery::RECIPIENT_DEPOSITS {
                let filler = rocky_credentials::SealedDelivery::seal(recipient, b"filler")?;
                mailbox.deposit(depositor, &filler, crate::enrollment::now()?)?;
            }
            Ok(())
        })
        .await?;
    let before = a.events("envelopes_delivered").await?.len();
    snap::value(
        a.database
            .revoke_identity(removed.identity().await?.parse()?),
    )
    .await?;
    relay(&a, &b).await?;
    snap::value(a.database.sync_now()).await?;
    let mailbox = custody.node().await?;
    let mut rounds = a.events("envelopes_delivered").await?.split_off(before);
    rounds.retain(|round| round["peer"] == mailbox.as_str());
    assert!(rounds.len() >= 2, "both syncs delivered: {rounds:?}");
    for round in &rounds {
        assert_eq!(round["refused"], 1, "no room for B's rotation: {round}");
    }
    let status = snap::value(
        a.database
            .partition_status(rocky_sdk::PartitionTarget::Group {
                group: "list".to_owned(),
                values: (values()).clone(),
            }),
    )
    .await?;
    let rotated: Vec<&Value> = status["recipients"]
        .as_array()
        .ok_or("recipients")?
        .iter()
        .filter(|entry| entry["node"] == node && entry["epoch"]["generation"] == 2)
        .collect();
    assert_eq!(rotated.len(), 1);
    assert_eq!(rotated[0]["state"], "pending", "the sender keeps it");
    for device in [removed, other, a, custody, b] {
        device.close().await?;
    }
    Ok(())
}

#[test]
fn a_mailbox_refuses_at_both_bounds_and_forgets_what_expired() -> Result<(), Failure> {
    use crate::delivery::{DEPOSIT_LIFETIME, MAILBOX_DEPOSITS, MAILBOX_FULL, RECIPIENT_DEPOSITS};
    let root = tempfile::tempdir()?;
    let mut mailbox = Mailbox::open(&root.path().join("mailbox.json"))?;
    let depositor = rocky_secure::PrivateKey::new().public_key();
    let recipients: Vec<_> = (0..MAILBOX_DEPOSITS / RECIPIENT_DEPOSITS + 1)
        .map(|_| rocky_secure::PrivateKey::new().public_key())
        .collect();
    let deposit = |mailbox: &mut Mailbox, recipient, time| {
        let delivery = rocky_credentials::SealedDelivery::seal(recipient, b"envelope")?;
        mailbox.deposit(depositor, &delivery, time)
    };
    for _ in 0..RECIPIENT_DEPOSITS {
        assert_eq!(deposit(&mut mailbox, recipients[0], 0)?, None);
    }
    let full = deposit(&mut mailbox, recipients[0], 0)?.ok_or("stored past the bound")?;
    assert!(full.starts_with(MAILBOX_FULL), "{full}");
    for recipient in &recipients[1..MAILBOX_DEPOSITS / RECIPIENT_DEPOSITS] {
        for _ in 0..RECIPIENT_DEPOSITS {
            assert_eq!(deposit(&mut mailbox, *recipient, 1)?, None);
        }
    }
    assert_eq!(mailbox.len(), MAILBOX_DEPOSITS);
    let last = *recipients.last().ok_or("recipient")?;
    let full = deposit(&mut mailbox, last, 1)?.ok_or("stored past the bound")?;
    assert!(full.starts_with(MAILBOX_FULL), "{full}");
    mailbox.save()?;
    let mut mailbox = Mailbox::open(&root.path().join("mailbox.json"))?;
    assert_eq!(mailbox.len(), MAILBOX_DEPOSITS);
    assert!(mailbox.sweep(DEPOSIT_LIFETIME, |_| false));
    assert_eq!(mailbox.len(), MAILBOX_DEPOSITS - RECIPIENT_DEPOSITS);
    assert!(mailbox.sweep(DEPOSIT_LIFETIME + 1, |_| false));
    assert_eq!(mailbox.len(), 0);
    assert_eq!(deposit(&mut mailbox, last, DEPOSIT_LIFETIME + 1)?, None);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recipient_revoked_after_the_deposit_gets_nothing_and_the_deposit_is_removed()
-> Result<(), Failure> {
    let Mesh {
        root,
        hub,
        a,
        custody,
        b,
    } = Mesh::unpartitioned(tempfile::tempdir()?.into()).await?;
    let node = b.node().await?;
    let identity = b.identity().await?;
    let b = b.stop().await?;
    create_list(&a).await?;
    share(&a, &[&node]).await?;
    snap::value(a.database.sync_now()).await?;
    assert_eq!(custody.mailbox()?.len(), 1);
    let custody = custody.restart_with(|_| Ok(())).await?;
    assert_eq!(
        custody.mailbox()?.len(),
        1,
        "a restarted mailbox keeps the deposit"
    );
    snap::value(a.database.revoke_identity(identity.parse()?)).await?;
    assert_eq!(a.state(&node).await?, Value::Null, "the sender dropped it");
    snap::value(a.database.sync_now()).await?;
    assert_eq!(
        custody.mailbox()?.len(),
        0,
        "custody learns the statement from A and removes B's deposit"
    );
    let b = b.start().await?;
    let _ = relay(&b, &a).await;
    assert_eq!(
        b.events("identity_revoked").await?.len(),
        1,
        "B reached custody, which answered with B's revocation"
    );
    assert_eq!(b.epochs().await?, Vec::<Value>::new(), "B received nothing");
    for device in [a, custody, b] {
        device.close().await?;
    }
    drop((root, hub));
    Ok(())
}

impl Device {
    async fn restart_with(
        self,
        change: impl FnOnce(&mut Mailbox) -> Result<(), Failure>,
    ) -> Result<Self, Failure> {
        let stopped = self.stop().await?;
        let mut mailbox = Mailbox::open(&stopped.directory.join("mailbox.json"))?;
        change(&mut mailbox)?;
        mailbox.save()?;
        stopped.start().await
    }
}

#[derive(Clone, Copy)]
enum At {
    Sender,
    Mailbox,
    Recipient,
}
async fn sweep(direct: bool, at: At, stage: fault::Stage) -> Result<(), Failure> {
    let Mesh {
        root,
        hub,
        a,
        custody,
        b,
    } = Mesh::new().await?;
    if direct {
        introduce(&a, &b).await?;
    }
    let node = b.node().await?;
    let stopped: rocky_secure::PublicKey = match at {
        At::Sender => a.node().await?.parse()?,
        At::Mailbox => custody.node().await?.parse()?,
        At::Recipient => node.parse()?,
    };
    b.online(direct).await?;
    snap::value(
        a.database.share_partition(
            rocky_sdk::PartitionTarget::Group {
                group: "list".to_owned(),
                values: (values()).clone(),
            },
            serde_json::from_value::<std::collections::BTreeSet<rocky_credentials::PublicKey>>(
                json!([node]),
            )
            .expect("typed fixture"),
            None,
        ),
    )
    .await?;
    let pending = || -> Result<bool, Failure> {
        Ok(fault::STOPS
            .lock()
            .map_err(|_| "poisoned")?
            .contains(&(stopped, stage)))
    };
    fault::STOPS
        .lock()
        .map_err(|_| "poisoned")?
        .push((stopped, stage));
    let mut devices = [Some(a), Some(custody), Some(b)];
    let index = match at {
        At::Sender => 0,
        At::Mailbox => 1,
        At::Recipient => 2,
    };
    let mut restarted = false;
    for _ in 0..4 {
        for (from, to) in [(0, 2), (2, 0)] {
            let (Some(sender), Some(receiver)) = (&devices[from], &devices[to]) else {
                return Err("a device is missing".into());
            };
            if !direct {
                receiver.online(false).await?;
            }
            sender.online(true).await?;
            let _ = snap::observe(sender.database.sync_now()).await;
            if !restarted && !pending()? {
                restarted = true;
                let device = devices[index].take().ok_or("stopped device")?;
                if stage == fault::Stage::Deposited {
                    let bytes = rocky_secure::read_authority_entry(
                        &device.directory.join("rocky.sqlite"),
                        crate::enrollment::AUTHORITY_IDENTITY_KEY,
                    )?
                    .ok_or("identity")?
                    .0;
                    let saved: Value = serde_json::from_slice(&bytes)?;
                    let pending = saved["deliveries"].as_object().ok_or("outbox")?;
                    assert!(
                        pending.values().any(|entry| entry["targets"]
                            .as_array()
                            .is_some_and(|targets| !targets.is_empty())
                            && entry["mailboxes"] == json!([])),
                        "a lost deposit answer keeps its durable mailbox reservation"
                    );
                }
                devices[index] = Some(device.restart().await?);
            }
        }
    }
    assert!(restarted, "{stage:?} was never reached");
    let [Some(a), Some(custody), Some(b)] = devices else {
        return Err("a device is missing".into());
    };
    assert_eq!(b.epochs().await?, [json!(1)], "{stage:?}");
    assert_eq!(a.state(&node).await?, "delivered", "{stage:?}");
    assert_eq!(custody.mailbox()?.len(), 0, "{stage:?}");
    for device in [a, custody, b] {
        device.close().await?;
    }
    drop((root, hub));
    Ok(())
}
#[tokio::test(flavor = "multi_thread")]
async fn a_crash_of_the_recipient_after_a_direct_install_loses_no_envelope() -> Result<(), Failure>
{
    sweep(true, At::Recipient, fault::Stage::Installed).await
}
#[tokio::test(flavor = "multi_thread")]
async fn a_crash_of_the_sender_before_saving_a_direct_receipt_loses_no_envelope()
-> Result<(), Failure> {
    sweep(true, At::Sender, fault::Stage::Receipts).await
}
#[tokio::test(flavor = "multi_thread")]
async fn a_crash_of_the_mailbox_after_storing_a_deposit_loses_no_envelope() -> Result<(), Failure> {
    sweep(false, At::Mailbox, fault::Stage::Stored).await
}
#[tokio::test(flavor = "multi_thread")]
async fn a_crash_of_the_sender_before_recording_a_deposit_loses_no_envelope() -> Result<(), Failure>
{
    sweep(false, At::Sender, fault::Stage::Deposited).await
}
#[tokio::test(flavor = "multi_thread")]
async fn a_crash_of_the_recipient_after_installing_a_deposit_loses_no_envelope()
-> Result<(), Failure> {
    sweep(false, At::Recipient, fault::Stage::Installed).await
}
#[tokio::test(flavor = "multi_thread")]
async fn a_crash_of_the_mailbox_before_keeping_the_receipt_loses_no_envelope() -> Result<(), Failure>
{
    sweep(false, At::Mailbox, fault::Stage::Replaced).await
}
#[tokio::test(flavor = "multi_thread")]
async fn a_crash_of_the_sender_before_saving_a_relayed_receipt_loses_no_envelope()
-> Result<(), Failure> {
    sweep(false, At::Sender, fault::Stage::Receipts).await
}
#[tokio::test(flavor = "multi_thread")]
async fn a_crash_of_the_mailbox_before_forgetting_a_taken_receipt_loses_no_envelope()
-> Result<(), Failure> {
    sweep(false, At::Mailbox, fault::Stage::Forgotten).await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crash_of_the_recipient_after_preparing_bootstrap_loses_no_envelope()
-> Result<(), Failure> {
    sweep(false, At::Recipient, fault::Stage::Prepared).await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_custody_grant_on_an_unrelated_scope_is_enough_to_hold_deliveries() -> Result<(), Failure>
{
    let mesh = Mesh::new().await?;
    let mailbox = join(
        &mesh.root,
        &mesh.hub,
        &mesh.a,
        "raw-custody",
        crate::Access::Store,
    )
    .await?;
    introduce(&mesh.a, &mailbox).await?;
    introduce(&mesh.b, &mailbox).await?;
    explicit_syncs_only(&[&mesh.a, &mesh.b, &mailbox]).await?;
    mesh.custody.online(false).await?;
    let recipient = mesh.b.node().await?;
    mesh.b.online(false).await?;
    share(&mesh.a, &[&recipient]).await?;
    let _row_exchange_may_be_refused_without_a_list_grant =
        snap::observe(mesh.a.database.sync_now()).await;
    assert_eq!(
        mesh.a.state(&recipient).await?,
        "deposited",
        "the delivery round succeeds independently of row sync"
    );
    assert_eq!(mailbox.mailbox()?.len(), 1);
    mesh.a.online(false).await?;
    mesh.b.online(true).await?;
    let _ = snap::observe(mesh.b.database.sync_now()).await;
    assert_eq!(mesh.b.epochs().await?, [json!(1)]);
    mailbox.close().await?;
    mesh.close().await
}
