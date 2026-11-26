use super::*;

impl Node {
    pub(super) async fn unlink(&self, target: rocky_credentials::PeerId) -> Result<(), Failure> {
        let (own, issuer) = {
            let settings = self.settings.lock().await;
            let mut known = self.replica.revocations().await;
            known.merge_in(&settings.revocations, settings.database_context()?, now()?)?;
            if let Some(certificate) = settings.credentials.certificate() {
                known.link(certificate.clone())?;
            }
            let own = known.certificates().any(|certificate| {
                certificate.issuer == settings.identity
                    && rocky_credentials::PeerId::of(certificate.capability.subject) == target
            });
            (own, settings.issuer())
        };
        if own {
            return self.unlink_own(target).await.map(drop);
        }
        let (owner, node) = self
            .membership()
            .await?
            .all()
            .find(|(_, linked)| rocky_credentials::PeerId::of(linked.node) == target)
            .map(|(owner, linked)| (owner, linked.node))
            .ok_or(crate::LinkError::UnknownDevice)?;
        if !issuer {
            return Err(Box::new(crate::Error::refused(
                crate::Right::Administer,
                crate::DenyReason::AdminRequired,
            )));
        }
        self.revoke_with(
            crate::enrollment::Revoke::Node { owner, node },
            &Root::default(),
        )
        .await?;
        self.activity
            .record(crate::activity::ActivityKind::NodeUnlinked {
                identity: owner,
                node,
            });
        Ok(())
    }
    pub(super) async fn unlink_own(
        &self,
        target: rocky_credentials::PeerId,
    ) -> Result<crate::membership::DeviceRevocationReceipt, Failure> {
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        let graph = self.replica.tip_graph(self.domain().scope).await?;
        let mut settings = self.settings.lock().await;
        let identity = settings.identity().map_err(|_| {
            crate::Error::refused(crate::Right::Sign, crate::DenyReason::IdentityRequired)
        })?;
        let mut known = settings.revocations.clone();
        known.merge_in(
            &self.replica.revocations().await,
            settings.database_context()?,
            now()?,
        )?;
        if let Some(certificate) = settings.credentials.certificate() {
            known.link(certificate.clone())?;
        }
        if let Some(ended) = self.revocation_notice.revoked(&settings).await? {
            known.merge_in(
                &RevocationSet::try_from(vec![ended])?,
                settings.database_context()?,
                now()?,
            )?;
        }
        let held = known
            .statements()
            .find(|signed| {
                !signed.root()
                    && signed.statement.issuer == settings.identity
                    && signed.statement.nodes == [target].into()
                    && signed.statement.mode == rocky_credentials::RevocationMode::Lost
                    && signed.statement.identities.is_empty()
            })
            .cloned();
        let signed = match held {
            Some(signed) => signed,
            None => {
                let certificate = known
                    .certificates()
                    .find(|certificate| {
                        certificate.issuer == settings.identity
                            && rocky_credentials::PeerId::of(certificate.capability.subject)
                                == target
                    })
                    .ok_or_else(|| refused("this identity did not link that node"))?
                    .clone();
                let node = certificate.capability.subject;
                let cut = graph.heads(&settings.identity, &node)?;
                let cut = if cut.statements.is_empty() {
                    Vec::new()
                } else {
                    vec![cut]
                };
                rocky_credentials::SignedRevocation::sign(
                    rocky_credentials::Revocation {
                        app: self.domain().manifest().app,
                        issuer: settings.identity,
                        time: now()?,
                        nodes: [target].into(),
                        identities: Default::default(),
                        capabilities: Default::default(),
                        cut,
                        mode: rocky_credentials::RevocationMode::Lost,
                    },
                    &*identity,
                    &settings.identity_links().with_certificate(certificate),
                )?
            }
        };
        drop(identity);
        let id = signed.id()?;
        known.merge_in(
            &RevocationSet::try_from(vec![signed.clone()])?,
            settings.database_context()?,
            now()?,
        )?;
        let own_peer = rocky_credentials::PeerId::of(Settings::node_key(&self.vault)?.public_key());
        let own = own_peer == target;
        let mut draft = settings.clone();
        if !own {
            draft.learn_revocations(&known)?;
        }
        let peers = draft
            .peers
            .keys()
            .copied()
            .map(rocky_credentials::PeerId::of)
            .chain([target])
            .filter(|peer| *peer != own_peer)
            .collect();
        draft.device_revocations.entry(target).or_insert(
            crate::membership::DeviceRevocationReceipt {
                node: target,
                statement: id,
                local: false,
                delivered: Default::default(),
                pending: peers,
            },
        );
        if !own {
            draft.begin_exposure_rotation(&registry)?;
        }
        if own {
            let notice = crate::enrollment::RevocationNotice::encode(&draft, signed)?;
            settings
                .commit_and_forget(
                    draft,
                    &self.replica,
                    &[(
                        crate::enrollment::AUTHORITY_REVOKED_KEY,
                        Some(notice.as_slice()),
                    )],
                )
                .await?;
            self.revocation_notice.refresh(&settings).await;
        } else {
            settings.commit(draft, &self.replica).await?;
        }
        drop(settings);
        if !own {
            loop {
                let mut settings = self.settings.lock().await;
                let mut draft = settings.clone();
                let pending = draft
                    .authority
                    .as_ref()
                    .is_some_and(|a| a.rotation.is_some());
                if !pending && !draft.begin_exposure_rotation(&registry)? {
                    break;
                }
                settings.commit(draft, &self.replica).await?;
                let keys = settings
                    .rotate(&self.vault)?
                    .ok_or("missing unlink rotation")?;
                settings.save(&self.replica).await?;
                self.apply(&mut settings, keys).await?;
                self.hold_keys(&settings, &settings.partitions(&self.domain())?)
                    .await?;
                drop(settings);
                Box::pin(self.complete_revocation()).await?;
            }
        }
        if !own {
            self.replica.learn_revocations(&known).await?;
        } else if let Some(link) = self.current() {
            link.live
                .close(rocky_secure::network::link::CloseReason::Authority);
        }
        self.configure(|settings| {
            let receipt = settings
                .device_revocations
                .get_mut(&target)
                .ok_or("missing device revocation receipt")?;
            receipt.local = true;
            Ok(())
        })
        .await?;
        self.observations.touch();
        self.wake_sync();
        self.deliver_unlinks().await?;
        self.settings
            .lock()
            .await
            .device_revocations
            .get(&target)
            .cloned()
            .ok_or_else(|| "missing device revocation receipt".into())
    }
    pub(super) async fn deliver_unlinks(&self) -> Result<(), Failure> {
        let _delivery = self.unlink_delivery.lock().await;
        if !self.online.load(Ordering::SeqCst) {
            return Ok(());
        }
        let Some(link) = self.current() else {
            return Ok(());
        };
        let snapshot = self.settings.lock().await.clone();
        let pending: std::collections::BTreeSet<_> = snapshot
            .device_revocations
            .values()
            .filter(|receipt| receipt.local)
            .flat_map(|receipt| receipt.pending.iter().copied())
            .collect();
        let mut contacts: BTreeMap<_, _> = snapshot
            .contact_refs
            .iter()
            .map(|(peer, entry)| (*peer, entry.contact.clone()))
            .chain(
                snapshot
                    .peers
                    .iter()
                    .map(|(node, contact)| (rocky_credentials::PeerId::of(*node), contact.clone())),
            )
            .collect();
        contacts.retain(|peer, contact| {
            pending.contains(peer) && !contact.addresses(&*link.transport).is_empty()
        });
        use futures_util::StreamExt;
        let mut known = snapshot.revocations.clone();
        if let Some(ended) = self.revocation_notice.revoked(&snapshot).await? {
            known.merge_in(
                &RevocationSet::try_from(vec![ended])?,
                snapshot.database_context()?,
                now()?,
            )?;
        }
        {
            let mut retries = self
                .unlink_retries
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            retries.retain(|peer, _| contacts.contains_key(peer));
            contacts.retain(|peer, _| {
                let retry = retries.entry(*peer).or_insert_with(|| {
                    crate::scheduling::Retry::seeded(self.retry_seed, peer.as_bytes())
                });
                retry.ready()
            });
        }
        let request = PeerRequest::Unlink(Box::new(known));
        let transport = &*link.transport;
        let scope = self.domain().scope;
        let request = &request;
        let deliveries =
            futures_util::stream::iter(contacts.into_iter().map(|(peer, contact)| async move {
                let addresses = contact.addresses(transport);
                let answer = exchange(transport, &addresses, scope, request).await.ok();
                (peer, answer)
            }))
            .buffer_unordered(crate::enrollment::MAX_DIALED);
        futures_util::pin_mut!(deliveries);
        while let Some((peer, answer)) = deliveries.next().await {
            if let Some(retry) = self
                .unlink_retries
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get_mut(&peer)
            {
                retry.failed();
            }
            let Some(PeerReply::Unlink(answer)) = answer else {
                continue;
            };
            let known = match answer.verify(snapshot.database_context()?, peer) {
                Ok(known) => known,
                Err(_) => continue,
            };
            self.configure(|settings| {
                for receipt in settings.device_revocations.values_mut() {
                    if receipt.local
                        && known.get(&receipt.statement).is_some()
                        && receipt.pending.remove(&peer)
                    {
                        receipt.delivered.insert(peer);
                    }
                }
                Ok(())
            })
            .await?;
        }
        let pending: std::collections::BTreeSet<_> = self
            .settings
            .lock()
            .await
            .device_revocations
            .values()
            .filter(|receipt| receipt.local)
            .flat_map(|receipt| receipt.pending.iter().copied())
            .collect();
        self.unlink_retries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|peer, _| pending.contains(peer));
        Ok(())
    }
    pub(super) async fn accept_unlink(
        &self,
        known: RevocationSet,
    ) -> Result<RevocationSet, Failure> {
        known.verify_in(self.settings.lock().await.database_context()?)?;
        let registry = {
            let snapshot = self.settings_snapshot().await;
            if snapshot.issuer() {
                Some(self.registry(&snapshot).await?)
            } else {
                None
            }
        };
        let mut settings = self.settings.lock().await;
        known.verify_in(settings.database_context()?)?;
        if known.statements().next().is_none() {
            return Err(refused("the update contains no revocation"));
        }
        let mut draft = settings.clone();
        let added = draft.learn_revocations(&known)?;
        if !added.is_empty() {
            self.learned.store(true, Ordering::SeqCst);
        }
        if let Some(statement) = settings.revoking(&draft.revocations)? {
            if self
                .revocation_notice
                .record_unless_revoked(&settings, statement.clone())
                .await?
            {
                self.record_ended(&statement, settings.identity).await?;
            }
            drop(settings);
            if let Some(link) = self.current() {
                link.live
                    .close(rocky_secure::network::link::CloseReason::Authority);
            }
            self.observations.touch();
            return Ok(known);
        }
        let rotated = match &registry {
            Some(registry) => draft.begin_exposure_rotation(registry)?,
            None => false,
        };
        if !rotated && settings.renews_from_issuer() {
            drop(settings);
            self.wake_sync();
            return Ok(known);
        }
        if rotated {
            settings.commit(draft, &self.replica).await?;
            let keys = settings
                .rotate(&self.vault)?
                .ok_or("missing unlink rotation")?;
            settings.save(&self.replica).await?;
            self.apply(&mut settings, keys).await?;
            self.hold_keys(&settings, &settings.partitions(&self.domain())?)
                .await?;
        } else {
            settings
                .commit_with_revocations(draft, &self.replica, &known)
                .await?;
        }
        drop(settings);
        if rotated {
            Box::pin(self.complete_revocation()).await?;
        }
        self.observations.touch();
        self.wake_sync();
        Ok(known)
    }
    pub(super) async fn push_revocations(&self) {
        if self.sync_mode() != crate::SyncMode::Live {
            return;
        }
        let Some(link) = self.current() else {
            return;
        };
        let (known, contacts) = {
            let settings = self.settings.lock().await;
            let mut known = settings.revocations.clone();
            let Ok(database) = settings.database_context() else {
                return;
            };
            let Ok(time) = now() else {
                return;
            };
            let _ = known.merge_in(&self.replica.revocations().await, database, time);
            if known.statements().next().is_none() {
                return;
            }
            let own = Settings::node_key(&self.vault).ok().map(|k| k.public_key());
            let contacts: Vec<crate::enrollment::Contact> = settings
                .peers
                .iter()
                .filter(|(peer, _)| own != Some(**peer))
                .map(|(_, contact)| contact.clone())
                .collect();
            (known, contacts)
        };
        if contacts.is_empty() {
            return;
        }
        let request = PeerRequest::Unlink(Box::new(known));
        let transport = &*link.transport;
        let scope = self.domain().scope;
        let request = &request;
        let deliveries =
            futures_util::stream::iter(contacts.into_iter().map(|contact| async move {
                let addresses = contact.addresses(transport);
                let _ = exchange(transport, &addresses, scope, request).await;
            }))
            .buffer_unordered(crate::enrollment::MAX_DIALED);
        futures_util::pin_mut!(deliveries);
        while deliveries.next().await.is_some() {}
    }
    pub(super) async fn accept_link(
        &self,
        certificate: rocky_credentials::SignedCapability,
    ) -> Result<RevocationSet, Failure> {
        certificate.certifies(certificate.capability.subject, now()?)?;
        let mut current = self.settings.lock().await;
        if certificate.capability.extended()?.database != Some(current.database_context()?) {
            return Err(refused(
                "membership certificate belongs to another database",
            ));
        }
        current
            .identity_links()
            .with_certificate(certificate.clone())
            .historic_provenance(current.publisher, certificate.capability.subject)?;
        if certificate.issuer != current.identity {
            return Err(refused("only this identity links its nodes"));
        }
        let mut known = self.replica.revocations().await;
        if let Some(own) = current.credentials.certificate() {
            known.link(own.clone())?;
        }
        let membership = known.membership()?;
        let link = rocky_credentials::MembershipLink::from_certificate(&certificate)?;
        if link.sequence > membership.next_sequence(current.identity)? {
            return Err(refused("membership sequence skips the current head"));
        }
        if link.sequence == membership.next_sequence(current.identity)?
            && link.previous
                != membership
                    .head(current.identity)
                    .map(|head| head.certificate)
        {
            return Err(refused("membership link does not follow the current head"));
        }
        if link.sequence == membership.next_sequence(current.identity)?
            && !membership.can_link(current.identity, |node| {
                known.identity_unlinks(current.identity, node)
            })
        {
            return Err(refused("identity membership limit reached"));
        }
        known.link(certificate)?;
        let mut draft = current.clone();
        draft.learn_revocations(&known)?;
        current
            .commit_with_revocations(draft, &self.replica, &known)
            .await?;
        self.activity
            .record(crate::activity::ActivityKind::MembershipLinkRecorded {
                identity: (current.identity).to_owned(),
                node: link.node,
                sequence: link.sequence,
            });
        Ok(known)
    }
}
