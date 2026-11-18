use super::*;
use rocky_core::REPLAY_SEMANTICS;

const CONCURRENT_WRITER_AHEAD_SECONDS: u64 = 60;
const LATE_WRITER_AHEAD_SECONDS: u64 = 120;

const W1_CREATES_A_X_AND_B_Y: &str = "w1 creates A/x=10 and B/y=1";
const W1_TRIPLES_A: &str = "w1, before anyone else writes, triples every amount in A";
const W2_CREATES_A_W_AND_B_Y: &str = "w2, ahead and unaware of w1, creates A/w=20 and B/y=5";
const W3_ADDS_ONE_TO_X: &str = "w3, further ahead and aware only of w1's rows, adds 1 to every x";

#[derive(Debug, PartialEq)]
struct Replayed {
    rows: Vec<Vec<SqlValue>>,
    outcomes: Vec<(&'static str, TransactionOutcome)>,
}

fn row(list: &str, key: &str, value: i64) -> Vec<SqlValue> {
    vec![text(list), text(key), SqlValue::Integer(value)]
}

fn golden_under_replay_semantics_1() -> Replayed {
    let applied = TransactionOutcome::Applied { sealed: false };
    Replayed {
        rows: vec![row("A", "x", 31), row("B", "y", 1)],
        outcomes: vec![
            (W1_CREATES_A_X_AND_B_Y, applied),
            (
                W2_CREATES_A_W_AND_B_Y,
                TransactionOutcome::Dropped {
                    reason: DropReason::Constraint,
                    sealed: false,
                },
            ),
            (W3_ADDS_ONE_TO_X, applied),
            (W1_TRIPLES_A, applied),
        ],
    }
}

fn goldens_by_replay_semantics() -> Vec<(u32, Replayed)> {
    vec![(1, golden_under_replay_semantics_1())]
}

struct Logged {
    name: &'static str,
    transaction: Commitment,
    frames: Vec<EncryptedFrame>,
}

impl Logged {
    fn new(f: &Fixture, name: &'static str, frames: Vec<EncryptedFrame>) -> Self {
        Self {
            name,
            transaction: frames
                .iter()
                .find_map(|frame| {
                    let payload = payload(frame, key_of(f, frame), f);
                    payload.intent()?;
                    payload
                        .membership()
                        .map(|membership| membership.transaction)
                })
                .expect("a logged transaction carries an intent"),
            frames,
        }
    }
}

async fn ahead_by<T>(seconds: u64, write: impl std::future::Future<Output = T>) -> T {
    crate::CLOCK_SKEW.with(|skew| skew.set(seconds));
    let written = write.await;
    crate::CLOCK_SKEW.with(|skew| skew.set(0));
    written
}

async fn operation_log(f: &Fixture, root: &Path) -> Vec<Logged> {
    let mut w1 = Author::open(f, root, 1).await;
    let mut w2 = Author::open(f, root, 2).await;
    let mut w3 = Author::open(f, root, 3).await;
    w1.create(f, &[("A", "x", 10), ("B", "y", 1)], 1).await;
    let first = Logged::new(f, W1_CREATES_A_X_AND_B_Y, w1.fresh().await);
    w1.run(
        f,
        &[("UPDATE items SET amount=amount*3 WHERE list='A'", vec![])],
        2,
    )
    .await;
    let tripled = Logged::new(f, W1_TRIPLES_A, w1.fresh().await);
    ahead_by(
        CONCURRENT_WRITER_AHEAD_SECONDS,
        w2.create(f, &[("A", "w", 20), ("B", "y", 5)], 3),
    )
    .await;
    let conflicting = Logged::new(f, W2_CREATES_A_W_AND_B_Y, w2.fresh().await);
    w3.receive(&first.frames).await;
    ahead_by(
        LATE_WRITER_AHEAD_SECONDS,
        w3.run(
            f,
            &[(
                "UPDATE items SET amount=amount+? WHERE key=?",
                vec![SqlValue::Integer(1), text("x")],
            )],
            4,
        ),
    )
    .await;
    let incremented = Logged::new(f, W3_ADDS_ONE_TO_X, w3.fresh().await);
    vec![first, conflicting, incremented, tripled]
}

async fn replayed(f: &Fixture, replica: &EncryptedReplica<TypedRows>, log: &[Logged]) -> Replayed {
    let mut outcomes = Vec::new();
    for logged in log {
        outcomes.push((
            logged.name,
            replica
                .transaction_outcome(logged.transaction)
                .await
                .unwrap()
                .expect("every logged transaction replays"),
        ));
    }
    Replayed {
        rows: keys(f, replica, &["A", "B"]).await,
        outcomes,
    }
}

async fn receive_log(replica: &EncryptedReplica<TypedRows>, frames: &[EncryptedFrame]) {
    for status in replica.receive_all(frames.to_vec()).await {
        assert_ne!(status.unwrap(), ReceiveStatus::Rejected);
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_fixed_operation_log_replays_to_the_golden_result_of_its_replay_semantics() {
    let goldens = goldens_by_replay_semantics();
    assert!(
        goldens
            .windows(2)
            .all(|pair| pair[0].0 < pair[1].0 && pair[0].1 != pair[1].1),
        "a golden is recorded once per replay semantics and is never re-recorded in place"
    );
    let (recorded_under, golden) = goldens.last().expect("a golden replay is recorded");
    assert_eq!(
        REPLAY_SEMANTICS, *recorded_under,
        "a change to REPLAY_SEMANTICS appends the golden replay recorded under it"
    );
    let f = Fixture::build_class(false, PrivateKey::from_bytes(&[3; 32]), "replayed");
    assert!(
        f.scope("A") < f.scope("B"),
        "the conflicting member replays after one that succeeded, so its drop must roll that one back"
    );
    let root = rocky_platform::fs::Scratch::new().await;
    let log = operation_log(&f, root.path()).await;
    let whole: Vec<_> = log
        .iter()
        .flat_map(|logged| logged.frames.clone())
        .collect();

    let as_logged = receiver(
        &f,
        &root.path().join("as-logged.sqlite"),
        &PrivateKey::new(),
    )
    .await;
    receive_log(&as_logged, &whole).await;
    assert_eq!(
        &replayed(&f, &as_logged, &log).await,
        golden,
        "a changed replay result changes replay semantics: bump REPLAY_SEMANTICS and append its golden"
    );
    receive_log(&as_logged, &whole).await;
    assert_eq!(
        &replayed(&f, &as_logged, &log).await,
        golden,
        "replaying the log again changes nothing"
    );

    let reversed = receiver(&f, &root.path().join("reversed.sqlite"), &PrivateKey::new()).await;
    for logged in log.iter().rev() {
        receive_log(&reversed, &logged.frames).await;
    }
    assert_eq!(
        &replayed(&f, &reversed, &log).await,
        golden,
        "the golden replay holds for the reverse arrival order"
    );
}
