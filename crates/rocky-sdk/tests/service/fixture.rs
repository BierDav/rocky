use super::*;
use rocky_sdk::{Opened, Session};
use std::path::Path;

pub(super) struct Peer {
    pub opened: Opened,
    identity: Arc<dyn rocky_sdk::IdentityProvider>,
    pub session: Session,
    pub scope: Value,
}
impl Peer {
    pub async fn start(
        directory: &Path,
        schema: &str,
        bundle: Option<&TrustBundle>,
        invitation: Option<String>,
        create: bool,
    ) -> Result<Self, Failure> {
        let directory = directory.to_owned();
        let schema = schema.to_owned();
        let bundle = bundle.cloned();
        let identity: Arc<dyn rocky_sdk::IdentityProvider> =
            Arc::new(rocky_sdk::IdentityKey::from_secret(rocky_core::commitment(
                "fixture.service.identity",
                directory.to_string_lossy().as_bytes(),
            )));
        let startup_identity = identity.clone();
        let opened = tokio::task::spawn_blocking(move || {
            let open = Open {
                directory: &directory,
                schema: &schema,
                identity: Some(startup_identity),
                secrets: None,
                network: Some(Network::Owned(Arc::new(starter()))),
                coordination: bundle.map(|bundle| Coordination::Service(Box::new(bundle))),
            };
            match invitation {
                Some(invitation) => Database::join(open, &invitation),
                None if create => Database::create(open, None),
                None => Database::open(open),
            }
        })
        .await??;
        Ok(Self {
            identity,
            session: opened.database.session(),
            scope: serde_json::from_str(&opened.database.scope())?,
            opened,
        })
    }
    /// The root a created peer holds, as root-only calls name it.
    pub fn root(&self) -> Value {
        self.opened
            .root
            .as_ref()
            .map_or(Value::Null, |root| json!({"phrase": root.phrase()}))
    }
    pub async fn call(&self, request: Value) -> Result<Value, Failure> {
        let reply: Value = serde_json::from_str(
            &self
                .session
                .call_with_identity(
                    json!({"version":1,"scope":self.scope,"request":request}).to_string(),
                    self.identity.clone(),
                )
                .await?,
        )?;
        if reply["status"] == "ok" {
            Ok(reply["value"].clone())
        } else {
            Err(format!("{} failed: {}", request["type"], reply["error"]).into())
        }
    }
    pub async fn tips(&self, action: &str, body: Value) -> Result<Value, Failure> {
        let mut request = json!({"type":"tips", "action":action});
        for (name, value) in body.as_object().into_iter().flatten() {
            request[name] = value.clone();
        }
        self.call(request).await
    }
    pub async fn sync(&self) -> Result<Value, Failure> {
        self.call(json!({"type":"sync"})).await
    }
    pub async fn invite(&self, kind: &str) -> Result<String, Failure> {
        let profile = if kind == "issuer" {
            json!({"kind":kind})
        } else {
            json!({"kind":kind,"scopes":["counts"]})
        };
        Ok(self
            .call(json!({"type": "invitation", "profile":profile}))
            .await?["uri"]
            .as_str()
            .ok_or("invitation")?
            .to_owned())
    }
    pub async fn write(&self, key: &str, handle: u32) -> Result<Value, Failure> {
        self.call(json!({"type":"sql","sql":"BEGIN"})).await?;
        let result = self.insert_and_commit(key, handle).await;
        if result.is_err() {
            let _ = self.call(json!({"type":"sql","sql":"ROLLBACK"})).await;
        }
        result
    }
    async fn insert_and_commit(&self, key: &str, handle: u32) -> Result<Value, Failure> {
        let id = self
            .call(json!({"type":"next_id","table":"counts"}))
            .await?["id"]
            .clone();
        self.call(
            json!({"type":"sql","sql":"INSERT INTO counts(key,value,id) VALUES(?,5,?)",
            "values":[{"type":"text","value":key},{"type":"integer","value":id}]}),
        )
        .await?;
        self.call(json!({"type":"sql","sql":"COMMIT",
            "handle":rocky_core::commitment("rocky.sdk.service.test", &handle.to_be_bytes())}))
            .await
    }
    pub async fn keys(&self) -> Result<Value, Failure> {
        Ok(self.call(json!({"type":"read","query":{"sql":"SELECT key FROM counts ORDER BY key","values":[],"max_rows":1024}})).await?["rows"].clone())
    }
    pub async fn close(self) -> Result<(), Failure> {
        self.session.close().await;
        if let Some(network) = self.opened.network {
            network.close().await;
        }
        self.opened.database.close().await
    }
}

pub(super) async fn introduce(a: &Peer, t: &Peer) -> Result<(), Failure> {
    for (left, right) in [(a, t), (t, a)] {
        let contact = right.call(json!({"type":"contact"})).await?["uri"].clone();
        left.call(json!({"type":"peer_add","contact":contact}))
            .await?;
    }
    Ok(())
}
pub(super) async fn household(root: &Path, bundle: &TrustBundle) -> Result<(Peer, Peer), Failure> {
    let a = Peer::start(&root.join("a"), SCHEMA, Some(bundle), None, true).await?;
    a.tips("create", json!({"name":"main"})).await?;
    a.tips("follow", json!({"name":"main"})).await?;
    a.tips("genesis_minimum", json!({"minimum_in_sync":1}))
        .await?;
    a.tips("keep", json!({"kept":{"every":true}})).await?;
    let t = Peer::start(
        &root.join("t"),
        SCHEMA,
        Some(bundle),
        Some(a.invite("issuer").await?),
        false,
    )
    .await?;
    let owner = a.call(json!({"type":"session"})).await?;
    let trusted = t.call(json!({"type":"session"})).await?;
    assert_ne!(owner["principal"], trusted["principal"]);
    for identity in [&owner, &trusted] {
        assert_ne!(identity["principal"], identity["node"]);
    }
    let grant = a
        .tips(
            "trust",
            json!({"identity":trusted["principal"], "root": a.root()}),
        )
        .await?;
    t.tips("install_trust", json!({"grant":grant["grant"]}))
        .await?;
    let tenant = PrivateKey::new();
    a.call(
        json!({"type":"coordination_register","tenant":tenant.to_hex(),"registration":vec![1u8;32],
        "trusted":[owner["principal"],trusted["principal"]],"lifetime":3600,"root":a.root()}),
    )
    .await?;
    introduce(&a, &t).await?;
    t.sync().await?;
    a.sync().await?;
    assert_eq!(
        t.call(json!({"type":"coordinated"})).await?["trusted"],
        true
    );
    a.session.set_automatic_sync(true).await?;
    t.session.set_automatic_sync(true).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let mut connected = true;
        for peer in [&a, &t] {
            let peers = peer.call(json!({"type":"peers"})).await?;
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
        let a_graph = a.tips("inspect", json!({})).await?;
        let t_graph = t.tips("inspect", json!({})).await?;
        if seal(&a_graph, id)["final"] == true && seal(&t_graph, id)["final"] == true {
            return Ok(());
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "seal not final: A={a_graph} T={t_graph}"
        );
    }
}
