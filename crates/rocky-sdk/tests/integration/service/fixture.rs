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
    Connector,
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
            (Some(Reach::Connector), Some(transport)) => {
                Some(rocky_sdk::CoordinatorSlot::Connector(Arc::new(
                    rocky_coordinator::Connector::new(transport.clone()),
                )))
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
    pub async fn follow(&self, name: &str) -> Result<rocky_sdk::TipStatus, Failure> {
        self.session
            .follow_with_identity(name, self.identity.clone())
            .await
    }
    pub async fn move_bookmark(
        &self,
        name: &str,
        tip: &str,
    ) -> Result<rocky_sdk::TipStatus, Failure> {
        self.session
            .move_bookmark_with_identity(name, tip, self.identity.clone())
            .await
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
            self.session.invite(
                Some(
                    serde_json::from_value::<rocky_sdk::GrantProfile>(json!(profile))
                        .expect("typed fixture"),
                ),
                None,
            ),
        )
        .await?["uri"]
            .as_str()
            .ok_or("invitation")?
            .to_owned())
    }
    pub async fn write(&self, key: &str, _handle: u32) -> Result<Value, Failure> {
        let mut tx = self.opened.database.transaction().await?;
        let id = tx.next_id("counts", Vec::new()).await?;
        tx.execute(
            "INSERT INTO counts(key,value,id) VALUES(?,5,?)",
            vec![rocky_sdk::SqlValue::Text(key.to_owned()), id.sql()],
        )
        .await?;
        let committed = tx.commit().await?;
        Ok(json!({"committed": committed.map(|_| json!({"frames": 1}))}))
    }
    pub async fn keys(&self) -> Result<Value, Failure> {
        Ok(snap::read(
            &self.opened.database,
            "SELECT key FROM counts ORDER BY key",
            Vec::new(),
        )
        .await?["rows"]
            .clone())
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
pub(super) async fn household(root: &Path, bundle: &TrustBundle) -> Result<(Peer, Peer), Failure> {
    household_reaching(root, bundle, Reach::Client(bundle)).await
}
pub(super) async fn household_reaching(
    root: &Path,
    bundle: &TrustBundle,
    trusted_reach: Reach<'_>,
) -> Result<(Peer, Peer), Failure> {
    let a = Peer::start(&root.join("a"), SCHEMA, Some(bundle), None, true).await?;
    snap::value(a.session.create_bookmark("main", None)).await?;
    snap::value(a.session.follow("main")).await?;
    snap::value(a.session.set_genesis_minimum(1)).await?;
    snap::value(
        a.session.set_kept_seals(
            serde_json::from_value::<rocky_sdk::KeptSeals>(json!({"every":true}))
                .expect("typed fixture"),
        ),
    )
    .await?;
    let t = Peer::start_reaching(
        &root.join("t"),
        SCHEMA,
        Some(trusted_reach),
        Some(a.invite("issuer").await?),
        false,
    )
    .await?;
    let owner = snap::value(a.session.info()).await?;
    let trusted = snap::value(t.session.info()).await?;
    assert_ne!(owner["principal"], trusted["principal"]);
    for identity in [&owner, &trusted] {
        assert_ne!(identity["principal"], identity["node"]);
    }
    let grant = snap::value(a.session.trust(
        snap::public_key(&(trusted["principal"])),
        (a.root()).clone(),
    ))
    .await?;
    snap::value(t.session.install_trust(&(snap::text(&(grant["grant"]))))).await?;
    let tenant = PrivateKey::new();
    snap::value(
        a.publish_coordination(
            rocky_coordinator::control::ControlRequest {
                tenant: rocky_sdk::credentials::PrivateKey::from_bytes(&snap::digest(
                    &(tenant.to_hex()),
                )),
                trusted: serde_json::from_value::<Vec<rocky_sdk::credentials::PublicKey>>(json!([
                    owner["principal"],
                    trusted["principal"]
                ]))
                .expect("typed fixture"),
                lifetime: 3600,
                id: serde_json::from_value::<[u8; 32]>(json!(vec![1u8; 32]))
                    .expect("typed fixture"),
                revision: rocky_coordinator::control::ControlRevision::Register {
                    grant: serde_json::from_value::<
                        Option<Box<rocky_sdk::credentials::SignedCoordinationGrant>>,
                    >(json!(null))
                    .expect("typed fixture"),
                    admission: serde_json::from_value::<
                        Option<Box<rocky_sdk::credentials::SignedTenantAdmission>>,
                    >(json!(null))
                    .expect("typed fixture"),
                },
            },
            Some((a.root()).clone()),
        ),
    )
    .await?;
    introduce(&a, &t).await?;
    t.sync().await?;
    a.sync().await?;
    assert_eq!(
        snap::value(t.session.coordinated_status()).await?["trusted"],
        true
    );
    a.session.set_automatic_sync(true).await?;
    t.session.set_automatic_sync(true).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let mut connected = true;
        for peer in [&a, &t] {
            let peers = snap::value(async {
                let network = peer.session.network().await?;
                Ok(json!({"peers":network.peers,"discovery":network.discovery}))
            })
            .await?;
            connected &= peers["peers"]
                .as_array()
                .is_some_and(|peers| peers.iter().any(|peer| peer["link"]["state"] == "open"));
        }
        if connected {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no persistent peer link"
        );
        tokio::task::yield_now().await;
    }
    Ok((a, t))
}
pub(super) fn seal<'a>(inspection: &'a Value, id: &Value) -> &'a Value {
    inspection["seals"]
        .as_array()
        .and_then(|seals| seals.iter().find(|seal| seal["id"] == *id))
        .unwrap_or(&Value::Null)
}
pub(super) async fn final_on_both(a: &Peer, t: &Peer, id: &Value) -> Result<(), Failure> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        t.sync().await?;
        a.sync().await?;
        let a_graph = snap::value(a.session.inspect_tips()).await?;
        let t_graph = snap::value(t.session.inspect_tips()).await?;
        if seal(&a_graph, id)["final"] == true && seal(&t_graph, id)["final"] == true {
            return Ok(());
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "seal not final: A={a_graph} T={t_graph}"
        );
    }
}
