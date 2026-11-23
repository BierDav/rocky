use super::*;
use rocky_coordinator::protocol::nonce;

#[test]
fn saturated_voter_connections_are_counted_in_status_metrics() {
    let counters = Counters::default();
    let slots = Arc::new(Semaphore::new(VOTER_CONNECTION_SLOTS));
    let held: Vec<_> = (0..VOTER_CONNECTION_SLOTS)
        .map(|_| counters.connection_slot(&slots).unwrap())
        .collect();
    assert!(counters.connection_slot(&slots).is_none());
    assert_eq!(counters.snapshot().busy, 1);
    assert_eq!(counters.snapshot().requests, 0);
    drop(held);
    assert!(counters.connection_slot(&slots).is_some());
    assert_eq!(counters.snapshot().busy, 1);
}

const ELECTION_BOUND: std::time::Duration = std::time::Duration::from_secs(60);

const GENESIS_VOTERS: u64 = 3;

struct Cluster {
    directory: tempfile::TempDir,
    genesis: CertifiedGenesis,
    admin: PrivateKey,
    mesh: Arc<Mesh>,
    voters: Vec<Arc<VoterService>>,
}
impl Cluster {
    async fn start(members: u64) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = PrivateKey::new();
        let admin = PrivateKey::new();
        let keys: Vec<_> = (1..=GENESIS_VOTERS).map(|_| PrivateKey::new()).collect();
        let genesis = CertifiedGenesis::sign(
            Genesis {
                mesh: MeshId([0x71; 16]),
                configuration: [7; 16],
                voters: (1..)
                    .zip(&keys)
                    .map(|(id, key)| Voter {
                        id,
                        address: format!("127.0.0.1:{id}"),
                        key: key.public_key(),
                    })
                    .collect(),
                initial_voters: (1..=GENESIS_VOTERS).collect(),
                membership_authority: admin.public_key(),
                authorities: Vec::new(),
                max_control_lifetime: 86_400,
                public: Some(PublicService {
                    endpoints: vec![ClientEndpoint {
                        voter: 1,
                        kind: "memory".into(),
                        address: "voter-1".into(),
                    }],
                    admission: None,
                    limits: rocky_coordinator::Limits::default(),
                    tenants: 4,
                    registrations_per_day: 16,
                    tenant_lanes: 4,
                }),
            },
            &root,
        )
        .unwrap();
        let mesh = Arc::new(Mesh::default());
        let mut voters = Vec::new();
        for (id, key) in (1..=members).zip(keys) {
            let path = directory.path().join(format!("voter-{id}"));
            std::fs::create_dir_all(&path).unwrap();
            let store = Store::open(&path, genesis.clone(), true, genesis.limits()).unwrap();
            let route = Route::Mesh {
                mesh: mesh.clone(),
                from: id,
            };
            let voter = VoterService::start(store, genesis.clone(), id, key, route)
                .await
                .unwrap();
            mesh.join(id, &voter);
            voters.push(voter);
        }
        voters[0]
            .raft
            .initialize(
                genesis
                    .genesis
                    .voters
                    .iter()
                    .filter(|v| v.id <= members)
                    .map(|v| (v.id, BasicNode::new(v.address.clone())))
                    .collect::<std::collections::BTreeMap<_, _>>(),
            )
            .await
            .unwrap();
        Self {
            directory,
            genesis,
            admin,
            mesh,
            voters,
        }
    }
    fn voter(&self, id: u64) -> &Arc<VoterService> {
        &self.voters[usize::try_from(id - 1).unwrap()]
    }
    async fn leader_other_than(&self, excluded: Option<u64>) -> &Arc<VoterService> {
        let witness = self
            .voters
            .iter()
            .find(|voter| Some(voter.node) != excluded)
            .unwrap();
        let elected = witness
            .raft
            .wait(Some(ELECTION_BOUND))
            .metrics(
                |m| m.current_leader.is_some_and(|id| Some(id) != excluded),
                "a reachable voter leads",
            )
            .await
            .unwrap()
            .current_leader
            .unwrap();
        let leader = self.voter(elected);
        leader
            .raft
            .wait(Some(ELECTION_BOUND))
            .metrics(
                |m| m.current_leader == Some(elected) && quorum_acknowledged(m),
                "the elected voter hears from a quorum",
            )
            .await
            .unwrap();
        leader
    }
    async fn cut_off_until_its_quorum_ack_is_stale(&self, voter: &VoterService) {
        self.mesh.isolate(voter.node);
        voter
            .raft
            .wait(Some(ELECTION_BOUND))
            .metrics(
                |m| !quorum_acknowledged(m),
                "a cut-off leader's quorum acknowledgment goes stale",
            )
            .await
            .unwrap();
    }
    fn affirm_membership(&self, revision: u64) -> Request {
        let voters: BTreeSet<u64> = self.genesis.genesis.initial_voters.clone();
        Request::certified(
            self.genesis.id().unwrap(),
            nonce(),
            Operation::Membership {
                voters: voters.clone(),
                expected: voters,
                expected_revision: revision,
            },
            (&self.admin, None),
            now().unwrap(),
        )
        .unwrap()
    }
}

fn appended(voter: &VoterService) -> Option<u64> {
    voter.raft.metrics().borrow().last_log_index
}

fn affirmed(cluster: &Cluster) -> Result<Outcome, CoordinationError> {
    Ok(Outcome::Membership(
        cluster.genesis.genesis.initial_voters.clone(),
    ))
}

#[tokio::test(start_paused = true)]
async fn a_leader_cut_off_from_its_followers_refuses_writes_and_appends_nothing_once_its_quorum_ack_is_stale()
 {
    let cluster = Cluster::start(3).await;
    let leader = cluster.leader_other_than(None).await;
    assert_eq!(
        leader.answer(cluster.affirm_membership(0)).await.result,
        affirmed(&cluster),
        "a leader that hears from a quorum accepts writes"
    );

    cluster.cut_off_until_its_quorum_ack_is_stale(leader).await;
    let before = appended(leader);
    for _ in 0..3 {
        let reply = leader.answer(cluster.affirm_membership(1)).await;
        assert_eq!(
            reply.result,
            Err(CoordinationError::Unavailable),
            "a stale leader refuses with the retryable unavailable error"
        );
        assert_eq!(
            reply.leader, None,
            "a stale leader does not name itself so clients try the other voters"
        );
    }
    assert_eq!(
        appended(leader),
        before,
        "a leader without a fresh quorum acknowledgment appends nothing more"
    );

    let majority = cluster.leader_other_than(Some(leader.node)).await;
    assert_eq!(
        majority.answer(cluster.affirm_membership(1)).await.result,
        affirmed(&cluster),
        "the voters that still form a quorum elect a leader that accepts writes"
    );
}

#[tokio::test(start_paused = true)]
async fn a_stale_leader_accepts_writes_again_once_its_followers_answer() {
    let cluster = Cluster::start(3).await;
    let leader = cluster.leader_other_than(None).await;
    assert_eq!(
        leader.answer(cluster.affirm_membership(0)).await.result,
        affirmed(&cluster)
    );
    for follower in cluster.voters.iter().filter(|v| v.node != leader.node) {
        follower.raft.runtime_config().elect(false);
    }

    cluster.cut_off_until_its_quorum_ack_is_stale(leader).await;
    let before = appended(leader);
    let refused = leader.answer(cluster.affirm_membership(1)).await;
    assert_eq!(refused.result, Err(CoordinationError::Unavailable));
    assert_eq!(
        appended(leader),
        before,
        "a stale leader appends nothing while its followers are unreachable"
    );

    cluster.mesh.heal(leader.node);
    leader
        .raft
        .wait(Some(ELECTION_BOUND))
        .metrics(
            quorum_acknowledged,
            "the healed leader hears from a quorum again",
        )
        .await
        .unwrap();
    let reply = leader.answer(cluster.affirm_membership(1)).await;
    assert_eq!(
        reply.result,
        affirmed(&cluster),
        "a leader whose quorum acknowledges it again accepts writes"
    );
    assert_eq!(reply.leader, Some(leader.node));
}

#[tokio::test(start_paused = true)]
async fn a_fatal_consensus_state_ends_the_voter_with_an_error() {
    let cluster = Cluster::start(1).await;
    let voter = cluster.leader_other_than(None).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let serving = tokio::spawn(voter.clone().serve_voters(listener));

    std::fs::remove_dir_all(cluster.directory.path().join("voter-1/log")).unwrap();
    assert_eq!(
        voter.answer(cluster.affirm_membership(0)).await.result,
        Err(CoordinationError::Unavailable),
        "a write whose log append fails is refused"
    );

    let ended = tokio::time::timeout(ELECTION_BOUND, serving)
        .await
        .expect("a voter whose consensus halted stops serving")
        .unwrap();
    assert!(
        matches!(
            ended,
            Err(ServeError::ConsensusHalted(ref fatal)) if matches!(**fatal, Fatal::StorageError(_))
        ),
        "the voter ends with the consensus storage failure: {ended:?}"
    );
}
