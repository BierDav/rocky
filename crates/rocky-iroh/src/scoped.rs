//! The first frame routes a stream before the database sees any bytes.
use super::*;

const VERSION: u32 = 1;
const MAX_HEADER: usize = 512;

pub(super) struct Route {
    pub scope: std::sync::Mutex<Option<Scope>>,
    pub sender: mpsc::Sender<Incoming>,
    pub incoming: Mutex<mpsc::Receiver<Incoming>>,
    pub listening: AtomicBool,
    pub closed: watch::Sender<bool>,
    pub finder: Option<discovery::Finder>,
}
impl Route {
    pub fn new() -> Self {
        Self::with_finder(None)
    }
    pub fn with_finder(finder: Option<discovery::Finder>) -> Self {
        let (sender, incoming) = mpsc::channel(PENDING);
        Self {
            scope: Default::default(),
            sender,
            incoming: Mutex::new(incoming),
            listening: AtomicBool::new(false),
            closed: watch::channel(false).0,
            finder,
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ScopeFrame {
    version: u32,
    scope: Scope,
    lane: u8,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Accepted {
    version: u32,
    result: Result<(), ScopeRefusal>,
}

pub(super) async fn outbound(
    io: &mut impl SessionStream,
    scope: Option<Scope>,
    lane: u8,
) -> Result<(), Failure> {
    let scope = scope.ok_or(ScopeRefusal::UnknownScope)?;
    rocky_platform::time::timeout(TIMEOUT, async {
        framing::write(
            io,
            &rocky_credentials::encode(&ScopeFrame {
                version: VERSION,
                scope,
                lane,
            })?,
        )
        .await?;
        let reply: Accepted = rocky_credentials::decode_bounded(
            &framing::read_bounded(io, MAX_HEADER).await?,
            MAX_HEADER,
        )?;
        if reply.version != VERSION {
            return Err(ScopeRefusal::UnsupportedVersion.into());
        }
        reply.result.map_err(Into::into)
    })
    .await?
}

pub(super) async fn inbound(
    io: &mut impl SessionStream,
    shared: &Shared,
    lane: u8,
) -> Result<Arc<Route>, Failure> {
    let frame: ScopeFrame = rocky_credentials::decode_bounded(
        &framing::read_bounded(io, MAX_HEADER).await?,
        MAX_HEADER,
    )?;
    let result = if frame.version != VERSION || frame.lane != lane {
        Err(ScopeRefusal::UnsupportedVersion)
    } else {
        let route = shared
            .routes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&Some(frame.scope))
            .and_then(std::sync::Weak::upgrade);
        match route {
            None => Err(ScopeRefusal::UnknownScope),
            Some(route) if !route.listening.load(Ordering::SeqCst) || *route.closed.borrow() => {
                Err(ScopeRefusal::NotListening)
            }
            Some(route) if route.sender.capacity() == 0 => Err(ScopeRefusal::Busy),
            Some(route) => Ok(route),
        }
    };
    framing::write(
        io,
        &rocky_credentials::encode(&Accepted {
            version: VERSION,
            result: result.as_ref().map(|_| ()).map_err(Clone::clone),
        })?,
    )
    .await?;
    if result.is_err() {
        shared.refused.fetch_add(1, Ordering::Relaxed);
        deliver_before_close(io).await?;
    }
    result.map_err(Into::into)
}

/// Finishes `io` and waits for the peer's end, so a typed refusal arrives
/// before its dedicated link connection closes.
async fn deliver_before_close(io: &mut impl SessionStream) -> Result<(), Failure> {
    tokio::io::AsyncWriteExt::shutdown(io).await?;
    let mut rest = [0; 1];
    let _ = tokio::io::AsyncReadExt::read(io, &mut rest).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn only_current_alpns_are_accepted() {
        assert_eq!(protocol_named(b"rocky/rpc"), Some(Protocol::Rpc));
        assert_eq!(protocol_named(b"rocky/link"), Some(Protocol::Link));
        assert_eq!(
            protocol_named(b"rocky/coordinator/3"),
            Some(Protocol::Coordinate)
        );
        for old in [
            b"rocky/sdk-pair/1".as_slice(),
            b"rocky/sdk-renew/2",
            b"rocky/link/5",
        ] {
            assert_eq!(protocol_named(old), None);
        }
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test]
    async fn invalid_scope_versions_are_refused_before_dispatch() -> Result<(), Failure> {
        let options = Options::isolated("127.0.0.1:0");
        let server = options.start([81; 32]).await?;
        let port = server.transport();
        let scope = Scope {
            mesh: rocky_core::MeshId([1; 16]),
            app: rocky_core::AppId([2; 16]),
            domain: rocky_core::DomainId([3; 16]),
        };
        port.listen(Some(scope), true)?;
        let client = options.bind([82; 32]).await?;
        let connection = client
            .connect(server.endpoint().addr(), alpn(Protocol::Rpc))
            .await?;
        let (send, recv) = connection.open_bi().await?;
        let mut stream = tokio::io::join(recv, send);
        framing::write(
            &mut stream,
            &rocky_credentials::encode(&ScopeFrame {
                version: VERSION + 1,
                scope,
                lane: 0,
            })?,
        )
        .await?;
        let reply: Accepted = rocky_credentials::decode_bounded(
            &framing::read_bounded(&mut stream, MAX_HEADER).await?,
            MAX_HEADER,
        )?;
        assert_eq!(reply.result, Err(ScopeRefusal::UnsupportedVersion));
        tokio::io::AsyncWriteExt::shutdown(&mut stream).await?;
        assert!(
            server
                .0
                .routes
                .lock()
                .unwrap()
                .get(&Some(scope))
                .and_then(std::sync::Weak::upgrade)
                .unwrap()
                .incoming
                .try_lock()
                .unwrap()
                .try_recv()
                .is_err()
        );
        let duplicate = server.transport();
        assert!(duplicate.listen(Some(scope), true).is_err());
        server.close().await;
        client.close().await;
        Ok(())
    }
}
