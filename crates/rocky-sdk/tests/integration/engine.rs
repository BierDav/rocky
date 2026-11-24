use rocky_credentials::testing::MemoryKeys;
use rocky_sdk::{Database, Network, transport::memory::Hub};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
use support::snapshot as snap;

use crate::support;

type Failure = Box<dyn std::error::Error + Send + Sync>;

const SCHEMA: &str = r#"{"application":"shopping","version":1,"format":7,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},{"name":"notes","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"body","type":"text","not_null":false}],"primary_key":["key"]}]}"#;

fn iroh() -> rocky_iroh::Options {
    iroh_at(0)
}
fn iroh_at(port: u16) -> rocky_iroh::Options {
    rocky_iroh::Options::isolated(&format!("127.0.0.1:{port}"))
}
fn free_port() -> u16 {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("udp");
    socket.local_addr().expect("addr").port()
}

enum OpenMode {
    Create,
    Join {
        invitation: rocky_sdk::InvitationCode,
    },
    Resume,
}

struct Db {
    engine: Database,
    scope: rocky_sdk::Scope,
}
impl Db {
    async fn open(
        directory: &Path,
        mode: OpenMode,
        transport: rocky_iroh::Options,
    ) -> Result<Self, Failure> {
        Self::with_secrets(directory, mode, transport, None).await
    }
    async fn with_secrets(
        directory: &Path,
        mode: OpenMode,
        transport: rocky_iroh::Options,
        secrets: Option<Arc<dyn rocky_sdk::SecretStore>>,
    ) -> Result<Self, Failure> {
        Self::start(
            directory,
            mode,
            SCHEMA,
            Network::Owned(Arc::new(transport)),
            secrets,
        )
        .await
    }
    async fn start(
        directory: &Path,
        mode: OpenMode,
        schema: &str,
        network: Network,
        secrets: Option<Arc<dyn rocky_sdk::SecretStore>>,
    ) -> Result<Self, Failure> {
        let configuration = crate::common::configuration(
            directory,
            crate::support::snapshot::schema(schema),
            secrets,
            None,
            Some(network),
        );
        let engine = match mode {
            OpenMode::Create => configuration.create().await?.database,
            OpenMode::Join { invitation } => configuration.join(&invitation).await?.database,
            OpenMode::Resume => configuration.open().await?,
        };
        let scope = engine.scope();
        Ok(Self { engine, scope })
    }
    async fn create(directory: &Path, transport: rocky_iroh::Options) -> Result<Self, Failure> {
        Self::open(directory, OpenMode::Create, transport).await
    }
    async fn join(
        directory: &Path,
        invitation: &rocky_sdk::InvitationCode,
        transport: rocky_iroh::Options,
    ) -> Result<Self, Failure> {
        Self::open(
            directory,
            OpenMode::Join {
                invitation: invitation.clone(),
            },
            transport,
        )
        .await
    }
    async fn resume(directory: &Path, transport: rocky_iroh::Options) -> Result<Self, Failure> {
        Self::open(directory, OpenMode::Resume, transport).await
    }
    async fn invite(&self) -> rocky_sdk::InvitationCode {
        self.engine
            .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Full))
            .await
            .expect("invitation")
            .code
    }
    async fn sync(&self) -> Value {
        snap::ok(self.engine.sync_now()).await
    }
    async fn mutate(
        &self,
        handle: u8,
        payload: snap::Action,
        id: Option<&str>,
    ) -> Result<Value, Value> {
        snap::observe(snap::mutate(
            &self.engine,
            vec![handle; 32],
            payload,
            id.map(str::to_owned),
            None,
        ))
        .await
    }
    async fn add(&self, handle: u8, title: &str) -> String {
        let reply = self
            .mutate(
                handle,
                snap::Action::Create {
                    table: "items".to_owned(),
                    values: vec![
                        ("title".to_owned(), snap::field(json!(title))),
                        ("completed".to_owned(), snap::field(json!(false))),
                    ],
                },
                None,
            )
            .await
            .unwrap_or_else(|error| panic!("create failed: {error}"));
        reply["id"].as_str().unwrap_or_default().to_owned()
    }
    async fn items(&self) -> Vec<(String, String, i64)> {
        let page = snap::ok(snap::read(
            &self.engine,
            "SELECT key, title, completed FROM items ORDER BY key",
            Vec::new(),
        ))
        .await;
        page["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|row| {
                (
                    row[0]["value"].as_str().unwrap_or_default().to_owned(),
                    row[1]["value"].as_str().unwrap_or_default().to_owned(),
                    row[2]["value"]
                        .as_str()
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(-1),
                )
            })
            .collect()
    }
    async fn close(&self) {
        self.engine.close().await.expect("close");
    }
}
fn titles(items: &[(String, String, i64)]) -> Vec<&str> {
    let mut titles: Vec<&str> = items.iter().map(|(_, title, _)| title.as_str()).collect();
    titles.sort();
    titles
}

async fn converge(at: fn(u16) -> rocky_iroh::Options) -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let creator_port_kept_across_restart = free_port();
    let transport = || at(creator_port_kept_across_restart);
    let a_keys = Arc::new(MemoryKeys::default());
    let b_keys = Arc::new(MemoryKeys::default());
    let creator = Db::with_secrets(
        &root.path().join("a"),
        OpenMode::Create,
        transport(),
        Some(a_keys.clone()),
    )
    .await?;
    assert!(creator.engine.status().await?.authority.is_issuer);
    let member = Db::with_secrets(
        &root.path().join("b"),
        OpenMode::Join {
            invitation: creator.invite().await,
        },
        at(0),
        Some(b_keys.clone()),
    )
    .await?;
    assert!(!member.engine.status().await?.authority.is_issuer);
    assert_eq!(member.scope, creator.scope);

    let milk = creator.add(1, "Milk").await;
    assert!(support::reconciled(&member.sync().await));
    assert_eq!(titles(&member.items().await), ["Milk"]);

    snap::ok(member.engine.set_sync(rocky_sdk::SyncMode::Off)).await;
    assert_eq!(member.sync().await["skip"], "networking_disabled");
    creator
        .mutate(
            2,
            snap::Action::Set {
                table: "items".to_owned(),
                field: "title".to_owned(),
                value: snap::field(json!("Oat milk")),
            },
            Some(&milk),
        )
        .await
        .map_err(|error| error.to_string())?;
    member
        .mutate(
            3,
            snap::Action::Set {
                table: "items".to_owned(),
                field: "completed".to_owned(),
                value: snap::field(json!(true)),
            },
            Some(&milk),
        )
        .await
        .map_err(|error| error.to_string())?;
    member.add(4, "Eggs").await;
    snap::ok(member.engine.set_sync(rocky_sdk::SyncMode::Manual)).await;
    member.sync().await;
    assert_eq!(creator.items().await, member.items().await);
    assert!(
        creator
            .items()
            .await
            .contains(&(milk.clone(), "Oat milk".into(), 1)),
        "offline edits of different fields converge"
    );
    creator
        .mutate(
            5,
            snap::Action::Delete {
                table: "items".to_owned(),
            },
            Some(&milk),
        )
        .await
        .map_err(|error| error.to_string())?;
    member
        .mutate(
            6,
            snap::Action::Set {
                table: "items".to_owned(),
                field: "title".to_owned(),
                value: snap::field(json!("Late")),
            },
            Some(&milk),
        )
        .await
        .map_err(|error| error.to_string())?;
    member.sync().await;
    assert_eq!(
        titles(&member.items().await),
        ["Eggs"],
        "a delete is permanent: a later edit does not revive the row"
    );
    assert_eq!(creator.items().await, member.items().await);

    tokio::join!(member.close(), creator.close());
    let creator = Db::with_secrets(
        &root.path().join("a"),
        OpenMode::Resume,
        transport(),
        Some(a_keys),
    )
    .await?;
    let member = Db::with_secrets(
        &root.path().join("b"),
        OpenMode::Resume,
        at(0),
        Some(b_keys),
    )
    .await?;
    member.add(7, "Bread").await;
    creator.add(8, "Jam").await;
    member.sync().await;
    assert_eq!(
        titles(&creator.items().await),
        ["Bread", "Eggs", "Jam"],
        "both sides survive a restart and keep syncing"
    );
    assert_eq!(creator.items().await, member.items().await);
    tokio::join!(member.close(), creator.close());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn devices_converge_over_iroh() -> Result<(), Failure> {
    converge(iroh_at).await
}

#[tokio::test]
async fn mutations_are_durable_once_and_definite_rejections_are_final() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let db = Db::create(root.path(), iroh()).await?;
    let id = db.add(9, "Tea").await;
    assert_eq!(id.len(), 64);
    let replay = db
        .mutate(
            9,
            snap::Action::Create {
                table: "items".to_owned(),
                values: vec![
                    ("title".to_owned(), snap::field(json!("Tea"))),
                    ("completed".to_owned(), snap::field(json!(false))),
                ],
            },
            None,
        )
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(replay["replayed"], json!(true));
    db.close().await;
    let db = Db::resume(root.path(), iroh()).await?;
    let replay = db
        .mutate(
            9,
            snap::Action::Create {
                table: "items".to_owned(),
                values: vec![
                    ("title".to_owned(), snap::field(json!("Tea"))),
                    ("completed".to_owned(), snap::field(json!(false))),
                ],
            },
            None,
        )
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(
        replay["replayed"],
        json!(true),
        "the same handle reports its earlier commit after a restart"
    );
    assert_eq!(
        db.items().await.len(),
        1,
        "a replayed handle creates nothing else"
    );
    for (payload, entity) in [
        (
            snap::Action::Set {
                table: "notes".to_owned(),
                field: "body".to_owned(),
                value: snap::field(json!("x")),
            },
            Some(id.as_str()),
        ),
        (
            snap::Action::Set {
                table: "items".to_owned(),
                field: "completed".to_owned(),
                value: snap::field(json!("yes")),
            },
            Some(id.as_str()),
        ),
        (
            snap::Action::Set {
                table: "items".to_owned(),
                field: "missing".to_owned(),
                value: snap::field(json!("x")),
            },
            Some(id.as_str()),
        ),
        (
            snap::Action::Create {
                table: "items".to_owned(),
                values: vec![("title".to_owned(), snap::field(json!("Only")))],
            },
            None,
        ),
    ] {
        let error = db
            .mutate(10, payload, entity)
            .await
            .err()
            .ok_or("mutation should fail")?;
        assert_eq!(
            error["retryable"],
            json!(false),
            "another collection, an invalid value, an unknown field or a missing \
             field is a definite rejection: {error}"
        );
    }
    for sql in ["SELECT * FROM secure_rows", "DELETE FROM items"] {
        assert!(
            snap::observe(snap::read(&db.engine, sql, Vec::new()))
                .await
                .is_err(),
            "reads cannot reach protocol tables or write: {sql}"
        );
    }
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn renewal_revocation_and_logout_need_no_application_code() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let creator = Db::create(&root.path().join("a"), iroh()).await?;
    let alice = Db::join(&root.path().join("b"), &creator.invite().await, iroh()).await?;
    let invitation = creator.invite().await;
    let other = SCHEMA.replace("shopping", "other");
    assert!(
        Db::start(
            &root.path().join("wrong"),
            OpenMode::Join {
                invitation: invitation.clone(),
            },
            &other,
            Network::Owned(Arc::new(iroh())),
            None,
        )
        .await
        .is_err(),
        "a wrong application schema is refused"
    );
    let bob = Db::join(&root.path().join("c"), &invitation, iroh())
        .await
        .map_err(|error| {
            format!("a refused schema does not prevent installing the bundle: {error}")
        })?;

    let status = snap::ok(snap::device(&alice.engine)).await;
    assert_eq!(status["revocations"]["statements"], 0);
    assert_ne!(status["principal"], status["node"]);
    let renewed = snap::ok(async {
        alice.engine.renew().await?;
        snap::device(&alice.engine).await
    })
    .await;
    assert!(renewed["expires"].as_str() >= status["expires"].as_str());

    let identities = snap::ok(creator.engine.people()).await;
    assert_eq!(
        identities["identities"].as_array().map(Vec::len),
        Some(3),
        "the creator's, Alice's and Bob's identities"
    );
    let bob_identity = snap::ok(snap::device(&bob.engine)).await["principal"].clone();
    snap::ok(creator.engine.remove(
        rocky_sdk::PersonId::new(snap::public_key(&(bob_identity))),
        rocky_sdk::Removal::Departed,
    ))
    .await;
    assert_eq!(
        snap::ok(snap::device(&creator.engine)).await["revocations"]["statements"],
        1
    );
    creator.add(1, "After revocation").await;
    alice.sync().await;
    assert_eq!(
        snap::ok(snap::device(&alice.engine)).await["revocations"]["statements"],
        1,
        "Alice learns of Bob's revocation on her next sync"
    );
    assert_eq!(titles(&alice.items().await), ["After revocation"]);
    assert!(
        snap::observe(async {
            bob.engine.renew().await?;
            snap::device(&bob.engine).await
        })
        .await
        .is_err(),
        "renewal fetches the revocation a refused link need not carry"
    );
    assert!(
        snap::observe(bob.engine.sync_now()).await.is_err(),
        "a revoked device refuses to sync"
    );
    assert!(
        bob.items().await.is_empty(),
        "a revoked device reads nothing"
    );

    assert!(
        snap::observe(creator.engine.logout(rocky_sdk::Logout::KeepData))
            .await
            .is_err(),
        "the creator cannot log out"
    );
    snap::ok(alice.engine.logout(rocky_sdk::Logout::PurgeOnClose)).await;
    assert!(
        snap::observe(snap::read(&alice.engine, "SELECT 1", Vec::new()))
            .await
            .is_err(),
        "a logged-out member reads nothing"
    );
    alice.close().await;
    assert!(
        !root.path().join("b/identity.json").exists(),
        "purging removes the member's identity"
    );
    assert!(
        !root.path().join("b/secrets").exists(),
        "purging removes the member's secrets"
    );
    assert!(Db::resume(&root.path().join("b"), iroh()).await.is_err());
    snap::ok(bob.engine.logout(rocky_sdk::Logout::KeepData)).await;
    bob.close().await;
    assert!(Db::resume(&root.path().join("c"), iroh()).await.is_err());
    creator.close().await;
    Ok(())
}

#[tokio::test]
async fn an_application_transport_carries_enrollment_and_sync() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let open = |directory: &str, mode: OpenMode, name: &'static str| {
        let directory = root.path().join(directory);
        let network = Network::Owned(hub.connector(name));
        async move { Db::start(&directory, mode, SCHEMA, network, None).await }
    };
    let creator = open("a", OpenMode::Create, "a").await?;
    assert_eq!(
        creator.engine.status().await?.network.kind.as_deref(),
        Some("memory")
    );
    let invitation = creator.invite().await;
    let member = open(
        "b",
        OpenMode::Join {
            invitation: invitation.clone(),
        },
        "b",
    )
    .await?;
    creator.add(1, "Milk").await;
    member.add(2, "Eggs").await;
    assert!(support::reconciled(&member.sync().await));
    assert_eq!(titles(&creator.items().await), ["Eggs", "Milk"]);
    assert_eq!(creator.items().await, member.items().await);

    member.close().await;
    creator.close().await;
    let creator = open("a", OpenMode::Resume, "a").await?;
    let member = open("b", OpenMode::Resume, "b").await?;
    creator.add(3, "Jam").await;
    assert!(support::reconciled(&member.sync().await));
    assert_eq!(
        titles(&member.items().await),
        ["Eggs", "Jam", "Milk"],
        "both resume over the application transport"
    );
    member.close().await;
    creator.close().await;
    Ok(())
}
