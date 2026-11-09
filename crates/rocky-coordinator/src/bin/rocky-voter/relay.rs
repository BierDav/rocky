use iroh_relay::server::{ClientRateLimit, RelayConfig, Server, ServerConfig};
use std::{net::SocketAddr, num::NonZeroU32};

async fn start(address: SocketAddr) -> Result<Server, super::voter::Failure> {
    let mut relay = RelayConfig::new(address);
    // Bound each client's traffic; admission remains open to independent keys.
    relay.limits.client_rx = Some(ClientRateLimit::new(
        NonZeroU32::new(1024 * 1024).ok_or("relay rate must be positive")?,
    ));
    let mut config = ServerConfig::default();
    config.relay = Some(relay);
    Ok(Server::spawn(config).await?)
}

pub(super) async fn serve(address: SocketAddr) -> Result<(), super::voter::Failure> {
    let mut server = start(address).await?;
    eprintln!(
        "rocky relay: HTTP at {}; terminate TLS at the proxy",
        server.http_addr().ok_or("relay listener missing")?
    );
    tokio::select! {
        result = server.join() => { result??; }
        result = tokio::signal::ctrl_c() => {
            result?;
            server.shutdown().await?;
        }
        result = terminate() => {
            result?;
            server.shutdown().await?;
        }
    }
    Ok(())
}

#[cfg(unix)]
async fn terminate() -> std::io::Result<()> {
    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?
        .recv()
        .await;
    Ok(())
}
#[cfg(not(unix))]
async fn terminate() -> std::io::Result<()> {
    std::future::pending().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocky_iroh::iroh::{Endpoint, RelayMode, SecretKey, endpoint::presets};

    #[tokio::test]
    async fn relay_carries_authenticated_quic_without_ip_transports()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = start("127.0.0.1:0".parse()?).await?;
        let url: rocky_iroh::iroh::RelayUrl =
            format!("http://{}", server.http_addr().ok_or("listener missing")?).parse()?;
        let endpoint = || {
            Endpoint::builder(presets::Minimal)
                .secret_key(SecretKey::generate())
                .clear_ip_transports()
                .relay_mode(RelayMode::Custom(url.clone().into()))
                .alpns(vec![b"rocky-relay-test".to_vec()])
                .bind()
        };
        let (a, b) = tokio::try_join!(endpoint(), endpoint())?;
        let result = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            tokio::join!(a.online(), b.online());
            let address = b.addr();
            let receiver = async {
                let connection = b.accept().await.ok_or("no incoming connection")?.await?;
                let (mut send, mut recv) = connection.accept_bi().await?;
                let bytes = recv.read_to_end(64).await?;
                assert_eq!(bytes, b"coordinator request");
                send.write_all(b"signed reply").await?;
                send.finish()?;
                connection.closed().await;
                Ok::<_, Box<dyn std::error::Error>>(())
            };
            let sender = async {
                let connection = a.connect(address, b"rocky-relay-test").await?;
                let (mut send, mut recv) = connection.open_bi().await?;
                send.write_all(b"coordinator request").await?;
                send.finish()?;
                assert_eq!(recv.read_to_end(64).await?, b"signed reply");
                connection.close(0u32.into(), b"done");
                Ok::<_, Box<dyn std::error::Error>>(())
            };
            tokio::try_join!(receiver, sender)?;
            Ok::<_, Box<dyn std::error::Error>>(())
        })
        .await;
        a.close().await;
        b.close().await;
        server.shutdown().await?;
        result??;
        Ok(())
    }
}
