use rocky_sdk::{Engine, Network, transport::memory::Hub};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
use support::snapshot as snap;

use crate::support;

type Failure = Box<dyn std::error::Error + Send + Sync>;

const SCHEMA: &str = r#"{"application":"shopping","version":1,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},{"name":"notes","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"body","type":"text","not_null":false}],"primary_key":["key"]}]}"#;

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

struct Db {
    engine: Arc<Engine>,
    scope: rocky_sdk::Scope,
}
impl Db {
    fn open(
        directory: &Path,
        mode: rocky_sdk::EngineMode,
        transport: rocky_iroh::Options,
    ) -> Result<Self, Failure> {
        Self::with_secrets(directory, mode, transport, None)
    }
    fn with_secrets(
        directory: &Path,
        mode: rocky_sdk::EngineMode,
        transport: rocky_iroh::Options,
        secrets: Option<Arc<dyn rocky_sdk::SecretStore>>,
    ) -> Result<Self, Failure> {
        let options = rocky_sdk::EngineOptions {
            directory: directory.to_path_buf(),
            transport: Arc::new(transport),
            mode,
        };
        let engine = Engine::open(options, &rocky_sdk::Schema::parse(SCHEMA)?, secrets)?;
        let scope = engine.scope();

        Ok(Self { engine, scope })
    }
    fn create(directory: &Path, transport: rocky_iroh::Options) -> Result<Self, Failure> {
        Self::open(
            directory,
            rocky_sdk::EngineMode::Create {
                credential_ttl: None,
                root: None,
            },
            transport,
        )
    }
    fn join(
        directory: &Path,
        invitation: &str,
        transport: rocky_iroh::Options,
    ) -> Result<Self, Failure> {
        Self::open(
            directory,
            rocky_sdk::EngineMode::Join {
                invitation: snap::text(&(invitation)),
            },
            transport,
        )
    }
    fn resume(directory: &Path, transport: rocky_iroh::Options) -> Result<Self, Failure> {
        Self::open(directory, rocky_sdk::EngineMode::Resume, transport)
    }
    async fn invite(&self) -> String {
        snap::ok(self.engine.session().invite(
            Some(rocky_sdk::GrantProfile::Writer {
                scopes: ["items".to_owned()].into_iter().collect(),
            }),
            None,
        ))
        .await["uri"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }
    async fn sync(&self) -> Value {
        snap::ok(self.engine.session().sync_once()).await
    }
    async fn mutate(
        &self,
        handle: u8,
        payload: snap::Action,
        id: Option<&str>,
    ) -> Result<Value, Value> {
        snap::observe(snap::mutate(
            self.engine.session(),
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
        let page = snap::ok(self.engine.session().read(rocky_sdk::ReadQuery {
            sql: "SELECT key, title, completed FROM items ORDER BY key".to_owned(),
            values: vec![],
            max_rows: 1024,
            max_bytes: None,
            cursor: None,
        }))
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
    let a_keys = Arc::new(support::secrets::Keys::default());
    let b_keys = Arc::new(support::secrets::Keys::default());
    let creator = Db::with_secrets(
        &root.path().join("a"),
        rocky_sdk::EngineMode::Create {
            credential_ttl: None,
            root: None,
        },
        transport(),
        Some(a_keys.clone()),
    )?;
    assert_eq!(
        snap::ok(creator.engine.session().domain()).await["issuer"],
        json!(true)
    );
    let member = Db::with_secrets(
        &root.path().join("b"),
        rocky_sdk::EngineMode::Join {
            invitation: snap::text(&(creator.invite().await)),
        },
        at(0),
        Some(b_keys.clone()),
    )?;
    assert_eq!(
        snap::ok(member.engine.session().domain()).await["issuer"],
        json!(false)
    );
    assert_eq!(member.scope, creator.scope);

    let milk = creator.add(1, "Milk").await;
    assert!(support::reconciled(&member.sync().await));
    assert_eq!(titles(&member.items().await), ["Milk"]);

    snap::ok(member.engine.session().set_networking_enabled(false)).await;
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
    snap::ok(member.engine.session().set_networking_enabled(true)).await;
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
        rocky_sdk::EngineMode::Resume,
        transport(),
        Some(a_keys),
    )?;
    let member = Db::with_secrets(
        &root.path().join("b"),
        rocky_sdk::EngineMode::Resume,
        at(0),
        Some(b_keys),
    )?;
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
    let db = Db::create(root.path(), iroh())?;
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
    let handle = snap::mutation_handle(db.engine.session(), &[9; 32], None).await?;
    assert!(db.engine.session().mutation_committed(handle).await?);
    db.close().await;
    let db = Db::resume(root.path(), iroh())?;
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
            snap::observe(db.engine.session().read(rocky_sdk::ReadQuery {
                sql: snap::text(&(sql)),
                values: vec![],
                max_rows: 10,
                max_bytes: None,
                cursor: None
            }))
            .await
            .is_err(),
            "reads cannot reach protocol tables or write: {sql}"
        );
    }
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn pages_and_subscriptions_follow_committed_revisions() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let db = Db::create(root.path(), iroh())?;
    for handle in 1..=5 {
        db.add(handle, &format!("Item {handle}")).await;
    }
    let query = rocky_sdk::ReadQuery {
        sql: "SELECT title FROM items ORDER BY title".to_owned(),
        values: vec![],
        max_rows: 2,
        max_bytes: None,
        cursor: None,
    };
    let mut titles = Vec::new();
    let mut page = snap::ok(db.engine.session().read((query).clone())).await;
    loop {
        titles.extend(
            page["rows"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|row| row[0]["value"].clone()),
        );
        let Some(cursor) = page["next"].as_str() else {
            break;
        };
        let mut next = query.clone();
        next.cursor = Some((cursor).to_string());
        page = snap::ok(db.engine.session().read((next).clone())).await;
    }
    assert_eq!(titles.len(), 5);

    let subscription = snap::ok(async {
        let id = db.engine.session().subscribe((query).clone()).await?;
        Ok(json!({"subscription":id.to_string()}))
    })
    .await["subscription"]
        .clone();
    let first = snap::ok(
        db.engine
            .session()
            .next(snap::integer(&(subscription)) as u64),
    )
    .await;
    let waiting = {
        let engine = db.engine.clone();
        let subscription = snap::integer(&subscription) as u64;
        tokio::spawn(async move { engine.session().next(subscription).await })
    };
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());
    db.add(6, "A first").await;
    let changed = waiting.await??;
    assert_ne!(json!(changed.revision.to_string()), first["revision"]);
    assert_eq!(changed.rows[0][0].to_text().ok_or("title")?, "A first");
    snap::ok(
        db.engine
            .session()
            .unsubscribe(snap::integer(&(subscription)) as u64),
    )
    .await;
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn renewal_revocation_and_logout_need_no_application_code() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let creator = Db::create(&root.path().join("a"), iroh())?;
    let alice = Db::join(&root.path().join("b"), &creator.invite().await, iroh())?;
    let invitation = creator.invite().await;
    let wrong = rocky_sdk::EngineOptions {
        directory: root.path().join("wrong"),
        transport: Arc::new(iroh()),
        mode: rocky_sdk::EngineMode::Join {
            invitation: invitation.clone(),
        },
    };
    let other = rocky_sdk::Schema::parse(&SCHEMA.replace("shopping", "other"))?;
    assert!(
        Engine::open(wrong, &other, None).is_err(),
        "a wrong application schema is refused"
    );
    let bob = Db::join(&root.path().join("c"), &invitation, iroh())
        .map_err(|error| format!("a refused schema does not consume the invitation: {error}"))?;
    assert!(
        Db::join(&root.path().join("d"), &invitation, iroh()).is_err(),
        "a used invitation enrolls nobody else"
    );

    let status = snap::ok(alice.engine.session().info()).await;
    assert_eq!(status["revocations"]["statements"], 0);
    assert_ne!(status["principal"], status["node"]);
    let renewed = snap::ok(async {
        alice.engine.session().renew().await?;
        alice.engine.session().info().await
    })
    .await;
    assert!(renewed["expires"].as_str() >= status["expires"].as_str());

    let identities = snap::ok(creator.engine.session().identities()).await;
    assert_eq!(
        identities["identities"].as_array().map(Vec::len),
        Some(3),
        "the creator's, Alice's and Bob's identities"
    );
    let bob_identity = snap::ok(bob.engine.session().info()).await["principal"].clone();
    assert_eq!(
        snap::ok(
            creator
                .engine
                .session()
                .revoke_identity(&(snap::text(&(bob_identity))))
        )
        .await["revocations"]["statements"],
        1
    );
    creator.add(1, "After revocation").await;
    alice.sync().await;
    assert_eq!(
        snap::ok(alice.engine.session().info()).await["revocations"]["statements"],
        1,
        "Alice learns of Bob's revocation on her next sync"
    );
    assert_eq!(titles(&alice.items().await), ["After revocation"]);
    assert!(
        snap::observe(async {
            bob.engine.session().renew().await?;
            bob.engine.session().info().await
        })
        .await
        .is_err(),
        "renewal fetches the revocation a refused link need not carry"
    );
    assert!(
        snap::observe(bob.engine.session().sync_once())
            .await
            .is_err(),
        "a revoked device refuses to sync"
    );
    assert!(
        bob.items().await.is_empty(),
        "a revoked device reads nothing"
    );

    assert!(
        snap::observe(creator.engine.session().logout(false))
            .await
            .is_err(),
        "the creator cannot log out"
    );
    snap::ok(alice.engine.session().logout(true)).await;
    assert!(
        snap::observe(alice.engine.session().read(rocky_sdk::ReadQuery {
            sql: "SELECT 1".to_owned(),
            values: vec![],
            max_rows: 1,
            max_bytes: None,
            cursor: None
        }))
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
    assert!(Db::resume(&root.path().join("b"), iroh()).is_err());
    snap::ok(bob.engine.session().logout(false)).await;
    bob.close().await;
    assert!(Db::resume(&root.path().join("c"), iroh()).is_err());
    creator.close().await;
    Ok(())
}

#[tokio::test]
async fn an_application_transport_carries_enrollment_and_sync() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let open =
        |directory: &str, mode: rocky_sdk::EngineMode, name: &'static str| -> Result<Db, Failure> {
            let options = rocky_sdk::EngineOptions {
                directory: root.path().join(directory),
                transport: Arc::new(rocky_iroh::Options::default()),
                mode,
            };
            let network = Network::Owned(hub.connector(name));
            let engine = Engine::open_with(
                options,
                &rocky_sdk::Schema::parse(SCHEMA)?,
                None,
                Some(network),
            )?;
            let scope = engine.scope();

            Ok(Db { engine, scope })
        };
    let creator = open(
        "a",
        rocky_sdk::EngineMode::Create {
            credential_ttl: None,
            root: None,
        },
        "a",
    )?;
    assert_eq!(
        snap::ok(creator.engine.session().domain()).await["transport"],
        "memory"
    );
    let invitation = creator.invite().await;
    assert!(
        Db::join(&root.path().join("other"), &invitation, iroh()).is_err(),
        "a transport of another kind has no route in the invitation"
    );
    let member = open(
        "b",
        rocky_sdk::EngineMode::Join {
            invitation: snap::text(&(invitation)),
        },
        "b",
    )?;
    creator.add(1, "Milk").await;
    member.add(2, "Eggs").await;
    assert!(support::reconciled(&member.sync().await));
    assert_eq!(titles(&creator.items().await), ["Eggs", "Milk"]);
    assert_eq!(creator.items().await, member.items().await);

    member.close().await;
    creator.close().await;
    let creator = open("a", rocky_sdk::EngineMode::Resume, "a")?;
    let member = open("b", rocky_sdk::EngineMode::Resume, "b")?;
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
