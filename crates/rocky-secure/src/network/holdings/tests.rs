use super::*;
use crate::replica::roles::FrontierReply;
use crate::tests::group_sync_measurement::fixture::Fixture;
use rocky_core::{Right, TypedRows};

type Replica = EncryptedReplica<TypedRows>;

async fn pair<C: RowApplication, S: RowApplication>(
    client: &EncryptedReplica<C>,
    server: &EncryptedReplica<S>,
) -> (NativeSession, NativeSession) {
    let (cn, cc) = {
        let i = client.inner.lock().await;
        (i.config.node.clone(), i.config.credentials.clone())
    };
    let (sn, sc) = {
        let i = server.inner.lock().await;
        (i.config.node.clone(), i.config.credentials.clone())
    };
    super::link::testing::sessions((client, cn, cc), (server, sn, sc))
        .await
        .unwrap()
}

async fn open(f: &Fixture, dir: &std::path::Path, name: &str, rights: &[Right]) -> Replica {
    f.open(
        &dir.join(name),
        PrivateKey::new(),
        f.grants(None, rights),
        None,
        vec![],
    )
    .await
}

async fn refusal(client: &Replica, server: &Replica, message: Message) -> (String, u64, u64) {
    let (mut c, mut s) = pair(client, server).await;
    c.send(message).await.unwrap();
    let before = s.sent;
    let error = s.serve(server).await.unwrap_err().to_string();
    let messages = s.sent - before;
    let (closed, (received, count)) = tokio::join!(s.close(), async {
        let received = c.receive().await;
        let count = c.received;
        c.close().await.unwrap();
        (received, count)
    });
    closed.unwrap();
    assert!(
        matches!(&received, Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof),
        "{:?}",
        received.map(|_| ())
    );
    (error, messages, count)
}

fn requests(
    scope: ResourceScope,
    ordinary: ResourceScope,
    node: &PrivateKey,
    credentials: &CredentialChain,
) -> Vec<Message> {
    let frontier = ScopeFrontier {
        scope,
        logs: Vec::new(),
    };
    let invocation = EncryptedPackage {
        scope,
        generation: rocky_core::KeyEpoch::new(1, &[9; 32]),
        kind: PackageKind::Invocation,
        bytes: Vec::new(),
    };
    let request = rocky_core::StrongRequest {
        scope: scope.domain,
        caller: crate::secure_principal_id(node.public_key()),
        id: rocky_core::RequestId {
            node: crate::secure_node_id(node.public_key()),
            sequence: rocky_core::AuthorSequence::new(1).unwrap(),
        },
        method: "probe".into(),
        version: 1,
        schema: 1,
        handler: 1,
        client_request: [0; 32],
        arguments: encode(&invocation).unwrap(),
        policy: [0; 32],
        frontier: Default::default(),
        digest: [0; 32],
    };
    let command: EncryptedCommand = serde_json::from_value(serde_json::json!({
        "request": request.encode().unwrap(), "node": node.public_key(), "credentials": credentials,
        "signature": node.sign(b"invalid command: proof gate must run first")
    }))
    .unwrap();
    let result: ConfirmedCommand = serde_json::from_value(serde_json::json!({
        "command": command, "package": [], "receipt": [], "executor": node.public_key(),
        "credentials": credentials, "signature": node.sign(b"invalid result"),
        "parts": [{"scope": scope, "generation": rocky_core::KeyEpoch::new(1, &[9;32]), "kind": "CommandResult", "bytes": []}]
    })).unwrap();
    vec![
        RowMessage::Pull {
            scopes: vec![scope],
            after: None,
            quota: 1024,
        }
        .into(),
        RowMessage::Range {
            frontier: frontier.clone(),
            after: None,
            quota: 1024,
        }
        .into(),
        RowMessage::Held { frontier }.into(),
        RowMessage::Push {
            scope,
            frames: Vec::new(),
            voids: Vec::new(),
        }
        .into(),
        RowMessage::FrontierQuery { scope }.into(),
        CheckpointMessage::CheckpointOffer { scope }.into(),
        CheckpointMessage::CheckpointPull {
            scope,
            checkpoint: [0; 32],
            after: None,
            quota: 1024,
        }
        .into(),
        RowMessage::Read {
            scope,
            after: None,
            max_rows: 1,
        }
        .into(),
        horizon_relay::HorizonRelayMessage::Exchange {
            scope,
            signed: None,
            acks: Vec::new(),
            finalized: None,
        }
        .into(),
        HorizonMessage::MergeableCut { scope }.into(),
        HorizonMessage::Horizon { scope, after: None }.into(),
        HorizonMessage::HorizonRejoined {
            scope,
            horizon: [0; 32],
        }
        .into(),
        HorizonMessage::HorizonAck {
            scope,
            ack: rocky_credentials::HorizonAck::sign([0; 32], node, credentials.clone()).unwrap(),
        }
        .into(),
        CommandMessage::Execute {
            command: command.clone(),
            scopes: vec![scope],
        }
        .into(),
        CommandMessage::Execute {
            command,
            scopes: vec![ordinary],
        }
        .into(),
        CommandMessage::ResultPull {
            domain: scope.domain,
            sequence: 1,
            scopes: vec![scope],
        }
        .into(),
        CommandMessage::ResultPush { result }.into(),
        ExistenceMessage::BaseOffer {
            scope,
            expected: None,
        }
        .into(),
        ExistenceMessage::BasePull {
            scope,
            base: [0; 32],
            after: None,
        }
        .into(),
        ExistenceMessage::BaseStage {
            scope,
            page: ExistenceBasePage {
                base: [0; 32],
                rows: vec![],
                next: None,
            },
        }
        .into(),
        ExistenceMessage::BaseFinish { scope }.into(),
        ExistenceMessage::Frontier { scope }.into(),
        ExistenceMessage::Pull {
            scope,
            known: Default::default(),
            after: None,
        }
        .into(),
        ExistenceMessage::Push {
            scope,
            records: vec![],
        }
        .into(),
    ]
}

async fn transcript_case(custody: bool) {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::new(1);
    let server = open(
        &f,
        root.path(),
        "server.db",
        if custody {
            &[Right::Custody]
        } else {
            &[Right::Read]
        },
    )
    .await;
    let member = open(&f, root.path(), "member.db", &[Right::Read]).await;
    let scope = f.scope("customers", 1);
    server
        .register_partition(scope, f.tuple("customers", 1))
        .await
        .unwrap();
    if !custody {
        server.install_key(f.key("customers", 1)).await.unwrap();
    }
    server.subscribe(scope).await.unwrap();
    server.subscribe(f.scope("products", 0)).await.unwrap();
    // A reader server: the member even knows the tuple, yet lacks the reader
    // key. Keyless custody accepts the tuple as proof, which a link offers at
    // open, so there the member must not know it.
    if !custody {
        member
            .register_partition(scope, f.tuple("customers", 1))
            .await
            .unwrap();
    }
    let unknown = ResourceScope {
        encryption: [203; 32],
        ..scope
    };
    let (node, credentials) = {
        let inner = member.inner.lock().await;
        (inner.config.node.clone(), inner.config.credentials.clone())
    };
    for (known, missing) in requests(scope, f.scope("products", 0), &node, &credentials)
        .into_iter()
        .zip(requests(
            unknown,
            f.scope("products", 0),
            &node,
            &credentials,
        ))
    {
        let known = refusal(&member, &server, known).await;
        let missing = refusal(&member, &server, missing).await;
        assert_eq!(known, missing);
        assert!(known.0.contains(HOLDING_DENIED));
        assert_eq!(known.1, 0);
    }
    assert!(
        server
            .held_scopes(node.public_key(), &credentials, None)
            .await
            .unwrap()
            .0
            .contains(&scope)
    );
    let (mut c, mut s) = pair(&member, &server).await;
    c.send(RowMessage::ScopesQuery { after: None })
        .await
        .unwrap();
    s.serve_once(&server).await.unwrap();
    let RowMessage::Scopes { scopes, next } = c.reply().await.unwrap() else {
        panic!("scopes reply")
    };
    assert_eq!(scopes, vec![f.scope("products", 0)]);
    assert!(next.is_none());
    for asked in [scope, unknown] {
        c.send(FrontierMessage::FrontiersQuery {
            frontiers: vec![ScopeFrontier {
                scope: asked,
                logs: vec![],
            }],
            partitions: vec![],
            proofs: vec![],
        })
        .await
        .unwrap();
        s.serve_once(&server).await.unwrap();
        let FrontierMessage::Frontiers { states } = c.reply().await.unwrap() else {
            panic!("frontiers reply")
        };
        assert!(matches!(states.as_slice(), [FrontierReply::Refused(scope)] if *scope == asked));
    }
    let config = {
        let inner = server.inner.lock().await;
        SecureConfig {
            publisher: inner.config.publisher,
            manifest: inner.config.manifest.clone(),
            revocations: inner.config.revocations.clone(),
            node: inner.config.node.clone(),
            credentials: inner.config.credentials.clone(),
            keys: inner
                .config
                .keys
                .iter()
                .map(|key| KeyMaterial::at(key.scope, key.generation, key.key))
                .collect(),
        }
    };
    drop(server);
    let app = rocky_core::TypedCommands::new(f.app(), Default::default(), Default::default(), None)
        .unwrap();
    let server = EncryptedReplica::open(root.path().join("server.db"), app, config)
        .await
        .unwrap();
    let coordinator = rocky_coordinator::MemoryCoordinator::default();
    // Exercise both command dispatchers: the gate must precede payload decoding,
    // result lookup/import and all coordinator calls, even for invalid payloads.
    for command_only in [false, true] {
        for (known, missing) in requests(scope, f.scope("products", 0), &node, &credentials)
            .into_iter()
            .zip(requests(
                unknown,
                f.scope("products", 0),
                &node,
                &credentials,
            ))
        {
            if !matches!(known, Message::Commands(_)) {
                continue;
            }
            let mut transcripts = Vec::new();
            for message in [known, missing] {
                let (mut c, mut s) = pair(&member, &server).await;
                c.send(message).await.unwrap();
                let before = s.sent;
                let error = if command_only {
                    s.serve_command_once(&server, &coordinator)
                        .await
                        .unwrap_err()
                } else {
                    s.serve_with_commands(&server, &coordinator)
                        .await
                        .unwrap_err()
                };
                transcripts.push((error.to_string(), s.sent - before));
                let (closed, received) = tokio::join!(s.close(), async {
                    let received = c.receive().await;
                    c.close().await.unwrap();
                    received
                });
                closed.unwrap();
                assert!(
                    matches!(received, Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof)
                );
            }
            assert_eq!(transcripts[0], transcripts[1]);
            assert!(transcripts[0].0.contains(HOLDING_DENIED));
            assert_eq!(transcripts[0].1, 0);
        }
    }
}

#[tokio::test]
async fn every_reader_request_hides_an_unproved_partition() {
    transcript_case(false).await;
}
#[tokio::test]
async fn every_custody_request_hides_an_unproved_partition() {
    transcript_case(true).await;
}

#[tokio::test]
async fn proofs_are_session_bound_bounded_and_remembered() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::new(1);
    let server = open(&f, root.path(), "server.db", &[Right::Read]).await;
    let member = open(&f, root.path(), "member.db", &[Right::Read]).await;
    let scope = f.scope("customers", 1);
    f.keys(&server, 1, &["customers"]).await;
    // Opened before the member holds the partition, so its open proves nothing.
    let second = pair(&member, &server).await;
    f.keys(&member, 1, &["customers"]).await;
    server.subscribe(scope).await.unwrap();
    f.keys(&server, 2, &["customers"]).await;
    server.subscribe(f.scope("customers", 2)).await.unwrap();
    let (mut c, mut s) = pair(&member, &server).await;
    let captured = member.holding_proofs(c.binding, &[scope]).await.unwrap();
    let (mut c2, mut s2) = second;
    let query = |proofs| FrontierMessage::FrontiersQuery {
        frontiers: vec![ScopeFrontier {
            scope,
            logs: vec![],
        }],
        partitions: vec![],
        proofs,
    };
    c.send(query(captured.clone())).await.unwrap();
    s.serve_once(&server).await.unwrap();
    let FrontierMessage::Frontiers { states } = c.reply().await.unwrap() else {
        panic!("frontiers")
    };
    assert!(matches!(states.as_slice(), [FrontierReply::Held(_)]));
    // A later raw scoped request needs no proof or extra message.
    let before = s.sent;
    c.send(RowMessage::FrontierQuery { scope }).await.unwrap();
    s.serve_once(&server).await.unwrap();
    assert!(matches!(
        c.reply::<RowMessage>().await.unwrap(),
        RowMessage::Frontier { .. }
    ));
    assert_eq!(s.sent - before, 1);
    c.send(RowMessage::ScopesQuery { after: None })
        .await
        .unwrap();
    s.serve_once(&server).await.unwrap();
    let RowMessage::Scopes { scopes, next } = c.reply().await.unwrap() else {
        panic!("scopes")
    };
    assert_eq!(scopes, vec![scope]);
    assert!(next.is_none());
    c2.send(query(captured.clone())).await.unwrap();
    s2.serve_once(&server).await.unwrap();
    let FrontierMessage::Frontiers { states } = c2.reply().await.unwrap() else {
        panic!("frontiers")
    };
    assert!(matches!(states.as_slice(), [FrontierReply::Refused(_)]));
    let proof = captured[0].1[0].clone();
    c2.send(query(vec![(scope, vec![proof; 5])])).await.unwrap();
    assert!(
        s2.serve_once(&server)
            .await
            .unwrap_err()
            .to_string()
            .contains(HOLDING_DENIED)
    );
}

#[tokio::test]
async fn retained_siblings_and_previous_generations_prove_but_fresh_peers_refuse() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::new(1);
    let retained = open(&f, root.path(), "retained.db", &[Right::Read]).await;
    let fresh = open(&f, root.path(), "fresh.db", &[Right::Read]).await;
    let member = open(&f, root.path(), "member.db", &[Right::Read]).await;
    let scope = f.scope("customers", 1);
    for replica in [&retained, &fresh, &member] {
        replica
            .register_partition(scope, f.tuple("customers", 1))
            .await
            .unwrap();
    }
    let mut sibling_keys = [[11; 32], [12; 32]];
    sibling_keys.sort_by_key(|key| rocky_core::KeyEpoch::new(2, key));
    let newest = sibling_keys[1];
    for (generation, key) in [(1, [10; 32]), (2, sibling_keys[0]), (2, newest)] {
        retained
            .install_key_epoch(KeyMaterial::new(scope, generation, key))
            .await
            .unwrap();
    }
    fresh
        .install_key_epoch(KeyMaterial::new(scope, 2, newest))
        .await
        .unwrap();
    // Only the previous generation, then only the losing sibling.
    for (generation, key) in [(1, [10; 32]), (2, sibling_keys[0])] {
        member
            .install_key_epoch(KeyMaterial::new(scope, generation, key))
            .await
            .unwrap();
        if generation == 2 {
            member
                .retire_key_epoch(scope, rocky_core::KeyEpoch::new(1, &[10; 32]))
                .await
                .unwrap();
        }
        for (server, accepted) in [(&retained, true), (&fresh, false)] {
            let (mut c, mut s) = pair(&member, server).await;
            let serve = s.serve_to_end(server);
            let ask = async {
                let exchange = c.changed_scopes(&member, &[scope]).await.unwrap();
                assert_eq!(exchange.refused.is_empty(), accepted);
                c.close().await.unwrap();
            };
            let (served, ()) = tokio::join!(serve, ask);
            served.unwrap();
        }
    }
    for generation in 3..=7 {
        member
            .install_key_epoch(KeyMaterial::new(scope, generation, [generation as u8; 32]))
            .await
            .unwrap();
    }
    let proofs = member.holding_proofs([1; 32], &[scope]).await.unwrap();
    assert_eq!(proofs[0].1.len(), 4);
    assert!(proofs[0].1.windows(2).all(|p| p[0].epoch > p[1].epoch));
}

#[tokio::test]
async fn custody_tuple_offers_relay_only_with_both_grants() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::new(1);
    let writer = f
        .open(
            &root.path().join("writer.db"),
            PrivateKey::new(),
            f.grants(None, &[Right::Read, Right::Insert]),
            Some((PrivateKey::new(), 1)),
            vec![],
        )
        .await;
    let custody = open(&f, root.path(), "custody.db", &[Right::Custody]).await;
    let stranger = f
        .open(
            &root.path().join("stranger.db"),
            PrivateKey::new(),
            f.grants(Some(&[2]), &[Right::Custody]),
            None,
            vec![],
        )
        .await;
    let scope = f.scope("customers", 1);
    f.keys(&writer, 1, &["customers"]).await;
    writer
        .author_bundle([1; 32], vec![f.write("customers", 1)], vec![])
        .await
        .unwrap();
    let (mut c, mut s) = pair(&writer, &custody).await;
    let serve = s.serve_to_end(&custody);
    let ask = async {
        c.changed_scopes(&writer, &[scope]).await.unwrap();
        assert_eq!(
            c.push(&writer, scope, MAX_SECURE_FRAME / 2)
                .await
                .unwrap()
                .frames,
            1
        );
        c.close().await.unwrap();
    };
    let (served, ()) = tokio::join!(serve, ask);
    served.unwrap();
    assert!(custody.key_epochs(scope).await.is_empty());
    // Custody asks the reader with its tuple and grant, without any reader key.
    let (mut c, mut s) = pair(&custody, &writer).await;
    let serve = s.serve_to_end(&writer);
    let ask = async {
        assert!(
            c.changed_scopes(&custody, &[scope])
                .await
                .unwrap()
                .refused
                .is_empty()
        );
        c.close().await.unwrap();
    };
    let (served, ()) = tokio::join!(serve, ask);
    served.unwrap();
    let (mut c, mut s) = pair(&writer, &stranger).await;
    let serve = s.serve_to_end(&stranger);
    let ask = async {
        assert_eq!(
            c.changed_scopes(&writer, &[scope]).await.unwrap().refused,
            vec![scope]
        );
        c.close().await.unwrap();
    };
    let (served, ()) = tokio::join!(serve, ask);
    served.unwrap();
    for server in [&writer, &custody] {
        let (mut c, mut s) = pair(&stranger, server).await;
        let key = f.key("customers", 1);
        c.send(FrontierMessage::FrontiersQuery {
            frontiers: vec![ScopeFrontier {
                scope,
                logs: vec![],
            }],
            partitions: vec![(scope, f.tuple("customers", 1))],
            proofs: vec![(
                scope,
                vec![HoldingProof {
                    epoch: key.generation,
                    value: key_proof(&key, c.binding).unwrap(),
                }],
            )],
        })
        .await
        .unwrap();
        s.serve_once(server).await.unwrap();
        let FrontierMessage::Frontiers { states } = c.reply().await.unwrap() else {
            panic!("frontiers")
        };
        assert!(matches!(states.as_slice(), [FrontierReply::Refused(_)]));
    }
    let inner = stranger.inner.lock().await;
    let registered = crate::storage::rows(&inner.conn, "SELECT scope FROM secure_partitions")
        .await
        .unwrap();
    assert!(registered.is_empty());
}

#[tokio::test]
async fn acknowledgments_cannot_probe_unserved_frame_ids() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::new(1);
    let writer = f
        .open(
            &root.path().join("writer.db"),
            PrivateKey::new(),
            f.grants(None, &[Right::Read, Right::Insert]),
            Some((PrivateKey::new(), 1)),
            vec![],
        )
        .await;
    let member = open(&f, root.path(), "member.db", &[Right::Read]).await;
    f.keys(&writer, 1, &["customers"]).await;
    member
        .register_partition(f.scope("customers", 1), f.tuple("customers", 1))
        .await
        .unwrap();
    let frames = writer
        .author_bundle([1; 32], vec![f.write("customers", 1)], vec![])
        .await
        .unwrap();
    let existing = refusal(
        &member,
        &writer,
        RowMessage::Ack {
            id: frames[0].id().unwrap(),
            applied: true,
        }
        .into(),
    )
    .await;
    let unknown = refusal(
        &member,
        &writer,
        RowMessage::Ack {
            id: Hash::new(b"unknown frame"),
            applied: true,
        }
        .into(),
    )
    .await;
    assert_eq!(existing, unknown);
    assert_eq!(existing.1, 0);
    // A reader that proves the partition can pull and acknowledge normally.
    f.keys(&member, 1, &["customers"]).await;
    let (mut c, mut s) = pair(&member, &writer).await;
    let serve = s.serve_to_end(&writer);
    let ask = async {
        c.prove_scopes(&member, &[f.scope("customers", 1)])
            .await
            .unwrap();
        c.send(RowMessage::Range {
            frontier: member.frontier(f.scope("customers", 1)).await.unwrap(),
            after: None,
            quota: 16 * 1024,
        })
        .await
        .unwrap();
        let RowMessage::RangeFrames { page } = c.reply::<RowMessage>().await.unwrap() else {
            panic!("missing range frames");
        };
        assert_eq!(page.frames.len(), 1);
        c.send(RowMessage::Ack {
            id: page.frames[0].id().unwrap(),
            applied: false,
        })
        .await
        .unwrap();
        c.acknowledged("missing range acknowledgment")
            .await
            .unwrap();
        assert!(
            c.pull_page(&member, vec![f.scope("customers", 1)], None, 16 * 1024)
                .await
                .unwrap()
                .rebootstrap
                .is_empty()
        );
        c.close().await.unwrap();
    };
    let (served, ()) = tokio::join!(serve, ask);
    served.unwrap();
    assert_eq!(
        member.frontier(f.scope("customers", 1)).await.unwrap(),
        writer.frontier(f.scope("customers", 1)).await.unwrap()
    );
}

#[tokio::test]
async fn saved_peer_resolution_proves_the_pinned_reference_without_opening_a_session() {
    let f = Fixture::new(1);
    let root = tempfile::tempdir().unwrap();
    let server = open(&f, root.path(), "resolver", &[Right::Read]).await;
    let (node, credentials) = {
        let inner = server.inner.lock().await;
        (inner.config.node.clone(), inner.config.credentials.clone())
    };
    for expected in [node.public_key(), PrivateKey::new().public_key()] {
        let (client_stream, server_stream) = tokio::io::duplex(4096);
        let (resolved, accepted) = tokio::join!(
            NativeSession::resolve_peer(
                rocky_core::net::Stream::new(client_stream),
                rocky_credentials::PeerId::of(expected)
            ),
            super::link::Link::accept_or_resolve(
                server_stream,
                &server,
                node.clone(),
                credentials.clone(),
            )
        );
        assert!(accepted.unwrap().is_none());
        if expected == node.public_key() {
            assert_eq!(resolved.unwrap(), expected);
        } else {
            assert!(resolved.is_err());
        }
    }
}
