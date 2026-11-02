//! Counter claims: values, retries, authorization, window and budgets.
use super::coordinated_tests::{Fixture, T};
use super::*;
use crate::counters::{
    CLAIM_RECORDS, CLAIM_WINDOW, Claim, Counters, ISSUE_SKEW, Lookup, MAX_COUNTERS, MAX_NAME, Name,
    Reading,
};
use p2panda_core::PrivateKey;

fn name(text: &str) -> Name {
    Name::new(text).unwrap()
}
fn claim(n: u8, issued: u64) -> Claim {
    Claim {
        id: [n; 32],
        issued,
    }
}
impl Fixture {
    fn operate(
        &mut self,
        key: &PrivateKey,
        operation: Operation,
        time: u64,
    ) -> Result<Outcome, CoordinationError> {
        let request = crate::machine::self_certified(
            self.state.genesis.id().unwrap(),
            crate::transport::nonce(),
            operation,
            key,
            time,
        )
        .unwrap();
        self.state.apply_at(time, &request)
    }
    fn claim_as(
        &mut self,
        key: &PrivateKey,
        counter: &str,
        claim: Claim,
        time: u64,
    ) -> Result<u64, CoordinationError> {
        let operation = Operation::Claim {
            scope: self.scope,
            counter: name(counter),
            claim,
        };
        match self.operate(key, operation, time)? {
            Outcome::Claimed(value) => Ok(value),
            other => panic!("claim: {other:?}"),
        }
    }
    fn claim(
        &mut self,
        node: usize,
        counter: &str,
        claim: Claim,
        time: u64,
    ) -> Result<u64, CoordinationError> {
        let key = self.nodes[node].clone();
        self.claim_as(&key, counter, claim, time)
    }
    /// Reads through the linearizable read path, as a voter answers it.
    fn reading(&self, counter: &str, claim: Option<Claim>, time: u64) -> Reading {
        let request = crate::machine::self_certified(
            self.state.genesis.id().unwrap(),
            crate::transport::nonce(),
            Operation::Counter {
                scope: self.scope,
                counter: name(counter),
                claim,
            },
            &self.nodes[0],
            time,
        )
        .unwrap();
        assert!(request.read_only());
        match self.state.read(&request).unwrap() {
            Outcome::Counter(reading) => reading,
            other => panic!("reading: {other:?}"),
        }
    }
}

#[test]
fn claims_take_distinct_increasing_values_and_retries_repeat_them() {
    let mut fixture = Fixture::new(1, Limits::default());
    let retry_records = fixture.state.retry_records();
    let mut values = Vec::new();
    // Interleaved claims from both trusted nodes, each retried by the other.
    for n in 0..20u8 {
        let value = fixture
            .claim(usize::from(n % 2), "invoices", claim(n, T), T)
            .unwrap();
        let retried = fixture
            .claim(usize::from(n % 2 == 0), "invoices", claim(n, T), T + 1)
            .unwrap();
        assert_eq!(retried, value);
        values.push(value);
    }
    assert_eq!(values, (1..=20).collect::<Vec<_>>());
    // Every earlier claim still answers its own value.
    for n in 0..20u8 {
        assert_eq!(
            fixture.claim(0, "invoices", claim(n, T), T + 2),
            Ok(u64::from(n) + 1)
        );
    }
    // The same identity with another issue time is a different request.
    assert_eq!(
        fixture.claim(0, "invoices", claim(3, T + 1), T + 2),
        Err(CoordinationError::RequestConflict)
    );
    // Counters are independent; the same claim identity may name another.
    assert_eq!(fixture.claim(1, "receipts", claim(3, T), T + 2), Ok(1));
    assert_eq!(
        fixture.reading("invoices", Some(claim(7, T)), T + 2),
        Reading {
            value: 20,
            claim: Some(Lookup::Claimed(8)),
        }
    );
    assert_eq!(
        fixture.reading("invoices", Some(claim(99, T + 2)), T + 2),
        Reading {
            value: 20,
            claim: Some(Lookup::Unclaimed),
        }
    );
    assert_eq!(
        fixture.reading("unused", None, T + 2),
        Reading {
            value: 0,
            claim: None,
        }
    );
    // Claims keep no machine retry record: their identities deduplicate.
    assert_eq!(fixture.state.retry_records(), retry_records);
}

#[test]
fn snapshot_round_trip_keeps_values_and_claim_identities() {
    let mut fixture = Fixture::new(1, Limits::default());
    for n in 0..5 {
        fixture.claim(0, "invoices", claim(n, T), T).unwrap();
    }
    let image = crate::certified::encode(&fixture.state).unwrap();
    fixture.state = crate::certified::decode(&image).unwrap();
    assert_eq!(fixture.claim(1, "invoices", claim(2, T), T + 1), Ok(3));
    assert_eq!(fixture.claim(1, "invoices", claim(9, T), T + 1), Ok(6));
}

#[test]
fn only_trusted_nodes_claim_and_only_while_the_domain_is_open() {
    let mut fixture = Fixture::new(1, Limits::default());
    let stranger = PrivateKey::new();
    let owner = fixture.owner.clone();
    // Neither an unknown node nor the domain's control authority is a trusted node.
    for key in [&stranger, &owner] {
        assert_eq!(
            fixture.claim_as(key, "invoices", claim(1, T), T),
            Err(CoordinationError::Denied)
        );
        let read = Operation::Counter {
            scope: fixture.scope,
            counter: name("invoices"),
            claim: None,
        };
        assert_eq!(
            fixture.operate(key, read, T),
            Err(CoordinationError::Denied)
        );
    }
    // A request without a node certificate presents no trusted identity.
    let uncertified = Request::sign_at(
        fixture.state.genesis.id().unwrap(),
        [1; 32],
        Operation::Claim {
            scope: fixture.scope,
            counter: name("invoices"),
            claim: claim(1, T),
        },
        &fixture.nodes[0],
        T,
    )
    .unwrap();
    assert_eq!(
        fixture.state.apply_at(T, &uncertified),
        Err(CoordinationError::Denied)
    );
    let mut unknown = fixture.scope;
    unknown.domain = DomainId([9; 16]);
    let key = fixture.nodes[0].clone();
    assert_eq!(
        fixture.operate(
            &key,
            Operation::Claim {
                scope: unknown,
                counter: name("invoices"),
                claim: claim(1, T),
            },
            T,
        ),
        Err(CoordinationError::UnknownDomain)
    );
    assert_eq!(fixture.reading("invoices", None, T).value, 0);

    assert_eq!(fixture.claim(0, "invoices", claim(1, T), T), Ok(1));
    let close = Operation::Close {
        scope: fixture.scope,
    };
    fixture.operate(&owner, close, T).unwrap();
    // A closed domain takes no claim, not even a retry; its counters stay readable.
    assert_eq!(
        fixture.claim(0, "invoices", claim(2, T), T),
        Err(CoordinationError::Denied)
    );
    assert_eq!(
        fixture.claim(0, "invoices", claim(1, T), T),
        Err(CoordinationError::Denied)
    );
    assert_eq!(
        fixture.reading("invoices", Some(claim(1, T)), T),
        Reading {
            value: 1,
            claim: Some(Lookup::Claimed(1)),
        }
    );
}

/// The window outlasts the fixture's control, so it runs on the counters alone.
#[test]
fn claims_outside_the_window_are_expired_never_given_a_second_value() {
    let mut counters = Counters::default();
    let invoices = name("invoices");
    let mut take = |claim, time| counters.claim(&invoices, claim, time, usize::MAX);
    assert_eq!(take(claim(1, T), T), Ok(1));
    let end = T + CLAIM_WINDOW;
    assert_eq!(take(claim(1, T), end), Ok(1));
    assert_eq!(take(claim(1, T), end + 1), Err(CoordinationError::Expired));
    // A new claim issued that long ago is refused too: it cannot be told apart.
    assert_eq!(take(claim(2, T), end + 1), Err(CoordinationError::Expired));
    assert_eq!(take(claim(3, end), end + 1), Ok(2));
    // An issue time ahead of authority time beyond the skew is refused.
    assert_eq!(
        take(claim(4, end + 2 + ISSUE_SKEW), end + 1),
        Err(CoordinationError::ClockSkew {
            coordinator_time: end + 1,
        })
    );
    assert_eq!(take(claim(4, end + 1 + ISSUE_SKEW), end + 1), Ok(3));
    assert_eq!(
        counters.read(&invoices, Some(claim(3, end)), end + 1 + CLAIM_WINDOW),
        Ok(Reading {
            value: 3,
            claim: Some(Lookup::Expired),
        })
    );
    assert_eq!(
        counters.read(&invoices, Some(claim(3, end)), end + CLAIM_WINDOW),
        Ok(Reading {
            value: 3,
            claim: Some(Lookup::Claimed(2)),
        })
    );
}

#[test]
fn record_eviction_refuses_claims_it_can_no_longer_tell_apart() {
    let mut fixture = Fixture::new(1, Limits::default());
    let records = u64::try_from(CLAIM_RECORDS).unwrap();
    // Each claim is issued one second after the previous one.
    let claim = |n: u64| Claim {
        id: rocky_core::commitment("counter-test", &n.to_be_bytes()),
        issued: T + n,
    };
    for n in 0..=records {
        assert_eq!(
            fixture.claim(0, "invoices", claim(n), T + records),
            Ok(n + 1)
        );
    }
    let now = T + records;
    // The first claim was evicted, so its retry and any claim issued no
    // later than it are refused; every kept claim still answers.
    assert_eq!(
        fixture.claim(0, "invoices", claim(0), now),
        Err(CoordinationError::Expired)
    );
    let mut early = claim(records + 1);
    early.issued = T;
    assert_eq!(
        fixture.claim(0, "invoices", early, now),
        Err(CoordinationError::Expired)
    );
    assert_eq!(fixture.claim(0, "invoices", claim(1), now), Ok(2));
    assert_eq!(
        fixture.claim(0, "invoices", claim(records), now),
        Ok(records + 1)
    );
    assert_eq!(
        fixture.claim(0, "invoices", claim(records + 1), now),
        Ok(records + 2)
    );
    // Eviction keeps the accounted bytes bounded.
    let bytes = fixture
        .state
        .state
        .find(fixture.scope)
        .unwrap()
        .counters
        .bytes();
    assert_eq!(bytes, 128 + "invoices".len() + CLAIM_RECORDS * 64);
}

#[test]
fn counter_name_count_and_byte_budgets_fail_closed() {
    assert_eq!(Name::new(""), Err(CoordinationError::Configuration));
    assert_eq!(
        Name::new(vec![b'n'; MAX_NAME + 1]),
        Err(CoordinationError::Configuration)
    );
    assert!(Name::new(vec![b'n'; MAX_NAME]).is_ok());
    // An oversized name in a decoded request is refused as well.
    let long = crate::certified::encode(&vec![b'n'; MAX_NAME + 1]).unwrap();
    assert!(crate::certified::decode::<Name>(&long).is_err());

    let mut fixture = Fixture::new(1, Limits::default());
    for n in 0..MAX_COUNTERS {
        fixture
            .claim(0, &format!("series-{n}"), claim(1, T), T)
            .unwrap();
    }
    assert_eq!(
        fixture.claim(0, "one-too-many", claim(1, T), T),
        Err(CoordinationError::Backpressure)
    );
    // Existing counters keep counting.
    assert_eq!(fixture.claim(0, "series-0", claim(2, T), T), Ok(2));

    // Claims count against the tenant's bytes; a refused one takes no value.
    let mut fixture = Fixture::new(1, Limits::default());
    fixture.claim(0, "invoices", claim(1, T), T).unwrap();
    let used = fixture
        .state
        .tenant_used(fixture.owner.public_key())
        .unwrap();
    fixture.state.state.limits.tenant_bytes = used + 64;
    assert_eq!(fixture.claim(0, "invoices", claim(2, T), T), Ok(2));
    assert_eq!(
        fixture.claim(0, "invoices", claim(3, T), T),
        Err(CoordinationError::Backpressure)
    );
    assert_eq!(
        fixture.claim(0, "receipts", claim(3, T), T),
        Err(CoordinationError::Backpressure)
    );
    assert_eq!(
        fixture.reading("invoices", Some(claim(3, T)), T),
        Reading {
            value: 2,
            claim: Some(Lookup::Unclaimed),
        }
    );
    assert_eq!(fixture.reading("receipts", None, T).value, 0);
    // Retries of kept claims need no budget.
    assert_eq!(fixture.claim(1, "invoices", claim(1, T), T), Ok(1));
}
