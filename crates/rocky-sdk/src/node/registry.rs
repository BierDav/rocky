//! The issuer's side of the registry: answers that commit their rows
//! before they leave, the device list and labels. See
//! `docs/decisions/0048-replicated-issuer-registry.md`.
use super::*;
use crate::registry::{self, Changes, Registry};

impl Node {
    /// Every record this device holds; empty on a non-issuer, which holds
    /// no registry key.
    pub(crate) async fn registry(&self) -> Result<Registry, Failure> {
        if !self.settings.lock().await.issuer() {
            return Ok(Registry::default());
        }
        Box::pin(registry::read(&self.replica, self.domain().registry())).await
    }

    /// Commits `changes` with this issuer's own record in one transaction.
    async fn commit(&self, mut changes: Changes) -> Result<(), Failure> {
        let registry = self.registry().await?;
        let own = self
            .settings
            .lock()
            .await
            .own_record(&self.vault, &registry)?;
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
        let registry = self.registry().await?;
        let (reply, changes) = {
            let mut settings = self.settings.lock().await;
            let mut draft = settings.clone();
            let answer = draft.pair(&self.vault, request, &registry);
            // Attempts count even when the request is refused.
            draft.save(&self.path)?;
            *settings = draft;
            answer?
        };
        self.commit(changes).await?;
        if let Ok(paired) = rocky_credentials::PairRequest::from_bytes(request) {
            let profile = self
                .settings
                .lock()
                .await
                .authority
                .as_ref()
                .and_then(|authority| {
                    authority
                        .invitation
                        .as_ref()
                        .map(|invitation| invitation.profile.clone())
                });
            self.activity.record(
                "device_enrolled",
                json!({"node": paired.node.to_hex(), "profile": profile}),
            );
        }
        Ok(reply)
    }
    /// Answers a renewal of any unrevoked device once its record committed.
    pub(super) async fn answer_renewal(&self, request: &[u8]) -> Result<Vec<u8>, Failure> {
        let registry = self.registry().await?;
        let settings = self.settings.lock().await.clone();
        let (reply, changes) = settings.renew(&self.vault, request, &registry)?;
        self.commit(changes).await?;
        Ok(reply)
    }

    /// Issues a profile to a prepared device, granted on an issuer or
    /// delegated: identity saved, record committed, then returned.
    pub(super) async fn answer_grant(
        &self,
        device: crate::DeviceKeys,
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
        }
        Ok(supplied)
    }
    /// Records partitions granted or delegated on an issuer before the
    /// chain leaves it.
    pub(super) async fn record_holder(
        &self,
        device: &crate::DeviceKeys,
        grants: &[rocky_credentials::PartitionGrant],
        delegations: u8,
        chain: &CredentialChain,
    ) -> Result<(), Failure> {
        let registry = self.registry().await?;
        let mut changes = self
            .settings
            .lock()
            .await
            .holder_changes(&self.vault, device, grants, delegations, &registry)
            .map_err(|error| {
                if error.is::<registry::RegistryFull>() {
                    error
                } else {
                    refused(error)
                }
            })?;
        let leaf = chain.0.last().ok_or("missing partition credential")?;
        changes.capability(
            &device.node,
            registry::Capability {
                authors: leaf
                    .capability
                    .authors()?
                    .ok_or("principal has no author range")?,
                id: leaf.id()?,
                expires: chain.expires(),
            },
            registry.get(&device.node),
        )?;
        self.commit(changes).await
    }
    /// Writes the tombstone of a revoked device.
    pub(super) async fn record_revocation(&self, node: PublicKey) -> Result<(), Failure> {
        let mut changes = Changes::default();
        changes.revoked(&node)?;
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
                    .map(|rotation| rotation.nodes.clone())
            });
        let Some(nodes) = pending else {
            return Ok(());
        };
        for node in nodes {
            self.record_revocation(node).await?;
        }
        crate::enrollment::fault::check(crate::enrollment::fault::Stage::Committed)?;
        let registry = self.registry().await?;
        self.configure(|settings| {
            settings.complete_revocation(&self.vault, &self.domain(), &registry)
        })
        .await
    }

    /// Every other device's record, on an issuer.
    pub(super) async fn devices(&self) -> Result<Value, Failure> {
        let settings = self.settings.lock().await.clone();
        if !settings.issuer() {
            return Err(refused("only an issuer lists devices"));
        }
        let own = PrivateKey::from_bytes(&self.vault.get("node")?).public_key();
        let registry = self.registry().await?;
        let devices: Vec<Value> = registry
            .records
            .iter()
            .filter(|(node, record)| **node != own && record.device.is_some())
            .map(|(node, record)| {
                let device = record.device.as_ref();
                let capability = record.capability();
                json!({
                    "node": node.to_hex(),
                    "user": device.map(|device| device.user.to_hex()),
                    "kind": device.map(|device| device.kind),
                    "issuer": device.map(|device| device.issuer.to_hex()),
                    "enrolled": device.map(|device| device.enrolled.to_string()),
                    "revoked": record.revoked || settings.revoked_device(node, &registry),
                    "profile": record.profile(),
                    "capability": capability.map(|capability| data_encoding::HEXLOWER.encode(&capability.id)),
                    "expires": capability.map(|capability| capability.expires.to_string()),
                    "label": record.label,
                    "partitions": record.grants.iter().map(|grant| json!({
                        "grant": grant.grant, "delegations": grant.delegations,
                    })).collect::<Vec<_>>(),

                })
            })
            .collect();
        Ok(json!({"devices": devices}))
    }
    /// Sets the opaque application label of a recorded device.
    pub(super) async fn label(&self, request: &Value) -> Result<Value, Failure> {
        let node: PublicKey = request["node"]
            .as_str()
            .ok_or_else(|| refused("node must be a public key"))?
            .parse()
            .map_err(refused)?;
        let label: Vec<u8> = serde_json::from_value(request["label"].clone())
            .map_err(|_| refused("label must be bytes"))?;
        if label.len() > registry::MAX_LABEL {
            return Err(refused(format!(
                "a device label holds at most {} bytes",
                registry::MAX_LABEL
            )));
        }
        if !self.settings.lock().await.issuer() {
            return Err(refused("only an issuer labels devices"));
        }
        let registry = self.registry().await?;
        if registry
            .get(&node)
            .is_none_or(|record| record.device.is_none())
        {
            return Err(refused("the registry holds no such device"));
        }
        let mut changes = Changes::default();
        changes.label(&node, &label)?;
        self.commit(changes).await?;
        Ok(json!({}))
    }
}
