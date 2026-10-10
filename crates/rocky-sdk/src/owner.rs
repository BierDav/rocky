//! The owner's control of its domain on a coordination service: register
//! the domain under a tenant key and sign later control revisions. Only
//! identities and generations leave the device. See
//! `docs/runtime/public-coordination.md`.
#[cfg(feature = "iroh")]
pub(crate) use service::{Domain, Service};

/// Never constructed without iroh: no coordination service is reachable.
#[cfg(not(feature = "iroh"))]
pub(crate) enum Service {}

#[cfg(feature = "iroh")]
mod service {
    use crate::{Failure, enrollment::now, node::Refused, transport::Transport};
    use data_encoding::HEXLOWER;
    use rocky_coordinator::{
        certified::{ControlCertificate, DomainControl, TrustBundle},
        service::NetworkCoordinator,
    };
    use rocky_core::{PrincipalId, Scope};
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

    /// The service a database's commands are composed with.
    pub(crate) struct Service {
        bundle: TrustBundle,
        directory: PathBuf,
        /// One tenant client at a time holds its evidence file.
        lock: Mutex<()>,
    }

    /// What the owner's calls need from the open database.
    pub(crate) struct Domain<'a> {
        pub scope: Scope,
        pub application: String,
        pub owner: PrincipalId,
        pub schema: u32,
        pub handler: u32,
        /// The domain's publisher, whose coordination grant registering needs.
        pub anchor: PublicKey,
        /// Present on an issuer only, which signs that grant itself.
        pub publisher: Option<PrivateKey>,
        pub transport: Option<Arc<dyn Transport>>,
        /// The verified activation of this device's generation, to activate.
        pub activation: Option<rocky_core::Activation>,
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
    /// A device as `command_identity` reports it.
    fn device(identity: &Value) -> Result<(PublicKey, PrincipalId), Failure> {
        let node = PublicKey::from_bytes(&key::<32>(&identity["key"], "key")?)
            .map_err(|_| refused("key is not a valid node key"))?;
        Ok((node, PrincipalId(key(&identity["principal"], "principal")?)))
    }
    fn devices(request: &Value, name: &str) -> Result<Vec<(PublicKey, PrincipalId)>, Failure> {
        let list = request[name]
            .as_array()
            .filter(|list| list.len() <= 64)
            .ok_or_else(|| refused(format!("{name} must list at most 64 identities")))?;
        list.iter().map(device).collect()
    }

    impl Service {
        pub(crate) fn new(bundle: TrustBundle, directory: PathBuf) -> Self {
            Self {
                bundle,
                directory,
                lock: Mutex::new(()),
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
            let client = NetworkCoordinator::connect(
                &self.bundle,
                tenant.clone(),
                self.directory.join("tenant.evidence"),
                transport,
            )?;
            match kind {
                "command_service_usage" => {
                    return Ok(serde_json::to_value(client.usage().await?)?);
                }
                "command_service_close" => {
                    use rocky_core::Coordinator;
                    let head = client.head(domain.scope).await?;
                    // A retry reads the head again, including after closure.
                    // Keep each exact-head attempt's wire identity stable.
                    let close_id = rocky_core::commitment(
                        "rocky.sdk.service-close.v1",
                        &serde_json::to_vec(&(id(request, "id")?, &head))?,
                    );
                    return Ok(serde_json::to_value(
                        client.close_domain(head, close_id).await?,
                    )?);
                }
                "command_service_activate" => {
                    let activation = domain
                        .activation
                        .ok_or_else(|| refused("no verified activation was prepared"))?;
                    let head = client.activate(activation, id(request, "id")?).await?;
                    return Ok(json!({"active": true, "sequence": head.sequence}));
                }
                _ => {}
            }
            let revision = match kind {
                "command_service_register" => 1,
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
                    owner: domain.owner,
                    executors: devices(request, "executors")?
                        .into_iter()
                        .map(|(node, _)| node)
                        .collect(),
                    invokers: devices(request, "invokers")?,
                    schema: domain.schema,
                    handler: domain.handler,
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
                    domain.publisher.as_ref().map(PrivateKey::public_key),
                    &supplied,
                    &admission,
                ))?,
            );
            let time = self.signing_time(tenant.public_key(), request_id, inputs)?;
            let certificate = ControlCertificate::sign(control(time)?, &tenant)?;
            let reference = if revision == 1 {
                // The publisher's grant names this tenant for the application:
                // signed here on an issuer, or one an issuer signed earlier.
                let grant = match (supplied, domain.publisher) {
                    (None, Some(publisher)) => SignedCoordinationGrant::sign(
                        CoordinationGrant {
                            publisher: Publisher(publisher.public_key()),
                            application: domain.application.clone(),
                            tenant: CoordinatorControl(tenant.public_key()),
                            not_before: time.saturating_sub(60),
                            expires: time.saturating_add(3600),
                        },
                        &publisher,
                    )?,
                    (None, None) => {
                        return Err(refused(
                            "registering needs the publisher's coordination grant: an issuer signs one with command_coordination_grant",
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
