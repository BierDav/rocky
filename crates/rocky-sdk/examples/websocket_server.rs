#[cfg(target_family = "wasm")]
fn main() {}

#[cfg(not(target_family = "wasm"))]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    native::main().await
}

#[cfg(not(target_family = "wasm"))]
mod native {
    use http_body_util::Full;
    use hyper::{
        Request, Response, StatusCode,
        body::{Bytes, Incoming},
        header::{
            CONNECTION, CONTENT_TYPE, HeaderValue, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_KEY, UPGRADE,
        },
        server::conn::http1,
        service::service_fn,
    };
    use hyper_util::rt::TokioIo;
    use rocky_sdk::{
        Access, Affinity, Column, Configuration, IdentityKey, Invite, Network, Schema, Table,
        params,
    };
    use rocky_tcp::Attachment;
    use std::{
        convert::Infallible,
        path::{Path, PathBuf},
        sync::Arc,
    };
    use tokio::net::TcpListener;

    type Failure = Box<dyn std::error::Error + Send + Sync>;
    const SYNC_PATH: &str = "/rocky";
    const WEB_APP: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/browser-notes/www"
    );

    pub async fn main() -> Result<(), Failure> {
        let mut arguments = std::env::args().skip(1);
        let address = arguments.next().unwrap_or_else(|| "127.0.0.1:8080".into());
        let directory = PathBuf::from(
            arguments
                .next()
                .unwrap_or_else(|| "local/notes-server".into()),
        );
        let listener = TcpListener::bind(&address).await?;
        let route = format!("ws://{address}{SYNC_PATH}");
        let attachment = Attachment::external(route.clone());
        tokio::spawn(serve(listener, attachment.clone()));

        let schema = Schema::builder("notes", 1)
            .table(Table::replayed("notes").column(Column::new("body", Affinity::Text).required()))
            .build()?;
        let configuration = Configuration::new(&directory, &schema)?
            .network(Network::Borrowed(attachment.transport()));
        let owner_phrase = directory.with_extension("owner-phrase");
        let admin = match std::fs::read_to_string(&owner_phrase) {
            Ok(phrase) => {
                let owner = IdentityKey::from_phrase(phrase.trim())?;
                configuration.identity(Arc::new(owner)).open().await?
            }
            Err(_) => {
                let created = configuration.create().await?;
                std::fs::write(&owner_phrase, created.owner.phrase())?;
                let insert = "INSERT INTO notes(body) VALUES(?)";
                created
                    .database
                    .write(async |tx| {
                        tx.execute(insert, params!["served beside the web app"])
                            .await
                    })
                    .await?;
                created.database
            }
        };
        let invitation = admin.invite(Invite::new(Access::Full)).await?;
        println!("Rocky syncs at {route}; open the web app, valid ten minutes:");
        println!("http://{address}/#{}", invitation.code);

        tokio::signal::ctrl_c().await?;
        admin.close().await?;
        attachment.close().await;
        Ok(())
    }

    async fn serve(listener: TcpListener, attachment: Attachment) {
        while let Ok((stream, _)) = listener.accept().await {
            let attachment = attachment.clone();
            tokio::spawn(async move {
                let service = service_fn(move |request| route(request, attachment.clone()));
                let _ = http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .with_upgrades()
                    .await;
            });
        }
    }

    async fn route(
        mut request: Request<Incoming>,
        attachment: Attachment,
    ) -> Result<Response<Full<Bytes>>, Infallible> {
        let upgrade = request
            .headers()
            .get(UPGRADE)
            .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"websocket"));
        let key = request.headers().get(SEC_WEBSOCKET_KEY).cloned();
        let accept = key.and_then(|key| {
            HeaderValue::from_str(&rocky_tcp::websocket_accept_key(key.as_bytes())).ok()
        });
        let (true, SYNC_PATH, Some(accept)) = (upgrade, request.uri().path(), accept) else {
            return Ok(web_app(request.uri().path()));
        };
        let upgrading = hyper::upgrade::on(&mut request);
        tokio::spawn(async move {
            if let Ok(upgraded) = upgrading.await {
                let _ = attachment.admit_websocket(TokioIo::new(upgraded)).await;
            }
        });
        let mut response = Response::new(Full::new(Bytes::new()));
        *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
        let headers = response.headers_mut();
        headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
        headers.insert(CONNECTION, HeaderValue::from_static("Upgrade"));
        headers.insert(SEC_WEBSOCKET_ACCEPT, accept);
        Ok(response)
    }

    fn web_app(path: &str) -> Response<Full<Bytes>> {
        let file = match path {
            "/" => "index.html",
            path => path.trim_start_matches('/'),
        };
        let kind = match file.rsplit('.').next() {
            Some("html") => "text/html",
            Some("js") => "text/javascript",
            Some("wasm") => "application/wasm",
            _ => "application/octet-stream",
        };
        let read = (!file.contains("..")).then(|| std::fs::read(Path::new(WEB_APP).join(file)));
        let Some(Ok(body)) = read else {
            let mut missing = Response::new(Full::default());
            *missing.status_mut() = StatusCode::NOT_FOUND;
            return missing;
        };
        let mut response = Response::new(Full::new(body.into()));
        let headers = response.headers_mut();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static(kind));
        response
    }
}
