//! Identities through the SDK: further nodes of an enrolled identity join
//! without an issuer, and revoking one node or the identity stops exactly
//! those nodes. See `docs/runtime/sdk-enrollment.md#identities`.
// The SDK's blocking constructors, native transports and paused clocks.
#![cfg(not(target_family = "wasm"))]
use common::{Device, Failure};
use rocky_sdk::{
    Database, FileIdentityProvider, IdentityKey, IdentityProvider, Network, Open,
    transport::memory::Hub,
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

mod common;

fn open<'a>(
    directory: &'a Path,
    hub: &Arc<Hub>,
    name: &str,
    identity: Option<Arc<dyn IdentityProvider>>,
) -> Open<'a> {
    Open {
        identity,
        directory,
        schema: common::SCHEMA,
        secrets: None,
        network: Some(Network::Owned(hub.connector(name))),
        coordination: None,
    }
}
/// The identity in `directory`, as each of its node processes opens it.
fn person(directory: &Path) -> Result<Arc<dyn IdentityProvider>, Failure> {
    Ok(Arc::new(FileIdentityProvider::open(directory)?))
}
fn identity(root: &Path, name: &str) -> Result<Value, Failure> {
    Ok(serde_json::from_slice(&std::fs::read(
        root.join(name).join("identity.json"),
    )?)?)
}
/// Whether `device` holds any secret of `role` in its file key store.
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

/// The owner, a member identity invited once, and two further nodes of that
/// identity, each certified by its provider from a node of the same
/// identity while no issuer is reachable.
struct Household {
    _root: tempfile::TempDir,
    hub: Arc<Hub>,
    owner: Device,
    nodes: [Device; 3],
}
async fn household() -> Result<Household, Failure> {
    let root = tempfile::tempdir()?;
    let path = root.path();
    let hub = Arc::new(Hub::default());
    let owner = Device::new(Database::create(
        open(&path.join("a"), &hub, "a", None),
        None,
    )?)?;
    let first = Device::new(Database::join(
        open(
            &path.join("m1"),
            &hub,
            "m1",
            Some(person(&path.join("person"))?),
        ),
        &owner.invite().await,
    )?)?;
    common::introduce(&owner, &first).await;
    owner.add(1, "Milk").await;
    first.sync().await.map_err(|error| error.to_string())?;
    // No issuer answers from here on.
    owner.close().await;
    let second = Device::new(
        common::link(
            open(
                &path.join("m2"),
                &hub,
                "m2",
                Some(person(&path.join("person"))?),
            ),
            &first.contact().await,
            &first.database.session(),
        )
        .await
        .map_err(|error| format!("second enrollment: {error}"))?,
    )?;
    let third = Device::new(
        common::link(
            open(
                &path.join("m3"),
                &hub,
                "m3",
                Some(person(&path.join("person"))?),
            ),
            &second.contact().await,
            &second.database.session(),
        )
        .await
        .map_err(|error| format!("third enrollment: {error}"))?,
    )?;
    let owner = Device::open(&path.join("a"), Some(Network::Owned(hub.connector("a"))))
        .map_err(|error| format!("reopen owner: {error}"))?;
    Ok(Household {
        _root: root,
        hub,
        owner,
        nodes: [first, second, third],
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn further_nodes_of_an_identity_join_without_an_issuer() -> Result<(), Failure> {
    let household = household().await?;
    let root = household._root.path();
    let key = person(&root.join("person"))?.public_key()?.to_hex();
    let mut nodes = Vec::new();
    for (device, name) in household.nodes.iter().zip(["m1", "m2", "m3"]) {
        let session = device.ok(json!({"type": "session"})).await;
        // One identity holds the grants; each node signs with its own key
        // under its own certificate, and none of them holds an identity key.
        assert_eq!(session["principal"], json!(key), "{name}");
        assert_eq!(
            session["profile"],
            json!({"kind": "writer", "scopes": ["items"]})
        );
        nodes.push(session["node"].clone());
        assert!(!holds(root, name, "identity")?, "{name}");
        assert!(!holds(root, name, "publisher")?, "{name}");
        assert_eq!(identity(root, name)?["identity"], json!(key));
    }
    nodes.sort_by_key(Value::to_string);
    nodes.dedup();
    assert_eq!(nodes.len(), 3);
    // The third node reads what the first wrote, through the second.
    household.nodes[1]
        .sync()
        .await
        .map_err(|error| error.to_string())?;
    household.nodes[2]
        .sync()
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(household.nodes[2].titles().await, ["Milk"]);
    // Each node writes its identity's table under its own number: the rows
    // never collide and every node reads them all.
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
    assert_eq!(ids.len(), 3);
    for device in &household.nodes {
        assert_eq!(
            device.titles().await,
            ["Milk", "Node 0", "Node 1", "Node 2"]
        );
    }
    // A provider of another identity is refused by the identity's nodes.
    let stranger = Database::enroll_identity(
        open(
            &root.join("x"),
            &household.hub,
            "x",
            Some(person(&root.join("stranger"))?),
        ),
        &household.nodes[0].contact().await,
    );
    assert!(stranger.is_err());
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
    let key = person(&household._root.path().join("person"))?
        .public_key()?
        .to_hex();
    // A node is its identity's to unlink, never an issuer's to revoke.
    let refused = owner
        .call(json!({"type": "revoke", "node": nodes[1].node().await}))
        .await
        .err()
        .ok_or("an issuer revoked a node")?;
    assert_eq!(refused["code"], "rejected", "{refused}");
    let synced = async |device: &Device| {
        device
            .sync()
            .await
            .is_ok_and(|report| common::reconciled(&report))
    };
    for node in nodes {
        assert!(synced(node).await);
    }
    // The identity: every node of it, those the owner never recorded too.
    owner.ok(json!({"type": "revoke", "identity": key})).await;
    for node in nodes {
        let refused = node.sync().await.err().ok_or("a revoked identity synced")?;
        assert_eq!(refused["code"], "revoked", "{refused}");
    }
    // The owner's own identity is never revoked.
    let own = owner.ok(json!({"type": "session"})).await["principal"].clone();
    assert!(
        owner
            .call(json!({"type": "revoke", "identity": own}))
            .await
            .is_err()
    );
    for device in nodes.iter().chain([owner]) {
        device.close().await;
    }
    Ok(())
}

/// The `status` and error message of `request` on `session`, with `identity`
/// supplied for the call.
async fn answer(
    database: &Database,
    request: Value,
    identity: Option<Arc<dyn IdentityProvider>>,
) -> Result<(String, String), Failure> {
    let session = database.session();
    let call = json!({
        "version": 1,
        "scope": serde_json::from_str::<Value>(&database.scope())?,
        "request": request,
    })
    .to_string();
    let reply: Value = serde_json::from_str(&match identity {
        Some(identity) => session.call_with_identity(call, identity).await?,
        None => session.call(call).await?,
    })?;
    Ok((
        reply["status"].as_str().unwrap_or_default().to_owned(),
        reply["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
    ))
}

#[tokio::test(flavor = "multi_thread")]
async fn routine_issuer_calls_need_no_identity_and_do_not_retain_a_supplied_one()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let path = root.path().join("a");
    let key = Arc::new(IdentityKey::generate());
    let weak = Arc::downgrade(&key);
    let created = Database::create(open(&path, &hub, "a", Some(key.clone())), None)?;
    created.database.close().await?;
    let database = Database::open(open(&path, &hub, "a", None))?.database;
    let member = IdentityKey::generate().public_key().to_hex();
    let grant = json!({"type": "claims", "action": "grant", "identity": member});
    assert_eq!(answer(&database, grant.clone(), None).await?.0, "ok");
    assert_eq!(answer(&database, grant, Some(key)).await?.0, "ok");
    assert!(
        weak.upgrade().is_none(),
        "the call retained its identity provider"
    );
    assert!(!holds(root.path(), "a", "identity")?);
    database.close().await?;
    Ok(())
}

/// Scans persisted settings and every vault role for a supplied identity secret.
fn no_identity_secret(directory: &std::path::Path, secrets: &[[u8; 32]]) -> Result<(), Failure> {
    let settings = std::fs::read(directory.join("identity.json"))?;
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
    let schema = rocky_sdk::Schema::parse(common::SCHEMA)?;
    let config = rocky_sdk::Configuration::new(&path, &schema)?;
    let opened = config.create().await?;
    assert!(opened.identity.is_none());
    let key = opened.root.ok_or("missing root")?;
    let restored = rocky_sdk::RootKey::from_phrase(&key.phrase())?;
    assert_eq!(
        identity(root.path(), "owner")?["identity"],
        json!(restored.identity().public_key())
    );
    no_identity_secret(&path, &[key.secret(), key.identity().secret()])?;
    opened.database.close().await?;
    let opened = config.open().await?;
    assert!(opened.root.is_none() && opened.identity.is_none());
    assert_eq!(
        answer(
            &opened.database,
            json!({"type":"tips","action":"create","name":"offline"}),
            None
        )
        .await?
        .0,
        "ok"
    );
    opened.database.close().await?;
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
    )?;
    let path = root.path().join("first");
    let config = rocky_sdk::Configuration::new(&path, &rocky_sdk::Schema::parse(common::SCHEMA)?)?
        .network(Network::Owned(hub.connector("first")));
    let opened = config.join_linkable(&owner.invite().await).await?;
    let phrase = opened.identity.ok_or("missing identity phrase")?;
    let key = IdentityKey::from_phrase(&phrase)?;
    let contact = opened.database.session().contact().await?;
    opened.database.close().await?;
    let reopened = config.open().await?;
    assert!(reopened.identity.is_none());
    let second_path = root.path().join("second");
    let second = common::link(
        open(
            &second_path,
            &hub,
            "second",
            Some(Arc::new(IdentityKey::from_phrase(&phrase)?)),
        ),
        &contact,
        &reopened.database.session(),
    )
    .await?;
    let third_path = root.path().join("third");
    let second_contact = second.database.session().contact().await?;
    let third = common::link(
        open(
            &third_path,
            &hub,
            "third",
            Some(Arc::new(IdentityKey::from_secret(key.secret()))),
        ),
        &second_contact,
        &second.database.session(),
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
    for (database, name) in [&reopened.database, &second.database, &third.database]
        .into_iter()
        .zip(["first", "second", "third"])
    {
        assert_eq!(
            answer(database, json!({"type":"renew"}), None).await?.0,
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
    )?;
    let session = owner.database.session();
    for (name, linkable) in [("single", false), ("linkable", true)] {
        let invitation = session.invitation().await?;
        let path = root.path().join(name);
        let config =
            rocky_sdk::Configuration::new(&path, &rocky_sdk::Schema::parse(common::SCHEMA)?)?
                .network(Network::Owned(hub.connector(name)));
        let attempted = if linkable {
            config.join_linkable(&invitation.uri).await
        } else {
            config.join(&invitation.uri).await
        };
        let error = attempted.err().ok_or("admitted without approval")?;
        let original = error
            .downcast_ref::<rocky_sdk::AwaitingAdmission>()
            .ok_or("pending admission")?;
        let other_path = root.path().join(format!("other-{name}"));
        let other_config =
            rocky_sdk::Configuration::new(&other_path, &rocky_sdk::Schema::parse(common::SCHEMA)?)?
                .network(Network::Owned(hub.connector("other")));
        assert!(
            other_config
                .resume_join(&invitation.uri, original)
                .await
                .is_err()
        );
        assert!(!other_path.exists());
        // A retry before approval keeps both the key and a generated phrase.
        let retry = config
            .resume_join(&invitation.uri, original)
            .await
            .err()
            .ok_or("admitted without approval")?;
        let pending = retry
            .downcast_ref::<rocky_sdk::AwaitingAdmission>()
            .ok_or("pending retry")?;
        assert_eq!(pending.code, original.code);
        let listed = session.discovered().await?.pending;
        assert_eq!(listed[0].code, pending.code);
        for entry in std::fs::read_dir(path.join("secrets"))? {
            assert!(!entry?.file_name().to_string_lossy().contains(".identity."));
        }
        session
            .admit(rocky_sdk::Admit {
                node: listed[0].node.clone(),
                code: pending.code.clone(),
                profile: rocky_sdk::GrantProfile::Writer {
                    scopes: ["items".into()].into(),
                },
                label: None,
            })
            .await?;
        let opened = config.resume_join(&invitation.uri, pending).await?;
        assert_eq!(opened.identity.is_some(), linkable);
        assert_eq!(identity(root.path(), name)?["single_use"], !linkable);
        let secret = opened
            .identity
            .as_deref()
            .map(IdentityKey::from_phrase)
            .transpose()?
            .map(|key| key.secret());
        let secrets = secret.into_iter().collect::<Vec<_>>();
        no_identity_secret(&path, &secrets)?;
        opened.database.close().await?;
        let opened = config.open().await?;
        assert!(opened.identity.is_none());
        assert_eq!(
            answer(&opened.database, json!({"type":"renew"}), None)
                .await?
                .0,
            "ok"
        );
        no_identity_secret(&path, &secrets)?;
        opened.database.close().await?;
    }
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_application_root_signer_requires_an_explicit_owner_identity() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("custom");
    let config = rocky_sdk::Configuration::new(&path, &rocky_sdk::Schema::parse(common::SCHEMA)?)?;
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
    assert!(opened.root.is_none() && opened.identity.is_none());
    assert_eq!(
        self::identity(root.path(), "custom")?["identity"],
        json!(identity.public_key())
    );
    no_identity_secret(&path, &[identity.secret()])?;
    opened.database.close().await?;
    let opened = config.open().await?;
    assert_eq!(
        answer(
            &opened.database,
            json!({"type":"tips","action":"create","name":"keyless"}),
            None
        )
        .await?
        .0,
        "ok"
    );
    opened.database.close().await?;
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
        .session()
        .unlink(&nodes[1].node().await, person(&root.join("person"))?)
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
            .session()
            .unlink(&nodes[index].node().await, person(&root.join("person"))?)
            .await?;
    }
    assert_ne!(
        before,
        identity(root, "a")?["keys"]["items"],
        "the issuer received the last node's unlink"
    );
    let replacement = Device::new(
        common::link(
            open(
                &root.join("m4"),
                &household.hub,
                "m4",
                Some(person(&root.join("person"))?),
            ),
            &contact,
            &nodes[0].database.session(),
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
        replacement.ok(json!({"type":"session"})).await["principal"],
        person(&root.join("person"))?.public_key()?.to_hex()
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
        for (index, root) in [root_a.clone(), root_b].into_iter().enumerate() {
            let owner_name = format!("owner{index}");
            let owner = Device::new(Database::create_with_root(
                open(&path.join(&owner_name), &hub, &owner_name, None),
                None,
                root,
            )?)?;
            let profile = if index == 0 {
                json!({"kind":"issuer"})
            } else {
                json!({"kind":"reader","scopes":["items"]})
            };
            let name = format!("member{index}");
            let member = Device::new(Database::join(
                open(&path.join(&name), &hub, &name, Some(personal.clone())),
                &owner.invite_as(profile).await,
            )?)?;
            common::introduce(&owner, &member).await;
            if index == 0 {
                let grant = owner
                    .database
                    .session()
                    .trust(&personal.public_key()?.to_hex(), root_a.clone())
                    .await?;
                member
                    .database
                    .session()
                    .install_trust(&grant.grant)
                    .await?;
            }
            owner.add(1, &format!("database {index}")).await;
            member.sync().await.map_err(|error| error.to_string())?;
            let sibling_name = format!("sibling{index}");
            let sibling = Device::new(
                common::link(
                    open(
                        &path.join(&sibling_name),
                        &hub,
                        &sibling_name,
                        Some(personal.clone()),
                    ),
                    &member.contact().await,
                    &member.database.session(),
                )
                .await?,
            )?;
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
        // A valid invitation token cannot be replayed under another database's
        // context, even when both databases pin the same root and identity.
        let invitation = owners[0].invite().await;
        let decode = |uri: &str, prefix: &str| -> Result<Value, Failure> {
            Ok(serde_json::from_slice(
                &data_encoding::BASE64URL_NOPAD
                    .decode(uri.strip_prefix(prefix).ok_or("URI prefix")?.as_bytes())?,
            )?)
        };
        let foreign_contact = decode(&owners[1].contact().await, "rocky://peer/")?;
        // Discovery/link requests signed by this same reusable identity must still
        // match the answering node's database before it records a pending request.
        let mut foreign_link = decode(&members[0].contact().await, "rocky://peer/")?;
        let other_member = decode(&members[1].contact().await, "rocky://peer/")?;
        foreign_link["database"] = other_member["database"].clone();
        foreign_link["publisher"] = other_member["publisher"].clone();
        let foreign_link = format!(
            "rocky://peer/{}",
            data_encoding::BASE64URL_NOPAD.encode(&serde_json::to_vec(&foreign_link)?)
        );
        let rejected_link = path.join("foreign_link");
        let config = rocky_sdk::Configuration::new(
            &rejected_link,
            &rocky_sdk::Schema::parse(common::SCHEMA)?,
        )?
        .network(Network::Owned(hub.connector("foreign_link")));
        let before = identity(path, "member0")?;
        let pending_before = members[0].database.session().discovered().await?.links;
        assert!(config.link(&foreign_link, personal.clone()).await.is_err());
        assert!(!rejected_link.join("identity.json").exists());
        assert_eq!(
            members[0].database.session().discovered().await?.links,
            pending_before
        );
        assert_eq!(identity(path, "member0")?, before);

        let mut foreign_invitation = decode(&invitation, "rocky://join/")?;
        foreign_invitation["database"] = foreign_contact["database"].clone();
        foreign_invitation["publisher"] = foreign_contact["publisher"].clone();
        let foreign_uri = format!(
            "rocky://join/{}",
            data_encoding::BASE64URL_NOPAD.encode(&serde_json::to_vec(&foreign_invitation)?)
        );
        let rejected = path.join("foreign_join");
        assert!(
            Database::join(
                open(&rejected, &hub, "foreign_join", Some(personal.clone())),
                &foreign_uri
            )
            .is_err()
        );
        assert!(!rejected.join("identity.json").exists());
        assert_eq!(
            owners[0]
                .ok(json!({"type":"invitation_open","uri":invitation}))
                .await["open"],
            true
        );
        let person_key = personal.public_key()?.to_hex();
        let record = |view: Value| -> Result<Value, Failure> {
            let mut record = view["identities"]
                .as_array()
                .ok_or("identity view")?
                .iter()
                .find(|record| record["identity"] == person_key)
                .ok_or("personal identity record")?
                .clone();
            record
                .as_object_mut()
                .ok_or("identity record")?
                .remove("nodes");
            Ok(record)
        };
        if revoked_b {
            owners[1]
                .ok(json!({"type":"revoke","identity":person_key}))
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
                if identity == &person_key)));
        let linked_a = siblings[0].node().await;
        assert!(trusted_alerts(&owners[0]).await?.iter().any(|alert|
            matches!(&alert.kind, rocky_sdk::AlertKind::TrustedNodeLinked { identity, node, .. }
                if identity == &person_key && node == &linked_a)));
        let before_registry_b = record(owners[1].ok(json!({"type":"identities"})).await)?;
        assert_eq!(before_registry_b["profile"]["kind"], "reader");
        let before_b = identity(path, "owner1")?["keys"].clone();
        let before_authority_b = identity(path, "owner1")?["revocations"].clone();
        let before_heard_b = identity(path, "owner1")?["authority"]["heard"].clone();
        let before_a = identity(path, "owner0")?["keys"].clone();
        members[0]
            .database
            .session()
            .unlink(&siblings[0].node().await, personal.clone())
            .await?;
        members[0].sync().await.map_err(|error| error.to_string())?;
        owners[0].sync().await.map_err(|error| error.to_string())?;
        assert_ne!(identity(path, "owner0")?["keys"], before_a);
        assert_eq!(identity(path, "owner1")?["keys"], before_b);
        members[0].add(2, "surviving writer").await;
        assert!(siblings[0].sync().await.is_err());
        owners[0]
            .database
            .session()
            .revoke_trusted(&person_key, root_a.clone())
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
            record(owners[1].ok(json!({"type":"identities"})).await)?,
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
    let owner = Device::new(Database::create(
        open(
            &directory.path().join("owner"),
            &hub,
            "owner",
            Some(key.clone()),
        ),
        None,
    )?)?;
    let path = directory.path().join("new");
    let schema = rocky_sdk::Schema::parse(common::SCHEMA)?;
    let config = rocky_sdk::Configuration::new(&path, &schema)?
        .network(Network::Owned(hub.connector("new")));
    let contact = owner.contact().await;
    let pending = match config.link(&contact, key.clone()).await {
        Ok(_) => return Err("unconfirmed link installed credentials".into()),
        Err(error) => Arc::new(
            *error
                .downcast::<rocky_sdk::AwaitingLink>()
                .map_err(|error| error.to_string())?,
        ),
    };
    assert_eq!(pending.code.len(), 6);
    assert!(pending.code.bytes().all(|digit| digit.is_ascii_digit()));
    let session = owner.database.session();
    let shown = session
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
    assert!(session.confirm_link(&pending.node, wrong).await.is_err());
    // Even the correct joining-side code cannot release a bundle until the answering side confirms.
    assert!(
        config
            .confirm_link(&contact, key.clone(), pending.clone(), &pending.code)
            .await
            .is_err()
    );
    assert!(!path.join("identity.json").exists());
    assert!(!session.discovered().await?.links[0].confirmed);
    session.confirm_link(&pending.node, &pending.code).await?;
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
    linked.database.close().await?;
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
    let discovered = member.database.session().discovered().await?;
    assert!(discovered.pending.is_empty());
    let seen = discovered
        .seen
        .iter()
        .find(|seen| seen.node == sibling_node)
        .ok_or("certified sibling not seen")?;
    assert_eq!(
        seen.identity,
        identity(household._root.path(), "m2")?["identity"]
            .as_str()
            .ok_or("identity")?
    );
    assert!(seen.last_seen > 0);
    assert!(!seen.registry_known);
    assert!(seen.unknown_identity);
    assert!(!seen.revoked && !seen.unlinked);
    common::introduce(owner, sibling).await;
    owner.sync().await.map_err(|error| error.to_string())?;
    let issuer_seen = owner.database.session().discovered().await?.seen;
    let seen = issuer_seen
        .iter()
        .find(|seen| seen.node == sibling_node)
        .ok_or("issuer has no seen sibling")?;
    assert!(seen.registry_known && !seen.unknown_identity);
    member
        .database
        .session()
        .unlink(
            &sibling_node,
            person(&household._root.path().join("person"))?,
        )
        .await?;
    assert!(
        member
            .database
            .session()
            .discovered()
            .await?
            .seen
            .iter()
            .any(|seen| seen.node == sibling_node && seen.unlinked)
    );
    owner
        .ok(json!({"type":"revoke", "identity": seen.identity}))
        .await;
    assert!(
        owner
            .database
            .session()
            .discovered()
            .await?
            .seen
            .iter()
            .any(|seen| seen.node == sibling_node && seen.revoked)
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
        .session()
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
    let owner = Device::new(Database::create_with_root(
        open(&path.join("owner"), &hub, "owner", None),
        None,
        root.clone(),
    )?)?;
    owner
        .database
        .session()
        .create_bookmark("main", None)
        .await?;
    owner.database.session().follow("main").await?;
    let first = Device::new(Database::join(
        open(&path.join("first"), &hub, "first", Some(personal.clone())),
        &owner.invite_as(json!({"kind":"issuer"})).await,
    )?)?;
    common::introduce(&owner, &first).await;
    let person = personal.public_key()?.to_hex();
    let grant = owner
        .database
        .session()
        .trust(&person, root.clone())
        .await?;
    first.database.session().install_trust(&grant.grant).await?;
    first.sync().await.map_err(|error| error.to_string())?;
    first.database.session().follow("main").await?;
    let second = Device::new(
        common::link(
            open(&path.join("second"), &hub, "second", Some(personal.clone())),
            &first.contact().await,
            &first.database.session(),
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
    second.database.session().follow("main").await?;
    let mut seals = Vec::new();
    for (index, signer) in [&first, &second, &first, &second].into_iter().enumerate() {
        signer
            .add(100 + index as u8, &format!("sealed {index}"))
            .await;
        let sealed = signer.ok(json!({"type":"tips","action":"seal"})).await;
        seals.push(sealed["tip"].clone());
        for _ in 0..2 {
            for device in all {
                device.sync().await.map_err(|error| error.to_string())?;
            }
        }
    }
    let third = Device::new(
        common::link(
            open(&path.join("third"), &hub, "third", Some(personal.clone())),
            &second.contact().await,
            &second.database.session(),
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
        .session()
        .unlink(&third_node, personal.clone())
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
        .session()
        .revoke_trusted(&person, root)
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
    let inspection = owner.ok(json!({"type":"tips","action":"inspect"})).await;
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
    )?;
    assert_eq!(trusted_alerts(&reopened).await?, before);
    reopened.close().await;
    Ok(())
}
