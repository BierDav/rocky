use crate::{
    Failure, Route,
    enrollment::{Contact, Settings, now},
    secrets::Vault,
};
use data_encoding::HEXLOWER;
use rocky_core::{Commitment, Scope, commitment};
use rocky_credentials::{DiscoverySecret, PrivateKey, PublicKey, Revocation};
use rocky_secure::network::{
    ContactHandler, ContactHint, ContactRequest, ContactResponse, NativeSession,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContactMeta {
    pub source: String,
    pub learned: u64,
    pub trusted: bool,
    pub local: bool,
}
const AGE: u64 = 30 * 24 * 60 * 60;
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Discovery {
    version: u32,
    root: Option<DiscoverySecret>,
    values: BTreeMap<String, DiscoverySecret>,
    current: Option<Commitment>,
    previous: Option<(Commitment, u64)>,
}
impl Discovery {
    fn load(vault: &Vault) -> Result<Self, Failure> {
        match vault.load_bytes("discovery")? {
            None => Ok(Self {
                version: 1,
                root: None,
                values: BTreeMap::new(),
                current: None,
                previous: None,
            }),
            Some(bytes) => {
                let state: Self = serde_json::from_slice(&bytes)?;
                if state.version != 1
                    || state.values.len() > rocky_credentials::MAX_LIVE_REVOCATIONS
                {
                    return Err("unsupported or overfull discovery state".into());
                }
                Ok(state)
            }
        }
    }
    fn save(&self, vault: &Vault) -> Result<(), Failure> {
        if self.values.len() > rocky_credentials::MAX_LIVE_REVOCATIONS {
            return Err("discovery value capacity reached".into());
        }
        vault.store_bytes("discovery", &serde_json::to_vec(self)?)?;
        Ok(())
    }
}
pub(crate) type Signer<'a> = (
    rocky_credentials::Signer<'a>,
    &'a rocky_credentials::CredentialChain,
);
pub(crate) fn create_root(vault: &Vault, signer: Signer<'_>, scope: Scope) -> Result<(), Failure> {
    let mut state = Discovery::load(vault)?;
    state.root = Some(DiscoverySecret::sign(
        scope,
        None,
        crate::enrollment::random(),
        signer,
    )?);
    state.save(vault)
}
pub(crate) fn mint_value(
    vault: &Vault,
    signer: Signer<'_>,
    (publisher, scope): (PublicKey, Scope),
    set: &rocky_credentials::RevocationSet,
    statement: &Revocation,
) -> Result<(), Failure> {
    let mut state = Discovery::load(vault)?;
    let id = HEXLOWER.encode(&statement.id()?);
    let time = now()?;
    state.values.retain(|named, value| {
        (named == &id
            || value
                .statement
                .and_then(|id| set.get(&id))
                .is_some_and(|signed| signed.live(time)))
            && value.verify_unrevoked(publisher, scope, set).is_ok()
    });
    if let std::collections::btree_map::Entry::Vacant(entry) = state.values.entry(id) {
        entry.insert(DiscoverySecret::sign(
            scope,
            Some(statement.id()?),
            crate::enrollment::random(),
            signer,
        )?);
        state.save(vault)?;
    }
    Ok(())
}
impl Settings {
    pub(crate) fn learn_contact(
        &mut self,
        vault: &Vault,
        hint: ContactHint,
        source: &str,
        time: u64,
    ) -> Result<bool, Failure> {
        hint.validate()?;
        if hint.node == PrivateKey::from_bytes(&vault.get("node")?).public_key()
            || self.revocations.revokes(&hint.node)
        {
            return Ok(false);
        }
        self.bind_saved_contact(hint.node);
        if self
            .contact_meta
            .get(&hint.node)
            .is_some_and(|meta| meta.source == "application")
        {
            return Ok(false);
        }
        let expired: Vec<_> = self
            .contact_meta
            .iter()
            .filter(|(_, meta)| {
                meta.source != "application" && meta.learned.saturating_add(AGE) < time
            })
            .map(|(node, _)| *node)
            .collect();
        for node in expired {
            self.remove_peer(node)?;
            self.contact_meta.remove(&node);
        }
        if !self.peers.contains_key(&hint.node)
            && self.contact_count() >= crate::enrollment::MAX_CONTACTS
        {
            let oldest = self
                .peers
                .keys()
                .filter(|node| {
                    self.contact_meta
                        .get(node)
                        .is_none_or(|meta| meta.source != "application")
                })
                .min_by_key(|node| self.contact_meta.get(node).map_or(0, |meta| meta.learned))
                .copied();
            let Some(oldest) = oldest else {
                return Ok(false);
            };
            self.remove_peer(oldest)?;
            self.contact_meta.remove(&oldest);
        }
        self.peers.insert(
            hint.node,
            Contact {
                routes: hint
                    .routes
                    .into_iter()
                    .map(Route::try_from)
                    .collect::<Result<_, _>>()?,
            },
        );
        self.contact_meta.insert(
            hint.node,
            ContactMeta {
                source: source.into(),
                learned: time,
                trusted: hint.trusted,
                local: false,
            },
        );
        Ok(true)
    }
    pub(crate) fn would_learn(&self, node: PublicKey, address: &str, local: bool) -> bool {
        if self.revocations.revokes(&node) {
            return false;
        }
        let meta = self.contact_meta.get(&node);
        let marked = !local || meta.is_some_and(|meta| meta.local);
        let known = self.peers.get(&node).is_some_and(|contact| {
            meta.is_some_and(|meta| meta.source == "application")
                || contact.routes.iter().any(|route| route.as_str() == address)
        });
        !(known && marked)
    }
    pub(crate) fn learn_found(
        &mut self,
        vault: &Vault,
        node: PublicKey,
        address: String,
        local: bool,
        time: u64,
    ) -> Result<bool, Failure> {
        let route = Route::try_from(address)?;
        let hint = ContactHint {
            node,
            routes: vec![route.as_str().to_owned()],
            trusted: false,
        };
        if !self.would_learn(node, route.as_str(), local)
            || node == PrivateKey::from_bytes(&vault.get("node")?).public_key()
        {
            return Ok(false);
        }
        let Some(contact) = self.peers.get_mut(&node) else {
            let learned = self.learn_contact(vault, hint, "discovery", time)?;
            if let Some(meta) = self.contact_meta.get_mut(&node).filter(|_| learned) {
                meta.local = local;
            }
            return Ok(learned);
        };
        let meta = self
            .contact_meta
            .entry(node)
            .or_insert_with(|| ContactMeta {
                source: "enrollment".into(),
                learned: time,
                trusted: false,
                local: false,
            });
        meta.local |= local;
        if meta.source != "application" {
            contact.learn(vec![route]);
        }
        Ok(true)
    }
    pub(crate) fn contact_hint(&self, node: PublicKey) -> Option<ContactHint> {
        if self.revocations.revokes(&node) {
            return None;
        }
        let contact = self.peers.get(&node)?;
        if contact.routes.is_empty() {
            return None;
        }
        Some(ContactHint {
            node,
            routes: contact
                .routes
                .iter()
                .map(|route| route.as_str().to_owned())
                .collect(),
            trusted: self.issuers.contains(&node)
                || self
                    .contact_meta
                    .get(&node)
                    .is_some_and(|meta| meta.trusted),
        })
    }
    pub(crate) fn discovery_exchange(
        &self,
        vault: &Vault,
        root: Option<Box<DiscoverySecret>>,
        values: Vec<DiscoverySecret>,
        after: Option<Commitment>,
    ) -> Result<ContactResponse, Failure> {
        let scope = self.domain()?.scope;
        let mut state = Discovery::load(vault)?;
        let previous = state.clone();
        let time = now()?;
        if let Some(root) = root {
            let root = *root;
            root.verify_root(self.publisher, scope)?;
            if state.root.as_ref().is_some_and(|held| held != &root) {
                return Err("discovery root mismatch".into());
            }
            state.root = Some(root);
        }
        if !values.is_empty() {
            state.values.retain(|id, value| {
                self.counts_in_topics(value, scope, time)
                    && value
                        .statement
                        .is_some_and(|statement| HEXLOWER.encode(&statement) == *id)
            });
        }
        for value in values {
            let id = value.statement.ok_or("discovery value lacks statement")?;
            if self.revocations.get(&id).is_none() {
                return Err("discovery value names an unknown statement".into());
            }
            if !self.counts_in_topics(&value, scope, time) {
                value.verify_historic(self.publisher, scope)?;
                continue;
            }
            let id = HEXLOWER.encode(&id);
            if state
                .values
                .get(&id)
                .is_none_or(|held| value.value < held.value)
            {
                state.values.insert(id, value);
            }
        }
        if state != previous {
            state.save(vault)?;
        }
        let mut page = state
            .values
            .values()
            .filter(|value| after.is_none_or(|after| value.statement.is_some_and(|id| id > after)));
        let values: Vec<_> = page.by_ref().take(32).cloned().collect();
        let next = if page.next().is_some() {
            values.last().and_then(|value| value.statement)
        } else {
            None
        };
        Ok(ContactResponse::Discovery {
            root: state.root.map(Box::new),
            values,
            next,
        })
    }
    fn counts_in_topics(&self, value: &DiscoverySecret, scope: Scope, time: u64) -> bool {
        value
            .statement
            .and_then(|id| self.revocations.get(&id))
            .is_some_and(|signed| {
                signed.live(time)
                    && value
                        .verify_unrevoked(self.publisher, scope, &self.revocations)
                        .is_ok()
            })
    }
    fn next_discovery(
        &self,
        loaded: &Discovery,
        time: u64,
    ) -> Result<(Discovery, Commitment), Failure> {
        let mut state = loaded.clone();
        let root = state
            .root
            .as_ref()
            .ok_or("discovery root not received in an admitted session")?;
        let scope = self.domain()?.scope;
        root.verify_root(self.publisher, scope)?;
        if state.values.iter().any(|(id, value)| {
            value
                .statement
                .is_none_or(|statement| HEXLOWER.encode(&statement) != *id)
        }) {
            return Err("invalid stored discovery value".into());
        }
        let mut bytes = root.value.to_vec();
        state
            .values
            .retain(|_, value| self.counts_in_topics(value, scope, time));
        for signed in self
            .revocations
            .statements()
            .filter(|signed| signed.live(time))
        {
            let id = signed.id()?;
            bytes.extend(id);
            match state.values.get(&HEXLOWER.encode(&id)) {
                Some(value) => {
                    bytes.push(1);
                    bytes.extend(value.value);
                }
                None => bytes.push(0),
            }
        }
        let topic = commitment("rocky.discovery-topic.v1", &bytes);
        if state.current != Some(topic) {
            state.previous = state.current.map(|old| (old, time.saturating_add(86400)));
            state.current = Some(topic);
        }
        state.previous = state.previous.filter(|(_, until)| time < *until);
        Ok((state, topic))
    }
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn discovery_topics(
        &self,
        vault: &Vault,
        time: u64,
    ) -> Result<(Commitment, Option<Commitment>), Failure> {
        let (state, topic) = self.next_discovery(&Discovery::load(vault)?, time)?;
        Ok((topic, state.previous.map(|(topic, _)| topic)))
    }
    pub(crate) fn advance_discovery_topics(
        &self,
        vault: &Vault,
        time: u64,
    ) -> Result<(Commitment, Option<Commitment>), Failure> {
        let loaded = Discovery::load(vault)?;
        let (state, topic) = self.next_discovery(&loaded, time)?;
        if state != loaded {
            state.save(vault)?;
        }
        Ok((topic, state.previous.map(|(topic, _)| topic)))
    }
}
pub(crate) async fn exchange(
    session: &mut NativeSession,
    handler: &dyn ContactHandler,
) -> Result<(), Failure> {
    let peer = session.peer();
    let ContactResponse::Handover { contacts } = handler
        .answer(
            peer,
            ContactRequest::Handover {
                contacts: Vec::new(),
            },
        )
        .await?
    else {
        return Err("invalid local contact response".into());
    };
    let ContactResponse::Handover { contacts } = session
        .exchange_contacts(ContactRequest::Handover { contacts })
        .await?
    else {
        return Err("invalid handover response".into());
    };
    handler
        .answer(peer, ContactRequest::Handover { contacts })
        .await?;
    let (mut after, mut own_after) = (None, None);
    for _ in 0..=rocky_credentials::MAX_LIVE_REVOCATIONS / 32 {
        let ContactResponse::Discovery {
            root,
            values,
            next: own_next,
        } = handler
            .answer(
                peer,
                ContactRequest::Discovery {
                    root: None,
                    values: Vec::new(),
                    after: own_after,
                },
            )
            .await?
        else {
            return Err("invalid local discovery response".into());
        };
        let ContactResponse::Discovery { root, values, next } = session
            .exchange_contacts(ContactRequest::Discovery {
                root,
                values,
                after,
            })
            .await?
        else {
            return Err("invalid discovery response".into());
        };
        handler
            .answer(
                peer,
                ContactRequest::Discovery {
                    root,
                    values,
                    after: None,
                },
            )
            .await?;
        if next.is_none() && own_next.is_none() {
            return Ok(());
        }
        after = next.or(after);
        own_after = own_next.or(own_after);
    }
    Err("discovery exchange exceeds live bound".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FileSecretStore, SecretStore, schema::Schema};
    use rocky_credentials::testing::UntrustedIssuer as _;
    use rocky_credentials::{Revocation, SignedRevocation};
    use std::{collections::BTreeSet, sync::Arc};
    const SCHEMA: &str = r#"{"application":"contacts","version":1,"format":7,"tables":[{"name":"items","class":"replayed","columns":[{"name":"title","type":"text","not_null":true}],"primary_key":["id"]}]}"#;
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn equal_sets_derive_equal_topics_missing_values_differ_and_substitution_is_refused()
    -> Result<(), Failure> {
        let dir = rocky_platform::fs::Scratch::new().await;
        let store: Arc<dyn SecretStore> = Arc::new(FileSecretStore::new(dir.path()));
        let mut a = Settings::create_owned(
            &Schema::parse(SCHEMA)?,
            &store,
            crate::enrollment::DEFAULT_TTL,
        )?;
        let av = a.vault(&store);
        let publisher = a.identity;
        let owner = a.issuer_links();
        let node = PrivateKey::from_bytes(&av.get("node")?);
        let credentials = a.credentials.clone();
        let old = a.advance_discovery_topics(&av, now()?)?.0;
        let statement = Revocation {
            app: a.domain()?.manifest().app,
            issuer: publisher,
            time: now()?,
            nodes: BTreeSet::new(),
            identities: [PrivateKey::new().public_key()].into(),
            capabilities: BTreeSet::new(),
            cut: Default::default(),
            mode: rocky_credentials::RevocationMode::Lost,
        };
        let signed = SignedRevocation::sign(
            statement.clone(),
            rocky_credentials::Signer::node(&node, &credentials)?,
            &owner,
        )?;
        a.revocations.merge(
            &rocky_credentials::RevocationSet::try_from(vec![signed])?,
            a.publisher,
            statement.app,
            now()?,
        )?;
        mint_value(
            &av,
            (
                rocky_credentials::Signer::node(&node, &credentials)?,
                &owner,
            ),
            (a.publisher, a.domain()?.scope),
            &a.revocations,
            &statement,
        )?;
        let mut b = a.clone();
        b.installation = "other".into();
        let bv = b.vault(&store);
        let a_state = Discovery::load(&av)?;
        b.discovery_exchange(&bv, a_state.root.clone().map(Box::new), Vec::new(), None)?;
        let missing = b.advance_discovery_topics(&bv, now()?)?.0;
        let (new, previous) = a.advance_discovery_topics(&av, now()?)?;
        assert_ne!(missing, new);
        assert_ne!(old, new);
        assert_eq!(previous, Some(old));
        b.discovery_exchange(&bv, None, a_state.values.values().cloned().collect(), None)?;
        assert_eq!(b.advance_discovery_topics(&bv, now()?)?.0, new);
        let mut wrong = a_state.values.values().next().ok_or("value")?.clone();
        wrong.value[0] ^= 1;
        assert!(b.discovery_exchange(&bv, None, vec![wrong], None).is_err());
        let mut missing_statement = b.clone();
        missing_statement.revocations = Default::default();
        assert_ne!(missing_statement.discovery_topics(&bv, now()?)?.0, new);
        assert_eq!(
            b.discovery_topics(&bv, now()?)?.0,
            new,
            "a copy that lacks the statement leaves its stored value in place"
        );
        assert_eq!(a.advance_discovery_topics(&av, now()? + 86400)?.1, None);
        assert_eq!(Discovery::load(&av)?.values, a_state.values);
        Ok(())
    }
    struct Fixture {
        _dir: rocky_platform::fs::Scratch,
        store: Arc<dyn SecretStore>,
        root: rocky_credentials::RootKey,
        settings: Settings,
        member: PrivateKey,
    }
    impl Fixture {
        async fn new() -> Result<Self, Failure> {
            let dir = rocky_platform::fs::Scratch::new().await;
            let store: Arc<dyn SecretStore> = Arc::new(FileSecretStore::new(dir.path()));
            let root = rocky_credentials::RootKey::generate();
            let settings = Settings::create(
                &Schema::parse(SCHEMA)?,
                &store,
                crate::enrollment::DEFAULT_TTL,
                std::sync::Arc::new(root.identity()),
                &root,
            )?;
            let member = PrivateKey::from_bytes(&settings.vault(&store).get("node")?);
            Ok(Self {
                _dir: dir,
                store,
                root,
                settings,
                member,
            })
        }
        fn revocation(
            &self,
            nodes: BTreeSet<rocky_credentials::PeerId>,
            identities: BTreeSet<PublicKey>,
        ) -> Result<(Revocation, rocky_credentials::RevocationSet), Failure> {
            let statement = self.statement(nodes, identities)?;
            let signed = SignedRevocation::sign(
                statement.clone(),
                rocky_credentials::Signer::from(&self.root),
                &rocky_credentials::CredentialChain(Vec::new()),
            )?;
            Ok((
                statement,
                rocky_credentials::RevocationSet::try_from(vec![signed])?,
            ))
        }
        fn owner_revocation(
            &self,
            identities: BTreeSet<PublicKey>,
        ) -> Result<rocky_credentials::RevocationSet, Failure> {
            let signer = rocky_credentials::Signer::node(&self.member, &self.settings.credentials)?;
            let signed = SignedRevocation::sign(
                Revocation {
                    issuer: signer.principal()?,
                    ..self.statement(BTreeSet::new(), identities)?
                },
                signer,
                &self.settings.issuer_links(),
            )?;
            assert!(!signed.root());
            Ok(rocky_credentials::RevocationSet::try_from(vec![signed])?)
        }
        fn statement(
            &self,
            nodes: BTreeSet<rocky_credentials::PeerId>,
            identities: BTreeSet<PublicKey>,
        ) -> Result<Revocation, Failure> {
            Ok(Revocation {
                app: self.settings.domain()?.manifest().app,
                issuer: self.settings.publisher,
                time: now()?,
                nodes,
                identities,
                capabilities: BTreeSet::new(),
                cut: Default::default(),
                mode: rocky_credentials::RevocationMode::Lost,
            })
        }
        fn learn(
            peer: &mut Settings,
            set: &rocky_credentials::RevocationSet,
        ) -> Result<(), Failure> {
            let (publisher, app) = (peer.publisher, peer.domain()?.manifest().app);
            peer.revocations.merge(set, publisher, app, now()?)?;
            Ok(())
        }
        fn honest(&self, statement: &Revocation) -> Result<DiscoverySecret, Failure> {
            Ok(DiscoverySecret::sign(
                self.settings.domain()?.scope,
                Some(statement.id()?),
                crate::enrollment::random(),
                (
                    rocky_credentials::Signer::from(&self.root),
                    &rocky_credentials::CredentialChain(Vec::new()),
                ),
            )?)
        }
        fn delegate(
            &self,
            issued_at: u64,
            lifetime: u64,
        ) -> Result<(PrivateKey, PrivateKey, rocky_credentials::CredentialChain), Failure> {
            let delegate = PrivateKey::new();
            let node = PrivateKey::new();
            let owner =
                rocky_credentials::Authority::issuer(&self.member, &self.settings.credentials)?;
            let credentials = owner
                .chain_of(owner.issue_untrusted_issuer(
                    delegate.public_key(),
                    rocky_credentials::Validity {
                        issued_at,
                        lifetime: std::time::Duration::from_secs(lifetime),
                        clock_skew: std::time::Duration::from_secs(1),
                    },
                )?)
                .certify(&delegate, &node)?;
            Ok((delegate, node, credentials))
        }
        fn peer(&self, installation: &str) -> Result<(Settings, Vault), Failure> {
            let mut peer = self.settings.clone();
            peer.installation = installation.into();
            let vault = peer.vault(&self.store);
            let root = Discovery::load(&self.settings.vault(&self.store))?
                .root
                .map(Box::new);
            peer.discovery_exchange(&vault, root, Vec::new(), None)?;
            Ok((peer, vault))
        }
    }
    fn held(vault: &Vault) -> Result<Vec<DiscoverySecret>, Failure> {
        Ok(Discovery::load(vault)?.values.into_values().collect())
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn a_value_signed_by_a_revoked_member_counts_as_absent_whether_stored_before_or_after_the_revocation()
    -> Result<(), Failure> {
        let mut fixture = Fixture::new().await?;
        let scope = fixture.settings.domain()?.scope;
        let (unrelated, unrelated_set) =
            fixture.revocation(BTreeSet::new(), [PrivateKey::new().public_key()].into())?;
        let (_, member_revoked) = fixture.revocation(
            [rocky_credentials::PeerId::of(fixture.member.public_key())].into(),
            BTreeSet::new(),
        )?;
        Fixture::learn(&mut fixture.settings, &unrelated_set)?;
        let credentials = fixture.settings.credentials.clone();
        let forged = DiscoverySecret::sign(
            scope,
            Some(unrelated.id()?),
            crate::enrollment::random(),
            (
                rocky_credentials::Signer::node(&fixture.member, &credentials)?,
                &fixture.settings.issuer_links(),
            ),
        )?;
        let honest = fixture.honest(&unrelated)?;
        let (mut exchanged, exchanged_vault) = fixture.peer("exchanged")?;
        let (mut idle, idle_vault) = fixture.peer("idle")?;
        let (mut late, late_vault) = fixture.peer("late")?;
        for (peer, vault) in [(&exchanged, &exchanged_vault), (&idle, &idle_vault)] {
            peer.discovery_exchange(vault, None, vec![forged.clone()], None)?;
            assert_eq!(held(vault)?, vec![forged.clone()]);
        }
        for peer in [&mut exchanged, &mut idle, &mut late] {
            Fixture::learn(peer, &member_revoked)?;
        }
        exchanged.discovery_exchange(&exchanged_vault, None, vec![honest.clone()], None)?;
        assert_eq!(held(&exchanged_vault)?, vec![honest.clone()]);
        let time = now()?;
        assert_eq!(
            idle.advance_discovery_topics(&idle_vault, time)?.0,
            late.advance_discovery_topics(&late_vault, time)?.0
        );
        forged.verify_historic(late.publisher, scope)?;
        late.discovery_exchange(
            &late_vault,
            None,
            vec![forged.clone(), honest.clone()],
            None,
        )?;
        idle.discovery_exchange(&idle_vault, None, vec![honest.clone()], None)?;
        let topic = exchanged
            .advance_discovery_topics(&exchanged_vault, time)?
            .0;
        for (peer, vault) in [(&idle, &idle_vault), (&late, &late_vault)] {
            assert_eq!(held(vault)?, vec![honest.clone()]);
            assert_eq!(peer.advance_discovery_topics(vault, time)?.0, topic);
        }
        Ok(())
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn a_delegated_issuers_value_outlives_its_chain_but_not_a_revocation_by_its_owner()
    -> Result<(), Failure> {
        let mut fixture = Fixture::new().await?;
        let scope = fixture.settings.domain()?.scope;
        let (statement, set) =
            fixture.revocation(BTreeSet::new(), [PrivateKey::new().public_key()].into())?;
        Fixture::learn(&mut fixture.settings, &set)?;
        let lifetime = 60;
        let (delegate, delegate_node, delegate_credentials) =
            fixture.delegate(statement.time + lifetime, lifetime)?;
        let minted = DiscoverySecret::sign(
            scope,
            Some(statement.id()?),
            crate::enrollment::random(),
            (
                rocky_credentials::Signer::node(&delegate_node, &delegate_credentials)?,
                &delegate_credentials.capabilities(),
            ),
        )?;
        let (mut holder, holder_vault) = fixture.peer("holder")?;
        let (mut empty, empty_vault) = fixture.peer("empty")?;
        holder.discovery_exchange(&holder_vault, None, vec![minted.clone()], None)?;
        let expired = statement.time + 10 * lifetime;
        assert!(
            set.get(&statement.id()?)
                .is_some_and(|signed| signed.live(expired))
        );
        assert_ne!(
            holder.advance_discovery_topics(&holder_vault, expired)?.0,
            empty.advance_discovery_topics(&empty_vault, expired)?.0
        );
        assert_eq!(held(&holder_vault)?, vec![minted]);
        let delegate_revoked = fixture.owner_revocation([delegate.public_key()].into())?;
        Fixture::learn(&mut holder, &delegate_revoked)?;
        Fixture::learn(&mut empty, &delegate_revoked)?;
        assert_eq!(
            holder.advance_discovery_topics(&holder_vault, expired)?.0,
            empty.advance_discovery_topics(&empty_vault, expired)?.0
        );
        assert!(held(&holder_vault)?.is_empty());
        Ok(())
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn a_discovery_root_is_taken_only_from_the_root_key_or_a_trusted_identity()
    -> Result<(), Failure> {
        let fixture = Fixture::new().await?;
        let scope = fixture.settings.domain()?.scope;
        let (_, node, credentials) = fixture.delegate(now()?, 3600)?;
        let foreign = DiscoverySecret::sign(
            scope,
            None,
            crate::enrollment::random(),
            (
                rocky_credentials::Signer::node(&node, &credentials)?,
                &credentials.capabilities(),
            ),
        )?;
        foreign.verify_historic(fixture.settings.publisher, scope)?;
        let mut joiner = fixture.settings.clone();
        joiner.installation = "joiner".into();
        let vault = joiner.vault(&fixture.store);
        assert!(
            joiner
                .discovery_exchange(&vault, Some(Box::new(foreign)), Vec::new(), None)
                .is_err()
        );
        assert_eq!(Discovery::load(&vault)?.root, None);
        let (statement, set) =
            fixture.revocation(BTreeSet::new(), [PrivateKey::new().public_key()].into())?;
        Fixture::learn(&mut joiner, &set)?;
        assert!(
            joiner
                .discovery_exchange(
                    &vault,
                    Some(Box::new(fixture.honest(&statement)?)),
                    Vec::new(),
                    None
                )
                .is_err()
        );
        assert_eq!(Discovery::load(&vault)?.root, None);
        let owned = Discovery::load(&fixture.settings.vault(&fixture.store))?
            .root
            .ok_or("the owner holds a root")?;
        joiner.discovery_exchange(&vault, Some(Box::new(owned.clone())), Vec::new(), None)?;
        assert_eq!(Discovery::load(&vault)?.root, Some(owned));
        Ok(())
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn peers_given_two_honest_values_for_one_statement_in_either_order_keep_the_lower()
    -> Result<(), Failure> {
        let mut fixture = Fixture::new().await?;
        let (statement, set) =
            fixture.revocation(BTreeSet::new(), [PrivateKey::new().public_key()].into())?;
        Fixture::learn(&mut fixture.settings, &set)?;
        let (first, second) = (fixture.honest(&statement)?, fixture.honest(&statement)?);
        let lower = if first.value < second.value {
            first.clone()
        } else {
            second.clone()
        };
        let (one, one_vault) = fixture.peer("one")?;
        let (other, other_vault) = fixture.peer("other")?;
        one.discovery_exchange(&one_vault, None, vec![first.clone()], None)?;
        one.discovery_exchange(&one_vault, None, vec![second.clone()], None)?;
        other.discovery_exchange(&other_vault, None, vec![second], None)?;
        other.discovery_exchange(&other_vault, None, vec![first], None)?;
        assert_eq!(held(&one_vault)?, vec![lower.clone()]);
        assert_eq!(held(&other_vault)?, vec![lower]);
        let time = now()?;
        assert_eq!(
            one.advance_discovery_topics(&one_vault, time)?.0,
            other.advance_discovery_topics(&other_vault, time)?.0
        );
        Ok(())
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn a_rotation_mints_again_over_a_stored_value_whose_minter_was_revoked()
    -> Result<(), Failure> {
        let mut fixture = Fixture::new().await?;
        let scope = fixture.settings.domain()?.scope;
        let (statement, set) =
            fixture.revocation(BTreeSet::new(), [PrivateKey::new().public_key()].into())?;
        let (_, member_revoked) = fixture.revocation(
            [rocky_credentials::PeerId::of(fixture.member.public_key())].into(),
            BTreeSet::new(),
        )?;
        Fixture::learn(&mut fixture.settings, &set)?;
        let credentials = fixture.settings.credentials.clone();
        let revoked = DiscoverySecret::sign(
            scope,
            Some(statement.id()?),
            crate::enrollment::random(),
            (
                rocky_credentials::Signer::node(&fixture.member, &credentials)?,
                &fixture.settings.issuer_links(),
            ),
        )?;
        let (mut minter, vault) = fixture.peer("minter")?;
        minter.discovery_exchange(&vault, None, vec![revoked.clone()], None)?;
        Fixture::learn(&mut minter, &member_revoked)?;
        mint_value(
            &vault,
            (
                rocky_credentials::Signer::from(&fixture.root),
                &rocky_credentials::CredentialChain(Vec::new()),
            ),
            (minter.publisher, scope),
            &minter.revocations,
            &statement,
        )?;
        let minted = held(&vault)?;
        assert_eq!(minted.len(), 1);
        assert_ne!(minted, vec![revoked]);
        Ok(())
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn a_full_list_evicts_the_oldest_learned_contact_and_preserves_application_routes()
    -> Result<(), Failure> {
        let dir = rocky_platform::fs::Scratch::new().await;
        let store: Arc<dyn SecretStore> = Arc::new(FileSecretStore::new(dir.path()));
        let mut settings = Settings::create_owned(
            &Schema::parse(SCHEMA)?,
            &store,
            crate::enrollment::DEFAULT_TTL,
        )?;
        let vault = settings.vault(&store);
        let configured = PrivateKey::new().public_key();
        settings.peers.insert(
            configured,
            Contact {
                routes: vec![Route::try_from(String::from("memory:configured")).expect("a route")],
            },
        );
        settings.contact_meta.insert(
            configured,
            ContactMeta {
                source: "application".into(),
                learned: 0,
                trusted: false,
                local: false,
            },
        );
        let oldest = PrivateKey::new().public_key();
        let hint = |node| ContactHint {
            node,
            routes: vec!["memory:learned".into()],
            trusted: false,
        };
        settings.learn_contact(&vault, hint(oldest), "handover", 1)?;
        for at in 2..64 {
            settings.learn_contact(&vault, hint(PrivateKey::new().public_key()), "handover", at)?;
        }
        assert_eq!(settings.peers.len(), 64);
        let newest = PrivateKey::new().public_key();
        settings.learn_contact(&vault, hint(newest), "request", 64)?;
        assert!(!settings.peers.contains_key(&oldest));
        assert!(settings.contact_meta.values().any(|meta| meta.learned == 2));
        assert!(settings.peers.contains_key(&newest));
        assert_eq!(
            settings.peers[&configured].routes[0].as_str(),
            "memory:configured"
        );
        assert!(!settings.learn_contact(&vault, hint(configured), "handover", 65)?);
        settings.learn_contact(
            &vault,
            hint(PrivateKey::new().public_key()),
            "handover",
            AGE + 66,
        )?;
        assert_eq!(settings.peers.len(), 2);
        assert!(settings.peers.contains_key(&configured));
        Ok(())
    }
}
