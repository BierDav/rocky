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
#[cfg(test)]
pub(crate) fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, crate::Failure> {
    Ok(rocky_credentials::encode(value)?)
}
#[cfg(test)]
pub(crate) fn decode<T: serde::Serialize + serde::de::DeserializeOwned>(
    bytes: &[u8],
) -> Result<T, crate::Failure> {
    Ok(rocky_credentials::decode_bounded(
        bytes,
        rocky_credentials::MAX_PEER_RPC_MESSAGE,
    )?)
}
