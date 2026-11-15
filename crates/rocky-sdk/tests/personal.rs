//! A personal database pins its owner and namespace in the root-signed
//! genesis; owner devices hold independent certificates. A verified passkey
//! PRF names the account; only device keys are stored.
#![cfg(not(target_family = "wasm"))]
use rocky_sdk::{
    CallFailure, Configuration, Freshness, GrantProfile, IdentityProvider, Network, Open,
    PersonalAccount, PersonalNamespace, PersonalSignInError, RootKey, Schema, SecretStore,
    VerifiedPasskeyPrf,
    transport::{BoxFuture, Connected, Connector, memory::Hub},
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

mod common;
use common::{Device, Failure};

fn namespace() -> PersonalNamespace {
    PersonalNamespace::new("shopping.example", "account root").expect("namespace")
}
/// The PRF the fixture's passkey returns for `namespace`.
fn prf() -> VerifiedPasskeyPrf {
    VerifiedPasskeyPrf::new(&namespace().prf_input().expect("input"), [3; 32])
}
fn root() -> RootKey {
    RootKey::from_secret([3; 32])
}
/// A stand-in authenticator: credential `credential`'s PRF over the
/// namespace's evaluation input.
fn passkey(credential: u8, namespace: &PersonalNamespace) -> VerifiedPasskeyPrf {
    let input = namespace.prf_input().expect("input");
    let output = rocky_core::commitment(
        "fixture.passkey-prf",
        &[[credential; 32].as_slice(), input.as_bytes()].concat(),
    );
    VerifiedPasskeyPrf::new(&input, output)
}
fn config(directory: &Path) -> Result<Configuration, Failure> {
    Configuration::new(directory, &Schema::parse(common::SCHEMA)?)
}
async fn create(directory: &Path, hub: &Arc<Hub>) -> Result<Device, Failure> {
    let personal = config(directory)?
        .network(Network::Owned(hub.connector("owner")))
        .create_personal(prf(), namespace())
        .await?;
    Device::new(personal.opened)
}
fn open<'a>(directory: &'a Path, hub: &Arc<Hub>, name: &str) -> Open<'a> {
    Open {
        identity: Some(Arc::new(root().identity())),
        directory,
        schema: common::SCHEMA,
        secrets: None,
        network: Some(Network::Owned(hub.connector(name))),
        coordination: None,
    }
}
fn saved(directory: &Path) -> Result<Value, Failure> {
    Ok(serde_json::from_slice(&std::fs::read(
        directory.join("identity.json"),
    )?)?)
}
fn code(error: &Failure) -> String {
    CallFailure::from_failure(error).code().to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn owner_devices_link_independently_and_merge_without_forks() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let owner = create(&directory.path().join("owner"), &hub).await?;
    let identity = root().identity().public_key();
    let scope = *owner.scope();
    assert_eq!(scope.mesh, namespace().mesh(identity)?);
    assert_eq!(scope.domain, namespace().domain(identity)?);
    assert_eq!(owner.identity().await, identity.to_hex());

    // Each sibling still confirms both codes; no request flag skips them.
    let contact = owner.contact().await;
    let mut siblings = Vec::new();
    for name in ["laptop", "phone"] {
        let path = directory.path().join(name);
        let opened = common::link(open(&path, &hub, name), &contact, owner.session()).await?;
        let chain: rocky_credentials::CredentialChain =
            serde_json::from_value(saved(&path)?["credentials"].clone())?;
        let certificate = chain.certificate().ok_or("certificate")?;
        assert_eq!(certificate.membership()?, None);
        assert!(certificate.capability.extended()?.personal.is_some());
        siblings.push(Device::new(opened)?);
    }
    // Both write before they ever meet each other.
    siblings[0].add(1, "from laptop").await;
    siblings[1].add(2, "from phone").await;
    owner.add(3, "from owner").await;
    common::introduce(&siblings[0], &siblings[1]).await;
    for _ in 0..2 {
        for device in siblings.iter().chain([&owner]) {
            device.sync().await.map_err(|error| error.to_string())?;
        }
    }
    for (device, name) in siblings
        .iter()
        .zip(["laptop", "phone"])
        .chain([(&owner, "owner")])
    {
        assert_eq!(
            device.titles().await,
            ["from laptop", "from owner", "from phone"]
        );
        let known: rocky_credentials::RevocationSet =
            serde_json::from_value(saved(&directory.path().join(name))?["revocations"].clone())?;
        assert!(!known.has_revocations());
        let membership = known.membership()?;
        assert!(!membership.is_forked(identity));
        assert_eq!(membership.devices(identity).count(), 3);
        let status = device.session.status().await?;
        assert!(status.authority.verified_revocation.is_none());
    }
    for device in siblings.iter().chain([&owner]) {
        device.close().await;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_personal_database_admits_only_custody_principals() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let owner = create(&directory.path().join("owner"), &hub).await?;
    let session = owner.session();
    let scopes = ["items".to_owned()].into();
    for profile in [
        None,
        Some(GrantProfile::Reader {
            scopes: Clone::clone(&scopes),
        }),
        Some(GrantProfile::Writer {
            scopes: Clone::clone(&scopes),
        }),
        Some(GrantProfile::Issuer),
    ] {
        let error = session.invite(profile, None).await.expect_err("refused");
        assert_eq!(code(&error), "personal_owner_only");
    }
    let stranger = rocky_sdk::IdentityKey::generate().public_key().to_hex();
    let root: Arc<dyn IdentityProvider> = Arc::new(root());
    let error = session.trust(&stranger, root).await.expect_err("refused");
    assert_eq!(code(&error), "personal_owner_only");
    let error = session
        .grant_claims(&stranger, None, 1)
        .await
        .expect_err("refused");
    assert_eq!(code(&error), "personal_owner_only");
    // Partition grants and delegations name no one but the owner either.
    let device = rocky_sdk::NodeKeys {
        identity: rocky_sdk::IdentityKey::generate().public_key(),
        node: rocky_sdk::IdentityKey::generate().public_key(),
    };
    let Err(error) = session.grant_partitions(device, Vec::new(), 1).await else {
        return Err("a personal database issued partitions to another identity".into());
    };
    assert_eq!(code(&error), "personal_owner_only");
    let Err(error) = session.delegate_partitions(device, Vec::new(), 1).await else {
        return Err("a personal database issued partitions to another identity".into());
    };
    assert_eq!(code(&error), "personal_owner_only");
    session
        .invitation_with(GrantProfile::Custody { scopes })
        .await?;
    owner.close().await;

    // An explicit identity cannot replace the root-derived owner.
    let other = directory.path().join("other");
    let Err(error) = Configuration::new(&other, &Schema::parse(common::SCHEMA)?)?
        .identity(Arc::new(rocky_sdk::IdentityKey::generate()))
        .create_personal(passkey(9, &namespace()), namespace())
        .await
    else {
        return Err("an explicit identity created a personal database".into());
    };
    assert_eq!(error.code(), "wrong_owner");
    assert!(!other.join("identity.json").exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn recovering_a_personal_archive_keeps_its_owner() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let path = directory.path().join("owner");
    let owner = create(&path, &hub).await?;
    owner.add(1, "kept").await;
    let archive = directory.path().join("owner.backup");
    owner.database.backup(&archive).await?;
    owner.close().await;
    std::fs::remove_dir_all(&path)?;

    // Another identity never recovers it.
    let other = directory.path().join("other");
    let Err(error) = Configuration::new(&other, &Schema::parse(common::SCHEMA)?)?
        .identity(Arc::new(rocky_sdk::IdentityKey::generate()))
        .recover(&archive, &root())
        .await
    else {
        return Err("another identity recovered a personal database".into());
    };
    assert_eq!(code(&error), "wrong_owner");

    let recovered = directory.path().join("recovered");
    let opened = Configuration::new(&recovered, &Schema::parse(common::SCHEMA)?)?
        .network(Network::Owned(hub.connector("recovered")))
        .recover(&archive, &root())
        .await?;
    let device = Device::new(opened)?;
    let identity = root().identity().public_key();
    assert_eq!(device.identity().await, identity.to_hex());
    let chain: rocky_credentials::CredentialChain =
        serde_json::from_value(saved(&recovered)?["credentials"].clone())?;
    let certificate = chain.certificate().ok_or("certificate")?;
    assert_eq!(certificate.issuer, identity);
    assert!(certificate.capability.extended()?.personal.is_some());
    device.add(2, "recovered").await;
    assert_eq!(device.titles().await, ["kept", "recovered"]);
    device.close().await;
    Ok(())
}

/// Every secret written, by name; reads and deletes pass through.
#[derive(Default)]
struct Recording {
    secrets: Mutex<BTreeMap<String, Vec<u8>>>,
    writes: AtomicUsize,
    /// Names ending in this fail to store, once.
    fail: Mutex<Option<String>>,
}
impl Recording {
    fn values(&self) -> BTreeMap<String, Vec<u8>> {
        self.secrets.lock().expect("secrets").clone()
    }
}
impl SecretStore for Recording {
    fn load(&self, name: &str) -> Result<Option<Vec<u8>>, rocky_sdk::credentials::ProviderError> {
        Ok(self.secrets.lock().expect("secrets").get(name).cloned())
    }
    fn store(
        &self,
        name: &str,
        secret: &[u8],
    ) -> Result<(), rocky_sdk::credentials::ProviderError> {
        let mut fail = self.fail.lock().expect("fail");
        if fail.as_deref().is_some_and(|suffix| name.ends_with(suffix)) {
            *fail = None;
            return Err("injected secret store failure".into());
        }
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.secrets
            .lock()
            .expect("secrets")
            .insert(name.into(), secret.to_vec());
        Ok(())
    }
    fn delete(&self, name: &str) -> Result<(), rocky_sdk::credentials::ProviderError> {
        self.secrets.lock().expect("secrets").remove(name);
        Ok(())
    }
}
/// A connector that counts every transport it is asked to start.
struct Counting {
    inner: Arc<dyn Connector>,
    starts: AtomicUsize,
}
impl Connector for Counting {
    fn start(&self, accepts: bool, secret: [u8; 32]) -> BoxFuture<'_, Result<Connected, Failure>> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        self.inner.start(accepts, secret)
    }
}
/// Every file under `directory`, with its bytes.
fn files(directory: &Path) -> Result<BTreeMap<std::path::PathBuf, Vec<u8>>, Failure> {
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
fn account(credential: u8, namespace: &PersonalNamespace) -> PersonalAccount {
    PersonalAccount::derive(passkey(credential, namespace), namespace).expect("account")
}
fn refusal<T>(result: Result<T, PersonalSignInError>) -> PersonalSignInError {
    match result {
        Ok(_) => panic!("the personal create succeeded"),
        Err(error) => error,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_passkey_restores_the_same_account_on_an_empty_device() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let mut scopes = Vec::new();
    for (name, credential, purpose) in [
        ("first", 1, "account root"),
        // Another empty device with the same passkey.
        ("again", 1, "account root"),
        ("other-namespace", 1, "other root"),
        ("other-credential", 2, "account root"),
    ] {
        let namespace = PersonalNamespace::new("shopping.example", purpose)?;
        let expected = account(credential, &namespace).locator();
        let path = directory.path().join(name);
        let personal = config(&path)?
            .create_personal(passkey(credential, &namespace), namespace.clone())
            .await?;
        assert_eq!(personal.freshness, Freshness::LocallyAnchored);
        assert!(personal.restored.complete());
        assert!(personal.restored.readers.contains_key("items"));
        assert!(
            personal.opened.root.is_none(),
            "the root is never handed out"
        );
        let device = Device::new(personal.opened)?;
        assert_eq!(*device.scope(), expected.scope("shopping"));
        assert_eq!(device.identity().await, expected.owner.to_hex());
        let saved = saved(&path)?;
        assert_eq!(saved["publisher"], Value::from(expected.publisher.to_hex()));
        assert_eq!(saved["identity"], Value::from(expected.owner.to_hex()));
        device.close().await;
        scopes.push((*device.scope(), expected.publisher, saved["keys"].clone()));
    }
    assert_eq!(scopes[0].0, scopes[1].0);
    assert_eq!(scopes[0].1, scopes[1].1);
    // Data keys are random, never derived from the passkey.
    assert_ne!(scopes[0].2, scopes[1].2);
    for other in &scopes[2..] {
        assert_ne!(other.0.mesh, scopes[0].0.mesh);
        assert_ne!(other.0.domain, scopes[0].0.domain);
        assert_ne!(other.1, scopes[0].1);
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn only_device_keys_and_the_discovery_capability_are_stored() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("device");
    let store = Arc::new(Recording::default());
    let expected = account(1, &namespace());
    let forbidden = [expected.root().secret(), expected.identity().secret()];
    let personal = config(&path)?
        .secrets(store.clone())
        .create_personal(passkey(1, &namespace()), namespace())
        .await?;
    let device = Device::new(personal.opened)?;
    device.add(1, "kept").await;
    device.close().await;

    let stored = store.values();
    let installation = saved(&path)?["installation"]
        .as_str()
        .ok_or("installation")?
        .to_owned();
    let named = |role: &str| stored.get(&format!("rocky.{installation}.{role}"));
    assert_eq!(
        named("personal-discovery").map(Vec::as_slice),
        Some(expected.discovery().topic().as_slice())
    );
    assert!(named("node").is_some() && named("transport").is_some());
    let records = files(&path)?;
    assert!(!records.keys().any(|file| file.ends_with("personal.intent")));
    let mut written: Vec<(String, &Vec<u8>)> = stored
        .iter()
        .map(|(name, bytes)| (name.clone(), bytes))
        .collect();
    written.extend(
        records
            .iter()
            .map(|(file, bytes)| (file.display().to_string(), bytes)),
    );
    for (name, bytes) in written {
        for secret in &forbidden {
            let hex = data_encoding::HEXLOWER.encode(secret);
            let array = secret
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let text = String::from_utf8_lossy(bytes);
            assert!(
                !bytes.windows(32).any(|window| window == secret)
                    && !text.contains(&hex)
                    && !text.contains(&array),
                "{name} holds a passkey secret"
            );
        }
    }
    // Reopening needs no passkey: the node keys alone.
    let reopened = Device::new(config(&path)?.secrets(store.clone()).open().await?)?;
    assert_eq!(reopened.titles().await, ["kept"]);
    reopened.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unsupported_prf_writes_and_sends_nothing() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("device");
    let store = Arc::new(Recording::default());
    let hub = Arc::new(Hub::default());
    let connector = Arc::new(Counting {
        inner: hub.connector("device"),
        starts: AtomicUsize::new(0),
    });
    let input = namespace().prf_input()?;
    let other = PersonalNamespace::new("shopping.example", "other root")?;
    for prf in [
        // Evaluated for another namespace, or no PRF output at all.
        passkey(1, &other),
        VerifiedPasskeyPrf::new(&input, [0; 32]),
        VerifiedPasskeyPrf::new(&input, *input.as_bytes()),
    ] {
        let error = refusal(
            config(&path)?
                .secrets(store.clone())
                .network(Network::Owned(connector.clone()))
                .create_personal(prf, namespace())
                .await,
        );
        assert!(
            matches!(error, PersonalSignInError::PrfUnsupported),
            "{error}"
        );
        assert_eq!(error.code(), "prf_unsupported");
    }
    assert!(!path.exists());
    assert_eq!(store.writes.load(Ordering::SeqCst), 0);
    assert_eq!(connector.starts.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn another_account_never_touches_the_local_database() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("device");
    let store = Arc::new(Recording::default());
    let personal = config(&path)?
        .secrets(store.clone())
        .create_personal(passkey(1, &namespace()), namespace())
        .await?;
    let device = Device::new(personal.opened)?;
    device.add(1, "mine").await;
    device.close().await;
    let (before, secrets) = (files(&path)?, store.values());

    let other = PersonalNamespace::new("shopping.example", "other root")?;
    for (prf, namespace, expected) in [
        (passkey(2, &namespace()), namespace(), "wrong_local_account"),
        (passkey(1, &other), other.clone(), "wrong_local_account"),
        // The same account: an explicit first create refuses it as well.
        (passkey(1, &namespace()), namespace(), "existing_state"),
    ] {
        let error = refusal(
            config(&path)?
                .secrets(store.clone())
                .create_personal(prf, namespace)
                .await,
        );
        assert_eq!(error.code(), expected, "{error}");
        assert_eq!(files(&path)?, before);
        assert_eq!(store.values(), secrets);
    }
    let reopened = Device::new(config(&path)?.secrets(store).open().await?)?;
    assert_eq!(reopened.titles().await, ["mine"]);
    reopened.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_older_identity_format_is_refused() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("device");
    let personal = config(&path)?
        .create_personal(passkey(1, &namespace()), namespace())
        .await?;
    personal.opened.database.close().await?;
    let mut identity = saved(&path)?;
    identity["version"] = Value::from(33);
    std::fs::write(path.join("identity.json"), serde_json::to_vec(&identity)?)?;
    let before = files(&path)?;
    let error = refusal(
        config(&path)?
            .create_personal(passkey(1, &namespace()), namespace())
            .await,
    );
    assert!(
        matches!(error, PersonalSignInError::UnsupportedFormat),
        "{error}"
    );
    let Err(error) = config(&path)?.open().await else {
        return Err("an older identity format opened".into());
    };
    assert!(
        error.to_string().contains("unsupported identity version"),
        "{error}"
    );
    assert_eq!(files(&path)?, before);
    Ok(())
}
