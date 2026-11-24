pub(crate) use rocky_credentials::{
    PEER_RPC_VERSION as VERSION, PeerError, PeerReply, PeerReplyFrame as ReplyFrame, PeerRequest,
    PeerRequestFrame as RequestFrame,
};
const REMOVED: &str = "removed";
pub(crate) fn failure(error: &crate::Failure) -> PeerError {
    let failure = crate::node::typed_error(error);
    PeerError {
        code: failure.code().to_owned(),
        message: failure.to_string(),
        retryable: failure.retryable(),
    }
}
pub(crate) fn remote(peer: &PeerError) -> crate::Error {
    if peer.code == REMOVED {
        return crate::Error::removed();
    }
    crate::Error::new(
        crate::ErrorKind::Remote {
            retryable: peer.retryable,
        },
        peer.message.clone(),
    )
}
