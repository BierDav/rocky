#![cfg(not(target_family = "wasm"))]
use super::*;
use crate::{Affinity, Column, Configuration, Schema, Table, VerifiedPasskeyPrf};
use std::sync::Mutex;

#[derive(Default)]
struct Store {
    secrets: Mutex<BTreeMap<String, Vec<u8>>>,
    fail: Mutex<Option<&'static str>>,
}
impl SecretStore for Store {
    fn load(&self, name: &str) -> Result<Option<Vec<u8>>, rocky_credentials::ProviderError> {
        Ok(self.secrets.lock().unwrap().get(name).cloned())
    }
    fn store(&self, name: &str, secret: &[u8]) -> Result<(), rocky_credentials::ProviderError> {
        let mut fail = self.fail.lock().unwrap();
        if fail.is_some_and(|suffix| name.ends_with(suffix)) {
            *fail = None;
            return Err("injected secret store failure".into());
        }
        self.secrets
            .lock()
            .unwrap()
            .insert(name.into(), secret.to_vec());
        Ok(())
    }
    fn delete(&self, name: &str) -> Result<(), rocky_credentials::ProviderError> {
        self.secrets.lock().unwrap().remove(name);
        Ok(())
    }
}
fn namespace() -> PersonalNamespace {
    PersonalNamespace::new("notes.example", "account root").unwrap()
}
fn prf(credential: u8) -> VerifiedPasskeyPrf {
    let input = namespace().prf_input().unwrap();
    VerifiedPasskeyPrf::new(&input, [credential; 32])
}
fn schema(column: &str) -> Schema {
    Schema::builder("notes", 1)
        .table(Table::replayed("notes").column(Column::new(column, Affinity::Text)))
        .build()
        .unwrap()
}
fn configuration(directory: &Path, store: &Arc<Store>, column: &str) -> Configuration {
    Configuration::new(directory, &schema(column))
        .unwrap()
        .secrets(store.clone())
}

#[tokio::test]
async fn an_interrupted_create_resumes_under_the_same_node_and_publication() -> Result<(), Failure>
{
    let directory = rocky_platform::fs::Scratch::new().await;
    let store = Arc::new(Store::default());
    let stored_after_epochs_and_genesis_are_pinned = ".personal-discovery";
    *store.fail.lock().unwrap() = Some(stored_after_epochs_and_genesis_are_pinned);
    let config = configuration(directory.path(), &store, "title");
    let Err(error) = config.create_personal(prf(1), namespace()).await else {
        return Err("the injected failure did not interrupt the create".into());
    };
    assert!(matches!(error, PersonalSignInError::Failed(_)), "{error}");
    let interrupted = Intent::load(directory.path())?.ok_or("the intent is kept")?;
    let Stage::Pinned { epochs, .. } = &interrupted.stage else {
        return Err("the interrupted create pinned nothing".into());
    };
    assert!(!directory.path().join("identity.json").exists());

    let created = config.create_personal(prf(1), namespace()).await?;
    created.opened.database.close().await?;
    let settings = Settings::load(&directory.path().join("rocky.sqlite"))?;
    assert_eq!(settings.installation, interrupted.installation);
    assert_eq!(settings.keys, *epochs);
    assert_eq!(created.restored.readers, *epochs);
    assert_eq!(
        settings.personal.map(|device| device.publication),
        Some(interrupted.publication)
    );
    let vault = settings.vault(&(store.clone() as Arc<dyn SecretStore>));
    assert_eq!(
        PrivateKey::from_bytes(&vault.get("node")?).public_key(),
        interrupted.node
    );
    let nodes = store
        .secrets
        .lock()
        .unwrap()
        .keys()
        .filter(|name| name.ends_with(".node"))
        .count();
    assert_eq!(nodes, 1, "no second device was prepared");
    assert!(
        Intent::load(directory.path())?.is_none(),
        "the finished intent is gone"
    );
    Ok(())
}

#[tokio::test]
async fn a_resumed_create_under_another_genesis_conflicts() -> Result<(), Failure> {
    let directory = rocky_platform::fs::Scratch::new().await;
    let store = Arc::new(Store::default());
    *store.fail.lock().unwrap() = Some(".personal-discovery");
    let original = configuration(directory.path(), &store, "title");
    assert!(original.create_personal(prf(1), namespace()).await.is_err());
    let intent = std::fs::read(Intent::path(directory.path()))?;

    let changed = configuration(directory.path(), &store, "body");
    let Err(error) = changed.create_personal(prf(1), namespace()).await else {
        return Err("a changed genesis resumed the create".into());
    };
    assert!(
        matches!(error, PersonalSignInError::GenesisConflict),
        "{error}"
    );
    assert_eq!(std::fs::read(Intent::path(directory.path()))?, intent);
    original
        .create_personal(prf(1), namespace())
        .await?
        .opened
        .database
        .close()
        .await?;
    Ok(())
}

#[tokio::test]
async fn another_accounts_or_formats_intent_is_refused_untouched() -> Result<(), Failure> {
    let directory = rocky_platform::fs::Scratch::new().await;
    let store = Arc::new(Store::default());
    *store.fail.lock().unwrap() = Some(".personal-discovery");
    let config = configuration(directory.path(), &store, "title");
    assert!(config.create_personal(prf(1), namespace()).await.is_err());
    let path = Intent::path(directory.path());
    let saved = std::fs::read(&path)?;
    let secrets = store.secrets.lock().unwrap().clone();

    let Err(error) = config.create_personal(prf(2), namespace()).await else {
        return Err("another account resumed the create".into());
    };
    assert!(
        matches!(error, PersonalSignInError::WrongLocalAccount),
        "{error}"
    );
    let mut older = Intent::load(directory.path())?.ok_or("intent")?;
    older.version = INTENT_VERSION + 1;
    std::fs::write(&path, rocky_credentials::encode(&older)?)?;
    let Err(error) = config.create_personal(prf(1), namespace()).await else {
        return Err("another intent format resumed the create".into());
    };
    assert!(
        matches!(error, PersonalSignInError::UnsupportedFormat),
        "{error}"
    );
    std::fs::write(&path, &saved)?;
    assert_eq!(*store.secrets.lock().unwrap(), secrets);
    config
        .create_personal(prf(1), namespace())
        .await?
        .opened
        .database
        .close()
        .await?;
    Ok(())
}

#[tokio::test]
async fn a_sign_in_intent_resumes_its_node_publication_and_certificate() -> Result<(), Failure> {
    let directory = rocky_platform::fs::Scratch::new().await;
    let store: Arc<dyn SecretStore> = Arc::new(Store::default());
    let account =
        PersonalAccount::derive(prf(1), &namespace()).map_err(|error| error.to_string())?;
    let capability =
        || rocky_credentials::DiscoveryCapability::from_secret(*account.discovery().topic());
    let (node, transport) = (PrivateKey::new(), [7; 32]);
    let mut first = Installing::sign_in(
        directory.path(),
        &store,
        (&account, capability()),
        (&node, transport),
    )?;
    assert!(!first.resumed());
    let epoch = KeyEpoch::new(1, &[5; 32]);
    let epochs = BTreeMap::from([("notes".to_owned(), vec![epoch])]);
    first.import([1; 32], epochs.clone())?;
    let locator = account.locator();
    let scope = locator.scope("notes");
    let grant = rocky_credentials::Authority::owner(account.root()).issue_trusted(
        locator.owner,
        scope,
        1,
    )?;
    let certificate = rocky_credentials::CredentialChain(vec![grant])
        .certify_personal(&account.identity(), &node, (locator.owner, [1; 32]))?
        .certificate()
        .ok_or("certificate")?
        .clone();
    first.certify(certificate.clone())?;

    let ignored_on_retry = PrivateKey::new();
    let mut again = Installing::sign_in(
        directory.path(),
        &store,
        (&account, capability()),
        (&ignored_on_retry, [8; 32]),
    )?;
    assert!(again.resumed());
    assert_eq!(again.publication(), first.publication());
    assert_eq!(again.installation(), first.installation());
    assert_eq!(again.certificate(), Some(&certificate));
    let saved = Installing::saved_keys(directory.path(), &store, &account)?.ok_or("keys")?;
    assert_eq!(saved.0.public_key(), node.public_key());
    assert_eq!(saved.1, transport);
    let later = KeyEpoch::new(2, &[6; 32]);
    again.import([1; 32], BTreeMap::from([("notes".to_owned(), vec![later])]))?;
    assert_eq!(again.certificate(), Some(&certificate));
    let Some(Intent {
        stage: Stage::Certified { epochs, .. },
        ..
    }) = Intent::load(directory.path())?
    else {
        return Err("the certified stage was not kept".into());
    };
    assert_eq!(
        epochs["notes"],
        vec![epoch, later],
        "a later import merges its epochs and keeps the certificate"
    );
    let intent = std::fs::read(Intent::path(directory.path()))?;
    let Err(error) = again.import([2; 32], epochs.clone()) else {
        return Err("another genesis was imported".into());
    };
    assert!(matches!(
        error.downcast_ref::<PersonalSignInError>(),
        Some(PersonalSignInError::GenesisConflict)
    ));
    assert_eq!(
        std::fs::read(Intent::path(directory.path()))?,
        intent,
        "another genesis conflicts and changes nothing"
    );
    let stranger =
        PersonalAccount::derive(prf(2), &namespace()).map_err(|error| error.to_string())?;
    assert!(
        Installing::saved_keys(directory.path(), &store, &stranger).is_err(),
        "another account's intent is refused"
    );
    let vault = Vault::new(store.clone(), again.installation());
    let secret = crate::groups::secret_role(&[3; 32], later);
    again.hold_secrets([([3; 32], later)])?;
    vault.put(&secret, &[9; 32])?;
    discard(directory.path(), &store)?;
    assert!(Intent::load(directory.path())?.is_none());
    assert!(Installing::saved_keys(directory.path(), &store, &account)?.is_none());
    assert!(
        vault.find("node")?.is_none(),
        "a revoked node's install goes with its keys"
    );
    assert!(
        vault.find(&secret)?.is_none(),
        "a revoked node's install goes with its partition secrets"
    );
    Ok(())
}

#[tokio::test]
async fn a_create_and_a_sign_in_never_resume_each_others_intent() -> Result<(), Failure> {
    let directory = rocky_platform::fs::Scratch::new().await;
    let store = Arc::new(Store::default());
    *store.fail.lock().unwrap() = Some(".personal-discovery");
    let config = configuration(directory.path(), &store, "title");
    assert!(config.create_personal(prf(1), namespace()).await.is_err());
    let path = Intent::path(directory.path());
    let created = std::fs::read(&path)?;
    let Err(error) = config
        .sign_in(prf(1), namespace(), crate::SignInSource::Discover)
        .await
    else {
        return Err("a sign-in resumed the create".into());
    };
    assert!(
        matches!(error, PersonalSignInError::ExistingState),
        "{error}"
    );
    assert_eq!(std::fs::read(&path)?, created);

    let other = rocky_platform::fs::Scratch::new().await;
    let account =
        PersonalAccount::derive(prf(1), &namespace()).map_err(|error| error.to_string())?;
    let secrets: Arc<dyn SecretStore> = store.clone();
    Installing::sign_in(
        other.path(),
        &secrets,
        (
            &account,
            rocky_credentials::DiscoveryCapability::from_secret(*account.discovery().topic()),
        ),
        (&PrivateKey::new(), [7; 32]),
    )?;
    let signing_in = std::fs::read(Intent::path(other.path()))?;
    let Err(error) = configuration(other.path(), &store, "title")
        .create_personal(prf(1), namespace())
        .await
    else {
        return Err("a create resumed the sign-in".into());
    };
    assert!(
        matches!(error, PersonalSignInError::ExistingState),
        "a sign-in's intent is no create's to resume: {error}"
    );
    assert_eq!(std::fs::read(Intent::path(other.path()))?, signing_in);
    assert!(!other.path().join("identity.json").exists());
    config
        .create_personal(prf(1), namespace())
        .await?
        .opened
        .database
        .close()
        .await?;
    Ok(())
}

#[test]
fn the_failing_store_meets_the_key_provider_contract() {
    rocky_credentials::testing::key_provider_contract(&Store::default());
}
