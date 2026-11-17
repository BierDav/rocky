use super::*;

fn account() -> PersonalAccount {
    let namespace = rocky_credentials::PersonalNamespace::new("notes.example", "account root")
        .expect("namespace");
    let input = namespace.prf_input().expect("input");
    PersonalAccount::derive(crate::VerifiedPasskeyPrf::new(&input, [4; 32]), &namespace)
        .expect("account")
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn only_an_owner_device_no_statement_ended_anchors() -> Result<(), Failure> {
    let account = account();
    let locator = account.locator();
    let scope = locator.scope("notes");
    let grant = rocky_credentials::Authority::owner(account.root()).issue_trusted(
        locator.owner,
        scope,
        1,
    )?;
    let node = PrivateKey::new();
    let chain = CredentialChain(vec![grant.clone()]).certify_personal(
        &account.identity(),
        &node,
        (locator.owner, [1; 32]),
    )?;
    let leaf = chain.certificate().ok_or("certificate")?;
    assert!(anchors(leaf, locator.owner, &RevocationSet::new())?);
    let tombstone = chain.unlink(&account.identity(), scope.app, 2, node.public_key())?;
    let ended = RevocationSet::try_from(vec![tombstone])?;
    assert!(
        !anchors(leaf, locator.owner, &ended)?,
        "the responder's own set carries the tombstone that ended it"
    );
    assert!(
        !anchors(&grant, locator.owner, &RevocationSet::new())?,
        "the root's own grant is no owner device"
    );
    Ok(())
}

#[cfg(not(target_family = "wasm"))]
mod installs {
    use super::account;
    use crate::{
        Affinity, Column, Configuration, Database, Failure, Freshness, Network, PersonalOpened,
        PersonalSignInError, Schema, SignInSource, Table, VerifiedBackup, params,
        personal::fault::{CRASHES, Stage},
        transport::memory::Hub,
    };
    use rocky_platform::fs::Scratch;
    use std::{
        path::Path,
        sync::{Arc, atomic::Ordering},
    };

    fn schema() -> Result<Schema, Failure> {
        Schema::builder("notes", 1)
            .table(Table::replayed("notes").column(Column::new("body", Affinity::Text).required()))
            .build()
    }
    fn forged() -> Result<Schema, Failure> {
        Schema::builder("notes", 1)
            .table(
                Table::replayed("notes")
                    .column(Column::new("body", Affinity::Text).required())
                    .column(Column::new("extra", Affinity::Text)),
            )
            .build()
    }
    fn prf() -> crate::VerifiedPasskeyPrf {
        let namespace = account().namespace().clone();
        let input = namespace.prf_input().expect("input");
        crate::VerifiedPasskeyPrf::new(&input, [4; 32])
    }
    fn hub() -> Arc<Hub> {
        let hub = Arc::new(Hub::default());
        hub.discovery.store(true, Ordering::SeqCst);
        hub
    }
    async fn write(database: &Database, body: &str) -> Result<(), Failure> {
        let mut tx = database.transaction().await?;
        let id = tx.next_id("notes", vec![]).await?;
        tx.execute("INSERT INTO notes(id,body) VALUES(?,?)", params![id, body])
            .await?;
        tx.commit().await?;
        Ok(())
    }
    async fn bodies(database: &Database) -> Result<Vec<String>, Failure> {
        let rows = database
            .query("SELECT body FROM notes ORDER BY body", params![])
            .await?
            .collect()
            .await?;
        Ok(rows
            .iter()
            .map(|row| row.get::<String>("body"))
            .collect::<Result<_, _>>()?)
    }
    fn known(directory: &Path) -> Result<rocky_credentials::RevocationSet, Failure> {
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(directory.join("identity.json"))?)?;
        Ok(serde_json::from_value(saved["revocations"].clone())?)
    }
    fn config(directory: &Path, network: Option<&Arc<Hub>>) -> Result<Configuration, Failure> {
        let name = directory
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("name")?;
        let config = Configuration::new(directory, &schema()?)?;
        Ok(match network {
            Some(hub) => config.network(Network::Owned(hub.connector(name))),
            None => config,
        })
    }
    async fn sign_in(
        directory: &Path,
        network: Option<&Arc<Hub>>,
        source: SignInSource,
    ) -> Result<PersonalOpened, PersonalSignInError> {
        config(directory, network)
            .map_err(PersonalSignInError::Failed)?
            .sign_in(prf(), account().namespace().clone(), source)
            .await
    }
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
    fn crash(directory: &Path, stage: Stage) {
        CRASHES
            .lock()
            .expect("faults")
            .push((directory.to_path_buf(), stage));
    }
    async fn crashed_then_resumed(
        directory: &Path,
        hub: Option<&Arc<Hub>>,
        stage: Stage,
        source: impl Fn() -> Result<SignInSource, Failure>,
    ) -> Result<PersonalOpened, Failure> {
        crash(directory, stage);
        let error = match sign_in(directory, hub, source()?).await {
            Ok(_) => return Err(format!("{stage:?}: signed in past the crash").into()),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("injected crash"), "{stage:?}: {error}");
        assert!(
            directory.join("personal.intent").exists(),
            "{stage:?}: the crash lost the intent"
        );
        if stage != Stage::Saved {
            assert!(!directory.join("identity.json").exists(), "{stage:?}");
            let refused = config(directory, hub)?.open().await.err();
            let refused = refused
                .ok_or("opened a partial install before its identity was saved")?
                .to_string();
            assert!(refused.contains("unfinished"), "{stage:?}: {refused}");
        }
        let personal = sign_in(directory, hub, source()?).await?;
        personal.opened.database.session().info().await?;
        assert!(
            !directory.join("personal.intent").exists(),
            "{stage:?}: the database thread ends the intent before it answers a call"
        );
        assert!(
            !CRASHES
                .lock()
                .map_err(|_| "faults")?
                .iter()
                .any(|(at, _)| at == directory),
            "{stage:?}: the crash was never reached"
        );
        Ok(personal)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_discovered_sign_in_crashed_at_any_stage_resumes_as_one_device() -> Result<(), Failure>
    {
        let root = Scratch::new().await;
        let hub = hub();
        let owner_path = root.path().join("owner");
        let owner = config(&owner_path, Some(&hub))?
            .create_personal(prf(), account().namespace().clone())
            .await?
            .opened
            .database;
        write(&owner, "from owner").await?;
        announced(&hub, "owner").await;
        let owner_key = account().locator().owner;
        for stage in [
            Stage::Prepared,
            Stage::Pinned,
            Stage::Importing,
            Stage::Certified,
            Stage::Published,
            Stage::Saved,
        ] {
            let directory = root.path().join(format!("{stage:?}").to_lowercase());
            let personal =
                crashed_then_resumed(&directory, Some(&hub), stage, || Ok(SignInSource::Discover))
                    .await?;
            assert!(
                personal.restored.complete(),
                "{stage:?}: {:?}",
                personal.restored
            );
            assert_eq!(personal.freshness, Freshness::LocallyAnchored, "{stage:?}");
            let device = personal.opened.database;
            let node = device.session().info().await?.node;
            let held = known(&owner_path)?;
            assert!(
                held.personal_devices(owner_key)?.contains(&node),
                "{stage:?}"
            );
            assert_eq!(
                held.certificates()
                    .filter(|certificate| certificate.capability.subject == node)
                    .count(),
                1,
                "{stage:?}: one certificate, however often it was published"
            );
            device.session().sync_once().await?;
            assert_eq!(bodies(&device).await?, ["from owner"], "{stage:?}");
            assert_eq!(
                device.session().freshness().await?,
                Freshness::LocallyAnchored
            );
            device.close().await?;
        }
        owner.close().await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_backup_sign_in_crashed_at_any_stage_resumes_as_one_device() -> Result<(), Failure> {
        let root = Scratch::new().await;
        let owner_path = root.path().join("owner");
        let owner = config(&owner_path, None)?
            .create_personal(prf(), account().namespace().clone())
            .await?
            .opened
            .database;
        write(&owner, "backed up").await?;
        let archive = root.path().join("owner.backup");
        owner.backup(&archive).await?;
        owner.close().await?;
        for stage in [
            Stage::Prepared,
            Stage::Pinned,
            Stage::Importing,
            Stage::Certified,
            Stage::Placed,
            Stage::Saved,
        ] {
            let directory = root.path().join(format!("{stage:?}").to_lowercase());
            let personal = crashed_then_resumed(&directory, None, stage, || {
                Ok(SignInSource::Backup(VerifiedBackup::open(&archive)?))
            })
            .await?;
            assert!(
                personal.restored.complete(),
                "{stage:?}: {:?}",
                personal.restored
            );
            assert_eq!(personal.freshness, Freshness::Unwitnessed, "{stage:?}");
            let device = personal.opened.database;
            let node = device.session().info().await?.node;
            let held = known(&directory)?;
            assert!(
                held.personal_devices(account().locator().owner)?
                    .contains(&node),
                "{stage:?}"
            );
            assert_eq!(
                held.certificates()
                    .filter(|certificate| certificate.capability.subject == node)
                    .count(),
                1,
                "{stage:?}"
            );
            assert_eq!(bodies(&device).await?, ["backed up"], "{stage:?}");
            assert_eq!(device.session().freshness().await?, Freshness::Unwitnessed);
            device.close().await?;
            assert!(!directory.join(super::STAGING).exists(), "{stage:?}");
        }
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_saved_backup_install_reopens_unwitnessed_without_resuming_sign_in()
    -> Result<(), Failure> {
        let root = Scratch::new().await;
        let owner = config(&root.path().join("owner"), None)?
            .create_personal(prf(), account().namespace().clone())
            .await?
            .opened
            .database;
        write(&owner, "backed up").await?;
        let archive = root.path().join("owner.backup");
        owner.backup(&archive).await?;
        owner.close().await?;
        let directory = root.path().join("restored");
        crash(&directory, Stage::Saved);
        let error = sign_in(
            &directory,
            None,
            SignInSource::Backup(VerifiedBackup::open(&archive)?),
        )
        .await
        .err()
        .ok_or("signed in past the crash")?;
        assert!(error.to_string().contains("injected crash"));
        let saved = crate::enrollment::Settings::load(&directory.join("identity.json"))?;
        assert!(saved.personal.ok_or("device")?.unwitnessed);
        let database = config(&directory, None)?.open().await?.database;
        assert_eq!(
            database.session().freshness().await?,
            Freshness::Unwitnessed
        );
        assert_eq!(bodies(&database).await?, ["backed up"]);
        database.close().await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_source_refused_while_staging_leaves_the_install_for_the_next() -> Result<(), Failure>
    {
        use crate::node::FORGED_SCHEMA;
        let root = Scratch::new().await;
        let hub = hub();
        let owner_path = root.path().join("owner");
        let owner = config(&owner_path, Some(&hub))?
            .create_personal(prf(), account().namespace().clone())
            .await?
            .opened
            .database;
        write(&owner, "from owner").await?;
        announced(&hub, "owner").await;
        let forge = || -> Result<(), Failure> {
            FORGED_SCHEMA
                .lock()
                .map_err(|_| "forged")?
                .push((owner_path.join("identity.json"), forged()?));
            Ok(())
        };
        forge()?;
        let laptop = root.path().join("laptop");
        let error = match sign_in(&laptop, Some(&hub), SignInSource::Discover).await {
            Ok(_) => return Err("installed a forged schema".into()),
            Err(error) => error,
        };
        assert!(matches!(error, PersonalSignInError::Failed(_)), "{error}");
        let intent = crate::personal::Intent::load(&laptop)?.ok_or("intent")?;
        assert_eq!(
            intent.stage,
            crate::personal::Stage::Prepared,
            "the authenticated genesis refuses a forged schema while staging"
        );
        assert!(
            intent.secrets.is_empty(),
            "an install refused while staging keeps nothing"
        );

        let honest_phone = sign_in(
            &root.path().join("phone"),
            Some(&hub),
            SignInSource::Discover,
        )
        .await?
        .opened
        .database;
        announced(&hub, "phone").await;
        forge()?;
        let personal = sign_in(&laptop, Some(&hub), SignInSource::Discover).await?;
        assert!(
            FORGED_SCHEMA.lock().map_err(|_| "forged")?.is_empty(),
            "the lying source was never asked"
        );
        assert!(personal.restored.complete(), "{:?}", personal.restored);
        let device = personal.opened.database;
        let node = device.session().info().await?.node;
        assert!(
            known(&root.path().join("phone"))?
                .personal_devices(account().locator().owner)?
                .contains(&node),
            "the honest phone's answer installs although the lying owner answers first"
        );
        device.session().sync_once().await?;
        assert_eq!(bodies(&device).await?, ["from owner"]);
        device.close().await?;
        honest_phone.close().await?;
        owner.close().await
    }
}
