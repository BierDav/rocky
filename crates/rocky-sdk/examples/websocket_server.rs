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
        header::{CONNECTION, HeaderValue, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_KEY, UPGRADE},
        server::conn::http1,
        service::service_fn,
    };
    use hyper_util::rt::TokioIo;
    use rocky_sdk::{
        Access, Affinity, Column, Configuration, Invite, Network, Schema, Table, params,
    };
    use rocky_tcp::Attachment;
    use std::convert::Infallible;
    use tokio::net::TcpListener;

    type Failure = Box<dyn std::error::Error + Send + Sync>;
    const SYNC_PATH: &str = "/rocky";

    pub async fn main() -> Result<(), Failure> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let route = format!("ws://{address}{SYNC_PATH}");
        let attachment = Attachment::external(route.clone());
        tokio::spawn(serve(listener, attachment.clone()));
        println!("The web app at http://{address}/ syncs Rocky at {route}");

        let schema = Schema::builder("notes", 1)
            .table(Table::replayed("notes").column(Column::new("body", Affinity::Text).required()))
            .build()?;
        let directory = tempfile::tempdir()?;
        let admin = Configuration::new(directory.path().join("admin"), &schema)?
            .network(Network::Borrowed(attachment.transport()))
            .create()
            .await?
            .database;
        let mut transaction = admin.transaction().await?;
        transaction
            .execute(
                "INSERT INTO notes(body) VALUES(?)",
                params!["served beside the web app"],
            )
            .await?;
        transaction.commit().await?;
        let invitation = admin.invite(Invite::new(Access::Full)).await?;

        let laptop = Configuration::new(directory.path().join("laptop"), &schema)?
            .network(rocky_tcp::Options::dialer().into())
            .join(&invitation.code)
            .await?
            .database;
        println!("The dial-only laptop's sync: {}", laptop.sync_now().await?);
        let rows = laptop
            .query("SELECT body FROM notes", params![])
            .await?
            .collect()
            .await?;
        let note: String = rows.first().ok_or("the laptop has no note")?.get("body")?;
        println!("The laptop reads: {note}");

        laptop.close().await?;
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
            return Ok(Response::new(Full::new(Bytes::from_static(
                b"The web app; Rocky syncs at /rocky\n",
            ))));
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
}
