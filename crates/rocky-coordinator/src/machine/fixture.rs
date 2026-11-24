use super::*;
use p2panda_core::PrivateKey;

pub(super) const T: u64 = 1_900_000_000;
pub(super) struct Fixture {
    pub(super) state: Metadata,
    pub(super) owner: PrivateKey,
    pub(super) nodes: Vec<PrivateKey>,
    pub(super) scope: Scope,
}
impl Fixture {
    pub(super) fn new(limits: Limits) -> Self {
        let root = PrivateKey::new();
        let owner = PrivateKey::new();
        let nodes: Vec<_> = (0..2).map(|_| PrivateKey::new()).collect();
        let scope = Scope {
            mesh: MeshId([1; 16]),
            app: AppId([2; 16]),
            domain: DomainId([3; 16]),
        };
        let certificate = ControlCertificate::sign(
            DomainControl {
                scope,
                configuration: [4; 16],
                revision: 1,
                admins: nodes.iter().map(PrivateKey::public_key).collect(),
                publisher: Some(Box::new(owner.public_key())),
                not_before: T - 1,
                expires: T + 100_000,
            },
            &owner,
        )
        .unwrap();
        let genesis = CertifiedGenesis::sign(
            Genesis {
                mesh: scope.mesh,
                configuration: [4; 16],
                voters: (1..=3)
                    .map(|id| Voter {
                        id,
                        address: format!("127.0.0.1:{}", 8000 + id),
                        key: PrivateKey::new().public_key(),
                    })
                    .collect(),
                initial_voters: [1, 2, 3].into(),
                membership_authority: owner.public_key(),
                authorities: vec![Authority {
                    scope,
                    issuer: owner.public_key(),
                }],
                max_control_lifetime: 200_000,
                public: None,
            },
            &root,
        )
        .unwrap();
        let mut state = Metadata::with_limits(genesis, limits).unwrap();
        state.control(certificate).unwrap();
        Self {
            state,
            owner,
            nodes,
            scope,
        }
    }
    pub(super) fn certified(&self, operation: Operation, key: &PrivateKey, time: u64) -> Request {
        Request::certified(
            self.state.genesis.id().unwrap(),
            crate::transport::nonce(),
            operation,
            (
                key,
                Some(crate::machine::self_certificate_in(
                    key,
                    rocky_credentials::DatabaseContext {
                        publisher: self.owner.public_key(),
                        scope: self.scope,
                    },
                )),
            ),
            time,
        )
        .unwrap()
    }
}
