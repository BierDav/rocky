use super::fixture::{Fixture, T};
use super::*;
use crate::counters::{
    CLAIM_WINDOW, Claim, Claimed, Claimed::*, Counters, MAX_CLAIM_LIFETIME, MAX_CLAIMS,
    MAX_COUNTERS, MAX_NAME, Name,
};
use p2panda_core::PrivateKey;

fn name(text: &str) -> Name {
    Name::new(text).unwrap()
}
fn claim(n: u8, issued: u64) -> Claim {
    Claim::fixed(n, n, issued + CLAIM_WINDOW)
}
impl Fixture {
    fn operate(
        &mut self,
        key: &PrivateKey,
        operation: Operation,
        time: u64,
    ) -> Result<Outcome, CoordinationError> {
        let request = self.certified(operation, key, time);
        self.state.apply_at(time, &request)
    }
    fn claim_as(
        &mut self,
        key: &PrivateKey,
        counter: &str,
        claim: Claim,
        time: u64,
    ) -> Result<Claimed, CoordinationError> {
        let operation = Operation::claim(self.scope, name(counter), claim);
        match self.operate(key, operation, time)? {
            Outcome::Claimed(claimed) => Ok(claimed),
            other => panic!("claim: {other:?}"),
        }
    }
    fn claim(
        &mut self,
        node: usize,
        counter: &str,
        claim: Claim,
        time: u64,
    ) -> Result<Claimed, CoordinationError> {
        let key = self.nodes[node].clone();
        self.claim_as(&key, counter, claim, time)
    }
    fn reading(&self, counter: &str, time: u64) -> i64 {
        let request = self.certified(
            Operation::Counter {
                scope: self.scope,
                counter: name(counter),
            },
            &self.nodes[0],
            time,
        );
        assert!(request.read_only());
        match self.state.read(&request).unwrap() {
            Outcome::Counter(value) => value,
            other => panic!("reading: {other:?}"),
        }
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn claims_take_distinct_increasing_values_and_retries_repeat_them() {
    let mut fixture = Fixture::new(Limits::default());
    let retry_records = fixture.state.retry_records();
    let mut values = Vec::new();
    for n in 0..20u8 {
        let Ok(First(value)) = fixture.claim(usize::from(n % 2), "invoices", claim(n, T), T) else {
            panic!("claim {n} takes a first number");
        };
        assert_eq!(
            fixture.claim(usize::from(n % 2 == 0), "invoices", claim(n, T), T + 1),
            Ok(First(value)),
            "a claim retried by the other admin node answers the same value"
        );
        values.push(value);
    }
    assert_eq!(values, (1..=20).collect::<Vec<_>>());
    for n in 0..20u8 {
        assert_eq!(
            fixture.claim(0, "invoices", claim(n, T), T + 2),
            Ok(First(i64::from(n) + 1)),
            "every earlier claim still answers its own value"
        );
    }
    assert_eq!(
        fixture.claim(0, "invoices", Claim::fixed(3, 99, T + CLAIM_WINDOW), T + 2),
        Ok(Taken(4)),
        "another attempt on a key in force finds the number taken"
    );
    assert_eq!(
        fixture.claim(1, "receipts", claim(3, T), T + 2),
        Ok(First(1)),
        "counters are independent; the same key may claim on another"
    );
    assert_eq!(fixture.reading("invoices", T + 2), 20);
    assert_eq!(fixture.reading("unused", T + 2), 0);
    assert_eq!(
        fixture.state.retry_records(),
        retry_records,
        "claims keep no machine retry record; their keys deduplicate"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn two_doors_scanning_one_ticket_admit_it_exactly_once() {
    for first_door in [0, 1] {
        let mut fixture = Fixture::new(Limits::default());
        let ticket = |door: u8| Claim::fixed(7, door, T + CLAIM_WINDOW);
        let doors = [first_door, 1 - first_door];
        let answers: Vec<_> = doors
            .iter()
            .map(|&door| fixture.claim(door, "door", ticket(door as u8), T).unwrap())
            .collect();
        assert_eq!(
            answers,
            [First(1), Taken(1)],
            "the first scan admits and the other door finds the ticket taken"
        );
        for (&door, answer) in doors.iter().zip(&answers) {
            assert_eq!(
                fixture.claim(door, "door", ticket(door as u8), T + 1),
                Ok(*answer),
                "each door's retry repeats its own answer"
            );
        }
        assert_eq!(
            fixture.reading("door", T + 1),
            1,
            "the ticket took one number"
        );
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn snapshot_round_trip_keeps_values_and_claim_keys() {
    let mut fixture = Fixture::new(Limits::default());
    for n in 0..5 {
        fixture.claim(0, "invoices", claim(n, T), T).unwrap();
    }
    let image = crate::certified::encode(&fixture.state).unwrap();
    fixture.state = crate::certified::decode(&image).unwrap();
    assert_eq!(
        fixture.claim(1, "invoices", claim(2, T), T + 1),
        Ok(First(3))
    );
    assert_eq!(
        fixture.claim(1, "invoices", Claim::fixed(2, 9, T + CLAIM_WINDOW), T + 1),
        Ok(Taken(3))
    );
    assert_eq!(
        fixture.claim(1, "invoices", claim(9, T), T + 1),
        Ok(First(6))
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn only_admin_nodes_claim_and_only_while_the_domain_is_open() {
    let mut fixture = Fixture::new(Limits::default());
    let stranger = PrivateKey::new();
    let owner = fixture.owner.clone();
    for key in [&stranger, &owner] {
        assert_eq!(
            fixture.claim_as(key, "invoices", claim(1, T), T),
            Err(CoordinationError::Denied(Refusal::NotMember)),
            "neither an unknown node nor the domain's control authority is an admin node"
        );
        let read = Operation::Counter {
            scope: fixture.scope,
            counter: name("invoices"),
        };
        assert_eq!(
            fixture.operate(key, read, T),
            Err(CoordinationError::Denied(Refusal::NotMember))
        );
    }
    let uncertified = Request::sign_at(
        fixture.state.genesis.id().unwrap(),
        [1; 32],
        Operation::claim(fixture.scope, name("invoices"), claim(1, T)),
        &fixture.nodes[0],
        T,
    )
    .unwrap();
    assert_eq!(
        fixture.state.apply_at(T, &uncertified),
        Err(CoordinationError::Denied(Refusal::NotMember)),
        "a request without a node certificate presents no admin identity"
    );
    let mut unknown = fixture.scope;
    unknown.domain = DomainId([9; 16]);
    let key = fixture.nodes[0].clone();
    assert_eq!(
        fixture.operate(
            &key,
            Operation::claim(unknown, name("invoices"), claim(1, T)),
            T,
        ),
        Err(CoordinationError::UnknownDomain)
    );
    assert_eq!(fixture.reading("invoices", T), 0);

    assert_eq!(fixture.claim(0, "invoices", claim(1, T), T), Ok(First(1)));
    let close = Operation::Close {
        scope: fixture.scope,
    };
    fixture.operate(&owner, close, T).unwrap();
    assert_eq!(
        fixture.claim(0, "invoices", claim(2, T), T),
        Err(CoordinationError::Denied(Refusal::ClosedDomain)),
        "a closed domain takes no claim, not even a retry"
    );
    assert_eq!(
        fixture.claim(0, "invoices", claim(1, T), T),
        Err(CoordinationError::Denied(Refusal::ClosedDomain))
    );
    assert_eq!(fixture.reading("invoices", T), 1);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_claim_past_its_expiry_takes_no_number_and_an_expired_key_claims_anew() {
    let mut counters = Counters::default();
    let door = name("door");
    let mut take = |claim, time| counters.claim(&door, claim, time, usize::MAX);
    let end = T + CLAIM_WINDOW;
    assert_eq!(take(claim(1, T), T), Ok(First(1)));
    assert_eq!(take(claim(1, T), end - 1), Ok(First(1)));
    assert_eq!(take(Claim::fixed(1, 2, end + 10), end - 1), Ok(Taken(1)));
    assert_eq!(
        take(claim(1, T), end),
        Err(CoordinationError::Expired),
        "a retry at its expiry is expired, never given a second number"
    );
    assert_eq!(
        take(Claim::fixed(1, 2, end + 10), end),
        Ok(First(2)),
        "a key whose claim expired may be claimed by a new attempt"
    );
    assert_eq!(
        take(claim(3, T - CLAIM_WINDOW), T),
        Err(CoordinationError::Expired),
        "a claim that expired before it arrived takes no number"
    );
    assert_eq!(
        take(Claim::fixed(4, 4, end + MAX_CLAIM_LIFETIME + 1), end),
        Err(CoordinationError::Configuration),
        "a claim may not stay in force beyond the longest lifetime"
    );
    assert_eq!(
        take(Claim::fixed(4, 4, end + MAX_CLAIM_LIFETIME), end),
        Ok(First(3))
    );
    assert_eq!(counters.read(&door), 3);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_full_counter_refuses_new_keys_and_never_admits_a_ticket_in_force_twice() {
    let mut counters = Counters::default();
    let door = name("door");
    let ticket = |n: usize, attempt: u8| {
        let mut key = [0; 32];
        key[..8].copy_from_slice(&(n as u64).to_be_bytes());
        (key, attempt)
    };
    let early = T + 10;
    for n in 0..MAX_CLAIMS {
        let expires = if n == 0 { early } else { T + CLAIM_WINDOW };
        let claim = crate::counters::Claim::keyed_for_test(ticket(n, 1), expires);
        assert_eq!(
            counters.claim(&door, claim, T, usize::MAX),
            Ok(First(n as i64 + 1))
        );
    }
    let newcomer = crate::counters::Claim::keyed_for_test(ticket(MAX_CLAIMS, 1), T + CLAIM_WINDOW);
    assert_eq!(
        counters.claim(&door, newcomer, T, usize::MAX),
        Err(CoordinationError::Full),
        "a full counter takes no number for a new key"
    );
    for n in [0, 1, MAX_CLAIMS / 2, MAX_CLAIMS - 1] {
        let rescan = crate::counters::Claim::keyed_for_test(ticket(n, 2), T + CLAIM_WINDOW);
        assert_eq!(
            counters.claim(&door, rescan, T, usize::MAX),
            Ok(Taken(n as i64 + 1)),
            "every ticket in force stays admitted once, however many came after it"
        );
    }
    assert_eq!(
        counters.claim(&door, newcomer, early, usize::MAX),
        Ok(First(MAX_CLAIMS as i64 + 1)),
        "an expired claim makes room, and numbers stay gap-free"
    );
    assert_eq!(
        counters.claim(
            &door,
            crate::counters::Claim::keyed_for_test(ticket(1, 2), T + CLAIM_WINDOW),
            early,
            usize::MAX
        ),
        Ok(Taken(2))
    );
    assert_eq!(counters.bytes(), 128 + "door".len() + MAX_CLAIMS * 96);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn counter_name_count_and_byte_budgets_fail_closed() {
    assert_eq!(Name::new(""), Err(CoordinationError::Configuration));
    assert_eq!(
        Name::new(vec![b'n'; MAX_NAME + 1]),
        Err(CoordinationError::Configuration)
    );
    assert!(Name::new(vec![b'n'; MAX_NAME]).is_ok());
    let long = crate::certified::encode(&vec![b'n'; MAX_NAME + 1]).unwrap();
    assert!(
        crate::certified::decode::<Name>(&long).is_err(),
        "an oversized name in a decoded request is refused as well"
    );

    let mut fixture = Fixture::new(Limits::default());
    for n in 0..MAX_COUNTERS {
        fixture
            .claim(0, &format!("series-{n}"), claim(1, T), T)
            .unwrap();
    }
    assert_eq!(
        fixture.claim(0, "one-too-many", claim(1, T), T),
        Err(CoordinationError::Backpressure)
    );
    assert_eq!(
        fixture.claim(0, "series-0", claim(2, T), T),
        Ok(First(2)),
        "existing counters keep counting"
    );

    let mut fixture = Fixture::new(Limits::default());
    fixture.claim(0, "invoices", claim(1, T), T).unwrap();
    let used = fixture
        .state
        .tenant_used(fixture.owner.public_key())
        .unwrap();
    fixture.state.state.limits.tenant_bytes = used + 96;
    assert_eq!(
        fixture.claim(0, "invoices", Claim::fixed(2, 2, T + 10), T),
        Ok(First(2)),
        "claims count against the tenant's bytes"
    );
    assert_eq!(
        fixture.claim(0, "invoices", claim(3, T), T),
        Err(CoordinationError::Backpressure)
    );
    assert_eq!(
        fixture.claim(0, "receipts", claim(3, T), T),
        Err(CoordinationError::Backpressure)
    );
    assert_eq!(fixture.reading("invoices", T), 2);
    assert_eq!(fixture.reading("receipts", T), 0);
    assert_eq!(
        fixture.claim(1, "invoices", claim(1, T), T),
        Ok(First(1)),
        "retries of kept claims need no budget"
    );
    assert_eq!(
        fixture.claim(0, "invoices", claim(3, T + 10), T + 10),
        Ok(First(3)),
        "expired claims free the bytes a new one needs"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn admin_siblings_claim_until_their_identity_unlinks_them() {
    use rocky_credentials::{Authority, CredentialChain, Revocation, SignedRevocation};
    let mut f = Fixture::new(Limits::default());
    let identity = f.nodes[0].clone();
    let grant = CredentialChain(vec![
        Authority::owner(&f.owner)
            .issue_trusted(identity.public_key(), f.scope, T)
            .unwrap(),
    ]);
    let nodes = [PrivateKey::new(), PrivateKey::new(), PrivateKey::new()];
    let mut previous = None;
    let chains: Vec<_> = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| {
            let chain = grant
                .clone()
                .certify_member(&identity, node, index as u64, previous)
                .unwrap();
            previous = Some(chain.certificate().unwrap().id().unwrap());
            chain
        })
        .collect();
    let request = |index: usize, n: u8| {
        Request::certified(
            f.state.genesis.id().unwrap(),
            [n; 32],
            Operation::claim(f.scope, name("siblings"), claim(n, T)),
            (&nodes[index], chains[index].certificate().cloned()),
            T,
        )
        .unwrap()
    };
    let requests: Vec<_> = (0..3)
        .map(|index| request(index, index as u8 + 1))
        .collect();
    for (index, request) in requests.iter().enumerate() {
        assert_eq!(
            f.state.apply_at(T, request),
            Ok(Outcome::Claimed(First(index as i64 + 1)))
        );
    }
    let unlink = SignedRevocation::sign(
        Revocation {
            app: f.scope.app,
            issuer: identity.public_key(),
            time: T,
            nodes: [rocky_credentials::PeerId::of(nodes[1].public_key())].into(),
            identities: Default::default(),
            capabilities: Default::default(),
            cut: Default::default(),
            mode: rocky_credentials::RevocationMode::Lost,
        },
        &identity,
        &chains[1],
    )
    .unwrap();
    let mut authority = RevocationSet::try_from(vec![unlink]).unwrap();
    for chain in &chains {
        authority
            .link(chain.certificate().unwrap().clone())
            .unwrap();
    }
    let push = Request::sign_at(
        f.state.genesis.id().unwrap(),
        [10; 32],
        Operation::Revoke {
            scope: f.scope,
            revocations: Box::new(authority),
        },
        &f.owner,
        T,
    )
    .unwrap();
    f.state.apply_at(T, &push).unwrap();
    for index in 0..3 {
        let request = Request::certified(
            f.state.genesis.id().unwrap(),
            [20 + index as u8; 32],
            Operation::claim(f.scope, name("after unlink"), claim(20 + index as u8, T)),
            (&nodes[index], chains[index].certificate().cloned()),
            T,
        )
        .unwrap();
        let result = f.state.apply_at(T, &request);
        if index == 1 {
            assert_eq!(result, Err(CoordinationError::Denied(Refusal::NotMember)));
        } else {
            assert!(matches!(result, Ok(Outcome::Claimed(_))));
        }
    }
}
