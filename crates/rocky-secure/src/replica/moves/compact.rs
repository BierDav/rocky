//! Carried witnesses share their original signed headers and binding chains.
use super::*;

pub(crate) struct ProofParts {
    pub(crate) origin: Vec<u8>,
    pub(crate) identity: i64,
    pub(crate) provenance: Commitment,
    pub(crate) bindings: Vec<u8>,
    pub(crate) header: Vec<u8>,
}

impl CreationProof {
    pub(crate) fn wire_parts(&self) -> Result<ProofParts, Error> {
        Ok(ProofParts {
            origin: encode(&self.stored()?.origin())?,
            identity: self.id.integer(),
            provenance: self.provenance,
            bindings: encode(&self.bindings)?,
            header: self.header.clone(),
        })
    }
    pub(crate) fn wire_references(origin: &[u8]) -> Result<(Commitment, Commitment), Error> {
        let origin: Origin = decode(origin)?;
        Ok((origin.bindings, origin.header))
    }
    pub(crate) fn from_wire_parts(
        origin: &[u8],
        identity: i64,
        provenance: Commitment,
        bindings: &[u8],
        header: &[u8],
    ) -> Result<Self, Error> {
        let origin: Origin = decode(origin)?;
        if origin.bindings != bindings_digest(bindings) || origin.header != header_digest(header) {
            return Err(denied("carried proof legend digest mismatch"));
        }
        Ok(Self {
            version: origin.version,
            manifest: origin.manifest,
            table: origin.table,
            id: rocky_core::RowIdentity::from_integer(identity)
                .map_err(|error| denied(&error.to_string()))?,
            provenance,
            stamp: origin.stamp,
            bindings: decode(bindings)?,
            signer: origin.signer,
            header: header.to_vec(),
        })
    }
}

impl Origin {
    pub(in crate::replica) fn checkpoint_bytes(&self) -> Result<Vec<u8>, Error> {
        encode(&(
            serde_bytes::ByteArray::new(self.manifest),
            serde_bytes::ByteArray::new(self.bindings),
            serde_bytes::ByteArray::new(self.header),
        ))
    }
    pub(in crate::replica) fn checkpoint_header(bytes: &[u8]) -> Result<Commitment, Error> {
        let (_, _, header): (
            serde_bytes::ByteArray<32>,
            serde_bytes::ByteArray<32>,
            serde_bytes::ByteArray<32>,
        ) = decode(bytes)?;
        Ok(header.into_array())
    }
    pub(in crate::replica) fn from_checkpoint(
        bytes: &[u8],
        header: &crypto::CreationHeader,
    ) -> Result<Self, Error> {
        let (manifest, bindings, digest): (
            serde_bytes::ByteArray<32>,
            serde_bytes::ByteArray<32>,
            serde_bytes::ByteArray<32>,
        ) = decode(bytes)?;
        if header_digest(header.bytes()) != digest.into_array() {
            return Err(denied("checkpoint birth header digest mismatch"));
        }
        let signed = header.signed();
        Ok(Self {
            version: 2,
            manifest: manifest.into_array(),
            table: signed.extensions.scope.table,
            stamp: Stamp {
                clock: signed.timestamp,
                author: signed
                    .extensions
                    .author
                    .ok_or_else(|| denied("checkpoint birth lacks an author"))?,
                sequence: signed.extensions.author_sequence,
            },
            bindings: bindings.into_array(),
            signer: signed.public_key.into(),
            header: digest.into_array(),
        })
    }
    pub(in crate::replica) fn checked_checkpoint_bytes(
        &self,
        header: &crypto::CreationHeader,
    ) -> Result<Vec<u8>, Error> {
        let bytes = self.checkpoint_bytes()?;
        if encode(&Self::from_checkpoint(&bytes, header)?)? != encode(self)? {
            return Err(Error::Corrupt(
                "birth origin differs from its signed header".into(),
            ));
        }
        Ok(bytes)
    }
}
