use super::*;
use crate::{
    membership::Membership,
    registry::{self, Changes, Registry},
};

impl Node {
    pub(crate) async fn settings_snapshot(&self) -> Shared<Settings> {
        self.settings.lock().await.clone()
    }
    pub(crate) async fn registry(&self, settings: &Settings) -> Result<Registry, Failure> {
        if !settings.issuer() {
            return Ok(Registry::default());
        }
        Box::pin(registry::read(&self.replica, self.domain().registry())).await
    }
    pub(crate) async fn membership(&self) -> Result<Membership, Failure> {
        let mut known = self.replica.revocations().await;
        let settings = self.settings.lock().await;
        known.merge(
            &settings.revocations,
            settings.publisher,
            self.domain().manifest().app,
            now()?,
        )?;
        if let Some(certificate) = settings.credentials.certificate() {
            known.link(certificate.clone())?;
        }
        Membership::replicated(&known, settings.database_context()?)
    }
    async fn see(&self, identity: PublicKey, node: PublicKey) -> Result<(), Failure> {
        let time = now()?;
        self.configure(|settings| {
            if let Some(issuing) = settings.authority.as_mut() {
                crate::membership::see(&mut issuing.seen, identity, node, time);
            }
            Ok(())
        })
        .await
    }

    pub(super) async fn commit(&self, mut changes: Changes) -> Result<(), Failure> {
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        let own = self.settings.lock().await.own_record(&registry)?;
        changes.extend(own);
        Box::pin(registry::write(
            &self.replica,
            self.domain().registry(),
            &changes,
        ))
        .await
    }
    pub(super) async fn record_self(&self) -> Result<(), Failure> {
        if !self.settings.lock().await.issuer() {
            return Ok(());
        }
        self.commit(Changes::default()).await
    }

    pub(super) async fn answer_pair(&self, request: &[u8]) -> Result<Vec<u8>, Failure> {
        if !self.settings.lock().await.issuer() {
            return Err(refused("only an issuer enrolls devices"));
        }
        self.expire_requests().await?;
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        let bootstrap = self.enrollment_bootstrap().await?;
        let held = self.replica.revocations().await;
        let (reply, changes, notes) = {
            let mut settings = self.settings.lock().await;
            let mut draft = settings.clone();
            draft.learn_revocations(&held)?;
            draft.bootstrap = Some(bootstrap);
            let answer = draft.pair(&self.vault, request, &registry);
            draft.bootstrap = None;
            settings.commit(draft, &self.path)?;
            answer?
        };
        self.commit(changes).await?;
        for note in &notes {
            if let crate::join_requests::Note::Enrolled { node, profile } = note {
                let paired = rocky_credentials::PairRequest::from_bytes(request)?;
                let known = registry.admitted(&paired.user);
                self.see(paired.user, *node).await?;
                if !known {
                    self.activity.record(crate::ActivityKind::IdentityAdmitted {
                        identity: (paired.user).to_owned(),
                        profile: profile.clone(),
                    });
                }
                self.activity.record(crate::ActivityKind::NodeLinked {
                    identity: (paired.user).to_owned(),
                    node: (node).to_owned(),
                });
            }
        }
        self.note(&notes).await;
        Ok(reply)
    }
    pub(super) async fn note(&self, notes: &[crate::join_requests::Note]) {
        let expires = self.settings.lock().await.pending_requests().1;
        for note in notes {
            if let Some(event) = note.event(expires) {
                self.activity.record(event);
            }
        }
    }
    async fn expire_requests(&self) -> Result<(), Failure> {
        let notes = self
            .configure(|settings| Ok(settings.expire_requests(now()?)))
            .await?;
        self.note(&notes).await;
        Ok(())
    }
    pub(super) async fn discovered(&self) -> Result<crate::Discovered, Failure> {
        self.expire_requests().await?;
        let settings = self.settings.lock().await.clone();
        let (pending, expires) = settings.pending_requests();
        let pending = pending
            .iter()
            .map(|request| crate::JoinRequest {
                node: request.node,
                nonce: request.nonce.clone(),
                invitation: request.invitation.clone(),
                name: request.name.clone(),
                received_epoch_seconds: request.received,
                expires_epoch_seconds: expires,
                code: request.code.clone(),
            })
            .collect();
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        let mut known = self.replica.revocations().await;
        known.verify_in(settings.database_context()?)?;
        known.merge(
            &settings.revocations,
            settings.publisher,
            self.domain().manifest().app,
            now()?,
        )?;
        let time = now()?;
        let met = self
            .met
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let seen = crate::membership::seen_devices(&known, &registry, |node| {
            self.last_seen(node, time)
                .into_iter()
                .chain(met.get(&node).copied())
                .chain(settings.contact_meta.get(&node).map(|meta| meta.learned))
                .max()
        });
        let links = self
            .link_requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(_, held)| time.saturating_sub(held.received) < 300)
            .map(|(node, held)| crate::LinkRequest {
                node: *node,
                code: held.code.clone(),
                received: held.received,
                confirmed: held.confirmed,
            })
            .collect();
        Ok(crate::Discovered {
            pending,
            seen,
            links,
        })
    }

    pub(super) async fn admit(&self, admit: crate::Admit) -> Result<(), Failure> {
        let node = admit.node;
        if !self.settings.lock().await.issuer() {
            return Err(refused("only an issuer admits devices"));
        }
        self.settings.lock().await.admits(Some(&admit.profile))?;
        self.expire_requests().await?;
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        let notes = self
            .configure(|settings| {
                settings.admit(&node, &admit.code, (admit.profile, admit.label), &registry)
            })
            .await?;
        self.note(&notes).await;
        Ok(())
    }

    pub(super) async fn refuse(&self, node: PublicKey) -> Result<(), Failure> {
        if !self.settings.lock().await.issuer() {
            return Err(refused("only an issuer refuses devices"));
        }
        self.expire_requests().await?;
        let notes = self.configure(|settings| settings.refuse(&node)).await?;
        self.note(&notes).await;
        Ok(())
    }

    pub(super) async fn answer_renewal(&self, request: &[u8]) -> Result<Vec<u8>, Failure> {
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        let settings = self.settings.lock().await.clone();
        let claims = settings.renew_claims(&self.vault, request, &registry, now()?)?;
        let (reply, changes) = settings.renew(&self.vault, request, &registry, claims)?;
        self.commit(changes).await?;
        let renewed = rocky_credentials::RenewRequest::from_bytes(request)?;
        if let Some(identity) = renewed.chain.principal() {
            self.see(identity, renewed.node).await?;
        }
        Ok(reply)
    }

    pub(super) async fn answer_grant(
        &self,
        device: crate::NodeKeys,
        issued: (Profile, Terms),
        delegate: bool,
        contact: Option<String>,
    ) -> Result<crate::Supplied, Failure> {
        let issuer = self.settings.lock().await.issuer();
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        let (supplied, changes) = if delegate {
            self.configure(|settings| {
                settings.delegate(&self.vault, device, issued, issuer.then_some(&registry))
            })
            .await?
        } else {
            self.configure(|settings| {
                settings.grant(&self.vault, device, issued, contact, &registry)
            })
            .await?
        };
        if issuer {
            self.commit(changes).await?;
            self.see(device.identity, device.node).await?;
        }
        Ok(supplied)
    }
    pub(super) async fn record_holder(
        &self,
        device: &crate::NodeKeys,
        grants: &[rocky_credentials::PartitionGrant],
        delegations: u8,
        chain: &CredentialChain,
    ) -> Result<(), Failure> {
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        let mut changes = self
            .settings
            .lock()
            .await
            .holder_changes(device, grants, delegations, &registry)
            .map_err(|error| {
                if error.is::<registry::RegistryFull>() {
                    error
                } else {
                    refused(error)
                }
            })?;
        let leaf = chain.0.last().ok_or("missing partition credential")?;
        changes.capability(
            &device.identity,
            registry::Capability {
                id: leaf.id()?,
                expires: chain.expires(),
            },
            registry.get(&device.identity),
        )?;
        self.commit(changes).await?;
        self.see(device.identity, device.node).await
    }
    pub(super) async fn record_revocation(&self, identity: PublicKey) -> Result<(), Failure> {
        let mut changes = Changes::default();
        changes.revoked(&identity)?;
        self.commit(changes).await
    }

    pub(crate) async fn complete_revocation(&self) -> Result<(), Failure> {
        let pending = self
            .settings
            .lock()
            .await
            .authority
            .as_ref()
            .and_then(|authority| {
                authority
                    .rotation
                    .as_ref()
                    .map(|rotation| rotation.identities.clone())
            });
        let Some(identities) = pending else {
            return Ok(());
        };
        for identity in identities {
            self.record_revocation(identity).await?;
        }
        crate::enrollment::fault::check(crate::enrollment::fault::Stage::Committed)?;
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        let membership = self.membership().await?;
        self.configure(|settings| {
            settings.complete_revocation(&self.vault, &self.domain(), (&registry, &membership))
        })
        .await
    }

    pub(super) async fn identities(&self) -> Result<Vec<crate::IdentityInfo>, Failure> {
        let settings = self.settings.lock().await.clone();
        if !settings.issuer() {
            return Err(refused("only an issuer lists identities"));
        }
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        let membership = self.membership().await?;
        let time = now()?;
        let trusted = registry.trusted_identities(
            settings.database_context()?,
            &settings.revocations,
            time,
        )?;
        registry
            .records
            .iter()
            .filter_map(|(identity, record)| {
                record
                    .admitted
                    .as_ref()
                    .map(|admitted| (identity, record, admitted))
            })
            .map(|(identity, record, admitted)| {
                let capability = record.capability();
                Ok(crate::IdentityInfo {
                    identity: *identity,
                    kind: admitted.kind,
                    issuer: admitted.issuer,
                    origin: admitted.origin,
                    enrolled: admitted.enrolled,
                    invitation: admitted.invitation.clone(),
                    single_use: admitted.single_use,
                    own: *identity == settings.identity,
                    revoked: settings.revoked_identity(identity, &registry),
                    profile: record.profile().cloned(),
                    capability: capability.map(|capability| capability.id),
                    expires: capability.map(|capability| capability.expires),
                    label: record.label.clone(),
                    partitions: record.grants.clone(),
                    trusted: registry
                        .trusted
                        .get(identity)
                        .map(|trust| -> Result<_, Failure> {
                            Ok(crate::TrustedIdentity {
                                capability: trust.grant.id()?,
                                granted: trust.granted,
                                live: trusted.contains(identity),
                            })
                        })
                        .transpose()?,
                    claims: registry.claims.get(identity).cloned(),
                    nodes: membership
                        .nodes(identity)
                        .map(|linked| crate::IdentityNode {
                            node: linked.node,
                            linked: linked.linked,
                            last_seen: self.last_seen(linked.node, time),
                            revoked: settings.tombstoned(&linked.node),
                        })
                        .collect(),
                })
            })
            .collect()
    }
    pub(super) async fn identity_for_node(
        &self,
        node: PublicKey,
    ) -> Result<Option<PublicKey>, Failure> {
        if !self.settings.lock().await.issuer() {
            return Err(refused("only an issuer lists identities"));
        }
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        let membership = self.membership().await?;
        Ok(registry
            .records
            .iter()
            .filter(|(_, record)| record.admitted.is_some())
            .find(|(identity, _)| membership.nodes(identity).any(|linked| linked.node == node))
            .map(|(identity, _)| *identity))
    }
    pub(super) async fn label(&self, identity: PublicKey, label: Vec<u8>) -> Result<(), Failure> {
        if label.len() > registry::MAX_LABEL {
            return Err(refused(format!(
                "an identity label holds at most {} bytes",
                registry::MAX_LABEL
            )));
        }
        if !self.settings.lock().await.issuer() {
            return Err(refused("only an issuer labels identities"));
        }
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        if !registry.admitted(&identity) {
            return Err(refused("the registry holds no such identity"));
        }
        let mut changes = Changes::default();
        changes.label(&identity, &label)?;
        self.commit(changes).await?;
        Ok(())
    }
}
