//! The issuer's side of the registry: answers that commit their rows
//! before they leave, the identity list and labels.
use super::*;
use crate::{
    membership::Membership,
    registry::{self, Changes, Registry},
};

impl Node {
    /// Every record this device holds; empty on a non-issuer, which holds
    /// no registry key.
    pub(crate) async fn registry(&self) -> Result<Registry, Failure> {
        if !self.settings.lock().await.issuer() {
            return Ok(Registry::default());
        }
        Box::pin(registry::read(&self.replica, self.domain().registry())).await
    }
    /// Which nodes each identity linked, as this node knows them.
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
    /// Remembers that `node` of `identity` was admitted or renewed here.
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

    /// Commits `changes` with this issuer's own record in one transaction.
    pub(super) async fn commit(&self, mut changes: Changes) -> Result<(), Failure> {
        let registry = self.registry().await?;
        let own = self.settings.lock().await.own_record(&registry)?;
        changes.extend(own);
        Box::pin(registry::write(
            &self.replica,
            self.domain().registry(),
            &changes,
        ))
        .await
    }
    /// Brings this issuer's own record up to date, as after a renewal.
    pub(super) async fn record_self(&self) -> Result<(), Failure> {
        if !self.settings.lock().await.issuer() {
            return Ok(());
        }
        self.commit(Changes::default()).await
    }

    /// Answers a pairing: the claim is saved, then the record committed,
    /// then the enrollment returned.
    pub(super) async fn answer_pair(&self, request: &[u8]) -> Result<Vec<u8>, Failure> {
        if !self.settings.lock().await.issuer() {
            return Err(refused("only an issuer enrolls devices"));
        }
        self.expire_requests().await?;
        let registry = self.registry().await?;
        let bootstrap = self.enrollment_bootstrap().await?;
        let (reply, changes, notes) = {
            let mut settings = self.settings.lock().await;
            let mut draft = settings.clone();
            draft.bootstrap = Some(bootstrap);
            let answer = draft.pair(&self.vault, request, &registry);
            // Attempts count even when the request is refused.
            draft.bootstrap = None;
            draft.save(&self.path)?;
            *settings = draft;
            answer?
        };
        self.commit(changes).await?;
        for note in &notes {
            if let crate::join_requests::Note::Enrolled { node, profile } = note {
                let paired = rocky_credentials::PairRequest::from_bytes(request)?;
                let known = registry.admitted(&paired.user);
                self.see(paired.user, *node).await?;
                if !known {
                    self.activity.record(
                        "identity_admitted",
                        json!({"identity": paired.user.to_hex(), "profile": profile}),
                    );
                }
                self.activity.record(
                    "node_linked",
                    json!({"identity": paired.user.to_hex(), "node": node.to_hex()}),
                );
            }
        }
        self.note(&notes).await;
        Ok(reply)
    }
    /// Surfaces join request changes as activity events.
    pub(super) async fn note(&self, notes: &[crate::join_requests::Note]) {
        let expires = self.settings.lock().await.pending_requests().1;
        for note in notes {
            if let Some((kind, detail)) = note.event(expires) {
                self.activity.record(kind, detail);
            }
        }
    }
    /// Ends the requests of an expired invitation and reports them.
    async fn expire_requests(&self) -> Result<(), Failure> {
        let notes = self
            .configure(|settings| Ok(settings.expire_requests(now()?)))
            .await?;
        self.note(&notes).await;
        Ok(())
    }
    /// The devices this node knows of without having enrolled them: the
    /// join requests waiting on it.
    pub(super) async fn discovered(&self) -> Result<Value, Failure> {
        self.expire_requests().await?;
        let (pending, expires) = self.settings.lock().await.pending_requests();
        let pending: Vec<Value> = pending
            .iter()
            .map(|request| request.view(expires))
            .collect();
        Ok(json!({"pending": pending}))
    }
    /// Admits a pending join request whose device shows `code`.
    pub(super) async fn admit(&self, request: &Value) -> Result<Value, Failure> {
        let node = node_of(request)?;
        let code = request["code"]
            .as_str()
            .ok_or_else(|| refused("code must be the 6 digits the device shows"))?;
        let profile: Profile = serde_json::from_value(request["profile"].clone())
            .map_err(|_| refused("admitting needs a grant profile: reader, writer or issuer"))?;
        let label: Option<Vec<u8>> = serde_json::from_value(request["label"].clone())
            .map_err(|_| refused("label must be bytes"))?;
        if !self.settings.lock().await.issuer() {
            return Err(refused("only an issuer admits devices"));
        }
        self.expire_requests().await?;
        let registry = self.registry().await?;
        let notes = self
            .configure(|settings| settings.admit(&node, code, (profile, label), &registry))
            .await?;
        self.note(&notes).await;
        Ok(json!({}))
    }
    /// Refuses a pending join request.
    pub(super) async fn refuse(&self, request: &Value) -> Result<Value, Failure> {
        let node = node_of(request)?;
        if !self.settings.lock().await.issuer() {
            return Err(refused("only an issuer refuses devices"));
        }
        self.expire_requests().await?;
        let notes = self.configure(|settings| settings.refuse(&node)).await?;
        self.note(&notes).await;
        Ok(json!({}))
    }
    /// Answers a renewal of any unrevoked device once its record committed.
    pub(super) async fn answer_renewal(&self, request: &[u8]) -> Result<Vec<u8>, Failure> {
        let registry = self.registry().await?;
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

    /// Issues a profile to a prepared device, granted on an issuer or
    /// delegated: identity saved, record committed, then returned.
    pub(super) async fn answer_grant(
        &self,
        device: crate::NodeKeys,
        issued: (Profile, Terms),
        delegate: bool,
        contact: Option<String>,
    ) -> Result<crate::Supplied, Failure> {
        let issuer = self.settings.lock().await.issuer();
        let registry = self.registry().await?;
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
    /// Records partitions granted or delegated on an issuer before the
    /// chain leaves it.
    pub(super) async fn record_holder(
        &self,
        device: &crate::NodeKeys,
        grants: &[rocky_credentials::PartitionGrant],
        delegations: u8,
        chain: &CredentialChain,
    ) -> Result<(), Failure> {
        let registry = self.registry().await?;
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
                authors: leaf
                    .capability
                    .authors()?
                    .ok_or("principal has no author range")?,
                id: leaf.id()?,
                expires: chain.expires(),
            },
            registry.get(&device.identity),
        )?;
        self.commit(changes).await?;
        self.see(device.identity, device.node).await
    }
    /// Writes the tombstone of a revoked identity.
    pub(super) async fn record_revocation(&self, identity: PublicKey) -> Result<(), Failure> {
        let mut changes = Changes::default();
        changes.revoked(&identity)?;
        self.commit(changes).await
    }

    /// The tombstone is the durable completion marker; a stale intent replays safely.
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
        let registry = self.registry().await?;
        let membership = self.membership().await?;
        self.configure(|settings| {
            settings.complete_revocation(&self.vault, &self.domain(), (&registry, &membership))
        })
        .await
    }

    /// Every identity's record with its linked nodes and its trust and
    /// claim state, on an issuer.
    pub(super) async fn identities(&self) -> Result<Value, Failure> {
        let settings = self.settings.lock().await.clone();
        if !settings.issuer() {
            return Err(refused("only an issuer lists identities"));
        }
        let registry = self.registry().await?;
        let membership = self.membership().await?;
        let time = now()?;
        let trusted = registry.trusted_identities(&settings.revocations);
        let identities: Vec<Value> = registry
            .records
            .iter()
            .filter(|(_, record)| record.admitted.is_some())
            .map(|(identity, record)| {
                let admitted = record.admitted.as_ref();
                let capability = record.capability();
                let nodes: Vec<Value> = membership
                    .nodes(identity)
                    .map(|linked| {
                        json!({
                            "node": linked.node.to_hex(),
                            "author": linked.author,
                            "linked": linked.linked.to_string(),
                            "last_seen": self.last_seen(linked.node, time).map(|at| at.to_string()),
                            "revoked": settings.tombstoned(&linked.node),
                        })
                    })
                    .collect();
                json!({
                    "identity": identity.to_hex(),
                    "kind": admitted.map(|admitted| admitted.kind),
                    "issuer": admitted.map(|admitted| admitted.issuer.to_hex()),
                    "origin": admitted.and_then(|admitted| admitted.origin).map(|origin| origin.to_hex()),
                    "enrolled": admitted.map(|admitted| admitted.enrolled.to_string()),
                    "invitation": admitted.and_then(|admitted| admitted.invitation.as_ref()),
                    "single_use": admitted.is_some_and(|admitted| admitted.single_use),
                    "own": *identity == settings.identity,
                    "revoked": settings.revoked_identity(identity, &registry),
                    "profile": record.profile(),
                    "capability": capability.map(|capability| data_encoding::HEXLOWER.encode(&capability.id)),
                    "expires": capability.map(|capability| capability.expires.to_string()),
                    "label": record.label,
                    "partitions": record.grants.iter().map(|grant| json!({
                        "grant": grant.grant, "delegations": grant.delegations,
                    })).collect::<Vec<_>>(),
                    "trusted": registry.trusted.get(identity).map(|trust| json!({
                        "capability": trust.grant.id().ok().map(|id| data_encoding::HEXLOWER.encode(&id)),
                        "granted": trust.granted.to_string(),
                        "live": trusted.contains(identity),
                    })),
                    "claims": registry.claims.get(identity).map(|claim| json!({
                        "right": claim.right,
                        "delegations": claim.delegations,
                        "issuer": claim.issuer.to_hex(),
                        "granted": claim.granted.to_string(),
                    })),
                    "nodes": nodes,
                })
            })
            .collect();
        Ok(json!({"identities": identities}))
    }
    /// Sets the opaque application label of a recorded identity.
    pub(super) async fn label(&self, request: &Value) -> Result<Value, Failure> {
        let identity: PublicKey = request["identity"]
            .as_str()
            .ok_or_else(|| refused("identity must be a public key"))?
            .parse()
            .map_err(refused)?;
        let label: Vec<u8> = serde_json::from_value(request["label"].clone())
            .map_err(|_| refused("label must be bytes"))?;
        if label.len() > registry::MAX_LABEL {
            return Err(refused(format!(
                "an identity label holds at most {} bytes",
                registry::MAX_LABEL
            )));
        }
        if !self.settings.lock().await.issuer() {
            return Err(refused("only an issuer labels identities"));
        }
        let registry = self.registry().await?;
        if !registry.admitted(&identity) {
            return Err(refused("the registry holds no such identity"));
        }
        let mut changes = Changes::default();
        changes.label(&identity, &label)?;
        self.commit(changes).await?;
        Ok(json!({}))
    }
}

fn node_of(request: &Value) -> Result<PublicKey, Failure> {
    request["node"]
        .as_str()
        .ok_or_else(|| refused("node must be a public key"))?
        .parse()
        .map_err(refused)
}
