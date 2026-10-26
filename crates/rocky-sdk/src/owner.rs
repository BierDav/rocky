//! The owner's control of its domain on a coordination service: register
//! the domain under a tenant key and sign later control revisions. Only
//! identities and generations leave the device. See
//! `docs/runtime/public-coordination.md`.
#[cfg(feature = "iroh")]
pub(crate) use service::{Domain, Service};

/// Never constructed without iroh: no coordination service is reachable.
#[cfg(not(feature = "iroh"))]
pub(crate) enum Service {}
#[cfg(not(feature = "iroh"))]
impl Service {
    pub(crate) fn certify(&self, _: &rocky_credentials::CredentialChain) {
        match *self {}
    }
}

#[cfg(feature = "iroh")]
mod service {
    use crate::{Failure, enrollment::now, node::Refused, transport::Transport};
    use data_encoding::HEXLOWER;
    use rocky_coordinator::{
        certified::{ControlCertificate, DomainControl, TrustBundle},
        service::NetworkCoordinator,
    };
    use rocky_core::Scope;
    use rocky_credentials::{
        CoordinationGrant, CoordinatorControl, PrivateKey, PublicKey, Publisher,
        SignedCoordinationGrant, SignedTenantAdmission,
    };
    use serde_json::{Value, json};
    use std::{path::PathBuf, sync::Arc};
    use tokio::sync::Mutex;

    /// Request signing times kept for retries, oldest evicted first.
    const MAX_SIGNED: usize = 256;

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Signed {
        tenant: [u8; 32],
        id: [u8; 32],
        inputs: rocky_core::Commitment,
        time: u64,
    }

    /// The metadata service composed with a database.
    pub(crate) struct Service {
        bundle: TrustBundle,
        directory: PathBuf,
        /// One tenant client at a time holds its evidence file.
        lock: Mutex<()>,
        /// This node's client of the domain's coordinated log.
        client: Option<Arc<NetworkCoordinator>>,
    }

    /// What the owner's calls need from the open database.
    pub(crate) struct Domain<'a> {
        pub scope: Scope,
        pub application: String,
        /// The domain's publisher, whose coordination grant registering needs.
        pub anchor: PublicKey,
        /// Present on the owner only, whose identity signs that grant itself.
        pub publisher: Option<Arc<dyn rocky_credentials::IdentityProvider>>,
        pub transport: Option<Arc<dyn Transport>>,
        pub request: &'a Value,
    }

    fn refused(message: impl Into<String>) -> Failure {
        Box::new(Refused(message.into()))
    }
    fn key<const N: usize>(value: &Value, name: &str) -> Result<[u8; N], Failure> {
        value
            .as_str()
            .and_then(|text| HEXLOWER.decode(text.as_bytes()).ok())
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| refused(format!("{name} must be {} hexadecimal digits", N * 2)))
    }
    fn id(request: &Value, name: &str) -> Result<[u8; 32], Failure> {
        serde_json::from_value::<Vec<u8>>(request[name].clone())
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| refused(format!("{name} must be 32 bytes")))
    }
    fn devices(request: &Value) -> Result<Vec<PublicKey>, Failure> {
        let list = request["trusted"]
            .as_array()
            .filter(|list| !list.is_empty() && list.len() <= 32)
            .ok_or_else(|| refused("trusted must list 1 through 32 identity keys"))?;
        list.iter()
            .map(|value| {
                PublicKey::from_bytes(&key::<32>(value, "trusted")?)
                    .map_err(|_| refused("invalid trusted identity key"))
            })
            .collect()
    }

    impl Service {
        pub(crate) fn new(bundle: TrustBundle, directory: PathBuf) -> Self {
            Self {
                bundle,
                directory,
                lock: Mutex::new(()),
                client: None,
            }
        }
        /// The log client whose requests present this node's certificate.
        pub(crate) fn with_client(mut self, client: Arc<NetworkCoordinator>) -> Self {
            self.client = Some(client);
            self
        }
        /// Presents the node certificate `chain` ends at from now on: the
        /// coordinator admits nodes of the identities a control trusts.
        pub(crate) fn certify(&self, chain: &rocky_credentials::CredentialChain) {
            if let (Some(client), Some(leaf)) = (&self.client, chain.0.last()) {
                client.certify(leaf.clone());
            }
        }

        pub(crate) async fn call(&self, kind: &str, domain: Domain<'_>) -> Result<Value, Failure> {
            let request = domain.request;
            let tenant = PrivateKey::from_bytes(&key::<32>(&request["tenant"], "tenant")?);
            let transport =
                domain
                    .transport
                    .clone()
                    .ok_or(crate::ServiceError::NetworkRequired(
                        "coordination service control",
                    ))?;
            let _held = self.lock.lock().await;
            let client = NetworkCoordinator::connect(&self.bundle, tenant.clone(), transport)?;
            match kind {
                "coordination_usage" => {
                    return Ok(serde_json::to_value(client.usage().await?)?);
                }
                "coordination_close" => {
                    client
                        .close_domain(domain.scope, id(request, "id")?)
                        .await?;
                    return Ok(json!({"closed": true}));
                }
                _ => {}
            }
            let revision = match kind {
                "coordination_register" => 1,
                _ => request["revision"]
                    .as_u64()
                    .filter(|revision| *revision > 1)
                    .ok_or_else(|| refused("a later control revision is above 1"))?,
            };
            let lifetime = request["lifetime"]
                .as_u64()
                .filter(|seconds| (60..=366 * 86_400).contains(seconds))
                .ok_or_else(|| refused("lifetime must be 60 s to 366 days"))?;
            let control = |time: u64| -> Result<DomainControl, Failure> {
                Ok(DomainControl {
                    scope: domain.scope,
                    configuration: self.bundle.genesis()?.genesis.configuration,
                    revision,
                    trusted: devices(request)?,
                    not_before: time.saturating_sub(60),
                    expires: time.saturating_add(lifetime),
                })
            };
            let (supplied, admission) = if revision == 1 {
                let supplied: Option<SignedCoordinationGrant> = match &request["grant"] {
                    Value::Null => None,
                    value => Some(
                        serde_json::from_value(value.clone())
                            .map_err(|_| refused("grant is not a coordination grant"))?,
                    ),
                };
                let admission: Option<SignedTenantAdmission> = match &request["admission"] {
                    Value::Null => None,
                    value => Some(
                        serde_json::from_value(value.clone())
                            .map_err(|_| refused("admission is not a tenant admission"))?,
                    ),
                };
                (supplied, admission)
            } else {
                (None, None)
            };
            let request_id = id(request, if revision == 1 { "registration" } else { "id" })?;
            // Everything signed except the time: a retry of these inputs
            // re-signs them at the first attempt's time.
            let inputs = rocky_core::commitment(
                "rocky.sdk.service-request.v1",
                &serde_json::to_vec(&(
                    control(0)?,
                    domain
                        .publisher
                        .as_ref()
                        .map(|owner| owner.public_key())
                        .transpose()?,
                    &supplied,
                    &admission,
                ))?,
            );
            let time = self.signing_time(tenant.public_key(), request_id, inputs)?;
            let certificate = ControlCertificate::sign(control(time)?, &tenant)?;
            let reference = if revision == 1 {
                // The owner's grant names this tenant for the application:
                // signed here on the owner, or one it signed earlier.
                let grant = match (supplied, domain.publisher) {
                    (None, Some(owner)) => SignedCoordinationGrant::sign(
                        CoordinationGrant {
                            publisher: Publisher(owner.public_key()?),
                            application: domain.application.clone(),
                            tenant: CoordinatorControl(tenant.public_key()),
                            not_before: time.saturating_sub(60),
                            expires: time.saturating_add(3600),
                        },
                        &*owner,
                    )?,
                    (None, None) => {
                        return Err(refused(
                            "registering needs the owner's coordination grant: the owner signs one with coordination_grant",
                        ));
                    }
                    (Some(supplied), _) => supplied,
                };
                grant
                    .verify(time)
                    .map_err(|error| refused(error.to_string()))?;
                if grant.grant.publisher != Publisher(domain.anchor)
                    || grant.grant.application != domain.application
                    || grant.grant.tenant != CoordinatorControl(tenant.public_key())
                {
                    return Err(refused(
                        "the coordination grant names another publisher, application or tenant",
                    ));
                }
                client
                    .register(grant, admission, certificate, request_id)
                    .await?
            } else {
                client.control(certificate, request_id).await?
            };
            Ok(json!({
                "tenant": tenant.public_key().to_hex(),
                "revision": revision,
                "control": HEXLOWER.encode(&reference),
                "expires": time.saturating_add(lifetime),
            }))
        }

        /// The signing time of request `id` with `inputs`: the first such
        /// attempt's, so the coordinator answers a retry from its retained
        /// outcome. Other inputs under the same ID sign anew and conflict
        /// there. Recorded before the request leaves, under the service lock.
        fn signing_time(
            &self,
            tenant: PublicKey,
            id: [u8; 32],
            inputs: rocky_core::Commitment,
        ) -> Result<u64, Failure> {
            let path = self.directory.join("tenant.requests");
            let mut signed: Vec<Signed> = match std::fs::read(&path) {
                Ok(bytes) => serde_json::from_slice(&bytes)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                Err(error) => return Err(error.into()),
            };
            let tenant = *tenant.as_bytes();
            let time = now()?;
            if let Some(old) = signed
                .iter()
                .find(|s| s.tenant == tenant && s.id == id && s.inputs == inputs)
            {
                return Ok(old.time);
            }
            if signed.len() >= MAX_SIGNED {
                signed.remove(0);
            }
            signed.push(Signed {
                tenant,
                id,
                inputs,
                time,
            });
            crate::secrets::write_private(&path, &serde_json::to_vec(&signed)?)?;
            Ok(time)
        }
    }
}
