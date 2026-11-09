use p2panda_core::PrivateKey;
use rocky_coordinator::{certified::*, service::NetworkCoordinator};
use rocky_core::*;
use rocky_credentials::{
    CoordinationGrant, CoordinatorControl, Publisher, SignedCoordinationGrant,
};
use rocky_iroh::iroh::{Endpoint, RelayMode, RelayUrl, SecretKey, endpoint::presets};
use std::path::Path;

/// A real tenant registration and cleanup through a relay-only client.
pub(super) async fn run(
    bundle: &Path,
    relay: &str,
    key: &Path,
) -> Result<(), super::voter::Failure> {
    let bundle = TrustBundle::from_json(&std::fs::read_to_string(bundle)?)?;
    let genesis = bundle.genesis()?;
    let key: PrivateKey = serde_json::from_value(serde_json::Value::String(
        std::fs::read_to_string(key)?.trim().into(),
    ))?;
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::generate())
        .clear_ip_transports()
        .relay_mode(RelayMode::Custom(relay.parse::<RelayUrl>()?.into()))
        .bind()
        .await?;
    let attachment = rocky_iroh::Attachment::serving(endpoint, Vec::new());
    let transport = attachment.transport();
    let result = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        let client = NetworkCoordinator::connect(&bundle, key.clone(), transport.clone())?;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
        let expires = now + genesis.genesis.max_control_lifetime.min(3600);
        let random = PrivateKey::new();
        let app = ApplicationManifest::app_id(*key.public_key().as_bytes(), "connection-check");
        let scope = Scope {
            mesh: MeshId(*random.as_bytes().first_chunk::<16>().ok_or("random mesh")?),
            app,
            domain: DomainId(*random.as_bytes().last_chunk::<16>().ok_or("random domain")?),
        };
        let grant = SignedCoordinationGrant::sign(CoordinationGrant {
            publisher: Publisher(key.public_key()),
            application: "connection-check".into(),
            tenant: CoordinatorControl(key.public_key()),
            not_before: now,
            expires,
        }, &key)?;
        let certificate = ControlCertificate::sign(DomainControl {
            scope,
            configuration: genesis.genesis.configuration,
            revision: 1,
            trusted: vec![key.public_key()],
            publisher: None,
            not_before: now,
            expires,
        }, &key)?;
        client.register(grant, None, certificate, commitment("rocky.check.register", random.as_bytes())).await?;
        let usage = client.usage().await?;
        client.close_domain(scope, commitment("rocky.check.close", random.as_bytes())).await?;
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({"relay": relay, "registered": true, "closed": true, "usage": usage}))?);
        Ok::<_, super::voter::Failure>(())
    }).await;
    transport.close().await;
    result??;
    Ok(())
}
