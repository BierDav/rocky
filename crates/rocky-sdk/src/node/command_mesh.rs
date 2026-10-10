//! One-hop command carrier; decision 0059 describes its authentication and bounds.
use super::*;
use rocky_credentials::Signature;

const VERSION: u32 = 2;
const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_FORWARDING: usize = 8;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u32,
    sender: PublicKey,
    credentials: CredentialChain,
    nonce: [u8; 32],
    forward: Option<rocky_core::NodeId>,
    envelope: Vec<u8>,
    signature: Signature,
}
impl Request {
    fn binding(&self) -> Result<Vec<u8>, Failure> {
        Ok(serde_json::to_vec(&(
            "rocky.command-carrier.request.v2",
            self.version,
            self.sender,
            &self.credentials,
            self.nonce,
            self.forward,
            &self.envelope,
        ))?)
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    version: u32,
    sender: PublicKey,
    credentials: CredentialChain,
    request: [u8; 32],
    delivery: Vec<u8>,
    signature: Signature,
}
impl Reply {
    fn binding(&self) -> Result<Vec<u8>, Failure> {
        Ok(serde_json::to_vec(&(
            "rocky.command-carrier.reply.v2",
            self.version,
            self.sender,
            &self.credentials,
            self.request,
            &self.delivery,
        ))?)
    }
}
#[derive(Default)]
pub(super) struct Slots(std::sync::Mutex<BTreeMap<PublicKey, usize>>);
struct Forwarding<'a> {
    slots: &'a Slots,
    caller: PublicKey,
}
impl Drop for Forwarding<'_> {
    fn drop(&mut self) {
        let mut active = self.slots.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = active.get_mut(&self.caller) {
            *count -= 1;
            if *count == 0 {
                active.remove(&self.caller);
            }
        }
    }
}
impl Slots {
    fn reserve(&self, caller: PublicKey) -> Result<Forwarding<'_>, Failure> {
        let mut active = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let count = active.entry(caller).or_default();
        if *count >= MAX_FORWARDING {
            return Err(Box::new(Deferred(
                "forwarder is busy for this caller".into(),
            )));
        }
        *count += 1;
        Ok(Forwarding {
            slots: self,
            caller,
        })
    }
}
impl Node {
    async fn admit_carrier(&self, node: PublicKey, chain: &CredentialChain) -> Result<(), Failure> {
        self.replica
            .admit_command_peer(node, chain)
            .await
            .map_err(refused)
    }
    async fn carrier_exchange(
        &self,
        transport: &dyn Transport,
        peer: PublicKey,
        contact: &Contact,
        forward: Option<rocky_core::NodeId>,
        envelope: Vec<u8>,
    ) -> Result<Vec<u8>, Failure> {
        let key = PrivateKey::from_bytes(&self.vault.get("node")?);
        let mut request = Request {
            version: VERSION,
            sender: key.public_key(),
            credentials: self.settings.lock().await.credentials.clone(),
            nonce: crate::enrollment::random(),
            forward,
            envelope,
            signature: key.sign(&[]),
        };
        request.signature = key.sign(&request.binding()?);
        let binding = commitment("rocky.command-carrier.request.v2", &request.binding()?);
        let bytes = serde_json::to_vec(&request)?;
        let stream = crate::enrollment::dial(
            transport,
            &contact.addresses(transport.kind()),
            Protocol::Command,
        )
        .await
        .map_err(|error| Box::new(Deferred(format!("no direct route: {error}"))) as Failure)?;
        let reply =
            tokio::time::timeout(TIMEOUT, crate::enrollment::exchange_stream(stream, &bytes))
                .await
                .map_err(|_| ServiceError::Unreachable("command route timed out".into()))??;
        let reply: Reply = serde_json::from_slice(&reply).map_err(refused)?;
        if reply.version != VERSION
            || reply.sender != peer
            || reply.request != binding
            || !peer.verify(&reply.binding()?, &reply.signature)
        {
            return Err(refused("invalid command carrier answer"));
        }
        self.admit_carrier(peer, &reply.credentials).await?;
        Ok(reply.delivery)
    }
    pub(super) async fn answer_command_carrier(&self, bytes: &[u8]) -> Result<Vec<u8>, Failure> {
        let request: Request = serde_json::from_slice(bytes).map_err(refused)?;
        if request.version != VERSION
            || !request
                .sender
                .verify(&request.binding()?, &request.signature)
        {
            return Err(refused("invalid command carrier request"));
        }
        let admission = self
            .admit_carrier(request.sender, &request.credentials)
            .await;
        let delivery = if let Err(error) = admission {
            Delivery::NotSubmitted {
                retryable: false,
                message: error.to_string(),
            }
            .to_bytes()?
        } else if let Some(executor) = request.forward {
            // Only the original caller may spend its forwarding slots.
            let command = self.replica.admit_command(&request.envelope).await?;
            if command.request()?.id.node != secure_node_id(request.sender) {
                return Err(refused(
                    "forwarding must be requested by the command caller",
                ));
            }
            let attempt = async {
                let _slot = self.forwarding.reserve(request.sender)?;
                let attached = self.network.read().await;
                let link = attached
                    .as_ref()
                    .ok_or(ServiceError::NetworkRequired("command forwarding"))?;
                let settings = self.settings.lock().await.clone();
                let (peer, contact) = settings
                    .peers
                    .iter()
                    .find(|(peer, _)| secure_node_id(**peer) == executor)
                    .ok_or_else(|| {
                        Box::new(Deferred("forwarder has no route to executor".into())) as Failure
                    })?;
                // A forwarded request is always delivered directly, never routed again.
                self.carrier_exchange(
                    &*link.transport,
                    *peer,
                    contact,
                    None,
                    request.envelope.clone(),
                )
                .await
            };
            match tokio::time::timeout(TIMEOUT, attempt).await {
                Ok(Ok(reply)) => reply,
                Ok(Err(error)) if error.is::<Deferred>() => Delivery::NotSubmitted {
                    retryable: true,
                    message: error.to_string(),
                }
                .to_bytes()?,
                Ok(Err(error)) => Delivery::Unknown(error.to_string()).to_bytes()?,
                Err(_) => Delivery::Unknown("forwarder timed out; retry the same ticket".into())
                    .to_bytes()?,
            }
        } else {
            self.execute_delivered(&request.envelope).await.to_bytes()?
        };
        let key = PrivateKey::from_bytes(&self.vault.get("node")?);
        let mut reply = Reply {
            version: VERSION,
            sender: key.public_key(),
            credentials: self.settings.lock().await.credentials.clone(),
            request: commitment("rocky.command-carrier.request.v2", &request.binding()?),
            delivery,
            signature: key.sign(&[]),
        };
        reply.signature = key.sign(&reply.binding()?);
        Ok(serde_json::to_vec(&reply)?)
    }
    pub(super) async fn route_command(
        &self,
        transport: &dyn Transport,
        executor: rocky_core::NodeId,
        envelope: Vec<u8>,
    ) -> Result<Vec<u8>, Failure> {
        let settings = self.settings.lock().await.clone();
        let candidates = self.dialed(&settings);
        if !settings
            .peers
            .keys()
            .any(|peer| secure_node_id(*peer) == executor)
        {
            self.request_contact(transport, executor).await?;
        }
        let settings = self.settings.lock().await.clone();
        if let Some((peer, contact)) = settings
            .peers
            .iter()
            .find(|(peer, _)| secure_node_id(**peer) == executor)
        {
            match self
                .carrier_exchange(transport, *peer, contact, None, envelope.clone())
                .await
            {
                Ok(answer) => return Ok(answer),
                Err(error) if error.is::<Refused>() => return Err(error),
                Err(_) => {}
            }
        }
        for (peer, contact) in candidates {
            if secure_node_id(peer) == executor {
                continue;
            }
            if let Ok(answer) = self
                .carrier_exchange(transport, peer, &contact, Some(executor), envelope.clone())
                .await
            {
                match Delivery::from_bytes(&answer)? {
                    Delivery::NotSubmitted {
                        retryable: true, ..
                    } => continue,
                    _ => return Ok(answer),
                }
            }
        }
        Err(Box::new(Deferred(
            "no admitted peer reaches the executor; retry the same ticket".into(),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn carrier_signatures_bind_the_envelope_route_and_answer_request() -> Result<(), Failure> {
        let key = PrivateKey::from_bytes(&[3; 32]);
        let mut request = Request {
            version: VERSION,
            sender: key.public_key(),
            credentials: CredentialChain(Vec::new()),
            nonce: [4; 32],
            forward: Some(rocky_core::NodeId([5; 16])),
            envelope: vec![6],
            signature: key.sign(&[]),
        };
        request.signature = key.sign(&request.binding()?);
        assert!(
            key.public_key()
                .verify(&request.binding()?, &request.signature)
        );
        request.envelope[0] ^= 1;
        assert!(
            !key.public_key()
                .verify(&request.binding()?, &request.signature)
        );
        request.envelope[0] ^= 1;
        request.forward = None;
        assert!(
            !key.public_key()
                .verify(&request.binding()?, &request.signature)
        );
        let mut reply = Reply {
            version: VERSION,
            sender: key.public_key(),
            credentials: CredentialChain(Vec::new()),
            request: [7; 32],
            delivery: vec![8],
            signature: key.sign(&[]),
        };
        reply.signature = key.sign(&reply.binding()?);
        reply.delivery[0] ^= 1;
        assert!(!key.public_key().verify(&reply.binding()?, &reply.signature));
        reply.delivery[0] ^= 1;
        reply.request[0] ^= 1;
        assert!(!key.public_key().verify(&reply.binding()?, &reply.signature));
        Ok(())
    }
    #[test]
    fn ninth_forward_is_retryable_and_dropping_work_removes_every_slot() -> Result<(), Failure> {
        let slots = Slots::default();
        let caller = PrivateKey::from_bytes(&[1; 32]).public_key();
        let other = PrivateKey::from_bytes(&[2; 32]).public_key();
        let mut work = Vec::new();
        for _ in 0..8 {
            work.push(slots.reserve(caller)?);
        }
        assert!(
            slots
                .reserve(caller)
                .err()
                .is_some_and(|error| error.is::<Deferred>())
        );
        let independent = slots.reserve(other)?;
        work.pop();
        work.push(slots.reserve(caller)?);
        drop(work);
        drop(independent);
        assert!(slots.0.lock().map_err(|_| "slots")?.is_empty());
        Ok(())
    }
}
