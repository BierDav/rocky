use crate::{certified::*, raft_store::Types};
use openraft::{
    BasicNode, RaftNetwork, RaftNetworkFactory,
    error::{InstallSnapshotError, NetworkError, RPCError, RaftError},
    network::RPCOption,
    raft::*,
};
use p2panda_core::{PrivateKey, PublicKey, Signature};
use rocky_core::*;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum Message {
    Client(Box<crate::machine::Request>),
    Append(AppendEntriesRequest<Types>),
    Vote(VoteRequest<u64>),
    Snapshot(InstallSnapshotRequest<Types>),
    Status,
    SnapshotNow,
    Initialize,
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Envelope {
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
            "rocky.coordination-wire.v2",
            &(self.genesis, self.key, self.nonce, self.time, &self.body),
        )
    }
    pub fn verify(&self, genesis: Commitment) -> Result<(), CoordinationError> {
        if self.genesis != genesis
            || now()?.abs_diff(self.time) > 30
            || !self.key.verify(&self.signing_bytes()?, &self.signature)
        {
            return Err(CoordinationError::Denied);
        }
        Ok(())
    }
}
pub(crate) fn nonce() -> Commitment {
    *PrivateKey::new().public_key().as_bytes()
}
pub(crate) async fn read(
    stream: &mut (impl AsyncRead + Unpin),
) -> Result<Envelope, CoordinationError> {
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
pub(crate) async fn write(
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
pub(crate) async fn exchange<T: Serialize + DeserializeOwned>(
    voter: &Voter,
    genesis: Commitment,
    key: &PrivateKey,
    message: Message,
) -> Result<T, CoordinationError> {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        let mut stream = TcpStream::connect(&voter.address)
            .await
            .map_err(|_| CoordinationError::Unavailable)?;
        converse(&mut stream, voter.key, genesis, key, message).await
    })
    .await
    .map_err(|_| CoordinationError::Unavailable)?
}
/// One signed request and the signed answer of `voter` on `stream`.
pub(crate) async fn converse<T: Serialize + DeserializeOwned>(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    voter: PublicKey,
    genesis: Commitment,
    key: &PrivateKey,
    message: Message,
) -> Result<T, CoordinationError> {
    let envelope = Envelope::sign(genesis, nonce(), &message, key)?;
    write(stream, &envelope).await?;
    let response = read(stream).await?;
    response.verify(genesis)?;
    if response.key != voter || response.nonce != envelope.nonce {
        return Err(CoordinationError::Denied);
    }
    decode(&response.body)
}
#[derive(Clone)]
pub(crate) struct Network {
    pub genesis: CertifiedGenesis,
    pub key: PrivateKey,
}
pub(crate) struct Connection {
    network: Network,
    voter: Option<Voter>,
    target: u64,
}
impl RaftNetworkFactory<Types> for Network {
    type Network = Connection;
    async fn new_client(&mut self, target: u64, node: &BasicNode) -> Connection {
        let voter = self
            .genesis
            .genesis
            .voters
            .iter()
            .find(|v| v.id == target && v.address == node.addr)
            .cloned();
        Connection {
            network: self.clone(),
            voter,
            target,
        }
    }
}
impl Connection {
    // The error type is fixed by openraft.
    #[allow(clippy::result_large_err)]
    async fn send<T: Serialize + DeserializeOwned, E: std::error::Error>(
        &self,
        message: Message,
    ) -> Result<T, RPCError<u64, BasicNode, E>> {
        exchange(
            self.voter.as_ref().ok_or_else(|| {
                RPCError::Network(NetworkError::new(&CoordinationError::Configuration))
            })?,
            self.network
                .genesis
                .id()
                .map_err(|e| RPCError::Network(NetworkError::new(&e)))?,
            &self.network.key,
            message,
        )
        .await
        .map_err(|e| RPCError::Network(NetworkError::new(&e)))
    }
}
impl RaftNetwork<Types> for Connection {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<Types>,
        _: RPCOption,
    ) -> Result<AppendEntriesResponse<u64>, RPCError<u64, BasicNode, RaftError<u64>>> {
        let result: Result<AppendEntriesResponse<u64>, RaftError<u64>> =
            self.send(Message::Append(rpc)).await?;
        result.map_err(|e| RPCError::RemoteError(openraft::error::RemoteError::new(self.target, e)))
    }
    async fn vote(
        &mut self,
        rpc: VoteRequest<u64>,
        _: RPCOption,
    ) -> Result<VoteResponse<u64>, RPCError<u64, BasicNode, RaftError<u64>>> {
        let result: Result<VoteResponse<u64>, RaftError<u64>> =
            self.send(Message::Vote(rpc)).await?;
        result.map_err(|e| RPCError::RemoteError(openraft::error::RemoteError::new(self.target, e)))
    }
    async fn install_snapshot(
        &mut self,
        rpc: InstallSnapshotRequest<Types>,
        _: RPCOption,
    ) -> Result<
        InstallSnapshotResponse<u64>,
        RPCError<u64, BasicNode, RaftError<u64, InstallSnapshotError>>,
    > {
        let result: Result<InstallSnapshotResponse<u64>, RaftError<u64, InstallSnapshotError>> =
            self.send(Message::Snapshot(rpc)).await?;
        result.map_err(|e| RPCError::RemoteError(openraft::error::RemoteError::new(self.target, e)))
    }
}
