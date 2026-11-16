//! Non-destructive personal sign-in: a device holding only the passkey
//! finds the database under its discovery topic, or takes a verified
//! backup, and becomes one more owner device. Nothing is revoked, rotated
//! or granted; an unreachable source creates nothing.
#![cfg(not(target_family = "wasm"))]
use rocky_sdk::{
    Configuration, Freshness, GrantProfile, Network, PersonalNamespace, PersonalOpened,
    PersonalSignInError, Schema, SignInSource, VerifiedBackup, VerifiedPasskeyPrf,
    transport::{BoxFuture, Connected, Connector, Stream, Transport, memory::Hub},
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

mod common;
use common::{Device, Failure};

fn namespace() -> PersonalNamespace {
    PersonalNamespace::new("shopping.example", "account root").expect("namespace")
}
/// Credential `credential`'s PRF over the namespace's evaluation input.
fn passkey(credential: u8, namespace: &PersonalNamespace) -> VerifiedPasskeyPrf {
    let input = namespace.prf_input().expect("input");
    let output = rocky_core::commitment(
        "fixture.passkey-prf",
        &[[credential; 32].as_slice(), input.as_bytes()].concat(),
    );
    VerifiedPasskeyPrf::new(&input, output)
}
fn prf() -> VerifiedPasskeyPrf {
    passkey(1, &namespace())
}
fn account() -> rocky_sdk::PersonalAccount {
    rocky_sdk::PersonalAccount::derive(prf(), &namespace()).expect("account")
}
fn hub() -> Arc<Hub> {
    let hub = Arc::new(Hub::default());
    hub.discovery.store(true, Ordering::SeqCst);
    hub
}
fn config(directory: &Path) -> Result<Configuration, Failure> {
    Configuration::new(directory, &Schema::parse(common::SCHEMA)?)
}
async fn create(directory: &Path, hub: &Arc<Hub>, name: &str) -> Result<Device, Failure> {
    let personal = config(directory)?
        .network(Network::Owned(hub.connector(name)))
        .create_personal(prf(), namespace())
        .await?;
    Device::new(personal.opened)
}
async fn sign_in(
    directory: &Path,
    network: Option<Network>,
    prf: VerifiedPasskeyPrf,
    namespace: PersonalNamespace,
) -> Result<PersonalOpened, PersonalSignInError> {
    let mut config = config(directory).map_err(PersonalSignInError::Failed)?;
    if let Some(network) = network {
        config = config.network(network);
    }
    config.sign_in(prf, namespace, SignInSource::Discover).await
}
fn owned(hub: &Arc<Hub>, name: &str) -> Option<Network> {
    Some(Network::Owned(hub.connector(name)))
}
fn saved(directory: &Path) -> Result<Value, Failure> {
    Ok(serde_json::from_slice(&std::fs::read(
        directory.join("identity.json"),
    )?)?)
}
fn known(directory: &Path) -> Result<rocky_credentials::RevocationSet, Failure> {
    Ok(serde_json::from_value(
        saved(directory)?["revocations"].clone(),
    )?)
}
fn identity_key() -> rocky_credentials::PublicKey {
    account().locator().owner
}
/// Every file under `directory`, with its bytes.
fn files(directory: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>, Failure> {
    let mut found = BTreeMap::new();
    if !directory.exists() {
        return Ok(found);
    }
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            found.extend(files(&path)?);
        } else {
            found.insert(path.clone(), std::fs::read(path)?);
        }
    }
    Ok(found)
}
/// An owner device's view a sign-in must leave untouched: its reader key
/// epochs and that no statement was ever recorded.
fn untouched(directory: &Path, keys: &Value) -> Result<(), Failure> {
    let saved = saved(directory)?;
    assert_eq!(&saved["keys"], keys, "a sign-in rotated an epoch");
    let known = known(directory)?;
    assert!(!known.has_revocations(), "a sign-in revoked something");
    assert!(!known.membership()?.is_forked(identity_key()));
    Ok(())
}
/// Waits until `name` announces under the passkey's bootstrap topic; the
/// bound only stops a hang.
async fn announced(hub: &Hub, name: &str) {
    let topic = *account().discovery().topic();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !hub.topics(name).contains(&topic) {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("announced under the bootstrap topic");
}
fn refusal<T>(result: Result<T, PersonalSignInError>) -> PersonalSignInError {
    match result {
        Ok(_) => panic!("the sign-in succeeded"),
        Err(error) => error,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_passkey_alone_signs_in_through_discovery_and_touches_no_sibling() -> Result<(), Failure>
{
    let directory = tempfile::tempdir()?;
    let hub = hub();
    let owner_path = directory.path().join("owner");
    let owner = create(&owner_path, &hub, "owner").await?;
    owner.add(1, "from owner").await;
    let keys = saved(&owner_path)?["keys"].clone();

    let path = directory.path().join("phone");
    let personal = sign_in(&path, owned(&hub, "phone"), prf(), namespace()).await?;
    // Another owner device answered: its durable store anchors the view.
    assert_eq!(personal.freshness, Freshness::LocallyAnchored);
    assert!(personal.restored.complete(), "{:?}", personal.restored);
    assert!(personal.restored.readers.contains_key("items"));
    assert!(personal.opened.root.is_none());
    let phone = Device::new(personal.opened)?;
    assert_eq!(phone.scope(), owner.scope());
    assert_eq!(phone.identity().await, identity_key().to_hex());
    phone.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(phone.titles().await, ["from owner"]);
    // The phone writes as itself; the owner reads it.
    phone.add(2, "from phone").await;
    phone.sync().await.map_err(|error| error.to_string())?;
    owner.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(owner.titles().await, ["from owner", "from phone"]);

    // An independent owner certificate, published to and kept by the owner.
    let chain: rocky_credentials::CredentialChain =
        serde_json::from_value(saved(&path)?["credentials"].clone())?;
    let certificate = chain.certificate().ok_or("certificate")?;
    assert_eq!(certificate.issuer, identity_key());
    assert!(certificate.capability.extended()?.personal.is_some());
    assert_eq!(certificate.membership()?, None);
    for device in [&owner_path, &path] {
        untouched(device, &keys)?;
        let devices = known(device)?.personal_devices(identity_key())?;
        assert_eq!(devices.len(), 2);
        assert!(devices.contains(&certificate.capability.subject));
    }
    assert!(!path.join("personal.intent").exists());
    for device in [&owner, &phone] {
        let status = device.session.status().await?;
        assert!(status.authority.verified_revocation.is_none());
    }
    phone.close().await;
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_sign_ins_against_one_snapshot_both_stay_valid() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let hub = hub();
    let owner_path = directory.path().join("owner");
    let owner = create(&owner_path, &hub, "owner").await?;
    owner.add(1, "from owner").await;
    let keys = saved(&owner_path)?["keys"].clone();
    let (laptop_path, phone_path) = (
        directory.path().join("laptop"),
        directory.path().join("phone"),
    );
    let (laptop, phone) = tokio::join!(
        sign_in(&laptop_path, owned(&hub, "laptop"), prf(), namespace()),
        sign_in(&phone_path, owned(&hub, "phone"), prf(), namespace()),
    );
    let (laptop, phone) = (Device::new(laptop?.opened)?, Device::new(phone?.opened)?);
    // Both write before they ever meet each other, then merge both ways.
    laptop.add(2, "from laptop").await;
    phone.add(3, "from phone").await;
    common::introduce(&laptop, &phone).await;
    for _ in 0..2 {
        for device in [&laptop, &phone, &owner] {
            device.sync().await.map_err(|error| error.to_string())?;
        }
    }
    for (device, path) in [
        (&laptop, &laptop_path),
        (&phone, &phone_path),
        (&owner, &owner_path),
    ] {
        assert_eq!(
            device.titles().await,
            ["from laptop", "from owner", "from phone"]
        );
        untouched(path, &keys)?;
        assert_eq!(known(path)?.personal_devices(identity_key())?.len(), 3);
    }
    for device in [laptop, phone, owner] {
        device.close().await;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_custodian_alone_restores_an_unwitnessed_view() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let hub = hub();
    let owner_path = directory.path().join("owner");
    let owner = create(&owner_path, &hub, "owner").await?;
    let invitation = owner
        .session
        .invitation_with(GrantProfile::Custody {
            scopes: ["items".to_owned()].into(),
        })
        .await?
        .uri;
    let custodian = Device::new(
        config(&directory.path().join("custodian"))?
            .network(Network::Owned(hub.connector("custodian")))
            .join(&invitation)
            .await?,
    )?;
    common::introduce(&owner, &custodian).await;
    owner.add(1, "kept by custody").await;
    owner.sync().await.map_err(|error| error.to_string())?;
    // The custodian announces where a passkey-only device looks.
    announced(&hub, "custodian").await;
    // Every owner device is lost; only the keyless custodian remains.
    owner.close().await;
    std::fs::remove_dir_all(&owner_path)?;

    let path = directory.path().join("phone");
    let personal = sign_in(&path, owned(&hub, "phone"), prf(), namespace()).await?;
    assert_eq!(personal.freshness, Freshness::Unwitnessed);
    assert!(personal.restored.readers.contains_key("items"));
    assert!(
        personal.restored.missing.is_empty(),
        "{:?}",
        personal.restored
    );
    assert!(
        personal.restored.unkeyed.is_empty(),
        "{:?}",
        personal.restored
    );
    // The custodian served the naming root's vouched record, which only
    // the passkey's root opens: everything the owner held is restored.
    assert!(personal.restored.naming);
    assert!(personal.restored.complete(), "{:?}", personal.restored);
    let phone = Device::new(personal.opened)?;
    assert_eq!(phone.session.freshness().await?, Freshness::Unwitnessed);
    phone.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(phone.titles().await, ["kept by custody"]);
    // Syncing with a custodian witnesses nothing.
    assert_eq!(phone.session.freshness().await?, Freshness::Unwitnessed);
    // The custodian keeps and relays the new device's certificate.
    let devices = known(&directory.path().join("custodian"))?.personal_devices(identity_key())?;
    assert!(devices.contains(&phone.session.info().await?.node));
    phone.close().await;
    custodian.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn partition_custody_receipts_and_a_keyless_backup_restore_only_retained_data()
-> Result<(), Failure> {
    use rocky_sdk::{
        PartitionCustody, PartitionSelection, PartitionTarget, ReplicaRole, ReplicaRoleConfig,
        SqlValue,
    };
    let directory = tempfile::tempdir()?;
    let hub = hub();
    let schema = Schema::parse(&common::SCHEMA.replace(
        r#""primary_key":["key"]"#,
        r#""primary_key":["key"],"partition_by":["key"],"partition_group":"list""#,
    ))?;
    let configuration = |path: &Path, name: &str| -> Result<Configuration, Failure> {
        Ok(Configuration::new(path, &schema)?.network(Network::Owned(hub.connector(name))))
    };
    let owner_path = directory.path().join("owner");
    let owner = Device::new(
        configuration(&owner_path, "owner")?
            .create_personal(prf(), namespace())
            .await?
            .opened,
    )?;
    owner.session.set_automatic_sync(false).await?;
    let target = |key: &str| PartitionTarget::Table {
        table: "items".into(),
        values: vec![SqlValue::Text(key.into())],
    };
    for key in ["registry", "chat"] {
        owner.session.create_partition(target(key)).await?;
        owner.session.execute("BEGIN", vec![]).await?;
        let id = owner
            .session
            .next_partition_id("items", vec![SqlValue::Text(key.into())])
            .await?;
        owner
            .session
            .execute(
                "INSERT INTO items(key,id,title,completed) VALUES(?,?,?,0)",
                vec![
                    SqlValue::Text(key.into()),
                    id.sql(),
                    SqlValue::Text(format!("{key} confidential")),
                ],
            )
            .await?;
        owner.session.execute("COMMIT", vec![]).await?;
    }
    let invitation = owner
        .session
        .invitation_with(GrantProfile::Custody {
            scopes: ["items".to_owned()].into(),
        })
        .await?
        .uri;
    let keeper_path = directory.path().join("custody");
    let keeper = Device::new(
        configuration(&keeper_path, "custody")?
            .join(&invitation)
            .await?,
    )?;
    keeper.session.set_automatic_sync(false).await?;
    common::introduce(&owner, &keeper).await;
    owner
        .session
        .set_replica_role(ReplicaRoleConfig {
            role: ReplicaRole::Partial,
        })
        .await?;
    for key in ["registry", "chat"] {
        owner
            .session
            .subscribe_partition(target(key), PartitionSelection::default())
            .await?;
    }
    let registry = owner
        .session
        .partition_address("items", vec![SqlValue::Text("registry".into())])
        .await?
        .scope;
    let chat = owner
        .session
        .partition_address("items", vec![SqlValue::Text("chat".into())])
        .await?
        .scope;
    owner
        .session
        .set_partition_custody(
            registry,
            PartitionCustody {
                peers: [rocky_credentials::PeerId::of(
                    keeper.session.info().await?.node,
                )]
                .into(),
                copies: 1,
            },
        )
        .await?;
    owner
        .session
        .set_partition_custody(chat, PartitionCustody::default())
        .await?;
    assert!(
        owner
            .session
            .evict_partition(target("registry"), Default::default())
            .await
            .is_err()
    );
    owner.sync_once().await?;
    assert_eq!(
        owner.titles().await,
        ["chat confidential", "registry confidential"]
    );
    assert!(
        !owner
            .session
            .evict_partition(target("registry"), Default::default())
            .await?
            .recovery_unconfirmed
    );
    keeper
        .session
        .set_replica_role(ReplicaRoleConfig {
            role: ReplicaRole::Partial,
        })
        .await?;
    keeper
        .session
        .subscribe_partition(target("registry"), PartitionSelection::default())
        .await?;
    keeper
        .session
        .evict_partition(target("chat"), Default::default())
        .await?;
    assert!(
        owner
            .session
            .evict_partition(target("chat"), Default::default())
            .await?
            .recovery_unconfirmed
    );
    assert_eq!(secrets_named(&keeper_path, ".partition-naming")?, 0);
    assert!(
        saved(&keeper_path)?["keys"]
            .as_object()
            .is_some_and(|keys| keys.is_empty())
    );
    assert!(
        keeper
            .session
            .read(rocky_sdk::ReadQuery::new("SELECT title FROM items", 8))
            .await
            .is_err()
    );
    let archive = directory.path().join("custody.backup");
    keeper.database.backup(&archive).await?;
    owner.close().await;
    keeper.close().await;
    assert!(files(&keeper_path)?.values().all(|bytes| {
        !bytes
            .windows(b"registry confidential".len())
            .any(|window| window == b"registry confidential")
    }));
    std::fs::remove_dir_all(owner_path)?;
    let path = directory.path().join("restored");
    let restored = Configuration::new(&path, &schema)?
        .sign_in(
            prf(),
            namespace(),
            SignInSource::Backup(VerifiedBackup::open(&archive)?),
        )
        .await?;
    assert!(restored.restored.naming);
    let restored = Device::new(restored.opened)?;
    assert_eq!(restored.titles().await, ["registry confidential"]);
    restored.close().await;
    Ok(())
}

/// An owner and a custodian holding "kept by custody", which announces
/// under the bootstrap topic; the owner closed, its directory kept.
async fn custody(directory: &Path, hub: &Arc<Hub>) -> Result<(PathBuf, Device), Failure> {
    let owner_path = directory.join("owner");
    let owner = create(&owner_path, hub, "owner").await?;
    let invitation = owner
        .session
        .invitation_with(GrantProfile::Custody {
            scopes: ["items".to_owned()].into(),
        })
        .await?
        .uri;
    let custodian = Device::new(
        config(&directory.join("custodian"))?
            .network(Network::Owned(hub.connector("custodian")))
            .join(&invitation)
            .await?,
    )?;
    common::introduce(&owner, &custodian).await;
    owner.add(1, "kept by custody").await;
    owner.sync().await.map_err(|error| error.to_string())?;
    announced(hub, "custodian").await;
    owner.close().await;
    Ok((owner_path, custodian))
}
/// The secret store entries of `directory` whose role ends in `role`.
fn secrets_named(directory: &Path, role: &str) -> Result<usize, Failure> {
    Ok(files(&directory.join("secrets"))?
        .keys()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(role))
        })
        .count())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_custodian_without_the_naming_record_reports_it_and_nothing_replaces_it()
-> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let hub = hub();
    let (owner_path, custodian) = custody(directory.path(), &hub).await?;
    custodian.close().await;
    // The custodian lost its record: it serves only key epochs.
    let custodian_path = directory.path().join("custodian");
    let mut held = saved(&custodian_path)?;
    assert!(
        held["naming_escrow"].is_object(),
        "{}",
        held["naming_escrow"]
    );
    held["naming_escrow"] = Value::Null;
    let connection = rusqlite::Connection::open(custodian_path.join("rocky.sqlite"))?;
    let table: String = connection.query_row("SELECT name FROM sqlite_schema WHERE type='table' AND name LIKE '%secure_custody_metadata'", [], |row| row.get(0))?;
    connection.execute(&format!("DELETE FROM [{table}]"), [])?;
    drop(connection);
    std::fs::write(
        custodian_path.join("identity.json"),
        serde_json::to_vec(&held)?,
    )?;
    std::fs::remove_dir_all(&owner_path)?;
    let custodian = Device::new(
        config(&custodian_path)?
            .network(Network::Owned(hub.connector("custodian")))
            .open()
            .await?,
    )?;
    announced(&hub, "custodian").await;

    let path = directory.path().join("phone");
    let personal = sign_in(&path, owned(&hub, "phone"), prf(), namespace()).await?;
    assert!(personal.restored.readers.contains_key("items"));
    assert!(personal.restored.missing.is_empty() && personal.restored.unkeyed.is_empty());
    // Naming future partitions is a separate gap, never a fresh root.
    assert!(!personal.restored.naming);
    assert!(!personal.restored.complete());
    let phone = Device::new(personal.opened)?;
    phone.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(phone.titles().await, ["kept by custody"]);
    phone.close().await;
    assert_eq!(secrets_named(&path, ".partition-naming")?, 0);
    custodian.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unwitnessed_device_stays_so_until_an_owner_device_reconciles() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let hub = hub();
    let (owner_path, custodian) = custody(directory.path(), &hub).await?;
    let path = directory.path().join("phone");
    let personal = sign_in(&path, owned(&hub, "phone"), prf(), namespace()).await?;
    assert_eq!(personal.freshness, Freshness::Unwitnessed);
    let phone = Device::new(personal.opened)?;
    assert_eq!(phone.session.freshness().await?, Freshness::Unwitnessed);
    phone.close().await;
    // Reopening keeps what the sign-in reported.
    let reopen = |name: &'static str, path: PathBuf| {
        let hub = hub.clone();
        async move {
            Device::new(
                config(&path)?
                    .network(Network::Owned(hub.connector(name)))
                    .open()
                    .await?,
            )
        }
    };
    let phone = reopen("phone", path.clone()).await?;
    assert_eq!(phone.session.freshness().await?, Freshness::Unwitnessed);
    // The owner device returns; its reconciliation witnesses the phone.
    let owner = reopen("owner", owner_path).await?;
    common::introduce(&owner, &phone).await;
    // With the custodian gone, the phone's pass reconciles with the owner
    // device, reachable again.
    custodian.close().await;
    phone.session.set_networking_enabled(true).await?;
    phone.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(phone.session.freshness().await?, Freshness::LocallyAnchored);
    phone.close().await;
    let phone = reopen("phone", path).await?;
    assert_eq!(phone.session.freshness().await?, Freshness::LocallyAnchored);
    phone.close().await;
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_source_creates_nothing() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("phone");
    // No network, or a network without discovery: no source.
    let error = refusal(sign_in(&path, None, prf(), namespace()).await);
    assert!(
        matches!(error, PersonalSignInError::SourceUnavailable),
        "{error}"
    );
    let silent = Arc::new(Hub::default());
    let error = refusal(sign_in(&path, owned(&silent, "phone"), prf(), namespace()).await);
    assert!(
        matches!(error, PersonalSignInError::SourceUnavailable),
        "{error}"
    );
    assert_eq!(error.code(), "source_unavailable");
    assert!(!path.exists(), "an unavailable source left files");
    // An existing empty directory stays empty.
    std::fs::create_dir_all(&path)?;
    let error = refusal(sign_in(&path, owned(&silent, "phone"), prf(), namespace()).await);
    assert!(
        matches!(error, PersonalSignInError::SourceUnavailable),
        "{error}"
    );
    assert!(files(&path)?.is_empty());
    assert!(std::fs::read_dir(&path)?.next().is_none());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_passkey_or_namespace_finds_nothing_and_touches_nothing() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let hub = hub();
    let owner_path = directory.path().join("owner");
    let owner = create(&owner_path, &hub, "owner").await?;
    owner.add(1, "mine").await;
    let before = files(&owner_path)?;
    let devices = known(&owner_path)?.personal_devices(identity_key())?;
    let other = PersonalNamespace::new("shopping.example", "other root")?;
    for (name, prf, namespace) in [
        ("stranger", passkey(2, &namespace()), namespace()),
        ("elsewhere", passkey(1, &other), other.clone()),
    ] {
        let path = directory.path().join(name);
        let error = refusal(
            config(&path)?
                .network(Network::Owned(hub.connector(name)))
                .sign_in_wait(std::time::Duration::from_millis(200))
                .sign_in(prf, namespace, SignInSource::Discover)
                .await,
        );
        assert!(
            matches!(error, PersonalSignInError::SourceUnavailable),
            "{error}"
        );
        assert!(!path.exists());
    }
    assert_eq!(
        known(&owner_path)?.personal_devices(identity_key())?,
        devices
    );
    owner.close().await;
    // Only the owner's own writes changed its store since.
    let after = files(&owner_path)?;
    assert_eq!(
        before.get(&owner_path.join("identity.json")),
        after.get(&owner_path.join("identity.json"))
    );
    Ok(())
}

/// Forwards everything; the `drop`th request's reply is lost after the
/// responder answered it.
struct LosingTransport {
    inner: Arc<dyn Transport>,
    requests: Arc<AtomicUsize>,
    drop: usize,
}
impl Transport for LosingTransport {
    fn kind(&self) -> &str {
        self.inner.kind()
    }
    fn address(&self) -> BoxFuture<'_, Result<String, Failure>> {
        self.inner.address()
    }
    fn connect<'a>(
        &'a self,
        address: &'a str,
        protocol: rocky_core::net::Protocol,
        scope: Option<rocky_core::Scope>,
    ) -> BoxFuture<'a, Result<Stream, Failure>> {
        self.inner.connect(address, protocol, scope)
    }
    fn accept(&self) -> BoxFuture<'_, Option<rocky_core::net::Incoming>> {
        self.inner.accept()
    }
    fn listen(&self, scope: Option<rocky_core::Scope>, listening: bool) -> Result<(), Failure> {
        self.inner.listen(scope, listening)
    }
    fn request<'a>(
        &'a self,
        addresses: &'a [String],
        scope: rocky_core::Scope,
        request: &'a [u8],
    ) -> BoxFuture<'a, Result<Vec<u8>, Failure>> {
        Box::pin(async move {
            let reply = self.inner.request(addresses, scope, request).await?;
            if self.requests.fetch_add(1, Ordering::SeqCst) + 1 == self.drop {
                return Err("injected: the reply was lost".into());
            }
            Ok(reply)
        })
    }
    fn session<'a>(
        &'a self,
        addresses: &'a [String],
        scope: rocky_core::Scope,
    ) -> BoxFuture<'a, Result<Stream, Failure>> {
        self.inner.session(addresses, scope)
    }
    fn close(&self) -> BoxFuture<'_, ()> {
        self.inner.close()
    }
}
struct Losing {
    inner: Arc<dyn Connector>,
    drop: usize,
}
impl Connector for Losing {
    fn start(&self, accepts: bool, secret: [u8; 32]) -> BoxFuture<'_, Result<Connected, Failure>> {
        Box::pin(async move {
            let started = self.inner.start(accepts, secret).await?;
            Ok(Connected {
                transport: Arc::new(LosingTransport {
                    inner: started.transport,
                    requests: Arc::new(AtomicUsize::new(0)),
                    drop: self.drop,
                }),
                discovery: started.discovery,
                changes: started.changes,
            })
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_sign_in_resumes_under_the_same_node() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let hub = hub();
    let owner_path = directory.path().join("owner");
    let owner = create(&owner_path, &hub, "owner").await?;
    owner.add(1, "from owner").await;
    let path = directory.path().join("phone");
    // Hello, one bootstrap page, then the publication: the owner keeps the
    // certificate, and its reply is lost.
    let losing = Network::Owned(Arc::new(Losing {
        inner: hub.connector("phone"),
        drop: 3,
    }));
    let error = refusal(sign_in(&path, Some(losing), prf(), namespace()).await);
    assert!(matches!(error, PersonalSignInError::Failed(_)), "{error}");
    assert!(path.join("personal.intent").exists());
    assert!(!path.join("identity.json").exists());
    let published = known(&owner_path)?.personal_devices(identity_key())?;
    assert_eq!(published.len(), 2);

    let personal = sign_in(&path, owned(&hub, "phone"), prf(), namespace()).await?;
    let phone = Device::new(personal.opened)?;
    let node = phone.session.info().await?.node;
    // One node and one certificate: the retry published the same one.
    assert!(published.contains(&node));
    let held = known(&owner_path)?;
    assert_eq!(held.personal_devices(identity_key())?, published);
    assert_eq!(
        held.certificates()
            .filter(|certificate| certificate.capability.subject == node)
            .count(),
        1
    );
    assert!(saved(&path)?["personal"]["publication"].is_array());
    assert!(!path.join("personal.intent").exists());
    phone.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(phone.titles().await, ["from owner"]);
    phone.close().await;
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_node_never_signs_in_again_under_that_node() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let hub = hub();
    let owner_path = directory.path().join("owner");
    let owner = create(&owner_path, &hub, "owner").await?;
    let path = directory.path().join("phone");
    let losing = Network::Owned(Arc::new(Losing {
        inner: hub.connector("phone"),
        drop: 3,
    }));
    refusal(sign_in(&path, Some(losing), prf(), namespace()).await);
    let owner_node = owner.node().await;
    let node = known(&owner_path)?
        .personal_devices(identity_key())?
        .into_iter()
        .find(|node| node.to_hex() != owner_node)
        .ok_or("the published node")?;
    // The owner unlinks that node before its install finished.
    let identity: Arc<dyn rocky_sdk::IdentityProvider> = Arc::new(account().identity());
    owner
        .session
        .unlink(rocky_credentials::PeerId::of(node), identity)
        .await?;
    let error = refusal(sign_in(&path, owned(&hub, "phone"), prf(), namespace()).await);
    assert!(
        matches!(error, PersonalSignInError::RevokedNode { node: revoked } if revoked == rocky_credentials::PeerId::of(node)),
        "{error}"
    );
    assert_eq!(error.code(), "device_revoked");
    assert!(!path.join("personal.intent").exists());
    // The passkey holder signs in again as a fresh node.
    let personal = sign_in(&path, owned(&hub, "phone"), prf(), namespace()).await?;
    let phone = Device::new(personal.opened)?;
    assert_ne!(phone.session.info().await?.node, node);
    phone.close().await;
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_backup_restores_offline_and_its_device_keeps_working() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let hub = hub();
    let owner_path = directory.path().join("owner");
    let owner = create(&owner_path, &hub, "owner").await?;
    owner.add(1, "backed up").await;
    let archive = directory.path().join("owner.backup");
    owner.database.backup(&archive).await?;
    owner.close().await;
    let keys = saved(&owner_path)?["keys"].clone();

    let backup = VerifiedBackup::open(&archive)?;
    assert_eq!(backup.devices()?.len(), 1);
    // Another account's passkey finds no source in it.
    let stranger = directory.path().join("stranger");
    let error = refusal(
        config(&stranger)?
            .sign_in(
                passkey(2, &namespace()),
                namespace(),
                SignInSource::Backup(backup),
            )
            .await,
    );
    assert!(
        matches!(error, PersonalSignInError::SourceUnavailable),
        "{error}"
    );
    assert!(!stranger.exists());

    let path = directory.path().join("phone");
    let personal = config(&path)?
        .sign_in(
            prf(),
            namespace(),
            SignInSource::Backup(VerifiedBackup::open(&archive)?),
        )
        .await?;
    assert_eq!(personal.freshness, Freshness::Unwitnessed);
    assert!(personal.restored.complete(), "{:?}", personal.restored);
    let phone = Device::new(personal.opened)?;
    assert_eq!(phone.session.freshness().await?, Freshness::Unwitnessed);
    assert_eq!(phone.titles().await, ["backed up"]);
    phone.add(2, "restored").await;
    let node = phone.session.info().await?.node;
    phone.close().await;
    untouched(&path, &saved(&path)?["keys"])?;
    assert_eq!(saved(&path)?["keys"], keys, "the restore made a new key");

    let reopened = Device::new(config(&path)?.open().await?)?;
    assert_eq!(reopened.session.freshness().await?, Freshness::Unwitnessed);
    reopened.close().await;
    // The archived device returns and still works beside the new one.
    let owner = Device::new(
        config(&owner_path)?
            .network(Network::Owned(hub.connector("owner")))
            .open()
            .await?,
    )?;
    let phone = Device::new(
        config(&path)?
            .network(Network::Owned(hub.connector("phone")))
            .open()
            .await?,
    )?;
    // Reopened, the restore is still unwitnessed.
    assert_eq!(phone.session.freshness().await?, Freshness::Unwitnessed);
    common::introduce(&owner, &phone).await;
    for device in [&phone, &owner] {
        device.sync().await.map_err(|error| error.to_string())?;
    }
    assert_eq!(owner.titles().await, ["backed up", "restored"]);
    // The archived owner device reconciled with it: now anchored.
    assert_eq!(phone.session.freshness().await?, Freshness::LocallyAnchored);
    untouched(&owner_path, &keys)?;
    assert!(
        known(&owner_path)?
            .personal_devices(identity_key())?
            .contains(&node)
    );
    owner.close().await;
    phone.close().await;
    Ok(())
}

/// A schema of the same application in another version: another genesis.
const OTHER: &str = r#"{"application":"shopping","version":2,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},{"name":"notes","class":"replayed","columns":[{"name":"key","type":"text","not_null":true}],"primary_key":["key"]}]}"#;

#[tokio::test(flavor = "multi_thread")]
async fn independent_creates_are_a_conflict_and_both_stay() -> Result<(), Failure> {
    // Two explicit first creates of one account and place, of the same
    // schema and of another version: each pins its own genesis.
    for second_schema in [common::SCHEMA, OTHER] {
        let directory = tempfile::tempdir()?;
        let hub = hub();
        let first_path = directory.path().join("first");
        let first = create(&first_path, &hub, "first").await?;
        first.add(1, "first").await;
        let second_path = directory.path().join("second");
        let second = Device::new(
            Configuration::new(&second_path, &Schema::parse(second_schema)?)?
                .network(Network::Owned(hub.connector("second")))
                .create_personal(prf(), namespace())
                .await?
                .opened,
        )?;
        assert_eq!(first.scope(), second.scope());
        assert_ne!(
            saved(&first_path)?["genesis"],
            saved(&second_path)?["genesis"]
        );
        announced(&hub, "first").await;
        announced(&hub, "second").await;
        let pinned = |path: &Path| -> Result<Value, Failure> {
            let saved = saved(path)?;
            Ok(serde_json::json!([
                saved["genesis"],
                saved["keys"],
                saved["revocations"]
            ]))
        };
        let before = (pinned(&first_path)?, pinned(&second_path)?);
        let path = directory.path().join("phone");
        let error = refusal(sign_in(&path, owned(&hub, "phone"), prf(), namespace()).await);
        assert!(
            matches!(error, PersonalSignInError::GenesisConflict),
            "{error}"
        );
        assert!(!path.exists());
        first.close().await;
        second.close().await;
        // Neither store was chosen, changed or ended: discovery hints aside,
        // each keeps its genesis, keys and authority.
        assert_eq!(pinned(&first_path)?, before.0);
        assert_eq!(pinned(&second_path)?, before.1);
        let first = Device::new(config(&first_path)?.open().await?)?;
        assert_eq!(first.titles().await, ["first"]);
        first.close().await;
    }
    Ok(())
}

/// Which of a sign-in's requests a forging source refuses.
#[derive(Clone, Copy, PartialEq)]
enum Forged {
    Bootstrap,
    Publish,
}
/// Forwards everything, but answers the first `times` requests of `kind`
/// with a refusal carrying `code`, as a lying source can.
struct ForgingTransport {
    inner: Arc<dyn Transport>,
    kind: Forged,
    code: &'static str,
    left: AtomicUsize,
}
impl Transport for ForgingTransport {
    fn kind(&self) -> &str {
        self.inner.kind()
    }
    fn address(&self) -> BoxFuture<'_, Result<String, Failure>> {
        self.inner.address()
    }
    fn connect<'a>(
        &'a self,
        address: &'a str,
        protocol: rocky_core::net::Protocol,
        scope: Option<rocky_core::Scope>,
    ) -> BoxFuture<'a, Result<Stream, Failure>> {
        self.inner.connect(address, protocol, scope)
    }
    fn accept(&self) -> BoxFuture<'_, Option<rocky_core::net::Incoming>> {
        self.inner.accept()
    }
    fn listen(&self, scope: Option<rocky_core::Scope>, listening: bool) -> Result<(), Failure> {
        self.inner.listen(scope, listening)
    }
    fn request<'a>(
        &'a self,
        addresses: &'a [String],
        scope: rocky_core::Scope,
        request: &'a [u8],
    ) -> BoxFuture<'a, Result<Vec<u8>, Failure>> {
        use rocky_credentials::{IdentityEnrollment, LinkEnrollment, PeerRequest};
        Box::pin(async move {
            let kind = match rocky_credentials::PeerRequestFrame::from_bytes(request)?.request {
                PeerRequest::Identity(IdentityEnrollment::Personal(_)) => Some(Forged::Bootstrap),
                PeerRequest::LinkEnroll(LinkEnrollment::Personal(_)) => Some(Forged::Publish),
                _ => None,
            };
            let forge = kind == Some(self.kind)
                && self
                    .left
                    .try_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                        left.checked_sub(1)
                    })
                    .is_ok();
            if !forge {
                return self.inner.request(addresses, scope, request).await;
            }
            Ok(rocky_credentials::PeerReplyFrame {
                version: rocky_credentials::PEER_RPC_VERSION,
                result: Err(rocky_credentials::PeerError {
                    code: self.code.to_owned(),
                    message: "forged".to_owned(),
                    retryable: false,
                }),
            }
            .to_bytes()?)
        })
    }
    fn session<'a>(
        &'a self,
        addresses: &'a [String],
        scope: rocky_core::Scope,
    ) -> BoxFuture<'a, Result<Stream, Failure>> {
        self.inner.session(addresses, scope)
    }
    fn close(&self) -> BoxFuture<'_, ()> {
        self.inner.close()
    }
}
struct Forging {
    inner: Arc<dyn Connector>,
    kind: Forged,
    code: &'static str,
    times: usize,
}
impl Connector for Forging {
    fn start(&self, accepts: bool, secret: [u8; 32]) -> BoxFuture<'_, Result<Connected, Failure>> {
        Box::pin(async move {
            let started = self.inner.start(accepts, secret).await?;
            Ok(Connected {
                transport: Arc::new(ForgingTransport {
                    inner: started.transport,
                    kind: self.kind,
                    code: self.code,
                    left: AtomicUsize::new(self.times),
                }),
                discovery: started.discovery,
                changes: started.changes,
            })
        })
    }
}
fn forging(
    hub: &Arc<Hub>,
    name: &str,
    kind: Forged,
    code: &'static str,
    times: usize,
) -> Option<Network> {
    Some(Network::Owned(Arc::new(Forging {
        inner: hub.connector(name),
        kind,
        code,
        times,
    })))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sources_unverified_refusal_never_ends_the_install() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let hub = hub();
    let owner_path = directory.path().join("owner");
    let owner = create(&owner_path, &hub, "owner").await?;
    owner.add(1, "from owner").await;
    // A claimed revocation ends only that source's attempt: the install,
    // its node and keys stay for the retry.
    let path = directory.path().join("phone");
    let claimed = forging(
        &hub,
        "phone",
        Forged::Bootstrap,
        "device_revoked",
        usize::MAX,
    );
    let error = refusal(sign_in(&path, claimed, prf(), namespace()).await);
    assert!(
        !matches!(error, PersonalSignInError::RevokedNode { .. }),
        "{error}"
    );
    assert!(path.join("personal.intent").exists());
    // A claimed device limit is reported, and the published certificate's
    // install stays too.
    let full = forging(&hub, "phone", Forged::Publish, "device_limit", usize::MAX);
    let error = refusal(sign_in(&path, full, prf(), namespace()).await);
    assert!(
        matches!(error, PersonalSignInError::Capacity { .. }),
        "{error}"
    );
    assert!(path.join("personal.intent").exists());
    let phone = Device::new(
        sign_in(&path, owned(&hub, "phone"), prf(), namespace())
            .await?
            .opened,
    )?;
    let node = phone.session.info().await?.node;
    assert_eq!(
        known(&owner_path)?.personal_devices(identity_key())?.len(),
        2
    );
    phone.close().await;

    // With another source reachable, one lying source does not stop it.
    let laptop_path = directory.path().join("laptop");
    let once = forging(&hub, "laptop", Forged::Bootstrap, "device_revoked", 1);
    let phone = Device::new(
        config(&path)?
            .network(Network::Owned(hub.connector("phone")))
            .open()
            .await?,
    )?;
    announced(&hub, "phone").await;
    let laptop = Device::new(
        sign_in(&laptop_path, once, prf(), namespace())
            .await?
            .opened,
    )?;
    assert_ne!(laptop.session.info().await?.node, node);
    laptop.close().await;
    phone.close().await;
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sign_in_never_takes_back_an_installed_database() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let hub = hub();
    let owner_path = directory.path().join("owner");
    let owner = create(&owner_path, &hub, "owner").await?;
    let path = directory.path().join("phone");
    let losing = Network::Owned(Arc::new(Losing {
        inner: hub.connector("phone"),
        drop: 3,
    }));
    refusal(sign_in(&path, Some(losing), prf(), namespace()).await);
    let intent = std::fs::read(path.join("personal.intent"))?;
    let phone = Device::new(
        sign_in(&path, owned(&hub, "phone"), prf(), namespace())
            .await?
            .opened,
    )?;
    phone.add(2, "unsynced").await;
    phone.close().await;
    // A crash between saving the identity and ending the intent left it:
    // signing in again hands out that database as it is.
    std::fs::write(path.join("personal.intent"), &intent)?;
    let resumed = sign_in(&path, owned(&hub, "phone"), prf(), namespace()).await?;
    assert_eq!(resumed.freshness, Freshness::LocallyAnchored);
    assert!(resumed.restored.complete(), "{:?}", resumed.restored);
    let phone = Device::new(resumed.opened)?;
    assert_eq!(phone.titles().await, ["unsynced"]);
    phone.close().await;
    assert!(!path.join("personal.intent").exists());
    // Opening ends a leftover intent too, and the local write stays.
    std::fs::write(path.join("personal.intent"), &intent)?;
    let phone = Device::new(config(&path)?.open().await?)?;
    assert_eq!(phone.titles().await, ["unsynced"]);
    phone.close().await;
    assert!(!path.join("personal.intent").exists());
    // Without an intent, an installed database is never signed in again.
    let before = files(&path)?;
    let error = refusal(sign_in(&path, owned(&hub, "phone"), prf(), namespace()).await);
    assert!(
        matches!(error, PersonalSignInError::ExistingState),
        "{error}"
    );
    assert_eq!(files(&path)?, before);
    owner.close().await;
    Ok(())
}

/// One raw peer RPC to `address`, as an unauthenticated stranger sends it.
async fn ask(
    transport: &Connected,
    address: &str,
    scope: rocky_core::Scope,
    request: &rocky_credentials::PeerRequest,
) -> Result<rocky_credentials::PeerReply, rocky_credentials::PeerError> {
    let bytes = transport
        .request(
            &[address.to_owned()],
            scope,
            &request.to_bytes().expect("request"),
        )
        .await
        .expect("transport");
    rocky_credentials::PeerReplyFrame::from_bytes(&bytes)
        .expect("reply")
        .result
}
async fn challenge(
    transport: &Connected,
    address: &str,
    scope: rocky_core::Scope,
) -> (
    rocky_credentials::PersonalHello,
    rocky_credentials::PersonalChallenge,
) {
    use rocky_credentials::{IdentityEnrollment, IdentityReply, PeerReply, PeerRequest};
    let hello = rocky_credentials::PersonalHello::fresh();
    let request = PeerRequest::Identity(IdentityEnrollment::PersonalHello(hello.clone()));
    match ask(transport, address, scope, &request).await {
        Ok(PeerReply::Identity(IdentityReply::PersonalChallenge(challenge))) => (hello, *challenge),
        _ => panic!("no personal challenge"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn each_personal_request_path_refuses_what_its_trust_forbids() -> Result<(), Failure> {
    use rocky_credentials::{
        DatabaseContext, IdentityEnrollment, LinkEnrollment, PeerReply, PeerRequest, PersonalAct,
        PersonalAuthorization, PersonalPublication,
    };
    let directory = tempfile::tempdir()?;
    let hub = hub();
    let owner = create(&directory.path().join("owner"), &hub, "owner").await?;
    let scope = *owner.scope();
    let probe = hub.connector("probe").start(false, [0; 32]).await?;
    let account = account();
    let database = DatabaseContext {
        publisher: account.locator().publisher,
        scope,
    };
    let (hello, offered) = challenge(&probe, "owner", scope).await;
    let (genesis, _) = offered.verify(&hello, database, (identity_key(), &namespace()))?;
    let descriptor = genesis.descriptor()?;
    let node = rocky_credentials::PrivateKey::new();
    let bootstrap = |nonce, responder, owner: &dyn rocky_sdk::IdentityProvider| {
        PersonalAuthorization::sign(
            (account.root(), owner),
            &node,
            (database, responder),
            (descriptor, nonce),
            PersonalAct::Bootstrap { page: 0 },
        )
        .expect("authorization")
        .0
    };
    let fetch = |authorization: PersonalAuthorization| {
        PeerRequest::Identity(IdentityEnrollment::Personal(Box::new(authorization)))
    };
    let identity = account.identity();
    // A nonce the responder never issued.
    let refused = ask(
        &probe,
        "owner",
        scope,
        &fetch(bootstrap([9; 32], offered.responder, &identity)),
    )
    .await;
    assert!(refused.is_err());
    // Another identity than the pinned owner cannot authorize.
    let stranger = rocky_sdk::IdentityKey::generate();
    let refused = ask(
        &probe,
        "owner",
        scope,
        &fetch(bootstrap(offered.nonce(), offered.responder, &stranger)),
    )
    .await;
    assert_eq!(
        refused.err().map(|error| error.code),
        Some("wrong_owner".to_owned())
    );
    // An authorization for another responder.
    let elsewhere = rocky_credentials::PrivateKey::new().public_key();
    let refused = ask(
        &probe,
        "owner",
        scope,
        &fetch(bootstrap(offered.nonce(), elsewhere, &identity)),
    )
    .await;
    assert!(refused.is_err());
    // The genuine one answers once; its replay is refused.
    let genuine = bootstrap(offered.nonce(), offered.responder, &identity);
    assert!(matches!(
        ask(&probe, "owner", scope, &fetch(genuine.clone())).await,
        Ok(PeerReply::Identity(_))
    ));
    assert!(ask(&probe, "owner", scope, &fetch(genuine)).await.is_err());

    // A publication must name its node's own certificate.
    let (hello, offered) = challenge(&probe, "owner", scope).await;
    offered.verify(&hello, database, (identity_key(), &namespace()))?;
    let grant = rocky_credentials::Authority::owner(account.root()).issue_trusted(
        identity_key(),
        scope,
        1,
    )?;
    let other = rocky_credentials::PrivateKey::new();
    let certificate = rocky_credentials::CredentialChain(vec![grant])
        .certify_personal(&identity, &other, (identity_key(), descriptor))?
        .certificate()
        .ok_or("certificate")?
        .clone();
    let (authorization, _) = PersonalAuthorization::sign(
        (account.root(), &identity),
        &node,
        (database, offered.responder),
        (descriptor, offered.nonce()),
        PersonalAct::Publish {
            certificate: certificate.id()?,
            publication: [1; 16],
        },
    )?;
    let publish =
        PeerRequest::LinkEnroll(LinkEnrollment::Personal(Box::new(PersonalPublication {
            authorization,
            certificate: certificate.clone(),
        })));
    assert!(ask(&probe, "owner", scope, &publish).await.is_err());
    // Nor is the shared link path a bypass: only this identity's node links.
    let shared = PeerRequest::LinkEnroll(LinkEnrollment::Shared(Box::new(certificate)));
    assert!(ask(&probe, "owner", scope, &shared).await.is_err());
    // A device that only bootstrapped published nothing: the owner device
    // knows itself alone.
    let devices = known(&directory.path().join("owner"))?.personal_devices(identity_key())?;
    assert_eq!(
        devices.iter().map(|node| node.to_hex()).collect::<Vec<_>>(),
        [owner.node().await]
    );
    probe.transport.close().await;
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shared_database_answers_no_personal_hello() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let hub = hub();
    let shared = Device::create(&directory.path().join("shared"), owned(&hub, "shared"))?;
    let probe = hub.connector("probe").start(false, [0; 32]).await?;
    let request = rocky_credentials::PeerRequest::Identity(
        rocky_credentials::IdentityEnrollment::PersonalHello(
            rocky_credentials::PersonalHello::fresh(),
        ),
    );
    assert!(
        ask(&probe, "shared", *shared.scope(), &request)
            .await
            .is_err()
    );
    probe.transport.close().await;
    shared.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn sign_in_observes_existing_devices_as_read_and_only_new_devices_raise_notices()
-> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let hub = hub();
    let owner = create(&directory.path().join("owner"), &hub, "owner").await?;
    owner.session.set_automatic_sync(false).await?;
    let phone = Device::new(
        sign_in(
            &directory.path().join("phone"),
            owned(&hub, "phone"),
            prf(),
            namespace(),
        )
        .await?
        .opened,
    )?;
    phone.session.set_automatic_sync(false).await?;
    common::introduce(&owner, &phone).await;
    phone.sync().await.map_err(|e| e.to_string())?;
    let owner_node = rocky_sdk::PeerId::of(owner.session.info().await?.node);
    let phone_node = rocky_sdk::PeerId::of(phone.session.info().await?.node);
    let baseline = phone.session.personal_device_notices().await?;
    assert_eq!(baseline.len(), 1, "{baseline:?}");
    assert_eq!(baseline[0].node, owner_node);
    assert!(baseline[0].read && baseline[0].acknowledged);
    assert!(
        !phone
            .session
            .status()
            .await?
            .alerts
            .iter()
            .any(|alert| { matches!(alert.kind, rocky_sdk::AlertKind::TrustedNodeLinked { .. }) })
    );
    owner
        .session
        .acknowledge_personal_device_notice(phone_node)
        .await?;

    let laptop = Device::new(
        sign_in(
            &directory.path().join("laptop"),
            owned(&hub, "laptop"),
            prf(),
            namespace(),
        )
        .await?
        .opened,
    )?;
    laptop.session.set_automatic_sync(false).await?;
    let laptop_node = rocky_sdk::PeerId::of(laptop.session.info().await?.node);
    common::introduce(&owner, &laptop).await;
    for device in [&owner, &phone] {
        device.sync().await.map_err(|e| e.to_string())?;
        device.sync().await.map_err(|e| e.to_string())?;
        let notices = device.session.personal_device_notices().await?;
        let unread: Vec<_> = notices.iter().filter(|notice| !notice.read).collect();
        assert_eq!(unread.len(), 1, "{notices:?}");
        assert_eq!(unread[0].node, laptop_node);
        assert!(!unread[0].acknowledged);
        assert_eq!(
            device
                .session
                .status()
                .await?
                .alerts
                .iter()
                .filter(|alert| {
                    matches!(alert.kind, rocky_sdk::AlertKind::TrustedNodeLinked { .. })
                })
                .count(),
            1
        );
    }
    laptop.close().await;
    phone.close().await;
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn personal_notices_are_local_durable_and_deduplicated_after_custody_delivery()
-> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let hub = hub();
    let owner_path = directory.path().join("owner");
    let owner = create(&owner_path, &hub, "owner").await?;
    owner.session.set_automatic_sync(false).await?;
    assert!(owner.session.personal_device_notices().await?.is_empty());
    let invitation = owner
        .session
        .invitation_with(GrantProfile::Custody {
            scopes: ["items".to_owned()].into(),
        })
        .await?
        .uri;
    let custodian = Device::new(
        config(&directory.path().join("custodian"))?
            .network(Network::Owned(hub.connector("custodian")))
            .join(&invitation)
            .await?,
    )?;
    custodian.session.set_automatic_sync(false).await?;
    common::introduce(&owner, &custodian).await;
    owner.sync().await.map_err(|e| e.to_string())?;
    owner.close().await;
    // No owner device is serving; the new device publishes to custody.
    let phone = Device::new(
        config(&directory.path().join("phone"))?
            .network(Network::Owned(hub.connector("phone")))
            .sign_in(prf(), namespace(), SignInSource::Discover)
            .await?
            .opened,
    )?;
    phone.session.set_automatic_sync(false).await?;
    let node = rocky_sdk::PeerId::of(phone.session.info().await?.node);
    phone.close().await;
    let owner = Device::new(
        config(&owner_path)?
            .network(Network::Owned(hub.connector("owner")))
            .open()
            .await?,
    )?;
    owner.session.set_automatic_sync(false).await?;
    owner.sync().await.map_err(|e| e.to_string())?;
    let notices = owner.session.personal_device_notices().await?;
    assert_eq!(notices.len(), 1, "{notices:?}");
    let notice = notices[0].clone();
    assert_eq!(notice.node, node);
    assert_eq!(notice.scope, *owner.scope());
    assert_eq!(notice.owner, identity_key());
    assert!(!notice.read && !notice.acknowledged);
    owner.session.read_personal_device_notice(node).await?;
    assert!(owner.session.personal_device_notices().await?[0].read);
    owner
        .session
        .acknowledge_personal_device_notice(node)
        .await?;
    owner.session.renew().await?;
    owner.sync().await.map_err(|e| e.to_string())?;
    owner.sync().await.map_err(|e| e.to_string())?;
    let retained = owner.session.personal_device_notices().await?;
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].first_observed, notice.first_observed);
    assert!(retained[0].read && retained[0].acknowledged);
    owner.close().await;
    let owner = Device::new(config(&owner_path)?.open().await?)?;
    assert_eq!(owner.session.personal_device_notices().await?, retained);
    assert!(
        !owner
            .session
            .status()
            .await?
            .alerts
            .iter()
            .any(|alert| matches!(&alert.kind, rocky_sdk::AlertKind::TrustedNodeLinked { .. }))
    );
    owner.close().await;
    custodian.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_passkey_revoke_is_precise_idempotent_durable_and_delivered_to_the_ended_node()
-> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let hub = hub();
    let owner_path = directory.path().join("owner");
    let owner = create(&owner_path, &hub, "owner").await?;
    owner.session.set_automatic_sync(false).await?;
    let phone = Device::new(
        sign_in(
            &directory.path().join("phone"),
            owned(&hub, "phone"),
            prf(),
            namespace(),
        )
        .await?
        .opened,
    )?;
    let laptop = Device::new(
        sign_in(
            &directory.path().join("laptop"),
            owned(&hub, "laptop"),
            prf(),
            namespace(),
        )
        .await?
        .opened,
    )?;
    for device in [&phone, &laptop] {
        device.session.set_automatic_sync(false).await?;
        common::introduce(&owner, device).await;
        device.sync().await.map_err(|e| e.to_string())?;
    }
    let target_key = phone.session.info().await?.node;
    let target = rocky_sdk::PeerId::of(target_key);
    let before = saved(&owner_path)?["keys"].clone();
    let wrong = owner
        .session
        .revoke_personal_device(target, passkey(2, &namespace()))
        .await;
    assert!(matches!(wrong, Err(PersonalSignInError::WrongLocalAccount)));
    assert_eq!(saved(&owner_path)?["keys"], before);
    assert!(!known(&owner_path)?.has_revocations());
    let receipt = owner
        .session
        .revoke_personal_device(target, prf())
        .await
        .map_err(|e| format!("initial revoke: {e:?}"))?;
    assert!(receipt.local);
    assert!(receipt.delivered.contains(&target), "{receipt:?}");
    let after = saved(&owner_path)?["keys"].clone();
    assert_ne!(before, after);
    let known = known(&owner_path)?;
    let statement = known.get(&receipt.statement).ok_or("statement")?;
    assert_eq!(statement.statement.nodes, [target].into());
    assert_eq!(statement.statement.mode, rocky_sdk::RevocationMode::Lost);
    assert!(statement.statement.identities.is_empty());
    assert_eq!(known.personal_devices(identity_key())?.len(), 2);
    assert!(matches!(
        phone
            .session
            .status()
            .await
            .map_err(|e| format!("ended status: {e}"))?
            .authority
            .verified_revocation
            .map(|v| v.reason),
        Some(rocky_sdk::RevocationReason::Node)
    ));
    assert!(phone.sync().await.is_err());
    assert!(phone.session.execute("BEGIN", Vec::new()).await.is_err());
    assert!(phone.session.seal(false).await.is_err());
    let repeated = owner.session.revoke_personal_device(target, prf()).await?;
    assert_eq!(repeated, receipt);
    assert_eq!(saved(&owner_path)?["keys"], after);
    // A sibling remains a writer and trusted sealer.
    laptop
        .sync()
        .await
        .map_err(|e| format!("sibling sync: {e}"))?;
    laptop.add(4, "survives").await;
    laptop
        .sync()
        .await
        .map_err(|e| format!("sibling write sync: {e}"))?;
    owner
        .sync()
        .await
        .map_err(|e| format!("owner write sync: {e}"))?;
    assert_eq!(
        owner.titles().await,
        ["survives"],
        "sibling write after unlink"
    );
    let sealed = laptop
        .session
        .seal(false)
        .await
        .map_err(|e| format!("sibling seal: {e}"))?;
    laptop
        .sync()
        .await
        .map_err(|e| format!("sibling seal sync: {e}"))?;
    owner
        .sync()
        .await
        .map_err(|e| format!("owner seal sync: {e}"))?;
    let inspection = owner.session.inspect_tips().await?;
    assert!(
        inspection
            .seals
            .iter()
            .any(|seal| seal.id == sealed.tip && !seal.revoked),
        "{inspection:?}"
    );
    let notice = owner
        .session
        .personal_device_notices()
        .await?
        .into_iter()
        .find(|n| n.node == target)
        .ok_or("notice")?;
    assert!(notice.unlinked.is_some());
    let settled = saved(&owner_path)?["keys"].clone();
    phone.close().await;
    laptop.close().await;
    owner.close().await;
    let owner = Device::new(config(&owner_path)?.open().await?)?;
    assert_eq!(
        owner.session.revoke_personal_device(target, prf()).await?,
        receipt
    );
    assert_eq!(saved(&owner_path)?["keys"], settled);
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_personal_owner_reset_uses_passkey_authority_and_refuses_other_full_readers()
-> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let hub = hub();
    let owner_path = directory.path().join("owner");
    let owner = create(&owner_path, &hub, "owner").await?;
    owner.session.set_automatic_sync(false).await?;
    let laptop = Device::new(
        sign_in(
            &directory.path().join("laptop"),
            owned(&hub, "laptop"),
            prf(),
            namespace(),
        )
        .await?
        .opened,
    )?;
    laptop.session.set_automatic_sync(false).await?;
    common::introduce(&owner, &laptop).await;
    laptop.add(1, "covered").await;
    laptop.sync().await.map_err(|e| e.to_string())?;
    laptop.add(2, "discarded").await;
    let keep = rocky_sdk::ResetKeep {
        also: [rocky_sdk::PeerId::of(laptop.session.info().await?.node)].into(),
        ..Default::default()
    };
    let Err(error) = owner.session.sign_out_personal_devices(prf(), keep).await else {
        panic!("kept another full reader");
    };
    assert_eq!(error.code(), "not_custodian");
    assert!(known(&owner_path)?.reset_heads().is_empty());
    let receipt = owner
        .session
        .sign_out_personal_devices(prf(), Default::default())
        .await?;
    assert_eq!(
        receipt.kept,
        [rocky_sdk::PeerId::of(owner.session.info().await?.node)].into()
    );
    assert_eq!(
        owner.session.freshness().await?,
        Freshness::FencedByOwner {
            reset: receipt.reset
        }
    );
    assert_eq!(saved(&owner_path)?["personal"]["unwitnessed"], true);
    assert_eq!(owner.titles().await, ["covered"]);
    assert!(laptop.sync().await.is_err());
    assert!(
        matches!(laptop.session.status().await?.authority.verified_revocation.map(|v| v.reason),
        Some(rocky_sdk::RevocationReason::Reset { reset }) if reset == receipt.reset)
    );
    let next = owner
        .session
        .sign_out_personal_devices(prf(), Default::default())
        .await?;
    assert_eq!(next.follows, [receipt.reset].into());
    // The passkey still authorizes a fresh node after the reset.
    let fresh = Device::new(
        sign_in(
            &directory.path().join("fresh"),
            owned(&hub, "fresh"),
            prf(),
            namespace(),
        )
        .await?
        .opened,
    )?;
    fresh.add(3, "new sign-in").await;
    fresh.close().await;
    laptop.close().await;
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_offline_revoke_keeps_pending_coverage_and_retries_after_restart() -> Result<(), Failure>
{
    let directory = tempfile::tempdir()?;
    let hub = hub();
    let owner_path = directory.path().join("owner");
    let owner = create(&owner_path, &hub, "owner").await?;
    owner.session.set_automatic_sync(false).await?;
    let phone = Device::new(
        sign_in(
            &directory.path().join("phone"),
            owned(&hub, "phone"),
            prf(),
            namespace(),
        )
        .await?
        .opened,
    )?;
    phone.session.set_automatic_sync(false).await?;
    common::introduce(&owner, &phone).await;
    let target = rocky_sdk::PeerId::of(phone.session.info().await?.node);
    phone.session.set_networking_enabled(false).await?;
    let receipt = owner.session.revoke_personal_device(target, prf()).await?;
    assert!(receipt.local && receipt.pending.contains(&target));
    assert!(!receipt.delivered.contains(&target));
    let epochs = saved(&owner_path)?["keys"].clone();
    owner.close().await;
    let owner = Device::new(
        config(&owner_path)?
            .network(Network::Owned(hub.connector("owner")))
            .open()
            .await?,
    )?;
    owner.session.set_automatic_sync(false).await?;
    assert_eq!(
        owner.session.device_revocation_receipt(target).await?,
        Some(receipt.clone())
    );
    phone.session.set_networking_enabled(true).await?;
    owner.sync().await.map_err(|e| e.to_string())?;
    let delivered = owner
        .session
        .device_revocation_receipt(target)
        .await?
        .ok_or("receipt")?;
    assert_eq!(delivered.statement, receipt.statement);
    assert!(delivered.local && delivered.delivered.contains(&target));
    assert!(!delivered.pending.contains(&target));
    assert_eq!(saved(&owner_path)?["keys"], epochs);
    assert!(matches!(
        phone
            .session
            .status()
            .await?
            .authority
            .verified_revocation
            .map(|v| v.reason),
        Some(rocky_sdk::RevocationReason::Node)
    ));
    phone.close().await;
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_self_unlink_retains_its_cause_and_receipt_after_restart() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let hub = hub();
    let owner_path = directory.path().join("owner");
    let owner = create(&owner_path, &hub, "owner").await?;
    let target = rocky_sdk::PeerId::of(owner.session.info().await?.node);
    let receipt = owner.session.revoke_personal_device(target, prf()).await?;
    assert!(receipt.local && receipt.pending.is_empty());
    assert!(matches!(
        owner
            .session
            .status()
            .await?
            .authority
            .verified_revocation
            .map(|v| v.reason),
        Some(rocky_sdk::RevocationReason::Node)
    ));
    assert!(owner.session.execute("BEGIN", Vec::new()).await.is_err());
    assert_eq!(
        owner.session.revoke_personal_device(target, prf()).await?,
        receipt
    );
    owner.close().await;
    let owner = Device::new(config(&owner_path)?.open().await?)?;
    assert_eq!(
        owner.session.device_revocation_receipt(target).await?,
        Some(receipt)
    );
    assert!(matches!(
        owner
            .session
            .status()
            .await?
            .authority
            .verified_revocation
            .map(|v| v.reason),
        Some(rocky_sdk::RevocationReason::Node)
    ));
    assert!(owner.session.execute("BEGIN", Vec::new()).await.is_err());
    owner.close().await;
    Ok(())
}
