//! The native side of `scripts/browser-sync`: a plain-HTTP iroh relay on
//! loopback, which a browser reaches without a trusted certificate, a public
//! coordination service of three voters whose client lanes it reaches through
//! that relay, and the domain owner's native node behind it. It prints the
//! relay, an invitation and the service's bundle as one JSON line, grants the
//! browser's identity a claim right for the `invoices` counter once it asks,
//! never trusting it on the service, serves until a line arrives on stdin,
//! then checks that the browser node's writes and claims reached it.
#[cfg(target_family = "wasm")]
fn main() {}

#[cfg(not(target_family = "wasm"))]
#[tokio::main]
async fn main() -> Result<(), rocky_sdk::Failure> {
    native::main().await
}

#[cfg(not(target_family = "wasm"))]
mod native {
    use iroh_relay::server::{RelayConfig, Server, ServerConfig};
    use rocky_coordinator::{
        certified::*,
        service::{NetworkCoordinator, VoterService},
    };
    use rocky_core::{MeshId, net::Protocol};
    use rocky_sdk::{
        Affinity, Column, Configuration, Coordination, Database, Failure, GrantProfile, Network,
        Schema, Table, counters, credentials::PrivateKey, iroh, params,
        transport::TransportOptions,
    };
    use serde_json::{Value, json};
    use std::{collections::BTreeSet, net::Ipv4Addr, path::Path, sync::Arc, time::Duration};

    pub async fn main() -> Result<(), Failure> {
        let mut config = ServerConfig::default();
        config.relay = Some(RelayConfig::new((Ipv4Addr::LOCALHOST, 0)));
        let relay = Server::spawn(config).await?;
        let url = format!(
            "http://{}",
            relay.http_addr().ok_or("the relay has no address")?
        );
        let directory = tempfile::tempdir()?;
        let bundle = service(directory.path(), &url).await?;

        let schema = Schema::builder("browser_sync", 1)
            .table(Table::replayed("notes").column(Column::new("body", Affinity::Text).required()))
            .table(
                Table::replayed("invoices")
                    .column(Column::new("claim", Affinity::Text).required())
                    .column(Column::new("number", Affinity::Integer).required()),
            )
            .build()?;
        let network = TransportOptions::Iroh {
            relay: Some(url.clone()),
            lookup: Some("disabled".into()),
            topic: false,
            local: false,
            bind: None,
        };
        let created = Configuration::new(directory.path().join("owner"), &schema)?
            .network(Network::Owned(Arc::new(network)))
            .coordination(Coordination::Service(Box::new(bundle.clone())))
            .create()
            .await?;
        let root = created.root.ok_or("a created domain returns its root")?;
        let db = created.database;
        register(&db, &root.phrase()).await?;
        let mut tx = db.transaction().await?;
        let id = tx.next_id("notes", vec![]).await?;
        tx.execute(
            "INSERT INTO notes(id,body) VALUES(?,?)",
            params![id, "from native"],
        )
        .await?;
        tx.commit().await?;
        let invitation = db
            .session()
            .pairing(GrantProfile::Writer {
                scopes: ["notes".into(), "invoices".into()].into(),
            })
            .await?;
        println!(
            "{}",
            json!({ "relay": url, "invitation": invitation.uri, "bundle": bundle })
        );

        // The browser node joins, syncs, asks for a claim right, claims and
        // writes, syncing each time, before the script sends the line.
        let granting = tokio::spawn(grant_members(db.clone()));
        tokio::task::spawn_blocking(|| std::io::stdin().read_line(&mut String::new())).await??;
        // It only ends early on a failure.
        if granting.is_finished() {
            granting.await??;
        } else {
            granting.abort();
        }
        let checked = check(&db).await;
        db.close().await?;
        checked?;
        eprintln!("browser-peer: the browser's write and claims arrived over the relay");
        Ok(())
    }

    /// Registers the domain with the owner's identity as its one trusted
    /// one; no later control revision trusts another. Registering without
    /// a coordination grant needs the root.
    async fn register(db: &Database, root: &str) -> Result<(), Failure> {
        let tenant = PrivateKey::new();
        let principal = call(db, json!({"type": "session"})).await?["principal"].clone();
        call(
            db,
            json!({"type": "coordination_register", "tenant": tenant.to_hex(),
                "registration": vec![1u8; 32], "trusted": [principal], "lifetime": 3600,
                "root": {"phrase": root}}),
        )
        .await?;
        Ok(())
    }

    /// Grants each identity that writes `member <principal>` a claim right
    /// for `invoices`, and answers `granted <principal>`.
    async fn grant_members(db: Arc<Database>) -> Result<(), Failure> {
        let mut granted = Vec::new();
        let session = db.session();
        loop {
            let rows = db
                .query(
                    "SELECT body FROM notes WHERE body LIKE 'member %'",
                    params![],
                )
                .await?
                .collect()
                .await?;
            for row in rows {
                let body = row.get::<String>("body")?;
                let member = body.trim_start_matches("member ").to_owned();
                if granted.contains(&member) {
                    continue;
                }
                session
                    .grant_claims(&member, Some(&[b"invoices"]), 1)
                    .await?;
                let mut tx = db.transaction().await?;
                let id = tx.next_id("notes", vec![]).await?;
                tx.execute(
                    "INSERT INTO notes(id,body) VALUES(?,?)",
                    params![id, format!("granted {member}")],
                )
                .await?;
                tx.commit().await?;
                granted.push(member);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// The browser's write arrived, and this node sees the counter the
    /// browser claimed from: its claim took 1, and the next claim here is 3.
    async fn check(db: &Database) -> Result<(), Failure> {
        let rows = db
            .query(
                "SELECT body FROM notes WHERE body LIKE 'from %' ORDER BY body",
                params![],
            )
            .await?
            .collect()
            .await?;
        let bodies = rows
            .iter()
            .map(|row| row.get::<String>("body"))
            .collect::<Result<Vec<_>, _>>()?;
        if bodies != ["from browser", "from native"] {
            return Err(format!("the native node holds {bodies:?}").into());
        }
        let rows = db
            .query("SELECT claim, number FROM invoices", params![])
            .await?
            .collect()
            .await?;
        let [row] = rows.as_slice() else {
            return Err(format!("the native node holds {} invoices", rows.len()).into());
        };
        let claim: counters::Claim = serde_json::from_str(&row.get::<String>("claim")?)?;
        let number = u64::try_from(row.get::<i64>("number")?)?;
        let invoices = counters::Name::new("invoices")?;
        let reading = db.counter(invoices.clone(), Some(claim)).await?;
        if (number, reading.value, reading.claim) != (1, 2, Some(counters::Lookup::Claimed(1))) {
            return Err(format!("invoice {number} and the counter at {reading:?}").into());
        }
        let next = db.claim(invoices, counters::Claim::fresh()?).await?;
        if next != 3 {
            return Err(format!("the native claim took {next}").into());
        }
        Ok(())
    }

    async fn call(db: &Database, request: Value) -> Result<Value, Failure> {
        let scope: Value = serde_json::from_str(&db.scope())?;
        let reply: Value = serde_json::from_str(
            &db.session()
                .call(json!({"version": 1, "scope": scope, "request": request}).to_string())
                .await?,
        )?;
        if reply["status"] == "ok" {
            Ok(reply["value"].clone())
        } else {
            Err(format!("{} failed: {}", request["type"], reply["error"]).into())
        }
    }

    /// Three voters of a public service with open admission, whose client
    /// lanes a browser reaches through the relay at `relay`, and its bundle.
    async fn service(directory: &Path, relay: &str) -> Result<TrustBundle, Failure> {
        let root = PrivateKey::new();
        let admin = PrivateKey::new();
        let keys: Vec<_> = (0..3).map(|_| PrivateKey::new()).collect();
        let secrets: Vec<[u8; 32]> = (0..3).map(|_| *PrivateKey::new().as_bytes()).collect();
        let mut voters = Vec::new();
        let mut endpoints = Vec::new();
        for (index, key) in keys.iter().enumerate() {
            let id = index as u64 + 1;
            let tcp = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?;
            voters.push(Voter {
                id,
                address: tcp.to_string(),
                key: key.public_key(),
            });
            let address = iroh::iroh::EndpointAddr::new(
                iroh::iroh::SecretKey::from_bytes(&secrets[index]).public(),
            )
            .with_relay_url(relay.parse()?);
            endpoints.push(ClientEndpoint {
                voter: id,
                kind: "iroh".into(),
                address: serde_json::to_string(&address)?,
            });
        }
        let genesis = CertifiedGenesis::sign(
            Genesis {
                mesh: MeshId([0x62; 16]),
                configuration: [8; 16],
                initial_voters: BTreeSet::from([1, 2, 3]),
                membership_authority: admin.public_key(),
                directory_definition: [0; 32],
                directory: None,
                authorities: Vec::new(),
                max_control_lifetime: 86_400,
                public: Some(PublicService {
                    endpoints,
                    admission: None,
                    limits: rocky_coordinator::Limits::default(),
                    tenants: 4,
                    registrations_per_day: 16,
                    tenant_lanes: 4,
                }),
                voters,
            },
            &root,
        )?;
        for (index, key) in keys.into_iter().enumerate() {
            let id = index as u64 + 1;
            let voter = VoterService::open(
                &directory.join(format!("voter-{id}")),
                genesis.clone(),
                root.public_key(),
                genesis.genesis.mesh,
                id,
                key,
                true,
            )
            .await?;
            let lane = iroh::Options {
                relay: Some(relay.into()),
                lookup: Some("disabled".into()),
                topic: false,
                local: false,
                bind: Some("127.0.0.1:0".into()),
            }
            .start_serving(secrets[index], vec![Protocol::Coordinate])
            .await?;
            tokio::spawn(voter.clone().serve_clients(lane.transport()));
            tokio::spawn(voter.serve());
        }
        let admin = NetworkCoordinator::open(
            genesis.clone(),
            root.public_key(),
            genesis.genesis.mesh,
            admin,
        )?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while admin.initialize(1).await.is_err() {
            if tokio::time::Instant::now() > deadline {
                return Err("the voters never started".into());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        while !admin.status(1).await.is_ok_and(|s| s.leader.is_some()) {
            if tokio::time::Instant::now() > deadline {
                return Err("the voters elected no leader".into());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok(TrustBundle::new(&genesis)?)
    }
}
