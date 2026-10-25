//! Bounded caches of immutable crypto results, never of current admission.
use super::*;

type PayloadEntries = BTreeMap<Hash, ([u8; 32], Arc<Vec<u8>>)>;
const OPENED_FRAMES: usize = 256;
const OPENED_BYTES: usize = 16 * 1024 * 1024;
#[derive(Default)]
struct OpenedPayloads {
    entries: PayloadEntries,
    order: std::collections::VecDeque<Hash>,
    bytes: usize,
}
impl OpenedPayloads {
    fn remember(&mut self, hash: Hash, key: [u8; 32], bytes: Arc<Vec<u8>>) -> Result<(), Error> {
        if bytes.len() > OPENED_BYTES {
            return Ok(());
        }
        if let Some((_, old)) = self.entries.remove(&hash) {
            self.bytes -= old.len();
            self.order.retain(|known| *known != hash);
        }
        while self.entries.len() >= OPENED_FRAMES || self.bytes + bytes.len() > OPENED_BYTES {
            let old = self
                .order
                .pop_front()
                .ok_or_else(|| Error::Corrupt("opened payload cache order".into()))?;
            let (_, bytes) = self
                .entries
                .remove(&old)
                .ok_or_else(|| Error::Corrupt("opened payload cache entry".into()))?;
            self.bytes -= bytes.len();
        }
        self.bytes += bytes.len();
        self.order.push_back(hash);
        self.entries.insert(hash, (key, bytes));
        Ok(())
    }
}
type VerifiedChains =
    BTreeMap<(Commitment, PublicKey, Commitment), rocky_credentials::VerifiedChain>;
#[derive(Default)]
pub(super) struct Cache {
    opened: std::sync::Mutex<OpenedPayloads>,
    #[cfg(test)]
    pub(super) opens: AtomicU64,
    chains: std::sync::Mutex<VerifiedChains>,
    #[cfg(test)]
    pub(super) verifications: AtomicU64,
}
impl<A: RowApplication> Inner<A> {
    pub(super) fn verified_chain(
        &self,
        chain: &CredentialChain,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<rocky_credentials::VerifiedChain, Error> {
        self.crypto
            .verify(chain, self.config.publisher, revocations, time)
    }
}

impl Cache {
    fn verify(
        &self,
        chain: &CredentialChain,
        publisher: PublicKey,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<rocky_credentials::VerifiedChain, Error> {
        let digest = rocky_core::commitment("rocky.credential-chain.v1", &encode(chain)?);
        let key = (
            digest,
            publisher,
            rocky_core::commitment("rocky.verified-chain-revocations.v1", &encode(revocations)?),
        );
        let mut cache = self
            .chains
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(verified) = cache.get(&key) {
            return Ok(verified.clone());
        }
        let verified = chain
            .verified(publisher, revocations, time)
            .map_err(credential)?;
        #[cfg(test)]
        self.verifications.fetch_add(1, Ordering::Relaxed);
        if cache.len() >= 256 {
            cache.clear();
        }
        cache.insert(key, verified.clone());
        Ok(verified)
    }

    pub(super) fn open(
        &self,
        frame: &EncryptedFrame,
        key: &KeyMaterial,
    ) -> Result<Arc<Vec<u8>>, Error> {
        let operation = frame.operation_ref()?;
        let mut cache = self
            .opened
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((held, bytes)) = cache.entries.get(&operation.hash)
            && held == &key.key
        {
            return Ok(bytes.clone());
        }
        let bytes = Arc::new(decrypt(
            &key.key,
            &encode(&operation.header.extensions)?,
            frame.body_bytes(),
        )?);
        #[cfg(test)]
        self.opens.fetch_add(1, Ordering::Relaxed);
        // Evict older frames first, retaining the whole newly prepared page.
        cache.remember(operation.hash, key.key, bytes.clone())?;
        Ok(bytes)
    }
}
impl<A: RowApplication> Inner<A> {
    pub(super) fn open_payload(&self, frame: &EncryptedFrame) -> Result<Arc<Vec<u8>>, Error> {
        let extension = &frame.operation_ref()?.header.extensions;
        let key = self
            .opening_key(extension.scope, extension.generation)
            .ok_or_else(|| denied("row payload key unavailable"))?;
        self.crypto.open(frame, key)
    }
}
impl<A: RowApplication> EncryptedReplica<A> {
    #[cfg(test)]
    pub(crate) async fn crypto_stats(&self) -> (u64, u64) {
        let inner = self.inner.lock().await;
        (
            inner.crypto.opens.load(Ordering::Relaxed),
            inner.crypto.verifications.load(Ordering::Relaxed),
        )
    }
    pub(super) async fn preopen(
        &self,
        frames: Vec<EncryptedFrame>,
    ) -> Result<Vec<EncryptedFrame>, Error> {
        // Verify immutable frame signatures before touching replica state.
        let frames = tokio::task::spawn_blocking(move || {
            for frame in &frames {
                let _ = frame.operation_ref();
            }
            frames
        })
        .await
        .map_err(|_| Error::Closed)?;
        let (crypto, keys, publisher, revocations) = {
            let inner = self.inner.lock().await;
            (
                inner.crypto.clone(),
                inner.opening_keys().cloned().collect::<Vec<_>>(),
                inner.config.publisher,
                inner.config.revocations.clone(),
            )
        };
        tokio::task::spawn_blocking(move || {
            for frame in &frames {
                let Ok(operation) = frame.operation_ref() else {
                    continue;
                };
                let extension = &operation.header.extensions;
                if let Some(key) = keys.iter().find(|key| {
                    key.scope == extension.scope && key.generation == extension.generation
                }) {
                    let _ = crypto.open(frame, key);
                }
                if let (Ok(chain), Ok(time)) = (frame.chain(), now()) {
                    let _ = crypto.verify(chain.credentials(), publisher, &revocations, time);
                }
            }
            frames
        })
        .await
        .map_err(|_| Error::Closed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plaintext_retention_is_bounded_even_for_empty_bodies_and_keeps_the_new_page() {
        let mut cache = OpenedPayloads::default();
        let hashes: Vec<_> = (0..OPENED_FRAMES * 2)
            .map(|n| Hash::new(n.to_le_bytes()))
            .collect();
        for hash in &hashes {
            cache
                .remember(*hash, [1; 32], Arc::new(Vec::new()))
                .unwrap();
        }
        assert_eq!(cache.entries.len(), OPENED_FRAMES);
        assert_eq!(cache.order.len(), OPENED_FRAMES);
        assert_eq!(cache.bytes, 0);
        assert!(
            hashes[..OPENED_FRAMES]
                .iter()
                .all(|hash| !cache.entries.contains_key(hash))
        );
        assert!(
            hashes[OPENED_FRAMES..]
                .iter()
                .all(|hash| cache.entries.contains_key(hash))
        );
    }
}
