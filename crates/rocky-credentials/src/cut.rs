//! Owner-signed cuts of a domain's coordinated log; see
//! `docs/decisions/0074-owner-signed-recovery-cuts.md`.
use crate::{
    CredentialChain, Error, PrivateKey, PublicKey, RevocationSet, Signature, decode_bounded,
    denied, encode, signing_bytes,
};
use rocky_core::{
    Commitment, MAX_COUNTER, PartitionTuple, PrincipalKind, ResourceScope, Right, RowSelection,
    TipLog,
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
    pub log: TipLog,
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
        signing_bytes("rocky.coordinated-cut.v2", &(statement, signer))
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
    /// The signer's issuer authority over every scope in `scopes`, with the
    /// authenticated tuple of a partitioned one, as of the cut's issue
    /// time: the publisher itself, or a publisher-rooted user or deployment
    /// chain that may delegate and grants whole-scope writes.
    pub fn verify_owner(
        &self,
        credentials: &CredentialChain,
        publisher: PublicKey,
        scopes: &[(ResourceScope, Option<&PartitionTuple>)],
    ) -> Result<(), Error> {
        self.verify()?;
        if scopes
            .iter()
            .any(|(scope, _)| scope.domain != self.statement.log.scope)
        {
            return Err(denied("cut authority names another domain"));
        }
        if self.signer == publisher {
            return Ok(());
        }
        let revocations = RevocationSet::new();
        let leaf = credentials.links(publisher, Some((&revocations, self.statement.issued)))?;
        let whole = |rows: &RowSelection, rights: &std::collections::BTreeSet<Right>| {
            *rows == RowSelection::All
                && [Right::Read, Right::Insert, Right::Update, Right::Delete]
                    .iter()
                    .all(|right| rights.contains(right))
        };
        if leaf.subject != self.signer || leaf.kind == PrincipalKind::Node || leaf.delegations == 0
        {
            return Err(denied("cut signer is not an issuer of the domain"));
        }
        let partitions = leaf.extended()?.partitions;
        for (scope, tuple) in scopes {
            let granted = leaf
                .grants
                .iter()
                .any(|grant| grant.scope == *scope && whole(&grant.rows, &grant.rights));
            let selected = match tuple {
                Some(tuple) => partitions.iter().any(|grant| {
                    grant
                        .resolve(scope.domain, scope.table, tuple)
                        .ok()
                        .flatten()
                        .is_some_and(|resolved| {
                            resolved.scope == *scope && whole(&resolved.rows, &resolved.rights)
                        })
                }),
                None => false,
            };
            if !granted && !selected {
                return Err(denied("cut signer is not an issuer of the domain"));
            }
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
            log: TipLog {
                scope: rocky_core::Scope {
                    mesh: rocky_core::MeshId([1; 16]),
                    app: rocky_core::AppId([2; 16]),
                    domain: rocky_core::DomainId([3; 16]),
                },
                tip: rocky_core::TipId([4; 32]),
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
            .verify_owner(&CredentialChain(Vec::new()), publisher.public_key(), &[])
            .unwrap();
        let by_other = SignedCut::sign(statement(), &other).unwrap();
        assert!(
            by_other
                .verify_owner(&CredentialChain(Vec::new()), publisher.public_key(), &[],)
                .is_err()
        );
        let scope = |table| ResourceScope {
            domain: statement().log.scope,
            table: [table; 32],
            encryption: [0; 32],
        };
        let grant = |scope, rights: &[Right]| rocky_core::ResourceGrant {
            scope,
            rows: RowSelection::All,
            rights: rights.iter().copied().collect(),
            fields: rocky_core::FieldSelection::All,
        };
        let authority = crate::Authority::owner(&publisher);
        let all = [Right::Read, Right::Insert, Right::Update, Right::Delete];
        let chain = CredentialChain(vec![
            authority
                .issue(
                    other.public_key(),
                    vec![grant(scope(1), &all), grant(scope(2), &[Right::Read])],
                    crate::AuthorRange::ALL,
                    (8, 100, 0),
                )
                .unwrap(),
        ]);
        let verify = |scopes: &[(ResourceScope, Option<&PartitionTuple>)]| {
            by_other.verify_owner(&chain, publisher.public_key(), scopes)
        };
        verify(&[(scope(1), None)]).unwrap();
        assert!(verify(&[(scope(1), None), (scope(2), None)]).is_err());
        // A partitioned scope is covered through a selector of its table.
        let column = [8; 32];
        let value = |text: &str| {
            rocky_core::PartitionValue::new(
                rocky_core::KeyColumn {
                    affinity: rocky_core::Affinity::Text,
                    collation: rocky_core::Collation::Binary,
                },
                rocky_core::SqlValue::Text(text.into()),
            )
            .unwrap()
        };
        let tuple = PartitionTuple::new([(column, value("north"))]).unwrap();
        let partitioned = tuple.scope(statement().log.scope, [3; 32]).unwrap();
        let selected = CredentialChain(vec![
            authority
                .issue_partition_terms(
                    other.public_key(),
                    vec![grant(scope(1), &all)],
                    vec![crate::PartitionGrant {
                        domain: statement().log.scope,
                        table: [3; 32],
                        selector: rocky_core::PartitionSelector(
                            [(column, rocky_core::PartitionSelection::Any)].into(),
                        ),
                        rows: RowSelection::All,
                        rights: all.into_iter().collect(),
                        fields: rocky_core::FieldSelection::All,
                    }],
                    (1, None),
                    (8, 100, 0),
                )
                .unwrap(),
        ]);
        let owner = |chain: &CredentialChain,
                     scopes: &[(ResourceScope, Option<&PartitionTuple>)]| {
            by_other.verify_owner(chain, publisher.public_key(), scopes)
        };
        owner(&selected, &[(partitioned, Some(&tuple))]).unwrap();
        assert!(owner(&selected, &[(partitioned, None)]).is_err());
        assert!(owner(&chain, &[(partitioned, Some(&tuple))]).is_err());
        let mut later = statement();
        later.issued = 200;
        let expired = SignedCut::sign(later, &other).unwrap();
        assert!(
            expired
                .verify_owner(&chain, publisher.public_key(), &[(scope(1), None)])
                .is_err()
        );
    }
}
