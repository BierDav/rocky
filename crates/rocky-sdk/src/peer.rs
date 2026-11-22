pub(crate) use rocky_credentials::{
    PEER_RPC_VERSION as VERSION, PeerError, PeerReply, PeerReplyFrame as ReplyFrame, PeerRequest,
    PeerRequestFrame as RequestFrame,
};
pub(crate) fn failure(error: &crate::Failure) -> PeerError {
    let failure = crate::node::call_failure(error);
    PeerError {
        code: failure.code,
        message: failure.detail,
        retryable: failure.retryable,
    }
}
