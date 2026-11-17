//! The signed coordination wire: framed envelopes and one request with its
//! voter's answer, over any ordered stream on every target.
use crate::certified::*;
use p2panda_core::{PrivateKey, PublicKey, Signature};
use rocky_core::*;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Variants encode by name, so a client without the voters' Raft messages
/// speaks the same format.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum WireMessage<A, V, S> {
    Client(Box<crate::machine::Request>),
    Append(A),
    Vote(V),
    Snapshot(S),
    Status,
    SnapshotNow,
    Initialize,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub genesis: Commitment,
    pub key: PublicKey,
    pub nonce: Commitment,
    pub time: u64,
    pub body: Vec<u8>,
    signature: Signature,
}
impl Envelope {
    pub fn sign<T: Serialize>(
        genesis: Commitment,
        nonce: Commitment,
        body: &T,
        key: &PrivateKey,
    ) -> Result<Self, CoordinationError> {
        Self::from_body(genesis, nonce, encode(body)?, key)
    }
    pub fn from_body(
        genesis: Commitment,
        nonce: Commitment,
        body: Vec<u8>,
        key: &PrivateKey,
    ) -> Result<Self, CoordinationError> {
        let mut e = Self {
            genesis,
            key: key.public_key(),
            nonce,
            time: now()?,
            body,
            signature: key.sign(b""),
        };
        e.signature = key.sign(&e.signing_bytes()?);
        Ok(e)
    }
    fn signing_bytes(&self) -> Result<Vec<u8>, CoordinationError> {
        bytes(
            "rocky.coordination-wire.v3",
            &(self.genesis, self.key, self.nonce, self.time, &self.body),
        )
    }
    pub fn authenticate(&self, genesis: Commitment) -> Result<(), CoordinationError> {
        if self.genesis != genesis || !self.key.verify(&self.signing_bytes()?, &self.signature) {
            return Err(CoordinationError::Denied);
        }
        Ok(())
    }
    /// A voter's check of a request's signing time.
    #[cfg(not(target_family = "wasm"))]
    pub fn verify(&self, genesis: Commitment) -> Result<(), CoordinationError> {
        self.authenticate(genesis)?;
        let coordinator_time = now()?;
        if coordinator_time.abs_diff(self.time) > 30 {
            return Err(CoordinationError::ClockSkew { coordinator_time });
        }
        Ok(())
    }
}
pub fn nonce() -> Commitment {
    *PrivateKey::new().public_key().as_bytes()
}
pub async fn read(stream: &mut (impl AsyncRead + Unpin)) -> Result<Envelope, CoordinationError> {
    let len = stream
        .read_u32()
        .await
        .map_err(|_| CoordinationError::Unavailable)? as usize;
    if len > MAX_FRAME {
        return Err(CoordinationError::Denied);
    }
    let mut bytes = vec![0; len];
    stream
        .read_exact(&mut bytes)
        .await
        .map_err(|_| CoordinationError::Unavailable)?;
    decode(&bytes)
}
pub async fn write(
    stream: &mut (impl AsyncWrite + Unpin),
    envelope: &Envelope,
) -> Result<(), CoordinationError> {
    let bytes = encode(envelope)?;
    if bytes.len() > MAX_FRAME {
        return Err(CoordinationError::Denied);
    }
    stream
        .write_u32(bytes.len() as u32)
        .await
        .map_err(|_| CoordinationError::Unavailable)?;
    stream
        .write_all(&bytes)
        .await
        .map_err(|_| CoordinationError::Unavailable)?;
    stream
        .flush()
        .await
        .map_err(|_| CoordinationError::Unavailable)
}
/// One signed request and the signed answer of `voter` on `stream`.
///
/// The fresh request nonce binds the answer to this call even when the
/// device's clock is wrong, so a reply may still report clock skew.
pub async fn converse<T: Serialize + DeserializeOwned>(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    voter: PublicKey,
    genesis: Commitment,
    key: &PrivateKey,
    message: impl Serialize,
) -> Result<T, CoordinationError> {
    let envelope = Envelope::sign(genesis, nonce(), &message, key)?;
    write(stream, &envelope).await?;
    let response = read(stream).await?;
    response.authenticate(genesis)?;
    if response.key != voter || response.nonce != envelope.nonce {
        return Err(CoordinationError::Denied);
    }
    decode(&response.body)
}

/// Clients never construct or decode Raft RPCs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum NoRaft {}
pub(crate) type Message = WireMessage<NoRaft, NoRaft, NoRaft>;

#[cfg(not(target_family = "wasm"))]
pub async fn exchange<T: Serialize + DeserializeOwned>(
    voter: &Voter,
    genesis: Commitment,
    key: &PrivateKey,
    message: impl Serialize,
) -> Result<T, CoordinationError> {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        let mut stream = tokio::net::TcpStream::connect(&voter.address)
            .await
            .map_err(|_| CoordinationError::Unavailable)?;
        converse(&mut stream, voter.key, genesis, key, message).await
    })
    .await
    .map_err(|_| CoordinationError::Unavailable)?
}
