//! Identity-owned links and unlinks; signing happens outside replica transactions.
use super::*;

impl Node {
    pub(super) async fn unlink(&self, node: PublicKey) -> Result<rocky_core::Commitment, Failure> {
        let registry = self.registry().await?;
        let graph = self.replica.tip_graph(self.domain().scope).await?;
        let mut settings = self.settings.lock().await;
        let identity = settings.identity()?;
        let mut known = self.replica.revocations().await;
        known.merge(
            &settings.revocations,
            settings.publisher,
            self.domain().manifest().app,
            now()?,
        )?;
        if let Some(certificate) = settings.credentials.certificate() {
            known.link(certificate.clone())?;
        }
        let certificate = known
            .certificates()
            .find(|certificate| {
                certificate.issuer == settings.identity && certificate.capability.subject == node
            })
            .ok_or_else(|| refused("this identity did not link that node"))?
            .clone();
        let cut = graph.heads(&settings.identity, &node)?;
        let cut = if cut.statements.is_empty() && cut.acknowledgments.is_none() {
            // Identity-owned unlink already defines the empty cut implicitly.
            Vec::new()
        } else {
            vec![cut]
        };
        let statement = rocky_credentials::Revocation {
            app: self.domain().manifest().app,
            issuer: settings.identity,
            time: now()?,
            nodes: [rocky_credentials::PeerId::of(node)].into(),
            identities: Default::default(),
            capabilities: Default::default(),
            cut,
            mode: rocky_credentials::RevocationMode::Lost,
            recovery: None,
        };
        // The identity signs outside the replica transaction. The target certificate proves ownership.
        let signed = rocky_credentials::SignedRevocation::sign(
            statement,
            &*identity,
            &settings.identity_links().with_certificate(certificate),
        )?;
        let id = signed.id()?;
        known.merge(
            &RevocationSet::try_from(vec![signed.clone()])?,
            settings.publisher,
            self.domain().manifest().app,
            now()?,
        )?;
        let mut draft = settings.clone();
        draft.learn_revocations(&known)?;
        let own = Settings::node_key(&self.vault)?.public_key() == node;
        let rotated = !own && draft.begin_exposure_rotation(&registry)?;
        draft.save(&self.path)?;
        *settings = draft;
        if rotated {
            let keys = settings
                .rotate(&self.vault)?
                .ok_or("missing unlink rotation")?;
            settings.save(&self.path)?;
            self.apply(&mut settings, keys).await?;
            self.hold_keys(&settings, &settings.partitions(&self.domain())?)
                .await?;
        } else {
            self.replica.learn_revocations(&known).await?;
        }
        if settings
            .credentials
            .certificate()
            .is_some_and(|certificate| certificate.capability.subject == node)
        {
            settings.record_revocation(&self.path, signed)?;
        }
        drop(settings);
        if rotated {
            Box::pin(self.complete_revocation()).await?;
        }
        self.forget_readers().await;
        self.wake_sync();
        // An unlinked last node cannot open a credentialed session. Deliver the
        // identity's signed evidence without asking that node to authenticate.
        if own && let Some(link) = self.current() {
            let contacts: Vec<_> = self
                .settings
                .lock()
                .await
                .peers
                .values()
                .map(|contact| contact.addresses(link.transport.kind()))
                .collect();
            let request = PeerRequest::Unlink(known.clone());
            for addresses in contacts.into_iter().take(crate::enrollment::MAX_DIALED) {
                if addresses.is_empty() {
                    continue;
                }
                if let Err(error) =
                    exchange(&*link.transport, &addresses, self.domain().scope, &request).await
                {
                    self.activity.record(crate::ActivityKind::UnlinkUnpushed {
                        error: (error.to_string()).to_owned(),
                    });
                }
            }
        }
        Ok(id)
    }
    pub(super) async fn accept_unlink(
        &self,
        known: RevocationSet,
    ) -> Result<RevocationSet, Failure> {
        known.verify_in(self.settings.lock().await.database_context()?)?;
        let registry = self.registry().await?;
        let mut settings = self.settings.lock().await;
        known.verify_in(settings.database_context()?)?;
        if !known
            .statements()
            .any(|signed| !signed.root() && !signed.statement.nodes.is_empty())
        {
            return Err(refused("the update contains no identity-owned unlink"));
        }
        let mut draft = settings.clone();
        draft.learn_revocations(&known)?;
        let ended = draft.revoking(&known)?;
        let rotated = ended.is_none() && draft.begin_exposure_rotation(&registry)?;
        draft.save(&self.path)?;
        if let Some(statement) = ended {
            draft.record_revocation(&self.path, statement)?;
        }
        *settings = draft;
        if rotated {
            let keys = settings
                .rotate(&self.vault)?
                .ok_or("missing unlink rotation")?;
            settings.save(&self.path)?;
            self.apply(&mut settings, keys).await?;
            self.hold_keys(&settings, &settings.partitions(&self.domain())?)
                .await?;
        } else {
            self.replica.learn_revocations(&known).await?;
        }
        drop(settings);
        if rotated {
            Box::pin(self.complete_revocation()).await?;
        }
        self.forget_readers().await;
        self.wake_sync();
        Ok(known)
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
            .provenance_with_families(
                current.publisher,
                certificate.capability.subject,
                &self.domain().families,
            )?;
        if current.single_use || certificate.issuer != current.identity {
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
        // Persist fork evidence too; arrival order must not choose a winner.
        draft.save(&self.path)?;
        *current = draft;
        self.replica.learn_revocations(&known).await?;
        self.activity
            .record(crate::ActivityKind::MembershipLinkRecorded {
                identity: (current.identity).to_owned(),
                node: link.node,
                sequence: link.sequence,
            });
        Ok(known)
    }
}
