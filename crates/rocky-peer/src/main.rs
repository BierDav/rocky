use rocky_sdk::Failure;

#[cfg(target_family = "wasm")]
fn main() -> Result<(), Failure> {
    Err("rocky-peer runs natively".into())
}

#[cfg(not(target_family = "wasm"))]
fn main() -> Result<(), Failure> {
    use rocky_sdk::CustodyPeer;
    let args: Vec<_> = std::env::args().skip(1).collect();
    let [config] = args.as_slice() else {
        return Err("usage: rocky-peer <configuration.json>".into());
    };
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Configuration {
        directory: std::path::PathBuf,
        schema: rocky_sdk::Schema,
        transport: rocky_iroh::Options,
        #[serde(default)]
        coordination: Option<rocky_coordinator::certified::TrustBundle>,
        #[serde(default)]
        invitation: Option<String>,
        #[serde(default)]
        peers: Vec<String>,
        #[serde(default)]
        enrollment_upstream: Option<String>,
    }
    let config: Configuration = serde_json::from_slice(&std::fs::read(config)?)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let coordinator_transport = config
        .coordination
        .as_ref()
        .map(|_| {
            runtime.block_on(coordinator_transport_on_an_ephemeral_port(
                &config.transport,
            ))
        })
        .transpose()?;
    let coordination = config
        .coordination
        .as_ref()
        .zip(coordinator_transport.as_ref())
        .map(|(bundle, transport)| {
            rocky_coordinator::Client::connect(bundle, transport.transport())
        })
        .transpose()?;
    let peer = CustodyPeer::open(rocky_sdk::CustodyConfiguration {
        coordination: coordination.map(rocky_sdk::CoordinatorSlot::Client),
        directory: config.directory,
        schema: config.schema,
        transport: std::sync::Arc::new(config.transport),
        invitation: config.invitation,
        peers: config.peers,
        enrollment_upstream: config.enrollment_upstream,
    })?;
    runtime.block_on(async {
        println!("{}", peer.contact().await?);
        #[cfg(unix)]
        {
            let mut terminate =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            tokio::select! {
                result = tokio::signal::ctrl_c() => result?,
                _ = terminate.recv() => {},
            }
        }
        #[cfg(not(unix))]
        tokio::signal::ctrl_c().await?;
        let result = peer.close().await;
        if let Some(transport) = coordinator_transport {
            transport.transport().close().await;
        }
        result
    })
}

#[cfg(not(target_family = "wasm"))]
async fn coordinator_transport_on_an_ephemeral_port(
    options: &rocky_iroh::Options,
) -> Result<rocky_iroh::Attachment, Failure> {
    let mut options = options.clone();
    if let Some(bind) = &options.bind {
        let mut address: std::net::SocketAddr = bind.parse()?;
        address.set_port(0);
        options.bind = Some(address.to_string());
    }
    options.local = false;
    options.topic = false;
    options
        .start_serving(
            *rocky_sdk::credentials::PrivateKey::new().as_bytes(),
            vec![],
        )
        .await
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn coordination_leaves_the_fixed_custody_port_available() -> Result<(), Failure> {
        let reserved = std::net::UdpSocket::bind("127.0.0.1:0")?;
        let bind = reserved.local_addr()?;
        drop(reserved);
        let options = rocky_iroh::Options::isolated(&bind.to_string());
        let coordinator = coordinator_transport_on_an_ephemeral_port(&options).await?;
        let custody = options.start([92; 32]).await?;
        assert!(
            custody
                .endpoint()
                .addr()
                .ip_addrs()
                .any(|address| *address == bind)
        );
        assert!(
            coordinator
                .endpoint()
                .addr()
                .ip_addrs()
                .all(|address| address.port() != bind.port())
        );
        custody.close().await;
        coordinator.close().await;
        Ok(())
    }
}
