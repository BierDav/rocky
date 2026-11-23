use super::*;
use crate::support::snapshot as snap;
use rocky_sdk::{Opened, Session};
use std::path::Path;

pub(super) struct Peer {
    pub opened: Opened,
    control: Option<rocky_coordinator::control::ControlClient>,
    application: String,
    publisher: rocky_sdk::credentials::PublicKey,
    coordinator_transport: Option<Arc<dyn rocky_core::net::Transport>>,
    identity: Arc<dyn rocky_sdk::IdentityProvider>,
    pub session: Session,
    pub scope: rocky_sdk::Scope,
}
#[derive(Clone, Copy)]
pub(super) enum Reach<'a> {
    Client(&'a TrustBundle),
}
impl Peer {
    pub async fn start(
        directory: &Path,
        schema: &str,
        bundle: Option<&TrustBundle>,
        invitation: Option<String>,
        create: bool,
    ) -> Result<Self, Failure> {
        Self::start_reaching(
            directory,
            schema,
            bundle.map(Reach::Client),
            invitation,
            create,
        )
        .await
    }
    pub async fn start_reaching(
        directory: &Path,
        schema: &str,
        reach: Option<Reach<'_>>,
        invitation: Option<String>,
        create: bool,
    ) -> Result<Self, Failure> {
        let bundle = match reach {
            Some(Reach::Client(bundle)) => Some(bundle),
            _ => None,
        };
        let directory = directory.to_owned();
        let schema = schema.to_owned();
        let coordinator_transport = if reach.is_some() {
            Some(
                starter()
                    .start(*PrivateKey::new().as_bytes())
                    .await?
                    .transport(),
            )
        } else {
            None
        };
        let coordination = match (reach, coordinator_transport.as_ref()) {
            (Some(Reach::Client(bundle)), Some(transport)) => {
                Some(rocky_sdk::CoordinatorSlot::Client(
                    rocky_coordinator::Client::connect(bundle, transport.clone())?,
                ))
            }
            _ => None,
        };
        let control = bundle
            .zip(coordinator_transport.as_ref())
            .map(|(bundle, transport)| {
                rocky_coordinator::control::ControlClient::new(
                    bundle.clone(),
                    transport.clone(),
                    directory.clone(),
                )
            });
        let application = rocky_sdk::Schema::parse(&schema)?.application;
        let settings_dir = directory.clone();
        let identity: Arc<dyn rocky_sdk::IdentityProvider> =
            Arc::new(rocky_sdk::IdentityKey::from_secret(rocky_core::commitment(
                "fixture.service.identity",
                directory.to_string_lossy().as_bytes(),
            )));
        let startup_identity = identity.clone();
        let opened = tokio::task::spawn_blocking(move || {
            let open = Open {
                directory: &directory,
                schema: crate::support::snapshot::schema(&schema),
                identity: Some(startup_identity),
                secrets: None,
                network: Some(Network::Owned(Arc::new(starter()))),
                coordination,
            };
            match invitation {
                Some(invitation) => Database::join(open, &invitation),
                None if create => Database::create(open, None),
                None => Database::open(open),
            }
        })
        .await??;
        let identity: Arc<dyn rocky_sdk::IdentityProvider> = match opened.identity.as_deref() {
            Some(phrase) => Arc::new(rocky_sdk::IdentityKey::from_phrase(phrase)?),
            None => identity,
        };
        let settings = crate::common::authority_json(&settings_dir, "identity")?;
        let publisher = serde_json::from_value(settings["publisher"].clone())?;
        Ok(Self {
            control,
            application,
            publisher,
            coordinator_transport,
            identity,
            session: opened.database.session(),
            scope: opened.database.scope(),
            opened,
        })
    }
    pub fn control(&self) -> &rocky_coordinator::control::ControlClient {
        self.control.as_ref().expect("fixture coordinator")
    }
    pub async fn publish_coordination(
        &self,
        request: rocky_coordinator::control::ControlRequest,
        root: Option<Arc<dyn rocky_sdk::IdentityProvider>>,
    ) -> Result<rocky_coordinator::control::ControlReceipt, Failure> {
        self.control
            .as_ref()
            .ok_or("fixture coordinator not configured")?
            .publish(
                rocky_coordinator::control::Domain {
                    scope: self.scope,
                    application: self.application.clone(),
                    publisher: self.publisher,
                },
                request,
                root,
            )
            .await
    }
    pub fn root(&self) -> Arc<dyn rocky_sdk::IdentityProvider> {
        Arc::new(rocky_sdk::RootKey::from_secret(
            self.opened.root.as_ref().expect("fixture root").secret(),
        ))
    }
    pub async fn sync(&self) -> Result<Value, Failure> {
        snap::value(self.session.sync_once_with_identity(self.identity.clone())).await
    }
    pub async fn invite(&self, kind: &str) -> Result<String, Failure> {
        let profile = if kind == "issuer" {
            json!({"kind":kind})
        } else {
            json!({"kind":kind,"scopes":["counts"]})
        };
        Ok(snap::value(
            self.session.provision(
                serde_json::from_value::<rocky_sdk::GrantProfile>(json!(profile))
                    .expect("typed fixture"),
                None,
            ),
        )
        .await?["uri"]
            .as_str()
            .ok_or("invitation")?
            .to_owned())
    }
    pub async fn close(self) -> Result<(), Failure> {
        self.session.close().await;
        if let Some(network) = self.opened.network {
            network.close().await;
        }
        self.opened.database.close().await?;
        if let Some(transport) = self.coordinator_transport {
            transport.close().await;
        }
        Ok(())
    }
}

pub(super) async fn introduce(a: &Peer, t: &Peer) -> Result<(), Failure> {
    for (left, right) in [(a, t), (t, a)] {
        let contact = snap::value(right.session.contact_info()).await?["uri"].clone();
        snap::value(async {
            let value = left.session.add_peer(&(snap::text(&(contact)))).await?;
            Ok(json!({"node":value}))
        })
        .await?;
    }
    Ok(())
}
