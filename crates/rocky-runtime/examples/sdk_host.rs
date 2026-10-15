//! Trusted inventory fixture for the Kotlin/JVM qualification consumer.
use inventory_app::{InventoryApp, Policy};
use rocky_core::{DomainId, MeshId, NodeId, PrincipalId, Scope};
use rocky_runtime::{Identity, Replica, sdk::Session};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::{BufRead, Write},
    sync::Arc,
};
use tokio::sync::{Semaphore, mpsc};

#[derive(Deserialize)]
struct Message {
    id: String,
    call: Value,
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os().nth(1).ok_or("provide database path")?;
    let scope = Scope {
        mesh: MeshId([11; 16]),
        app: inventory_app::app_id(),
        domain: DomainId([1; 16]),
    };
    let caller = PrincipalId([7; 16]);
    let node = NodeId([3; 16]);
    let replica = Replica::open(
        path,
        InventoryApp,
        Policy {
            scope,
            caller,
            reference: [9; 32],
            note_prefix: None,
        },
        Identity {
            scope,
            node,
            caller,
        },
    )
    .await?;
    let session = Arc::new(Session::new(replica.clone()));
    let (input, mut messages) = mpsc::channel::<Message>(32);
    let (output, mut replies) = mpsc::channel::<Value>(32);
    output
        .send(json!({"scope":scope,"version":rocky_runtime::sdk::VERSION}))
        .await?;
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut reader = stdin.lock();
        loop {
            let mut bytes = Vec::new();
            let mut limited =
                std::io::Read::take(&mut reader, rocky_runtime::MAX_FRAME_BYTES as u64 + 1);
            match limited.read_until(b'\n', &mut bytes) {
                Ok(0) | Err(_) => break,
                Ok(_) if bytes.len() > rocky_runtime::MAX_FRAME_BYTES => break,
                Ok(_) => {}
            }
            let Ok(message) = serde_json::from_slice(&bytes) else {
                break;
            };
            if input.blocking_send(message).is_err() {
                break;
            }
        }
    });
    let writer = std::thread::spawn(move || -> Result<(), std::io::Error> {
        let mut stdout = std::io::stdout().lock();
        while let Some(reply) = replies.blocking_recv() {
            serde_json::to_writer(&mut stdout, &reply)?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
        }
        Ok(())
    });
    let local = tokio::task::LocalSet::new();
    let budget = Arc::new(Semaphore::new(128));
    local
        .run_until(async {
            let mut tasks = tokio::task::JoinSet::new();
            while let Some(message) = messages.recv().await {
                let permit = budget
                    .clone()
                    .acquire_owned()
                    .await
                    .map_err(|_| "host closed")?;
                let session = session.clone();
                let output = output.clone();
                tasks.spawn_local(async move {
                    let _permit = permit;
                    let encoded = serde_json::to_vec(&message.call)?;
                    let result = session.dispatch_json(&encoded).await;
                    let reply: Value = serde_json::from_slice(&result)?;
                    output.send(json!({"id":message.id,"reply":reply})).await?;
                    Ok::<_, Box<dyn std::error::Error>>(())
                });
                // Do not retain completed task handles for the lifetime of a session.
                while let Some(task) = tasks.try_join_next() {
                    task??;
                }
            }
            session.close()?;
            while let Some(task) = tasks.join_next().await {
                task??;
            }
            Ok::<_, Box<dyn std::error::Error>>(())
        })
        .await?;
    replica.close().await?;
    drop(output);
    writer.join().map_err(|_| "stdout thread failed")??;
    Ok(())
}
