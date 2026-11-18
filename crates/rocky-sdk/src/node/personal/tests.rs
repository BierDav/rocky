use super::*;

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_flood_of_hellos_evicts_no_issued_nonce() {
    let mut nonces = Nonces::default();
    let act = PersonalAct::Bootstrap { page: 0 };
    let kept = nonces.issue(1_000);
    for _ in 0..4 * MAX_NONCES {
        nonces.issue(1_000);
    }
    nonces
        .spend(&kept, act, 1_001)
        .expect("the first nonce still spends");
    assert!(
        nonces.spend(&kept, act, 1_002).is_err(),
        "each act spends a nonce once"
    );
    nonces
        .spend(&kept, PersonalAct::Bootstrap { page: 1 }, 1_002)
        .expect("the next page");
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn only_a_live_nonce_this_responder_issued_spends() {
    let mut nonces = Nonces::default();
    let act = PersonalAct::Bootstrap { page: 0 };
    let issued = nonces.issue(1_000);
    let mut forged = issued;
    forged[31] ^= 1;
    assert!(nonces.spend(&forged, act, 1_001).is_err());
    assert!(
        Nonces::default().spend(&issued, act, 1_001).is_err(),
        "another process's nonce"
    );
    assert!(
        nonces.spend(&issued, act, 1_000 + NONCE_LIFETIME).is_err(),
        "a nonce presented after its lifetime"
    );
    assert!(nonces.spend(&issued, act, 999).is_err());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn spent_replay_state_is_bounded() {
    let mut nonces = Nonces::default();
    let act = PersonalAct::Bootstrap { page: 0 };
    for _ in 0..MAX_NONCES {
        let nonce = nonces.issue(1_000);
        nonces.spend(&nonce, act, 1_000).expect("spends");
    }
    let refused = nonces.issue(1_000);
    assert!(nonces.spend(&refused, act, 1_000).is_err());
    let later = nonces.issue(1_000 + NONCE_LIFETIME);
    nonces
        .spend(&later, act, 1_000 + NONCE_LIFETIME)
        .expect("room after expiry");
}

#[cfg(not(target_family = "wasm"))]
mod device_actions {
    use super::*;
    use crate::{PersonalAccount, PersonalNamespace, VerifiedPasskeyPrf};
    use rocky_credentials::testing::MemoryKeys;
    use std::path::Path;

    fn namespace() -> PersonalNamespace {
        PersonalNamespace::new("notes.example", "account root").unwrap()
    }
    fn prf() -> VerifiedPasskeyPrf {
        VerifiedPasskeyPrf::new(&namespace().prf_input().unwrap(), [1; 32])
    }
    async fn personal(
        directory: &Path,
        store: &Arc<dyn crate::SecretStore>,
    ) -> Result<Node, Failure> {
        let account = PersonalAccount::derive(prf(), &namespace())?;
        let (root, discovery) = account.into_parts();
        let installing = crate::personal::Installing::resume(
            directory,
            store,
            &root,
            (&namespace(), discovery),
        )?;
        let schema = crate::Schema::parse(
            r#"{"application":"notes","version":1,"format":6,"tables":[
          {"name":"notes","class":"replayed","columns":[{"name":"title","type":"text","not_null":true}],"primary_key":["title"]},
          {"name":"parts","class":"replayed","columns":[{"name":"list","type":"text","not_null":true}],"primary_key":["list"],"partition_by":["list"],"partition_group":"list"}]}"#,
        )?;
        let settings = Settings::create_personal(
            &schema,
            store,
            crate::enrollment::DEFAULT_TTL,
            (namespace(), installing),
            &root,
        )?;
        let vault = settings.vault(store);
        let domain = settings.domain()?;
        let replica = EncryptedReplica::open(
            directory.join("rocky.sqlite"),
            domain.app.clone(),
            settings.config(&vault, &domain)?,
        )
        .await?;
        replica
            .install_readable_tables(domain.logical().clone(), domain.scope, domain.readers())
            .await?;
        let path = directory.join("identity.json");
        settings.save(&path)?;
        let node = Node::new(replica, domain, vault, settings, path);
        node.record_self().await?;
        Ok(node)
    }
    fn certificate(
        settings: &Settings,
        key: &PrivateKey,
    ) -> Result<rocky_credentials::SignedCapability, Failure> {
        let account = PersonalAccount::derive(prf(), &namespace())?;
        let owner = account.identity();
        let chain = CredentialChain(vec![settings.owner_grant(owner.public_key())?.clone()])
            .certify_personal(
                &owner,
                key,
                (owner.public_key(), settings.manifest()?.descriptor()?),
            )?;
        chain
            .certificate()
            .cloned()
            .ok_or_else(|| "certificate".into())
    }

    #[tokio::test]
    async fn interrupted_personal_unlink_reuses_statement_reader_and_partition_epochs()
    -> Result<(), Failure> {
        use crate::enrollment::fault::{self, Stage};
        for stage in [
            Stage::Begun,
            Stage::Key,
            Stage::Signed,
            Stage::Renewed,
            Stage::Committed,
            Stage::Clearing,
        ] {
            let directory = tempfile::tempdir()?;
            let memory = Arc::new(MemoryKeys::default());
            let store: Arc<dyn crate::SecretStore> = memory.clone();
            let node = personal(directory.path(), &store).await?;
            node.create_partition(&crate::PartitionTarget::Group {
                group: "list".into(),
                values: vec![rocky_core::SqlValue::Text("a".into())],
            })
            .await?;
            let other = PrivateKey::new();
            let cert = certificate(&**node.settings.lock().await, &other)?;
            node.accept_link(cert.clone()).await?;
            let target = rocky_credentials::PeerId::of(other.public_key());
            fault::STOP.with(|stop| stop.set(Some((stage, 0))));
            assert!(
                node.revoke_personal_device(target, prf()).await.is_err(),
                "{stage:?}"
            );
            assert!(fault::STOP.with(|stop| stop.get()).is_none(), "{stage:?}");
            let saved = Settings::load(&node.path)?;
            let receipt = saved
                .device_revocations
                .get(&target)
                .ok_or("saved receipt")?
                .clone();
            assert!(!receipt.local);
            let intent = saved
                .authority
                .as_ref()
                .and_then(|a| a.rotation.as_ref())
                .ok_or("rotation")?;
            let keys: Vec<_> = intent
                .keys
                .iter()
                .map(|(name, epoch)| format!("rotation.reader.{name}.{epoch}"))
                .chain(
                    intent
                        .groups
                        .iter()
                        .map(|(group, epoch)| format!("rotation.partition.{group}.{epoch}")),
                )
                .collect();
            let made: Vec<_> = keys
                .iter()
                .map(|role| node.vault.find(role))
                .collect::<Result<_, _>>()?;
            drop(node);
            let reopened = crate::Configuration::new(directory.path(), &saved.schema)?
                .secrets(store.clone())
                .open()
                .await?;
            assert!(
                reopened
                    .database
                    .session()
                    .device_revocation_receipt(target)
                    .await?
                    .is_some_and(|r| r.local)
            );
            let resumed = reopened
                .database
                .session()
                .revoke_personal_device(target, prf())
                .await?;
            assert_eq!(resumed.statement, receipt.statement, "{stage:?}");
            assert!(resumed.local);
            let completed = Settings::load(&directory.path().join("identity.json"))?;
            assert!(
                completed
                    .authority
                    .as_ref()
                    .is_some_and(|a| a.rotation.is_none())
            );
            assert_eq!(completed.revocations.len(), 1);
            for epochs in completed.keys.values() {
                assert_eq!(epochs.len(), 2, "{stage:?}");
            }
            for group in completed.groups.values() {
                assert_eq!(group.epochs.len(), 2, "{stage:?}");
            }
            let vault = completed.vault(&store);
            for (role, made) in keys.iter().zip(made) {
                if let Some(made) = made {
                    assert_eq!(vault.get(role)?, made, "{stage:?}");
                }
            }
            let mut replay = completed.revocations.clone();
            assert!(!replay.link(cert)?);
            assert!(
                !replay
                    .personal_devices(completed.identity)?
                    .contains(&other.public_key())
            );
            assert_eq!(
                reopened
                    .database
                    .session()
                    .revoke_personal_device(target, prf())
                    .await?,
                resumed
            );
            reopened.database.close().await?;
            let account = PersonalAccount::derive(prf(), &namespace())?;
            for secret in memory.secrets().values() {
                assert_ne!(
                    secret.as_slice(),
                    account.root().secret(),
                    "root persisted at {stage:?}"
                );
                assert_ne!(
                    secret.as_slice(),
                    account.identity().secret(),
                    "identity persisted at {stage:?}"
                );
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn a_retired_device_acknowledges_only_the_policy_it_kept_durably() -> Result<(), Failure>
    {
        let directory = tempfile::tempdir()?;
        let store: Arc<dyn crate::SecretStore> = Arc::new(MemoryKeys::default());
        let node = personal(directory.path(), &store).await?;
        let own = rocky_credentials::PeerId::of(Settings::node_key(&node.vault)?.public_key());
        let receipt = node.revoke_personal_device(own, prf()).await?;
        let settings = node.settings.lock().await.clone();
        let account = PersonalAccount::derive(prf(), &namespace())?;
        let database = settings.database_context()?;
        let extra = rocky_credentials::Authority::owner(account.root()).revoke_in(
            rocky_credentials::Revocation {
                app: database.scope.app,
                issuer: account.locator().publisher,
                time: now()?,
                nodes: [rocky_credentials::PeerId::of(
                    PrivateKey::new().public_key(),
                )]
                .into(),
                identities: Default::default(),
                capabilities: Default::default(),
                cut: Vec::new(),
                mode: rocky_credentials::RevocationMode::Lost,
                recovery: None,
            },
            database,
        )?;
        let extra_id = extra.id()?;
        let ended = settings.revocation(&node.path)?.ok_or("cause")?;
        let update = RevocationSet::try_from(vec![ended, extra])?;
        let PeerReply::Unlink(reply) = node.answer_peer(PeerRequest::Unlink(update)).await? else {
            return Err("unlink reply".into());
        };
        let committed = reply.verify(database, own)?;
        assert!(committed.get(&receipt.statement).is_some());
        assert!(committed.get(&extra_id).is_none());
        assert!(
            Settings::load(&node.path)?
                .revocations
                .get(&extra_id)
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn self_ending_unlinks_are_checked_against_the_pinned_personal_genesis()
    -> Result<(), Failure> {
        let directory = tempfile::tempdir()?;
        let store: Arc<dyn crate::SecretStore> = Arc::new(MemoryKeys::default());
        let node = personal(directory.path(), &store).await?;
        let settings = node.settings.lock().await.clone();
        let account = PersonalAccount::derive(prf(), &namespace())?;
        let owner = account.identity();
        let own = Settings::node_key(&node.vault)?;
        let grant = CredentialChain(vec![settings.owner_grant(owner.public_key())?.clone()]);
        let chains = [
            grant
                .clone()
                .certify_personal(&owner, &own, (owner.public_key(), [9; 32]))?,
            grant.certify_member(&owner, own.public_key(), 0, None)?,
        ];
        let before = std::fs::read(&node.path)?;
        let held = node.replica.revocations().await;
        for chain in chains {
            let signed = chain.unlink(&owner, node.domain().scope.app, now()?, own.public_key())?;
            let update = RevocationSet::try_from(vec![signed])?;
            assert!(node.accept_unlink(update).await.is_err());
            assert_eq!(std::fs::read(&node.path)?, before);
            assert_eq!(node.replica.revocations().await, held);
            assert!(!directory.path().join("revoked.json").exists());
        }
        Ok(())
    }

    #[tokio::test]
    async fn unlink_delivery_backs_off_each_recipient_and_skips_suspended_passes()
    -> Result<(), Failure> {
        use crate::transport::{Connector, memory::Hub};
        use rocky_platform::time::{Duration, Instant};
        tokio::task::LocalSet::new()
            .run_until(async {
                let directory = tempfile::tempdir()?;
                let store: Arc<dyn crate::SecretStore> = Arc::new(MemoryKeys::default());
                let node = Arc::new(personal(directory.path(), &store).await?);
                let hub = Arc::new(Hub::default());
                let connected = hub.connector("owner").start(true, [1; 32]).await?;
                let transport = connected.transport.clone();
                node.link(connected, true).await?;
                let lost = PrivateKey::new();
                let cert = certificate(&**node.settings.lock().await, &lost)?;
                node.accept_link(cert).await?;
                node.configure(|settings| {
                    settings.learn_found(
                        &node.vault,
                        lost.public_key(),
                        transport.kind(),
                        "lost".into(),
                        false,
                        now()?,
                    )?;
                    Ok(())
                })
                .await?;
                let target = rocky_credentials::PeerId::of(lost.public_key());
                let receipt = node.revoke_personal_device(target, prf()).await?;
                assert!(receipt.pending.contains(&target));
                assert_eq!(hub.dials_to("lost"), 1);
                tokio::time::pause();
                for retry_only in [false, true, true] {
                    node.reconciliation_pass(Some(&*transport), false, retry_only)
                        .await?;
                }
                assert_eq!(hub.dials_to("lost"), 1);

                let other = PrivateKey::new();
                node.configure(|settings| {
                    settings.learn_found(
                        &node.vault,
                        other.public_key(),
                        transport.kind(),
                        "other".into(),
                        false,
                        now()?,
                    )?;
                    settings
                        .device_revocations
                        .get_mut(&target)
                        .ok_or("receipt")?
                        .pending
                        .insert(rocky_credentials::PeerId::of(other.public_key()));
                    Ok(())
                })
                .await?;
                node.deliver_unlinks().await?;
                assert_eq!(hub.dials_to("lost"), 1);
                assert_eq!(hub.dials_to("other"), 1);
                node.foreground.store(false, Ordering::SeqCst);
                tokio::time::advance(Duration::from_secs(80)).await;
                for retry_only in [false, true] {
                    let (report, _, _) = node
                        .reconciliation_pass(Some(&*transport), false, retry_only)
                        .await?;
                    assert_eq!(report.skip, Some(crate::SkipReason::Suspended));
                }
                node.deliver_unlinks().await?;
                assert_eq!(hub.dials_to("lost"), 1);
                assert_eq!(hub.dials_to("other"), 1);
                node.foreground.store(true, Ordering::SeqCst);
                for attempt in 2..=10 {
                    node.deliver_unlinks().await?;
                    assert_eq!(hub.dials_to("lost"), attempt);
                    let deadline = node
                        .unlink_retries
                        .lock()
                        .map_err(|_| "retries")?
                        .get(&target)
                        .and_then(crate::scheduling::Retry::deadline)
                        .ok_or("deadline")?;
                    assert!(deadline > Instant::now());
                    assert!(deadline <= Instant::now() + Duration::from_secs(75));
                    node.deliver_unlinks().await?;
                    assert_eq!(hub.dials_to("lost"), attempt);
                    tokio::time::advance(deadline - Instant::now()).await;
                }
                assert!(
                    node.settings.lock().await.device_revocations[&target]
                        .pending
                        .contains(&target)
                );
                tokio::time::resume();
                node.detach(None).await;
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn a_notice_uses_the_local_clock_and_renewed_certificates_keep_its_acknowledgement()
    -> Result<(), Failure> {
        let directory = tempfile::tempdir()?;
        let store: Arc<dyn crate::SecretStore> = Arc::new(MemoryKeys::default());
        let node = personal(directory.path(), &store).await?;
        let other = PrivateKey::new();
        let cert = certificate(&**node.settings.lock().await, &other)?;
        node.accept_link(cert.clone()).await?;
        let target = rocky_credentials::PeerId::of(other.public_key());
        let first = node.personal_device_notices().await?;
        assert_eq!(first.len(), 1);
        node.personal_device_notice(target, true).await?;
        let account = PersonalAccount::derive(prf(), &namespace())?;
        let mut renewed = cert.capability.clone();
        renewed.not_before += 1;
        let renewed = rocky_credentials::SignedCapability::sign(renewed, &account.identity())?;
        let mut known = node.replica.revocations().await;
        known.link(renewed)?;
        node.replica.learn_revocations(&known).await?;
        let retained = node.personal_device_notices().await?;
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].first_observed, first[0].first_observed);
        assert!(retained[0].read && retained[0].acknowledged);
        let mut observations = BTreeMap::new();
        crate::membership::observe_trusted(
            &known,
            &[account.locator().owner],
            42,
            &mut observations,
        )?;
        assert_eq!(
            observations[&account.locator().owner][&other.public_key()].linked,
            42
        );
        assert_ne!(cert.capability.not_before, 42);
        Ok(())
    }
}
