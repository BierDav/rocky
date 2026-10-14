//! Owner-signed cuts of a domain's coordinated log; see
//! `docs/decisions/0074-owner-signed-recovery-cuts.md`.
use crate::{
    CredentialChain, Error, PrivateKey, PublicKey, RevocationSet, Signature, decode_bounded,
    denied, encode, signing_bytes,
};
use rocky_core::{
    Commitment, MAX_COUNTER, PrincipalKind, ResourceScope, Right, RowSelection, Scope,
};
use serde::{Deserialize, Serialize};

/// Largest signed cut record a coordinator retains.
pub const MAX_CUT_BYTES: usize = 2048;
/// Largest owner credential chain carried beside a cut record.
pub const MAX_CUT_AUTHORITY_BYTES: usize = 8 * 1024;

/// A log position and the running hash after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CutPoint {
    pub position: u64,
    pub running: Commitment,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CutStatement {
    pub log: Scope,
    /// This log's cut count, from 1.
    pub sequence: u64,
    /// The digest of the previous cut.
    pub previous: Option<Commitment>,
    /// The last position the proposer verified against the window or genesis.
    pub verified: CutPoint,
    /// The canonical prefix end: `verified ≤ retained ≤ replaced`.
    pub retained: CutPoint,
    /// The exact coordinator commit the cut replaces.
    pub replaced: CutPoint,
    pub epoch: u64,
    pub issued: u64,
}
impl CutStatement {
    fn validate(&self) -> Result<(), Error> {
        if self.sequence == 0
            || self.sequence > MAX_COUNTER
            || (self.sequence == 1) != self.previous.is_none()
            || self.verified.position > self.retained.position
            || self.retained.position > self.replaced.position
            || self.replaced.position >= MAX_COUNTER
            || (self.retained.position == self.replaced.position
                && self.retained.running != self.replaced.running)
            || (self.verified.position == self.retained.position
                && self.verified.running != self.retained.running)
        {
            return Err(denied("invalid cut statement"));
        }
        Ok(())
    }
    /// The log position the cut itself occupies.
    pub fn entry(&self) -> u64 {
        self.replaced.position + 1
    }
    /// Whether this cut abandons the entry at `position`.
    pub fn abandons(&self, position: u64) -> bool {
        self.retained.position < position && position <= self.replaced.position
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedCut {
    pub statement: CutStatement,
    pub signer: PublicKey,
    signature: Signature,
}
impl SignedCut {
    fn bytes(statement: &CutStatement, signer: PublicKey) -> Result<Vec<u8>, Error> {
        signing_bytes("rocky.coordinated-cut.v1", &(statement, signer))
    }
    pub fn sign(statement: CutStatement, owner: &PrivateKey) -> Result<Self, Error> {
        statement.validate()?;
        let signer = owner.public_key();
        let signature = owner.sign(&Self::bytes(&statement, signer)?);
        let signed = Self {
            statement,
            signer,
            signature,
        };
        signed.to_bytes()?;
        Ok(signed)
    }
    /// The shape and the signer's signature; not the signer's authority.
    pub fn verify(&self) -> Result<(), Error> {
        self.statement.validate()?;
        if !self
            .signer
            .verify(&Self::bytes(&self.statement, self.signer)?, &self.signature)
        {
            return Err(denied("invalid cut signature"));
        }
        Ok(())
    }
    /// The entry hash the cut occupies its position with.
    pub fn digest(&self) -> Result<Commitment, Error> {
        Ok(rocky_core::commitment(
            "rocky.coordinated-cut.id.v1",
            &self.to_bytes()?,
        ))
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let bytes = encode(self)?;
        if bytes.len() > MAX_CUT_BYTES {
            return Err(denied("cut record quota"));
        }
        Ok(bytes)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let signed: Self = decode_bounded(bytes, MAX_CUT_BYTES)?;
        signed.verify()?;
        Ok(signed)
    }
    /// The signer's issuer authority over every scope in `scopes`, as of the
    /// cut's issue time: the publisher itself, or a publisher-rooted user or
    /// deployment chain that may delegate and grants whole-scope writes.
    pub fn verify_owner(
        &self,
        credentials: &CredentialChain,
        publisher: PublicKey,
        manifest: Commitment,
        scopes: &[ResourceScope],
    ) -> Result<(), Error> {
        self.verify()?;
        if scopes
            .iter()
            .any(|scope| scope.domain != self.statement.log)
        {
            return Err(denied("cut authority names another domain"));
        }
        if self.signer == publisher {
            return Ok(());
        }
        let revocations = RevocationSet::new();
        let leaf = credentials.links(
            publisher,
            manifest,
            Some((&revocations, self.statement.issued)),
        )?;
        let whole = [Right::Read, Right::Insert, Right::Update, Right::Delete];
        if leaf.subject != self.signer
            || matches!(leaf.kind, PrincipalKind::Node | PrincipalKind::Pseudonym)
            || leaf.delegations == 0
            || scopes.iter().any(|scope| {
                !leaf.grants.iter().any(|grant| {
                    grant.scope == *scope
                        && grant.rows == RowSelection::All
                        && whole.iter().all(|right| grant.rights.contains(right))
                })
            })
        {
            return Err(denied("cut signer is not an issuer of the domain"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn statement() -> CutStatement {
        let point = |position, byte| CutPoint {
            position,
            running: [byte; 32],
        };
        CutStatement {
            log: Scope {
                mesh: rocky_core::MeshId([1; 16]),
                app: rocky_core::AppId([2; 16]),
                domain: rocky_core::DomainId([3; 16]),
            },
            sequence: 1,
            previous: None,
            verified: point(2, 4),
            retained: point(3, 5),
            replaced: point(5, 6),
            epoch: 7,
            issued: 8,
        }
    }
    #[test]
    fn cut_records_roundtrip_bounded_and_refuse_invalid_shapes() {
        let owner = PrivateKey::new();
        let signed = SignedCut::sign(statement(), &owner).unwrap();
        let bytes = signed.to_bytes().unwrap();
        assert!(bytes.len() <= MAX_CUT_BYTES);
        assert_eq!(SignedCut::from_bytes(&bytes).unwrap(), signed);
        assert_eq!(signed.statement.entry(), 6);
        assert!(!signed.statement.abandons(3));
        assert!(signed.statement.abandons(4) && signed.statement.abandons(5));
        assert!(!signed.statement.abandons(6));

        let mut tampered = signed.clone();
        tampered.statement.retained.position = 4;
        assert_eq!(
            tampered.verify().unwrap_err().to_string(),
            "invalid cut signature"
        );
        let invalid = [
            CutStatement {
                sequence: 0,
                ..statement()
            },
            CutStatement {
                previous: Some([9; 32]),
                ..statement()
            },
            CutStatement {
                sequence: 2,
                ..statement()
            },
            CutStatement {
                verified: CutPoint {
                    position: 4,
                    running: [4; 32],
                },
                ..statement()
            },
            CutStatement {
                retained: CutPoint {
                    position: 6,
                    running: [5; 32],
                },
                ..statement()
            },
            // Abandoning nothing must name the replaced hash.
            CutStatement {
                retained: CutPoint {
                    position: 5,
                    running: [5; 32],
                },
                ..statement()
            },
        ];
        for statement in invalid {
            assert_eq!(
                SignedCut::sign(statement, &owner).unwrap_err().to_string(),
                "invalid cut statement"
            );
        }
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(SignedCut::from_bytes(&longer).is_err());
    }
    #[test]
    fn the_publisher_is_an_owner_and_another_key_needs_a_chain() {
        let publisher = PrivateKey::new();
        let other = PrivateKey::new();
        let by_publisher = SignedCut::sign(statement(), &publisher).unwrap();
        by_publisher
            .verify_owner(
                &CredentialChain(Vec::new()),
                publisher.public_key(),
                [0; 32],
                &[],
            )
            .unwrap();
        let by_other = SignedCut::sign(statement(), &other).unwrap();
        assert!(
            by_other
                .verify_owner(
                    &CredentialChain(Vec::new()),
                    publisher.public_key(),
                    [0; 32],
                    &[],
                )
                .is_err()
        );
        let scope = |table| ResourceScope {
            domain: statement().log,
            table: [table; 32],
            encryption: [0; 32],
        };
        let grant = |scope, rights: &[Right]| rocky_core::ResourceGrant {
            scope,
            rows: RowSelection::All,
            rights: rights.iter().copied().collect(),
            fields: Default::default(),
        };
        let authority = crate::Authority {
            publisher: &publisher,
            manifest: [0; 32],
        };
        let all = [Right::Read, Right::Insert, Right::Update, Right::Delete];
        let chain = CredentialChain(vec![
            authority
                .issue(
                    other.public_key(),
                    vec![grant(scope(1), &all), grant(scope(2), &[Right::Read])],
                    (8, 100, 0),
                )
                .unwrap(),
        ]);
        let verify = |scopes: &[ResourceScope]| {
            by_other.verify_owner(&chain, publisher.public_key(), [0; 32], scopes)
        };
        verify(&[scope(1)]).unwrap();
        assert!(verify(&[scope(1), scope(2)]).is_err());
        let mut later = statement();
        later.issued = 200;
        let expired = SignedCut::sign(later, &other).unwrap();
        assert!(
            expired
                .verify_owner(&chain, publisher.public_key(), [0; 32], &[scope(1)])
                .is_err()
        );
    }
}
