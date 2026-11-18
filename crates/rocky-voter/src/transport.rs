use openraft::raft::{AppendEntriesRequest, InstallSnapshotRequest, VoteRequest};
use p2panda_core::PrivateKey;
pub(crate) use rocky_coordinator::{
    certified::*,
    protocol::{Envelope, MAX_CLIENT_FRAME, exchange, nonce, read, read_bounded, write},
};
use rocky_core::CoordinationError;
use serde::{Serialize, de::DeserializeOwned};

pub(crate) type Message = rocky_coordinator::protocol::Message<
    AppendEntriesRequest<crate::raft_store::Types>,
    VoteRequest<u64>,
    InstallSnapshotRequest<crate::raft_store::Types>,
>;

use crate::raft_store::Types;
use openraft::{
    BasicNode, RaftNetwork, RaftNetworkFactory,
    error::{InstallSnapshotError, NetworkError, RPCError, RaftError},
    network::RPCOption,
    raft::*,
};

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
    #[expect(
        clippy::result_large_err,
        reason = "OpenRaft defines the RPC error type"
    )]
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
