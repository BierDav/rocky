//! Only the migration opening's identity may authorize its finite bulk run.
use crate::{
    AuthorBinding, CredentialChain, Endorsement, Error, IdentityProvider, PeerId, PublicKey,
    RevocationSet, TipGraph, decode_bounded, denied, encode,
};
use rocky_core::{Commitment, MigrationPublicationStatement};
use serde::{Deserialize, Serialize};

pub const MAX_MIGRATION_DESCRIPTOR_BYTES: usize = 32 * 1024;
const LABEL: &str = "rocky.migration-publication.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedMigrationPublication {
    pub statement: MigrationPublicationStatement,
    pub endorsement: Endorsement,
    pub writer: AuthorBinding,
}
impl SignedMigrationPublication {
    pub fn endorse(
        statement: MigrationPublicationStatement,
        signer: &dyn IdentityProvider,
        chain: &CredentialChain,
        writer: CredentialChain,
    ) -> Result<Self, Error> {
        statement.validate().map_err(|e| denied(&e.to_string()))?;
        let endorsement = Endorsement::sign(signer, chain, LABEL, &statement)?;
        let writer = writer
            .0
            .last()
            .ok_or_else(|| denied("migration writer certificate missing"))?
            .author_binding
            .clone();
        let signed = Self {
            statement,
            endorsement,
            writer,
        };
        signed.to_bytes()?;
        Ok(signed)
    }
    /// Identity excludes the endorsement so a renewed valid certificate is idempotent.
    pub fn id(&self) -> Result<Commitment, Error> {
        Self::statement_id(&self.statement)
    }
    pub fn statement_id(statement: &MigrationPublicationStatement) -> Result<Commitment, Error> {
        Ok(rocky_core::commitment(
            "rocky.migration-publication.id.v1",
            &encode(statement)?,
        ))
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let bytes = encode(self)?;
        if bytes.len() > MAX_MIGRATION_DESCRIPTOR_BYTES {
            return Err(denied("migration descriptor quota"));
        }
        Ok(bytes)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let signed: Self = decode_bounded(bytes, MAX_MIGRATION_DESCRIPTOR_BYTES)?;
        signed
            .statement
            .validate()
            .map_err(|e| denied(&e.to_string()))?;
        Ok(signed)
    }
    pub fn verify(
        &self,
        publisher: PublicKey,
        graph: &TipGraph,
        current: Option<(&RevocationSet, u64)>,
    ) -> Result<(), Error> {
        let s = &self.statement;
        s.validate().map_err(|e| denied(&e.to_string()))?;
        if graph.scope != s.scope {
            return Err(denied("migration descriptor domain"));
        }
        let opening = graph
            .openings
            .iter()
            .find(|opening| opening.id().ok() == Some(s.opening))
            .ok_or_else(|| denied("migration opening missing"))?;
        match current {
            Some((revocations, time)) => {
                opening.verify_current(publisher, graph, revocations, time)?
            }
            None => opening.verify(publisher, graph)?,
        }
        if !graph.migration_opening(opening, false)?
            || opening.tip()? != s.tip
            || opening.statement.seal != s.parent
            || opening.statement.schema != s.schema
        {
            return Err(denied("descriptor does not name a migration successor"));
        }
        self.endorsement
            .verify_tip_statement(publisher, s.scope, LABEL, s, current)?;
        let identity = opening.endorsement().signer(publisher);
        if self.endorsement.signer(publisher) != identity {
            return Err(denied("descriptor must be endorsed by the opening signer"));
        }
        let node = PublicKey::from_bytes(&s.writer).map_err(|_| denied("migration writer key"))?;
        self.writer.verify_node_issuer(identity, node, s.author)?;
        if let Some((revocations, _)) = current
            && (revocations.revokes_reference(&PeerId::of(node)) || revocations.revokes(&identity))
        {
            return Err(denied("migration writer revoked"));
        }
        Ok(())
    }
    pub fn verify_active(
        &self,
        publisher: PublicKey,
        graph: &TipGraph,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<(), Error> {
        self.verify(publisher, graph, Some((revocations, time)))?;
        graph.schema(self.statement.tip)?;
        let opening = graph
            .openings
            .iter()
            .find(|opening| opening.id().ok() == Some(self.statement.opening))
            .ok_or_else(|| denied("migration opening missing"))?;
        if !graph.migration_opening(opening, true)? {
            return Err(denied("migration minimum is no longer eligible"));
        }
        Ok(())
    }
}
