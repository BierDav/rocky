use crate::support::snapshot as snap;
use common::{Device, Failure};
use rocky_sdk::{
    Database, FileIdentityProvider, IdentityKey, IdentityProvider, Network, transport::memory::Hub,
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

use crate::common;

fn open(
    directory: &Path,
    hub: &Arc<Hub>,
    name: &str,
    identity: Option<Arc<dyn IdentityProvider>>,
) -> rocky_sdk::Configuration {
    crate::common::configuration(
        directory,
        crate::support::snapshot::schema(common::SCHEMA),
        None,
        identity,
        Some(Network::Owned(hub.connector(name))),
    )
}
fn person(directory: &Path) -> Result<Arc<dyn IdentityProvider>, Failure> {
    Ok(Arc::new(FileIdentityProvider::open(directory)?))
}
fn identity(root: &Path, name: &str) -> Result<Value, Failure> {
    common::authority_json(&root.join(name), "identity")
}
fn holds(root: &Path, name: &str, role: &str) -> Result<bool, Failure> {
    let installation = identity(root, name)?["installation"]
        .as_str()
        .ok_or("installation")?
        .to_owned();
    Ok(
        std::fs::read_dir(root.join(name).join("secrets"))?.any(|entry| {
            entry.is_ok_and(|entry| {
                entry.file_name().to_string_lossy() == format!("rocky.{installation}.{role}")
            })
        }),
    )
}

struct Household {
    _root: tempfile::TempDir,
    hub: Arc<Hub>,
    owner: Device,
    nodes: [Device; 3],
    person: Arc<dyn IdentityProvider>,
}
async fn household() -> Result<Household, Failure> {
    let root = tempfile::tempdir()?;
    let path = root.path();
    let hub = Arc::new(Hub::default());
    let owner = Device::new(open(&path.join("a"), &hub, "a", None).create().await?)?;
    let first_opened = open(
        &path.join("m1"),
        &hub,
        "m1",
        Some(person(&path.join("person"))?),
    )
    .join_linkable(&owner.invite().await)
    .await?;
    let person_key: Arc<dyn IdentityProvider> =
        Arc::new(IdentityKey::from_phrase(&first_opened.identity)?);
    let first = Device::new(first_opened.database)?;
    common::introduce(&owner, &first).await;
    owner.add(1, "Milk").await;
    first.sync().await.map_err(|error| error.to_string())?;
    owner.close().await;
    let second = Device::new(
        common::link(
            open(&path.join("m2"), &hub, "m2", None),
            person_key.clone(),
            &first.contact().await,
            &first.database.clone(),
        )
        .await
        .map_err(|error| format!("second enrollment: {error}"))?,
    )?;
    let third = Device::new(
        common::link(
            open(&path.join("m3"), &hub, "m3", None),
            person_key.clone(),
            &second.contact().await,
            &second.database.clone(),
        )
        .await
        .map_err(|error| format!("third enrollment: {error}"))?,
    )?;
    let owner = Device::open(&path.join("a"), Some(Network::Owned(hub.connector("a"))))
        .await
        .map_err(|error| format!("reopen owner: {error}"))?;
    Ok(Household {
        _root: root,
        hub,
        owner,
        nodes: [first, second, third],
        person: person_key,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn further_nodes_of_an_identity_join_without_an_issuer() -> Result<(), Failure> {
    let household = household().await?;
    let root = household._root.path();
    let key = household.person.public_key()?.to_hex();
    let mut nodes = Vec::new();
    for (device, name) in household.nodes.iter().zip(["m1", "m2", "m3"]) {
        let info = snap::ok(device.database.info()).await;
        assert_eq!(
            info["principal"],
            json!(key),
            "{name}: one identity holds the grants"
        );
        assert_eq!(info["access"], json!("full"));
        nodes.push(info["node"].clone());
        assert!(
            !holds(root, name, "identity")?,
            "{name}: no node holds an identity key"
        );
        assert!(!holds(root, name, "publisher")?, "{name}");
        assert_eq!(identity(root, name)?["identity"], json!(key));
    }
    nodes.sort_by_key(Value::to_string);
    nodes.dedup();
    assert_eq!(nodes.len(), 3, "each node signs with its own key");
    household.nodes[1]
        .sync()
        .await
        .map_err(|error| error.to_string())?;
    household.nodes[2]
        .sync()
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(
        household.nodes[2].titles().await,
        ["Milk"],
        "the third node reads what the first wrote, through the second"
    );
    let mut ids = Vec::new();
    for (index, device) in household.nodes.iter().enumerate() {
        ids.push(device.add(10 + index as u8, &format!("Node {index}")).await);
    }
    for index in [2, 1, 0, 1, 2] {
        household.nodes[index]
            .sync()
            .await
            .map_err(|error| error.to_string())?;
    }
    ids.sort();
    ids.dedup();
    assert_eq!(
        ids.len(),
        3,
        "each node writes its identity's table under its own number, so rows never collide"
    );
    for device in &household.nodes {
        assert_eq!(
            device.titles().await,
            ["Milk", "Node 0", "Node 1", "Node 2"],
            "every node reads every node's rows"
        );
    }
    let stranger = open(&root.join("x"), &household.hub, "x", None)
        .link(
            &household.nodes[0].contact().await,
            person(&root.join("stranger"))?,
        )
        .await;
    assert!(
        stranger.is_err(),
        "a provider of another identity is refused by the identity's nodes"
    );
    for device in household.nodes.iter().chain([&household.owner]) {
        device.close().await;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_issuer_revokes_the_identity_never_one_of_its_nodes() -> Result<(), Failure> {
    let household = household().await?;
    let Household { owner, nodes, .. } = &household;
    for node in nodes {
        common::introduce(owner, node).await;
    }
    let key = household.person.public_key()?.to_hex();
    let synced = async |device: &Device| {
        device
            .sync()
            .await
            .is_ok_and(|report| common::reconciled(&report))
    };
    for node in nodes {
        assert!(synced(node).await);
    }
    snap::ok(owner.database.revoke_identity(snap::public_key(&(key)))).await;
    for node in nodes {
        let refused = node.sync().await.err().ok_or("a revoked identity synced")?;
        assert_eq!(
            refused["code"], "revoked",
            "revoking an identity revokes every node of it, unrecorded ones too: {refused}"
        );
    }
    let own = snap::ok(owner.database.info()).await["principal"].clone();
    assert!(
        snap::observe(owner.database.revoke_identity(snap::public_key(&(own))))
            .await
            .is_err(),
        "the owner's own identity is never revoked"
    );
    for device in nodes.iter().chain([owner]) {
        device.close().await;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn routine_issuer_calls_need_no_identity_and_do_not_retain_a_supplied_one()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let path = root.path().join("a");
    let key = Arc::new(IdentityKey::generate());
    let weak = Arc::downgrade(&key);
    let created = open(&path, &hub, "a", Some(key.clone())).create().await?;
    created.database.close().await?;
    let database = open(&path, &hub, "a", None).open().await?;
    database.provision(rocky_sdk::Access::Full, None).await?;
    database.sync_now_with_identity(key).await?;
    assert!(
        weak.upgrade().is_none(),
        "the call retained its identity provider"
    );
    assert!(!holds(root.path(), "a", "identity")?);
    database.close().await?;
    Ok(())
}

const RELEASE_HANG_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

struct GatedRelease {
    signer: Arc<dyn IdentityProvider>,
    dropping: Option<tokio::sync::oneshot::Sender<()>>,
    gate: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    released: Arc<std::sync::atomic::AtomicBool>,
}
impl IdentityProvider for GatedRelease {
    fn public_key(&self) -> Result<rocky_credentials::PublicKey, rocky_credentials::Error> {
        self.signer.public_key()
    }
    fn sign(
        &self,
        purpose: rocky_credentials::Purpose,
        message: &[u8],
    ) -> Result<rocky_credentials::Signature, rocky_credentials::Error> {
        self.signer.sign(purpose, message)
    }
}
impl Drop for GatedRelease {
    fn drop(&mut self) {
        if let Some(dropping) = self.dropping.take() {
            let _ = dropping.send(());
        }
        if let Ok(gate) = self.gate.get_mut() {
            let _ = gate.recv_timeout(RELEASE_HANG_BOUND);
        }
        self.released
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

async fn returns_only_after_release<F: std::future::Future>(
    signer: Arc<dyn IdentityProvider>,
    call: impl FnOnce(Arc<dyn IdentityProvider>) -> F,
) -> Result<F::Output, Failure> {
    let (dropping, dropping_started) = tokio::sync::oneshot::channel();
    let (open_gate, gate) = std::sync::mpsc::channel();
    let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let provider = Arc::new(GatedRelease {
        signer,
        dropping: Some(dropping),
        gate: std::sync::Mutex::new(gate),
        released: released.clone(),
    });
    let weak = Arc::downgrade(&provider);
    let mut call = std::pin::pin!(call(provider));
    tokio::select! {
        biased;
        started = dropping_started => started?,
        _ = call.as_mut() => {
            return Err("the call returned before the database began releasing its identity".into());
        }
    }
    let still_pending = std::future::poll_fn(|context| {
        std::task::Poll::Ready(call.as_mut().poll(context).is_pending())
    })
    .await;
    assert!(
        still_pending,
        "the call returned while the database still held its identity"
    );
    open_gate.send(())?;
    let output = call.await;
    assert!(released.load(std::sync::atomic::Ordering::SeqCst));
    assert!(weak.upgrade().is_none());
    Ok(output)
}

async fn reopened_without_identity(
    root: &Path,
    hub: &Arc<Hub>,
) -> Result<(Database, IdentityKey), Failure> {
    let path = root.join("a");
    let key = IdentityKey::generate();
    let created = open(
        &path,
        hub,
        "a",
        Some(Arc::new(IdentityKey::from_secret(key.secret()))),
    )
    .create()
    .await?;
    created.database.close().await?;
    Ok((open(&path, hub, "a", None).open().await?, key))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_signed_call_returns_only_after_the_database_released_its_identity() -> Result<(), Failure>
{
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let (database, key) = reopened_without_identity(root.path(), &hub).await?;
    returns_only_after_release(Arc::new(key), |provider| {
        database.sync_now_with_identity(provider)
    })
    .await??;
    database.close().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_signed_call_returns_only_after_its_identity_was_dropped() -> Result<(), Failure>
{
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let issuer = Device::create(
        &root.path().join("issuer"),
        Some(Network::Owned(hub.connector("issuer"))),
    )
    .await?;
    let member = Device::join(
        &root.path().join("member"),
        &issuer.invite().await,
        Network::Owned(hub.connector("member")),
    )
    .await?;
    let member_database = member.database.clone();
    member_database.logout(false).await?;
    let refused = returns_only_after_release(Arc::new(IdentityKey::generate()), |provider| {
        member_database.sync_now_with_identity(provider)
    })
    .await?;
    let refusal = refused.err().ok_or("a logged-out device synced")?;
    assert!(refusal.to_string().contains("logged out"), "{refusal}");
    member.close().await;
    issuer.close().await;
    Ok(())
}

fn no_identity_secret(directory: &std::path::Path, secrets: &[[u8; 32]]) -> Result<(), Failure> {
    let settings = common::authority_bytes(directory, "identity")?.ok_or("identity")?;
    let value: Value = serde_json::from_slice(&settings)?;
    assert!(value.get("stored_identity").is_none());
    for entry in std::fs::read_dir(directory.join("secrets"))? {
        let entry = entry?;
        assert!(!entry.file_name().to_string_lossy().contains(".identity."));
        let bytes = std::fs::read(entry.path())?;
        for secret in secrets {
            assert!(!bytes.windows(secret.len()).any(|window| window == secret));
        }
    }
    for secret in secrets {
        let hex: String = secret.iter().map(|byte| format!("{byte:02x}")).collect();
        assert!(!String::from_utf8_lossy(&settings).contains(&hex));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn one_root_phrase_restores_the_owner_and_no_identity_key_is_persisted() -> Result<(), Failure>
{
    let root = tempfile::tempdir()?;
    let path = root.path().join("owner");
    let config = rocky_sdk::Configuration::new(&path, snap::schema(common::SCHEMA))?;
    let opened = config.create().await?;
    let key = opened.root;
    let restored = rocky_sdk::RootKey::from_phrase(&key.phrase())?;
    assert_eq!(
        identity(root.path(), "owner")?["identity"],
        json!(restored.identity().public_key())
    );
    no_identity_secret(&path, &[key.secret(), key.identity().secret()])?;
    opened.database.close().await?;
    let opened = config.open().await?;
    assert_eq!(outcome(opened.seal()).await?.0, "ok");
    opened.close().await?;
    no_identity_secret(&path, &[key.secret(), key.identity().secret()])?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn generated_linkable_identity_restores_from_phrase_and_secret_after_restart()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let owner = Device::create(
        &root.path().join("owner"),
        Some(Network::Owned(hub.connector("owner"))),
    )
    .await?;
    let path = root.path().join("first");
    let config = rocky_sdk::Configuration::new(&path, snap::schema(common::SCHEMA))?
        .network(Network::Owned(hub.connector("first")));
    let opened = config.join_linkable(&owner.invite().await).await?;
    let phrase = opened.identity;
    let key = IdentityKey::from_phrase(&phrase)?;
    let contact = opened.database.contact().await?.uri;
    opened.database.close().await?;
    let reopened = config.open().await?;
    let second_path = root.path().join("second");
    let second = common::link(
        open(&second_path, &hub, "second", None),
        Arc::new(IdentityKey::from_phrase(&phrase)?),
        &contact,
        &reopened.clone(),
    )
    .await?;
    let third_path = root.path().join("third");
    let second_contact = second.contact().await?.uri;
    let third = common::link(
        open(&third_path, &hub, "third", None),
        Arc::new(IdentityKey::from_secret(key.secret())),
        &second_contact,
        &second.clone(),
    )
    .await?;
    for (index, name) in ["first", "second", "third"].into_iter().enumerate() {
        let saved = identity(root.path(), name)?;
        assert_eq!(saved["identity"], json!(key.public_key()));
        assert_eq!(saved["single_use"], false);
        let chain: rocky_credentials::CredentialChain =
            serde_json::from_value(saved["credentials"].clone())?;
        assert_eq!(
            chain
                .certificate()
                .ok_or("certificate")?
                .membership()?
                .ok_or("membership")?
                .0,
            index as u64
        );
        no_identity_secret(&root.path().join(name), &[key.secret()])?;
    }
    for (database, name) in [&reopened, &second, &third]
        .into_iter()
        .zip(["first", "second", "third"])
    {
        assert_eq!(
            outcome(async {
                database.renew().await?;
                database.info().await
            })
            .await?
            .0,
            "ok"
        );
        no_identity_secret(&root.path().join(name), &[key.secret()])?;
        database.close().await?;
    }
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn approval_continuations_keep_identity_mode_and_phrase_until_admitted() -> Result<(), Failure>
{
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let owner = Device::create(
        &root.path().join("owner"),
        Some(Network::Owned(hub.connector("owner"))),
    )
    .await?;
    let owner_database = owner.database.clone();
    for (name, linkable) in [("single", false), ("linkable", true)] {
        let provisioning = owner_database
            .provision(rocky_sdk::Access::Full, None)
            .await?;
        let path = root.path().join(name);
        let config = rocky_sdk::Configuration::new(&path, snap::schema(common::SCHEMA))?
            .network(Network::Owned(hub.connector(name)));
        let (database, phrase) = if linkable {
            let joined = config.join_linkable(&provisioning.uri).await?;
            (joined.database, Some(joined.identity))
        } else {
            (config.join(&provisioning.uri).await?, None)
        };
        assert_eq!(identity(root.path(), name)?["single_use"], !linkable);
        let secret = phrase
            .as_deref()
            .map(IdentityKey::from_phrase)
            .transpose()?
            .map(|key| key.secret());
        let secrets = secret.into_iter().collect::<Vec<_>>();
        no_identity_secret(&path, &secrets)?;
        database.close().await?;
        let opened = config.open().await?;
        assert_eq!(
            outcome(async {
                opened.renew().await?;
                opened.info().await
            })
            .await?
            .0,
            "ok"
        );
        no_identity_secret(&path, &secrets)?;
        opened.close().await?;
    }
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_application_root_signer_requires_an_explicit_owner_identity() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("custom");
    let config = rocky_sdk::Configuration::new(&path, snap::schema(common::SCHEMA))?;
    let signer = Arc::new(rocky_credentials::PrivateKey::new());
    let error = config
        .create_with_root(signer.clone())
        .await
        .err()
        .ok_or("created without an owner identity")?;
    assert!(error.to_string().contains("explicit owner identity"));
    let identity = IdentityKey::generate();
    let opened = config
        .clone()
        .identity(Arc::new(IdentityKey::from_secret(identity.secret())))
        .create_with_root(signer)
        .await?;
    assert_eq!(
        self::identity(root.path(), "custom")?["identity"],
        json!(identity.public_key())
    );
    no_identity_secret(&path, &[identity.secret()])?;
    opened.close().await?;
    let opened = config.open().await?;
    assert_eq!(outcome(opened.seal()).await?.0, "ok");
    opened.close().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_identity_unlinks_one_sibling_and_the_issuer_rotates_its_reader_scope()
-> Result<(), Failure> {
    let household = household()
        .await
        .map_err(|error| format!("household: {error}"))?;
    let Household { owner, nodes, .. } = &household;
    let root = household._root.path();
    for node in nodes {
        common::introduce(owner, node).await;
    }
    for node in nodes {
        node.sync().await.map_err(|error| error.to_string())?;
    }
    let before = identity(root, "a")?["keys"].clone();
    nodes[0]
        .database
        .unlink(
            rocky_credentials::PeerId::of((nodes[1].node().await).parse()?),
            household.person.clone(),
        )
        .await?;
    nodes[0].sync().await.map_err(|error| error.to_string())?;
    owner.sync().await.map_err(|error| error.to_string())?;
    let after = identity(root, "a")?["keys"].clone();
    assert_ne!(
        before["items"], after["items"],
        "unlink must rotate the identity's reader keys"
    );
    for index in [0, 2] {
        nodes[index]
            .sync()
            .await
            .map_err(|error| error.to_string())?;
        nodes[index]
            .add(90 + index as u8, &format!("survivor {index}"))
            .await;
    }
    let refused = nodes[1].sync().await.err().ok_or("unlinked node synced")?;
    assert_eq!(refused["code"], "revoked", "{refused}");
    for device in nodes.iter().chain([owner]) {
        device.close().await;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_last_node_publishes_its_unlink_and_the_identity_links_again() -> Result<(), Failure> {
    let household = household().await?;
    let Household { owner, nodes, .. } = &household;
    let root = household._root.path();
    for node in nodes {
        common::introduce(owner, node).await;
        node.sync().await.map_err(|error| error.to_string())?;
    }
    nodes[0].sync().await.map_err(|error| error.to_string())?;
    let contact = nodes[0].contact().await;
    let before = identity(root, "a")?["keys"]["items"].clone();
    for index in [1, 2, 0] {
        nodes[0]
            .database
            .unlink(
                rocky_credentials::PeerId::of((nodes[index].node().await).parse()?),
                household.person.clone(),
            )
            .await?;
    }
    assert_ne!(
        before,
        identity(root, "a")?["keys"]["items"],
        "the issuer received the last node's unlink"
    );
    let replacement = Device::new(
        common::link(
            open(&root.join("m4"), &household.hub, "m4", None),
            household.person.clone(),
            &contact,
            &nodes[0].database.clone(),
        )
        .await?,
    )?;
    common::introduce(owner, &replacement).await;
    replacement
        .sync()
        .await
        .map_err(|error| error.to_string())?;
    replacement.add(100, "linked again").await;
    assert_eq!(
        snap::ok(replacement.database.info()).await["principal"],
        household.person.public_key()?.to_hex()
    );
    replacement.close().await;
    for device in nodes.iter().chain([owner]) {
        device.close().await;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_person_links_and_unlinks_independently_in_two_databases() -> Result<(), Failure> {
    for (shared_root, revoked_b) in [(false, false), (true, false), (false, true), (true, true)] {
        let directory = tempfile::tempdir()?;
        let path = directory.path();
        let hub = Arc::new(Hub::default());
        let root_a = Arc::new(rocky_sdk::RootKey::generate());
        let root_b = if shared_root {
            root_a.clone()
        } else {
            Arc::new(rocky_sdk::RootKey::generate())
        };
        let personal: Arc<dyn IdentityProvider> = Arc::new(IdentityKey::generate());
        let mut owners = Vec::new();
        let mut members = Vec::new();
        let mut siblings = Vec::new();
        let mut personals: Vec<Arc<dyn IdentityProvider>> = Vec::new();
        for (index, root) in [root_a.clone(), root_b].into_iter().enumerate() {
            let owner_name = format!("owner{index}");
            let owner = Device::new(
                open(&path.join(&owner_name), &hub, &owner_name, None)
                    .create_with_root(root)
                    .await?,
            )?;
            let profile = if index == 0 {
                rocky_sdk::Access::Full
            } else {
                rocky_sdk::Access::Read
            };
            let name = format!("member{index}");
            let member_opened = open(&path.join(&name), &hub, &name, Some(personal.clone()))
                .join_linkable(&owner.invite_as(profile).await)
                .await?;
            let db_personal: Arc<dyn IdentityProvider> =
                Arc::new(IdentityKey::from_phrase(&member_opened.identity)?);
            let member = Device::new(member_opened.database)?;
            common::introduce(&owner, &member).await;
            if index == 0 {
                let grant = owner
                    .database
                    .trust(db_personal.public_key()?, root_a.clone())
                    .await?;
                member.database.install_trust(&grant.grant).await?;
            }
            owner.add(1, &format!("database {index}")).await;
            member.sync().await.map_err(|error| error.to_string())?;
            let sibling_name = format!("sibling{index}");
            let sibling = Device::new(
                common::link(
                    open(&path.join(&sibling_name), &hub, &sibling_name, None),
                    db_personal.clone(),
                    &member.contact().await,
                    &member.database.clone(),
                )
                .await?,
            )?;
            personals.push(db_personal);
            common::introduce(&owner, &sibling).await;
            sibling.sync().await.map_err(|error| error.to_string())?;
            let saved = identity(path, &name)?;
            let chain: rocky_credentials::CredentialChain =
                serde_json::from_value(saved["credentials"].clone())?;
            assert_eq!(
                chain
                    .certificate()
                    .ok_or("certificate")?
                    .membership()?
                    .ok_or("membership")?
                    .0,
                0
            );
            let linked: rocky_credentials::CredentialChain =
                serde_json::from_value(identity(path, &sibling_name)?["credentials"].clone())?;
            assert_eq!(
                linked
                    .certificate()
                    .ok_or("certificate")?
                    .membership()?
                    .ok_or("membership")?
                    .0,
                1
            );
            owners.push(owner);
            members.push(member);
            siblings.push(sibling);
        }
        let invitation = owners[0].invite().await;
        let decode = |uri: &str, prefix: &str| -> Result<Value, Failure> {
            Ok(serde_json::from_slice(
                &data_encoding::BASE64URL_NOPAD
                    .decode(uri.strip_prefix(prefix).ok_or("URI prefix")?.as_bytes())?,
            )?)
        };
        let foreign_contact = decode(&owners[1].contact().await, "rocky://peer/")?;
        let mut foreign_link = decode(&members[0].contact().await, "rocky://peer/")?;
        let other_member = decode(&members[1].contact().await, "rocky://peer/")?;
        foreign_link["database"] = other_member["database"].clone();
        foreign_link["publisher"] = other_member["publisher"].clone();
        let foreign_link = format!(
            "rocky://peer/{}",
            data_encoding::BASE64URL_NOPAD.encode(&serde_json::to_vec(&foreign_link)?)
        );
        let rejected_link = path.join("foreign_link");
        let config = rocky_sdk::Configuration::new(&rejected_link, snap::schema(common::SCHEMA))?
            .network(Network::Owned(hub.connector("foreign_link")));
        let before = identity(path, "member0")?;
        let pending_before = members[0].database.discovered().await?.links;
        assert!(
            config
                .link(&foreign_link, personals[0].clone())
                .await
                .is_err(),
            "a link request by the same reusable identity must match the answering database"
        );
        assert!(!rejected_link.join("identity.json").exists());
        assert_eq!(
            members[0].database.discovered().await?.links,
            pending_before,
            "a foreign link request is refused before it is recorded as pending"
        );
        assert_eq!(identity(path, "member0")?, before);

        let mut foreign_invitation = decode(&invitation, "rocky://provision/")?;
        foreign_invitation["database"] = foreign_contact["database"].clone();
        foreign_invitation["publisher"] = foreign_contact["publisher"].clone();
        let foreign_uri = format!(
            "rocky://provision/{}",
            data_encoding::BASE64URL_NOPAD.encode(&serde_json::to_vec(&foreign_invitation)?)
        );
        let rejected = path.join("foreign_join");
        assert!(
            open(&rejected, &hub, "foreign_join", Some(personals[0].clone()))
                .join(&foreign_uri)
                .await
                .is_err(),
            "a provisioning bundle is never replayed under another database's context, \
             even one pinning the same root and identity"
        );
        assert!(!rejected.join("identity.json").exists());
        let person_key_0 = personals[0].public_key()?.to_hex();
        let person_key_1 = personals[1].public_key()?.to_hex();
        let record = |view: Value, key: &str| -> Result<Value, Failure> {
            let mut record = view["identities"]
                .as_array()
                .ok_or("identity view")?
                .iter()
                .find(|record| record["identity"] == key)
                .ok_or("personal identity record")?
                .clone();
            record
                .as_object_mut()
                .ok_or("identity record")?
                .remove("nodes");
            Ok(record)
        };
        if revoked_b {
            snap::ok(
                owners[1]
                    .database
                    .revoke_identity(snap::public_key(&(person_key_1))),
            )
            .await;
            for device in [&members[1], &siblings[1]] {
                assert!(device.sync().await.is_err());
            }
            assert!(members[0].sync().await.is_ok());
            assert!(siblings[0].sync().await.is_ok());
        }
        let before_alerts_b = trusted_alerts(&owners[1]).await?;
        assert!(!trusted_alerts(&owners[0]).await?.iter().any(|alert|
            matches!(&alert.kind, rocky_sdk::AlertKind::TrustedIdentityStale { identity, .. }
                if identity == &person_key_0)));
        let linked_a = siblings[0].node().await;
        assert!(trusted_alerts(&owners[0]).await?.iter().any(|alert|
            matches!(&alert.kind, rocky_sdk::AlertKind::TrustedNodeLinked { identity, node, .. }
                if identity == &person_key_0 && node == &linked_a)));
        let before_registry_b = record(
            snap::ok(owners[1].database.identities()).await,
            &person_key_1,
        )?;
        assert_eq!(before_registry_b["access"], "read");
        let before_b = identity(path, "owner1")?["keys"].clone();
        let before_authority_b = identity(path, "owner1")?["revocations"].clone();
        let before_heard_b = identity(path, "owner1")?["authority"]["heard"].clone();
        let before_a = identity(path, "owner0")?["keys"].clone();
        members[0]
            .database
            .unlink(
                rocky_credentials::PeerId::of((siblings[0].node().await).parse()?),
                personals[0].clone(),
            )
            .await?;
        members[0].sync().await.map_err(|error| error.to_string())?;
        owners[0].sync().await.map_err(|error| error.to_string())?;
        assert_ne!(identity(path, "owner0")?["keys"], before_a);
        assert_eq!(identity(path, "owner1")?["keys"], before_b);
        members[0].add(2, "surviving writer").await;
        assert!(siblings[0].sync().await.is_err());
        owners[0]
            .database
            .revoke_trusted(
                person_key_0.parse()?,
                rocky_sdk::RevocationMode::Compromised,
                root_a.clone(),
            )
            .await?;
        assert_eq!(trusted_alerts(&owners[1]).await?, before_alerts_b);
        assert_eq!(identity(path, "owner1")?["revocations"], before_authority_b);
        assert_eq!(
            identity(path, "owner1")?["authority"]["heard"],
            before_heard_b
        );
        owners[1].add(3, "still independent").await;
        for device in [&members[1], &siblings[1]] {
            if revoked_b {
                assert!(device.sync().await.is_err());
            } else {
                device.sync().await.map_err(|error| error.to_string())?;
                assert_eq!(device.titles().await, ["database 1", "still independent"]);
            }
        }
        assert_eq!(identity(path, "owner1")?["keys"], before_b);
        assert_eq!(
            record(
                snap::ok(owners[1].database.identities()).await,
                &person_key_1
            )?,
            before_registry_b
        );
        for device in owners.into_iter().chain(members).chain(siblings) {
            device.close().await;
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn linking_requires_both_codes_before_credentials_are_released() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let key: Arc<dyn IdentityProvider> = Arc::new(IdentityKey::generate());
    let owner = Device::new(
        open(
            &directory.path().join("owner"),
            &hub,
            "owner",
            Some(key.clone()),
        )
        .create()
        .await?,
    )?;
    let path = directory.path().join("new");
    let schema = snap::schema(common::SCHEMA).clone();
    let config = rocky_sdk::Configuration::new(&path, &schema)?
        .network(Network::Owned(hub.connector("new")));
    let contact = owner.contact().await;
    let pending = match config.link(&contact, key.clone()).await {
        Ok(_) => return Err("unconfirmed link installed credentials".into()),
        Err(rocky_sdk::Error {
            kind: rocky_sdk::ErrorKind::AwaitingLink(pending),
            ..
        }) => pending,
        Err(error) => return Err(format!("expected a pending link, found {error}").into()),
    };
    assert_eq!(pending.code.len(), 6);
    assert!(pending.code.bytes().all(|digit| digit.is_ascii_digit()));
    let owner_database = owner.database.clone();
    let shown = owner_database
        .discovered()
        .await?
        .links
        .into_iter()
        .find(|held| held.node == pending.node)
        .ok_or("missing request")?;
    assert_eq!(shown.code, pending.code);
    assert!(!shown.confirmed);
    let wrong = if pending.code == "000000" {
        "000001"
    } else {
        "000000"
    };
    assert!(
        config
            .confirm_link(&contact, key.clone(), pending.clone(), wrong)
            .await
            .is_err()
    );
    assert!(
        owner_database
            .confirm_link(pending.node, wrong)
            .await
            .is_err()
    );
    assert!(
        config
            .confirm_link(&contact, key.clone(), pending.clone(), &pending.code)
            .await
            .is_err(),
        "even the correct joining-side code releases no bundle before the answering side confirms"
    );
    assert!(!path.join("identity.json").exists());
    assert!(!owner_database.discovered().await?.links[0].confirmed);
    owner_database
        .confirm_link(pending.node, &pending.code)
        .await?;
    assert!(
        config
            .confirm_link(&contact, key.clone(), pending.clone(), wrong)
            .await
            .is_err()
    );
    assert!(!path.join("identity.json").exists());
    let linked = config
        .confirm_link(&contact, key, pending.clone(), &pending.code)
        .await?;
    let saved: rocky_credentials::CredentialChain =
        serde_json::from_value(identity(directory.path(), "new")?["credentials"].clone())?;
    assert_eq!(
        saved
            .certificate()
            .ok_or("certificate")?
            .membership()?
            .ok_or("membership")?
            .0,
        1
    );
    linked.close().await?;
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn certified_seen_nodes_are_visible_without_issuer_authority() -> Result<(), Failure> {
    let household = household().await?;
    let owner = &household.owner;
    let member = &household.nodes[0];
    let sibling = &household.nodes[1];
    let sibling_node = sibling.node().await;
    common::introduce(member, sibling).await;
    member.sync().await.map_err(|error| error.to_string())?;
    let discovered = member.database.discovered().await?;
    let seen = discovered
        .seen
        .iter()
        .find(|seen| seen.node.to_hex() == sibling_node)
        .ok_or("certified sibling not seen")?;
    assert_eq!(
        seen.identity,
        identity(household._root.path(), "m2")?["identity"]
            .as_str()
            .ok_or("identity")?
            .parse::<rocky_sdk::PublicKey>()?
    );
    assert!(seen.last_seen > 0);
    assert!(!seen.registry_known);
    assert!(seen.unknown_identity);
    assert!(!seen.revoked && !seen.unlinked);
    common::introduce(owner, sibling).await;
    owner.sync().await.map_err(|error| error.to_string())?;
    let issuer_seen = owner.database.discovered().await?.seen;
    let seen = issuer_seen
        .iter()
        .find(|seen| seen.node.to_hex() == sibling_node)
        .ok_or("issuer has no seen sibling")?;
    assert!(seen.registry_known && !seen.unknown_identity);
    member
        .database
        .unlink(
            rocky_credentials::PeerId::of(sibling_node.parse()?),
            household.person.clone(),
        )
        .await?;
    assert!(
        member
            .database
            .discovered()
            .await?
            .seen
            .iter()
            .any(|seen| seen.node.to_hex() == sibling_node && seen.unlinked)
    );
    snap::ok(
        owner
            .database
            .revoke_identity(snap::public_key(&(seen.identity))),
    )
    .await;
    assert!(
        owner
            .database
            .discovered()
            .await?
            .seen
            .iter()
            .any(|seen| seen.node.to_hex() == sibling_node && seen.revoked)
    );
    owner.close().await;
    for node in household.nodes {
        node.close().await;
    }
    Ok(())
}

async fn trusted_alerts(device: &Device) -> Result<Vec<rocky_sdk::Alert>, Failure> {
    Ok(device
        .database
        .status()
        .await?
        .alerts
        .into_iter()
        .filter(|alert| {
            matches!(
                alert.kind,
                rocky_sdk::AlertKind::TrustedNodeLinked { .. }
                    | rocky_sdk::AlertKind::TrustedNodeUnlinked { .. }
                    | rocky_sdk::AlertKind::TrustedIdentityStale { .. }
            )
        })
        .collect())
}

#[tokio::test(flavor = "multi_thread")]
async fn trusted_siblings_seal_and_observe_links_and_unlinks_durably() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let path = directory.path();
    let hub = Arc::new(Hub::default());
    let root = Arc::new(rocky_sdk::RootKey::generate());
    let personal: Arc<dyn IdentityProvider> = Arc::new(IdentityKey::generate());
    let owner = Device::new(
        open(&path.join("owner"), &hub, "owner", None)
            .create_with_root(root.clone())
            .await?,
    )?;
    let first_opened = open(&path.join("first"), &hub, "first", Some(personal.clone()))
        .join_linkable(&owner.invite_as(rocky_sdk::Access::Full).await)
        .await?;
    let personal: Arc<dyn IdentityProvider> =
        Arc::new(IdentityKey::from_phrase(&first_opened.identity)?);
    let first = Device::new(first_opened.database)?;
    common::introduce(&owner, &first).await;
    let person = personal.public_key()?.to_hex();
    let grant = owner.database.trust(person.parse()?, root.clone()).await?;
    first.database.install_trust(&grant.grant).await?;
    first.sync().await.map_err(|error| error.to_string())?;
    let second = Device::new(
        common::link(
            open(&path.join("second"), &hub, "second", None),
            personal.clone(),
            &first.contact().await,
            &first.database.clone(),
        )
        .await?,
    )?;
    common::introduce(&owner, &second).await;
    let all = [&owner, &first, &second];
    for _ in 0..3 {
        for device in all {
            device.sync().await.map_err(|error| error.to_string())?;
        }
    }
    let mut seals = Vec::new();
    for (index, signer) in [&first, &second, &first, &second].into_iter().enumerate() {
        signer
            .add(100 + index as u8, &format!("sealed {index}"))
            .await;
        let sealed = snap::ok(signer.database.seal()).await;
        seals.push(sealed["tip"].clone());
        for _ in 0..2 {
            for device in all {
                device.sync().await.map_err(|error| error.to_string())?;
            }
        }
    }
    let third = Device::new(
        common::link(
            open(&path.join("third"), &hub, "third", None),
            personal.clone(),
            &second.contact().await,
            &second.database.clone(),
        )
        .await?,
    )?;
    common::introduce(&owner, &third).await;
    common::introduce(&first, &third).await;
    for _ in 0..3 {
        for device in [&owner, &first, &second, &third] {
            device.sync().await.map_err(|error| error.to_string())?;
        }
    }
    let third_node = third.node().await;
    for device in [&owner, &first, &second, &third] {
        let alerts = trusted_alerts(device).await?;
        assert_eq!(
            alerts
                .iter()
                .filter(|alert| matches!(&alert.kind,
            rocky_sdk::AlertKind::TrustedNodeLinked { identity, node, .. }
            if identity == &person && node == &third_node))
                .count(),
            1,
            "{alerts:?}"
        );
    }
    first
        .database
        .unlink(
            rocky_credentials::PeerId::of((third_node).parse()?),
            personal.clone(),
        )
        .await?;
    for _ in 0..2 {
        for device in all {
            device.sync().await.map_err(|error| error.to_string())?;
        }
    }
    assert!(third.sync().await.is_err());
    for device in all {
        let alerts = trusted_alerts(device).await?;
        assert_eq!(
            alerts
                .iter()
                .filter(|alert| matches!(&alert.kind,
            rocky_sdk::AlertKind::TrustedNodeUnlinked { identity, node, .. }
            if identity == &person && node == &third_node))
                .count(),
            1,
            "{alerts:?}"
        );
    }
    let before = trusted_alerts(&owner).await?;
    let saved_before = identity(path, "owner")?["authority"]["trusted_nodes"].clone();
    assert_eq!(trusted_alerts(&owner).await?, before);
    assert_eq!(
        identity(path, "owner")?["authority"]["trusted_nodes"],
        saved_before
    );
    owner
        .database
        .revoke_trusted(
            person.parse()?,
            rocky_sdk::RevocationMode::Compromised,
            root,
        )
        .await?;
    let saved = identity(path, "owner")?;
    let revocations: rocky_credentials::RevocationSet =
        serde_json::from_value(saved["revocations"].clone())?;
    let cut = revocations
        .statements()
        .find(|signed| {
            signed.root()
                && signed
                    .statement
                    .identities
                    .contains(&personal.public_key().unwrap())
        })
        .ok_or("root cut")?;
    let lanes: std::collections::BTreeSet<_> = cut
        .statement
        .cut
        .iter()
        .map(|cut| cut.lane.to_hex())
        .collect();
    assert!(lanes.contains(&first.node().await) && lanes.contains(&second.node().await));
    assert!(!lanes.contains(&third_node));
    let inspection = snap::ok(owner.database.inspect_tips()).await;
    for seal in seals {
        assert!(
            inspection["seals"]
                .as_array()
                .ok_or("seals")?
                .iter()
                .any(|held| held["tip"] == seal && held["revoked"] == false),
            "{inspection}"
        );
    }
    assert!(first.sync().await.is_err());
    assert!(second.sync().await.is_err());
    first.close().await;
    second.close().await;
    third.close().await;
    owner.close().await;
    let reopened = Device::open(
        &path.join("owner"),
        Some(Network::Owned(hub.connector("owner"))),
    )
    .await?;
    assert_eq!(trusted_alerts(&reopened).await?, before);
    reopened.close().await;
    Ok(())
}

async fn outcome<T, E: Into<Failure>>(
    operation: impl std::future::Future<Output = Result<T, E>>,
) -> Result<(String, String), Failure> {
    operation.await.map_err(Into::into)?;
    Ok(("ok".into(), String::new()))
}
