use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize)]
struct Peer {
    path: std::path::PathBuf,
    schema: rocky_core::LogicalSchema,
    domain: rocky_core::Scope,
    publisher: PublicKey,
    manifest: SignedManifest,
    policy: RevocationSet,
    node: [u8; 32],
    credentials: CredentialChain,
    keys: Vec<(ResourceScope, rocky_core::KeyEpoch, [u8; 32])>,
}
#[derive(Serialize, Deserialize)]
enum Action {
    Receive(Vec<EncryptedFrame>),
    Edit(RowWrite, String),
    Move(i64, i64),
    Evict(ResourceScope),
}
#[derive(Serialize, Deserialize)]
struct Reply {
    frames: Vec<EncryptedFrame>,
    snapshots: Vec<(ResourceScope, BTreeMap<Commitment, RowFields>)>,
    error: Option<String>,
    follows: usize,
}
impl Peer {
    async fn capture(f: &Fixture, replica: &EncryptedReplica<TypedRows>, path: &Path) -> Self {
        let inner = replica.inner.lock().await;
        Self {
            path: path.to_owned(),
            schema: f.schema.clone(),
            domain: f.domain,
            publisher: inner.config.publisher,
            manifest: inner.config.manifest.clone(),
            policy: inner.config.revocations.clone(),
            node: *inner.config.node.as_bytes(),
            credentials: inner.config.credentials.clone(),
            keys: inner
                .config
                .keys
                .iter()
                .map(|key| (key.scope, key.generation, key.key))
                .collect(),
        }
    }
    fn run(&mut self, action: Action) -> Reply {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("request");
        let output = root.path().join("reply");
        std::fs::write(&input, encode(&(&self, &action)).unwrap()).unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::partition_foreign_keys::row_moves::process::peer_worker",
                "--ignored",
                "--nocapture",
            ])
            .env("ROCKY_MOVE_REQUEST", &input)
            .env("ROCKY_MOVE_REPLY", &output)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "peer failed: {} {}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        if let Action::Evict(scope) = action {
            self.keys.retain(|(held, _, _)| *held != scope);
        }
        decode_bounded(&std::fs::read(output).unwrap(), crate::MAX_BUNDLE_BYTES).unwrap()
    }
}

#[tokio::test]
#[ignore = "invoked by the process-isolated acceptance orchestrator"]
async fn peer_worker() {
    let input = std::env::var_os("ROCKY_MOVE_REQUEST").expect("peer request");
    let output = std::env::var_os("ROCKY_MOVE_REPLY").expect("peer reply");
    let (peer, action): (Peer, Action) =
        decode_bounded(&std::fs::read(input).unwrap(), crate::MAX_BUNDLE_BYTES).unwrap();
    let config = SecureConfig {
        publisher: peer.publisher,
        manifest: peer.manifest.clone(),
        revocations: peer.policy.clone(),
        node: PrivateKey::from_bytes(&peer.node),
        credentials: peer.credentials,
        keys: peer
            .keys
            .iter()
            .map(|(scope, generation, key)| KeyMaterial::at(*scope, *generation, *key))
            .collect(),
    };
    let f = Fixture {
        publisher: PrivateKey::new(),
        schema: peer.schema,
        domain: peer.domain,
        manifest: peer.manifest,
        revocations: peer.policy,
    };
    let replica = EncryptedReplica::open(&peer.path, f.app(), config)
        .await
        .unwrap();
    let mut error = None;
    match action {
        Action::Receive(frames) => send(&frames, &replica).await,
        Action::Edit(row, title) => {
            edit(&f, &replica, &row, &title, 200).await;
        }
        Action::Move(from, to) => {
            let scopes = peer.keys.iter().map(|(scope, _, _)| *scope).collect();
            let result = replica
                .session(scopes)
                .await
                .unwrap()
                .move_row(
                    [201; 32],
                    "items",
                    RowIdentity::new(1, 1).unwrap(),
                    &[SqlValue::Integer(from)],
                    &[SqlValue::Integer(to)],
                )
                .await;
            if let Err(refused) = result {
                error = Some(refused.to_string());
            }
        }
        Action::Evict(scope) => replica.evict_key(scope).await,
    }
    let mut snapshots = Vec::new();
    for (scope, _, _) in peer.keys {
        if let Ok(snapshot) = replica.snapshot(scope).await {
            snapshots.push((scope, snapshot.rows));
        }
    }
    let frames = crate::tests::stored_frames(
        &replica,
        "SELECT header,body FROM secure_operations ORDER BY sequence",
    )
    .await;
    let inner = replica.inner.lock().await;
    let follows = crate::storage::rows(&inner.conn, "SELECT * FROM secure_followed")
        .await
        .unwrap()
        .len();
    std::fs::write(
        output,
        encode(&Reply {
            frames,
            snapshots,
            error,
            follows,
        })
        .unwrap(),
    )
    .unwrap();
}
fn visible(reply: &Reply, scope: ResourceScope) -> &BTreeMap<Commitment, RowFields> {
    &reply
        .snapshots
        .iter()
        .find(|(held, _)| *held == scope)
        .unwrap()
        .1
}

#[tokio::test]
async fn six_scenarios_across_device_processes() {
    for scenario in 1..=6 {
        let f = Fixture::new();
        let root = tempfile::tempdir().unwrap();
        let a_path = root.path().join("a");
        let b_path = root.path().join("b");
        let partitions = [
            RowIdentity::new(1, 1).unwrap().integer(),
            RowIdentity::new(1, 2).unwrap().integer(),
        ];
        let a = phone(&f, &a_path, 1, &partitions).await;
        let b = phone(&f, &b_path, 2, &partitions).await;
        let row = setup(&f, &a, &b, &partitions).await;
        if scenario >= 5 {
            add_related(&f, &a, &b, partitions[0]).await;
            let mut change = row.change.clone();
            change.before = change.after.clone();
            let logical = f.schema.table("items").unwrap();
            let category = f
                .schema
                .field_id(logical, logical.column("category").unwrap());
            let mut value = Vec::new();
            SqlValue::Integer(RowIdentity::new(1, 1).unwrap().integer()).encode(&mut value);
            change.after.as_mut().unwrap().insert(category, value);
            send(
                &a.author_bundle([53; 32], vec![RowWrite::changed(change)], vec![])
                    .await
                    .unwrap(),
                &b,
            )
            .await;
        }
        let mut reader_peer = if scenario == 5 {
            let path = root.path().join("reader");
            let reader = f
                .open(
                    &path,
                    PrivateKey::new(),
                    f.grants(Some(&[partitions[1]]), &[Right::Read]),
                    None,
                )
                .await;
            f.keys(&reader, partitions[1], TABLES).await;
            Some(Peer::capture(&f, &reader, &path).await)
        } else {
            None
        };
        let mut a_peer = Peer::capture(&f, &a, &a_path).await;
        let mut b_peer = Peer::capture(&f, &b, &b_path).await;
        drop(a);
        drop(b);
        if scenario == 2 || scenario == 3 {
            for name in TABLES {
                a_peer.run(Action::Evict(f.scope(name, partitions[1])));
            }
        }
        if scenario == 6 {
            b_peer.run(Action::Evict(f.scope("attachments", partitions[0])));
        }
        let offline = if [1, 3, 4].contains(&scenario) {
            Some(a_peer.run(Action::Edit(row, "ticked".into())))
        } else {
            None
        };
        let moved = b_peer.run(Action::Move(partitions[0], partitions[1]));
        if scenario == 6 {
            assert!(moved.error.as_ref().unwrap().contains("attachments"));
            assert_eq!(visible(&moved, f.scope("items", partitions[0])).len(), 1);
            assert!(visible(&moved, f.scope("items", partitions[1])).is_empty());
            continue;
        }
        assert!(moved.error.is_none(), "{:?}", moved.error);
        let b_frames = moved.frames;
        let a_frames = if scenario == 2 || scenario == 3 {
            b_frames
                .iter()
                .filter(|frame| {
                    TABLES
                        .iter()
                        .any(|name| frame.scope().unwrap() == f.scope(name, partitions[0]))
                })
                .cloned()
                .collect()
        } else {
            b_frames.clone()
        };
        let after_move = a_peer.run(Action::Receive(a_frames.clone()));
        assert!(visible(&after_move, f.scope("items", partitions[0])).is_empty());
        if scenario == 2 || scenario == 3 {
            assert!(
                !after_move
                    .snapshots
                    .iter()
                    .any(|(scope, _)| *scope == f.scope("items", partitions[1]))
            );
            assert_eq!(after_move.follows, 0);
        }
        if scenario == 4 {
            let mut destination_row = f.write("items", partitions[1], 1, None);
            destination_row.change.before = destination_row.change.after.clone();
            // A normal destination update gets a later clock than the carried offline edit.
            let later = b_peer.run(Action::Edit(destination_row, "later".into()));
            a_peer.run(Action::Receive(later.frames));
        }
        if let Some(offline) = offline {
            b_peer.run(Action::Receive(offline.frames));
        }
        let b_final = b_peer.run(Action::Receive(after_move.frames));
        let a_final = a_peer.run(Action::Receive(a_frames));
        assert_eq!(
            a_final.follows, after_move.follows,
            "reopen/sync follows once"
        );
        let destination = visible(&b_final, f.scope("items", partitions[1]));
        assert_eq!(destination.len(), 1);
        let table = f.schema.table("items").unwrap();
        let title = f.schema.field_id(table, table.column("title").unwrap());
        let expected = if scenario == 1 {
            "ticked"
        } else if scenario == 4 {
            "later"
        } else {
            "Item"
        };
        let mut encoded = Vec::new();
        SqlValue::Text(expected.into()).encode(&mut encoded);
        assert_eq!(
            destination.values().next().unwrap()[&title],
            encoded,
            "scenario {scenario}"
        );
        if scenario == 5 {
            let destination_frames = b_final
                .frames
                .iter()
                .filter(|frame| {
                    TABLES
                        .iter()
                        .any(|name| frame.scope().unwrap() == f.scope(name, partitions[1]))
                })
                .cloned()
                .collect();
            let reader = reader_peer
                .as_mut()
                .unwrap()
                .run(Action::Receive(destination_frames));
            assert_eq!(
                visible(&reader, f.scope("attachments", partitions[1])).len(),
                1
            );
            let category = f.schema.field_id(table, table.column("category").unwrap());
            let mut null = Vec::new();
            SqlValue::Null.encode(&mut null);
            assert_eq!(
                visible(&reader, f.scope("items", partitions[1]))
                    .values()
                    .next()
                    .unwrap()[&category],
                null
            );
        }
    }
}
