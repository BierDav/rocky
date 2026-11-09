//! Bounded caches of immutable crypto results, never of current admission.
use super::*;

type PayloadEntries = BTreeMap<Hash, ([u8; 32], Arc<crate::wire::Opened>)>;
const OPENED_FRAMES: usize = 256;
const OPENED_BYTES: usize = 16 * 1024 * 1024;
#[derive(Default)]
struct OpenedPayloads {
    entries: PayloadEntries,
    order: std::collections::VecDeque<Hash>,
    bytes: usize,
}
impl OpenedPayloads {
    fn remember(
        &mut self,
        hash: Hash,
        key: [u8; 32],
        bytes: Arc<crate::wire::Opened>,
    ) -> Result<(), Error> {
        if bytes.payload.len() > OPENED_BYTES {
            return Ok(());
        }
        if let Some((_, old)) = self.entries.remove(&hash) {
            self.bytes -= old.payload.len();
            self.order.retain(|known| *known != hash);
        }
        while self.entries.len() >= OPENED_FRAMES || self.bytes + bytes.payload.len() > OPENED_BYTES
        {
            let old = self
                .order
                .pop_front()
                .ok_or_else(|| Error::Corrupt("opened payload cache order".into()))?;
            let (_, bytes) = self
                .entries
                .remove(&old)
                .ok_or_else(|| Error::Corrupt("opened payload cache entry".into()))?;
            self.bytes -= bytes.payload.len();
        }
        self.bytes += bytes.payload.len();
        self.order.push_back(hash);
        self.entries.insert(hash, (key, bytes));
        Ok(())
    }
}
type VerifiedChains =
    BTreeMap<(Commitment, PublicKey, Commitment, Commitment), rocky_credentials::VerifiedChain>;
#[derive(Default)]
pub(super) struct Cache {
    opened: std::sync::Mutex<OpenedPayloads>,
    headers: std::sync::Mutex<CreationHeaders>,
    #[cfg(test)]
    pub(super) header_verifications: AtomicU64,
    #[cfg(test)]
    pub(super) opens: AtomicU64,
    chains: std::sync::Mutex<VerifiedChains>,
    families: std::sync::Mutex<BTreeMap<Commitment, Arc<rocky_credentials::GrantFamilies>>>,
    #[cfg(test)]
    pub(super) verifications: AtomicU64,
}
/// Immutable signature validation only; authority is checked at import/apply.
pub(in crate::replica) struct CreationHeader {
    bytes: Vec<u8>,
    signed: p2panda_core::Header<Extensions>,
}
impl CreationHeader {
    pub(in crate::replica) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub(in crate::replica) fn signed(&self) -> &p2panda_core::Header<Extensions> {
        &self.signed
    }
}
const CREATION_HEADERS: usize = 8192;
const CREATION_HEADER_BYTES: usize = 8 * 1024 * 1024;
#[derive(Default)]
struct CreationHeaders {
    entries: BTreeMap<Commitment, Arc<CreationHeader>>,
    bytes: usize,
}
impl<A: RowApplication> Inner<A> {
    pub(super) fn families_at(
        &self,
        manifest: Commitment,
    ) -> Result<Arc<rocky_credentials::GrantFamilies>, Error> {
        let mut cache = self
            .crypto
            .families
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(families) = cache.get(&manifest) {
            return Ok(families.clone());
        }
        let definition = self
            .definition_at(&manifest)
            .ok_or_else(|| denied("grant family schema unavailable"))?;
        let families =
            Arc::new(rocky_credentials::GrantFamilies::new(definition, &[]).map_err(credential)?);
        if cache.len() >= 256 {
            cache.clear();
        }
        cache.insert(manifest, families.clone());
        Ok(families)
    }
    pub(crate) fn grant_families(&self) -> &rocky_credentials::GrantFamilies {
        self.workspace
            .typed
            .as_ref()
            .map_or(&self.families, |typed| &typed.families)
    }
    pub(super) fn verified_chain(
        &self,
        chain: &CredentialChain,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<rocky_credentials::VerifiedChain, Error> {
        chain
            .verify_database(
                self.config
                    .credentials
                    .database_context(self.config.publisher)
                    .map_err(credential)?,
            )
            .map_err(credential)?;
        self.crypto.verify(
            chain,
            self.config.publisher,
            revocations,
            time,
            Some(self.grant_families()),
        )
    }
}

impl Cache {
    pub(in crate::replica) fn creation_header(
        &self,
        bytes: &[u8],
    ) -> Result<Arc<CreationHeader>, Error> {
        let digest = moves::header_digest(bytes);
        let mut cache = self
            .headers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(header) = cache.entries.get(&digest) {
            return Ok(header.clone());
        }
        let header = Arc::new(CreationHeader {
            signed: crate::wire::verified_header(bytes)?,
            bytes: bytes.to_vec(),
        });
        #[cfg(test)]
        self.header_verifications.fetch_add(1, Ordering::Relaxed);
        if bytes.len() <= CREATION_HEADER_BYTES {
            if cache.entries.len() >= CREATION_HEADERS
                || cache.bytes + bytes.len() > CREATION_HEADER_BYTES
            {
                cache.entries.clear();
                cache.bytes = 0;
            }
            cache.bytes += bytes.len();
            cache.entries.insert(digest, header.clone());
        }
        Ok(header)
    }

    fn verify(
        &self,
        chain: &CredentialChain,
        publisher: PublicKey,
        revocations: &RevocationSet,
        time: u64,
        families: Option<&rocky_credentials::GrantFamilies>,
    ) -> Result<rocky_credentials::VerifiedChain, Error> {
        let default_families = rocky_credentials::GrantFamilies::default();
        let families = families.unwrap_or(&default_families);
        let digest = rocky_core::commitment("rocky.credential-chain.v1", &encode(chain)?);
        let key = (
            digest,
            publisher,
            rocky_core::commitment("rocky.verified-chain-revocations.v1", &encode(revocations)?),
            families.digest(),
        );
        let mut cache = self
            .chains
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(verified) = cache.get(&key) {
            return Ok(verified.clone());
        }
        let verified = chain
            .verified_with_families(publisher, revocations, time, families)
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
    ) -> Result<Arc<crate::wire::Opened>, Error> {
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
        let bytes = Arc::new(crate::wire::Opened::open(
            &key.key,
            &operation.header.extensions,
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
    /// The opened body of `frame`. Its sealed tuple must be the one this
    /// replica registered under the frame's keyed scope: an author that
    /// signs one partition's scope over another's tuple is refused.
    pub(super) fn open_payload(
        &self,
        frame: &EncryptedFrame,
    ) -> Result<Arc<crate::wire::Opened>, Error> {
        let extension = &frame.operation_ref()?.header.extensions;
        let key = self
            .opening_key(extension.scope, extension.generation)
            .ok_or_else(|| denied("row payload key unavailable"))?;
        let opened = self.crypto.open(frame, key)?;
        if opened.partition.as_ref() != self.partitions.get(&extension.scope) {
            return Err(denied(
                "sealed partition tuple differs from its keyed scope",
            ));
        }
        Ok(opened)
    }
}
impl<A: RowApplication> EncryptedReplica<A> {
    #[cfg(test)]
    pub(crate) async fn crypto_stats(&self) -> (u64, u64) {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
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
        let frames = rocky_platform::task::spawn_blocking(move || {
            for frame in &frames {
                let _ = frame.operation_ref();
            }
            frames
        })
        .await
        .map_err(|_| Error::Closed)?;
        let (crypto, keys, publisher, revocations) = {
            let inner = self.inner.lock(crate::replica::Class::Background).await;
            (
                inner.crypto.clone(),
                inner.opening_keys().cloned().collect::<Vec<_>>(),
                inner.config.publisher,
                inner.config.revocations.clone(),
            )
        };
        rocky_platform::task::spawn_blocking(move || {
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
                    let _ = crypto.verify(chain.credentials(), publisher, &revocations, time, None);
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
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn cached_headers_reject_changed_signatures_and_unsupported_formats() {
        let key = PrivateKey::from_bytes(&[81; 32]);
        let mut header = p2panda_core::Header {
            version: 1,
            public_key: key.public_key(),
            signature: None,
            payload_size: 0,
            payload_hash: None,
            timestamp: 1,
            seq_num: 0,
            backlink: None,
            previous: vec![],
            extensions: Extensions {
                tip: TipId([1; 32]),
                scope: ResourceScope {
                    domain: rocky_core::Scope {
                        mesh: rocky_core::MeshId([1; 16]),
                        app: rocky_core::AppId([2; 16]),
                        domain: rocky_core::DomainId([3; 16]),
                    },
                    encryption: [4; 32],
                    table: [5; 32],
                },
                generation: rocky_core::KeyEpoch::new(1, &[6; 32]),
                chain: [7; 32],
                author: Some(1),
                author_sequence: 1,
                row_format: crate::wire::ROW_FORMAT,
                births: vec![],
            },
        };
        header.sign(&key);
        let cache = Cache::default();
        let bytes = header.to_bytes();
        let first = cache.creation_header(&bytes).unwrap();
        assert!(Arc::ptr_eq(&first, &cache.creation_header(&bytes).unwrap()));
        header.timestamp += 1;
        assert!(cache.creation_header(&header.to_bytes()).is_err());
        header.sign(&key);
        assert!(cache.creation_header(&header.to_bytes()).is_ok());
        header.extensions.row_format -= 1;
        header.sign(&key);
        assert!(cache.creation_header(&header.to_bytes()).is_err());
        assert_eq!(cache.header_verifications.load(Ordering::Relaxed), 2);
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn plaintext_retention_is_bounded_even_for_empty_bodies_and_keeps_the_new_page() {
        let mut cache = OpenedPayloads::default();
        let hashes: Vec<_> = (0..OPENED_FRAMES * 2)
            .map(|n| Hash::new(n.to_le_bytes()))
            .collect();
        for hash in &hashes {
            cache
                .remember(
                    *hash,
                    [1; 32],
                    Arc::new(crate::wire::Opened {
                        partition: None,
                        payload: Vec::new(),
                    }),
                )
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
