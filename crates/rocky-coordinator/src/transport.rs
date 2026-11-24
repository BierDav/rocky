use crate::certified::*;
use crate::{CoordinationError, Refusal};
use p2panda_core::{PrivateKey, PublicKey, Signature};
use rocky_core::*;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

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
            "rocky.coordination-wire.v6",
            &(self.genesis, self.key, self.nonce, self.time, &self.body),
        )
    }
    pub fn authenticate(&self, genesis: Commitment) -> Result<(), CoordinationError> {
        if self.genesis != genesis || !self.key.verify(&self.signing_bytes()?, &self.signature) {
            return Err(CoordinationError::Denied(Refusal::Signature));
        }
        Ok(())
    }
    pub fn client_request(
        &self,
        genesis: Commitment,
    ) -> Result<crate::machine::Request, CoordinationError> {
        self.authenticate(genesis)?;
        match decode::<Message>(&self.body)? {
            WireMessage::Client(request) => Ok(*request),
            _ => Err(CoordinationError::Denied(Refusal::UnexpectedMessage)),
        }
    }
    #[cfg(not(target_family = "wasm"))]
    pub fn verify(&self, genesis: Commitment) -> Result<(), CoordinationError> {
        self.authenticate(genesis)?;
        let coordinator_time = now()?;
        if coordinator_time.abs_diff(self.time) > MAX_CLOCK_SKEW_SECONDS {
            return Err(CoordinationError::ClockSkew { coordinator_time });
        }
        Ok(())
    }
}
pub(crate) const MAX_CLOCK_SKEW_SECONDS: u64 = 30;

const CLIENT_MESSAGE_TAG_BYTES: usize = 8;

const ENCODED_BYTES_PER_ENVELOPE_BODY_BYTE: usize = 2;

const ENVELOPE_BYTES_BESIDES_BODY_ELEMENTS: usize = 285;

pub const MAX_CLIENT_FRAME: usize = ENVELOPE_BYTES_BESIDES_BODY_ELEMENTS
    + ENCODED_BYTES_PER_ENVELOPE_BODY_BYTE
        * (crate::machine::Request::MAX_CLIENT_BYTES + CLIENT_MESSAGE_TAG_BYTES);

pub fn nonce() -> Commitment {
    *PrivateKey::new().public_key().as_bytes()
}
pub async fn read(stream: &mut (impl AsyncRead + Unpin)) -> Result<Envelope, CoordinationError> {
    read_bounded(stream, MAX_FRAME).await
}
pub async fn read_bounded(
    stream: &mut (impl AsyncRead + Unpin),
    max_frame: usize,
) -> Result<Envelope, CoordinationError> {
    let len = stream
        .read_u32()
        .await
        .map_err(|_| CoordinationError::Unavailable)? as usize;
    if len > max_frame.min(MAX_FRAME) {
        return Err(CoordinationError::Denied(Refusal::Oversized));
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
        return Err(CoordinationError::Denied(Refusal::Oversized));
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
        return Err(CoordinationError::Denied(Refusal::Signature));
    }
    decode(&response.body)
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn wire_variants_encode_by_name_so_a_client_without_raft_types_speaks_the_voter_format()
    -> Result<(), CoordinationError> {
        let status = encode(&Message::Status)?;
        assert!(
            status
                .windows(b"Status".len())
                .any(|name| name == b"Status")
        );
        let at_voter: WireMessage<u8, u8, u8> = decode(&status)?;
        assert!(matches!(at_voter, WireMessage::Status));
        let at_client: Message = decode(&encode(&WireMessage::<u8, u8, u8>::Initialize)?)?;
        assert!(matches!(at_client, WireMessage::Initialize));
        assert!(
            decode::<Message>(&encode(&WireMessage::<u8, u8, u8>::Append(7))?).is_err(),
            "a client never decodes a Raft RPC"
        );
        Ok(())
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn the_client_frame_limit_holds_the_largest_answerable_request_in_its_worst_encoding()
    -> Result<(), CoordinationError> {
        let key = PrivateKey::new();
        let request = crate::machine::Request::certified(
            [0; 32],
            nonce(),
            crate::service::Operation::Usage,
            (&key, None),
            now()?,
        )?;
        assert_eq!(
            encode(&Message::Client(Box::new(request.clone())))?.len(),
            encode(&request)?.len() + CLIENT_MESSAGE_TAG_BYTES,
            "a client message adds its tag to the request"
        );
        let largest_body =
            vec![u8::MAX; crate::machine::Request::MAX_CLIENT_BYTES + CLIENT_MESSAGE_TAG_BYTES];
        let worst = Envelope::from_body([u8::MAX; 32], [u8::MAX; 32], largest_body, &key)?;
        let encoded = encode(&worst)?.len();
        assert!(
            encoded <= MAX_CLIENT_FRAME,
            "the largest answerable request fits the client frame limit: {encoded} > {MAX_CLIENT_FRAME}"
        );
        let reserved_for_a_64_bit_time_and_a_32_bit_body_length = 4 + 2;
        assert_eq!(
            MAX_CLIENT_FRAME - encoded,
            reserved_for_a_64_bit_time_and_a_32_bit_body_length,
            "the client frame limit is computed from the envelope encoding"
        );
        Ok(())
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_client_request_is_authorized_by_its_own_signature_whatever_key_carries_it()
    -> Result<(), CoordinationError> {
        let (genesis, node, session) = ([9; 32], PrivateKey::new(), PrivateKey::new());
        let request = crate::machine::Request::sign_at(
            genesis,
            nonce(),
            crate::service::Operation::Usage,
            &node,
            now()?,
        )?;
        let carried = Envelope::sign(
            genesis,
            nonce(),
            &Message::Client(Box::new(request.clone())),
            &session,
        )?;
        assert_eq!(carried.client_request(genesis)?, request);
        let status = Envelope::sign(genesis, nonce(), &Message::Status, &session)?;
        assert_eq!(
            status.client_request(genesis),
            Err(CoordinationError::Denied(Refusal::UnexpectedMessage))
        );
        Ok(())
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn an_answer_bound_to_another_request_nonce_is_refused() -> Result<(), CoordinationError>
    {
        let genesis = [6; 32];
        let (client, voter) = (PrivateKey::new(), PrivateKey::new());
        for answer_to_this_call in [true, false] {
            let (mut near, mut far) = tokio::io::duplex(4096);
            let answer = async {
                let request = read(&mut far).await?;
                let nonce = if answer_to_this_call {
                    request.nonce
                } else {
                    super::nonce()
                };
                write(&mut far, &Envelope::sign(genesis, nonce, &1u8, &voter)?).await
            };
            let (reply, answered) = tokio::join!(
                converse::<u8>(&mut near, voter.public_key(), genesis, &client, 0u8),
                answer
            );
            answered?;
            if answer_to_this_call {
                assert_eq!(reply, Ok(1));
            } else {
                assert_eq!(reply, Err(CoordinationError::Denied(Refusal::Signature)));
            }
        }
        Ok(())
    }
}
