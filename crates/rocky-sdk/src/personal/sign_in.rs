use super::{
    Installing, PersonalSignInError, RestoreCoverage, VerifiedBackup,
    bootstrap::{Metadata, Page},
};
use crate::{
    Failure, Freshness,
    enrollment::{Settings, exchange, now},
    escrow::{Archived, Recovered},
    groups::{GroupRecord, NAMING_ROLE, record_key, secret_role},
    peer::{PeerReply, PeerRequest},
    secrets::{SecretStore, Vault},
    transport::Connected,
};
use data_encoding::HEXLOWER;
use rocky_credentials::{
    CredentialChain, DatabaseContext, IdentityEnrollment, IdentityReply, LinkEnrollment, PeerError,
    PersonalAccount, PersonalAct, PersonalAuthorization, PersonalChallenge, PersonalHello,
    PersonalLink, PersonalPublication, PrivateKey, PublicKey, RevocationSet, SignedCapability,
    SignedManifest,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

const MAX_CANDIDATES: usize = 8;
const MAX_PAGES: u32 = 4096;

pub enum SignInSource {
    Discover,
    Backup(VerifiedBackup),
}

pub(crate) struct SignIn {
    pub(crate) account: Mutex<Option<PersonalAccount>>,
    pub(crate) source: Mutex<Option<SignInSource>>,
    pub(crate) wait: rocky_platform::time::Duration,
    pub(crate) outcome: Arc<Mutex<Option<(RestoreCoverage, Freshness)>>>,
}

pub(crate) const STAGING: &str = "restore";

pub(crate) fn prepare_sign_in(
    directory: &Path,
    account: &PersonalAccount,
    secrets: &Arc<dyn SecretStore>,
) -> Result<Option<super::Finished>, PersonalSignInError> {
    let Some(saved) = super::Intent::load(directory)? else {
        super::check_empty(directory, account)?;
        return Ok(None);
    };
    if let Some(finished) = saved.finished(directory, names(account), secrets)? {
        return Ok(Some(finished));
    }
    super::check_installed(directory, account)?;
    saved.continues(names(account), false)?;
    let removed = (|| -> Result<(), Failure> {
        for name in [
            "rocky.sqlite",
            "rocky.sqlite-wal",
            "rocky.sqlite-shm",
            "revoked.json",
            "bootstrap.json",
            "mailbox.json",
            crate::backup::MARKER,
        ] {
            match rocky_platform::fs::remove_file(directory.join(name)) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(error.into());
                }
                _ => {}
            }
        }
        if rocky_platform::fs::exists(directory.join(STAGING)) {
            rocky_platform::fs::remove_dir_all(directory.join(STAGING))?;
        }
        Ok(())
    })();
    removed.map_err(PersonalSignInError::Failed)?;
    Ok(None)
}

pub(crate) fn tidy(directory: &Path, before: Option<&[PathBuf]>) {
    if rocky_platform::fs::exists(super::Intent::path(directory)) {
        return;
    }
    match before {
        None => {
            let _ = rocky_platform::fs::remove_dir_all(directory);
        }
        Some(before) => {
            for entry in rocky_platform::fs::read_dir(directory).unwrap_or_default() {
                if !before.contains(&entry) {
                    let _ = rocky_platform::fs::remove_file(&entry)
                        .or_else(|_| rocky_platform::fs::remove_dir_all(&entry));
                }
            }
        }
    }
}

fn names(
    account: &PersonalAccount,
) -> (PublicKey, PublicKey, &rocky_credentials::PersonalNamespace) {
    let locator = account.locator();
    (locator.publisher, locator.owner, account.namespace())
}

fn database(account: &PersonalAccount, schema: &crate::Schema) -> DatabaseContext {
    let locator = account.locator();
    DatabaseContext {
        publisher: locator.publisher,
        scope: locator.scope(&schema.application),
    }
}

fn at_capacity(error: &Failure) -> bool {
    let limit = rocky_credentials::PersonalError::DeviceLimit {
        limit: rocky_credentials::MAX_PERSONAL_DEVICES,
    };
    error
        .downcast_ref::<PeerError>()
        .is_some_and(|error| error.code == limit.code())
}

struct Candidate {
    address: String,
    challenge: PersonalChallenge,
    genesis: SignedManifest,
}

pub(crate) async fn install(
    path: &Path,
    directory: &Path,
    secrets: &Arc<dyn SecretStore>,
    schema: &crate::Schema,
    sign_in: &SignIn,
    network: Option<crate::Network>,
) -> Result<(Settings, Option<(Connected, bool)>), Failure> {
    let account = sign_in
        .account
        .lock()
        .map_err(|_| "account poisoned")?
        .take()
        .ok_or("the sign-in was already attempted")?;
    let source = sign_in
        .source
        .lock()
        .map_err(|_| "source poisoned")?
        .take()
        .ok_or("the sign-in was already attempted")?;
    let installed = match source {
        SignInSource::Backup(backup) => {
            match from_backup(path, directory, secrets, schema, &account, &backup).await {
                Ok((settings, coverage)) => {
                    let link = match network {
                        Some(network) => {
                            let key = Settings::route_secret(&settings.vault(secrets))?;
                            Some(network.start(true, key).await?)
                        }
                        None => None,
                    };
                    Ok((settings, link, (coverage, Freshness::Unwitnessed)))
                }
                Err(error) => Err(error),
            }
        }
        SignInSource::Discover => {
            let network = network.ok_or(PersonalSignInError::SourceUnavailable)?;
            let (node, route_secret) = match Installing::saved_keys(directory, secrets, &account)? {
                Some(keys) => keys,
                None => (PrivateKey::new(), rocky_platform::entropy::random()),
            };
            let (transport, owned) = network.start(true, route_secret).await?;
            let installed = discovered(
                (path, directory),
                secrets,
                schema,
                &account,
                (&node, route_secret),
                (&transport, sign_in.wait),
            )
            .await;
            match installed {
                Ok((settings, coverage, freshness)) => {
                    Ok((settings, Some((transport, owned)), (coverage, freshness)))
                }
                Err(error) => {
                    if owned {
                        transport.close().await;
                    }
                    Err(error)
                }
            }
        }
    };
    let (settings, link, outcome) = match installed {
        Ok(installed) => installed,
        Err(error) => {
            let revoked_node = matches!(
                error.downcast_ref::<PersonalSignInError>(),
                Some(PersonalSignInError::RevokedNode { .. })
            );
            if revoked_node {
                super::discard(directory, secrets)?;
            }
            return Err(error);
        }
    };
    *sign_in.outcome.lock().map_err(|_| "outcome poisoned")? = Some(outcome);
    Ok((settings, link))
}

async fn discover(
    (transport, wait): (&Connected, rocky_platform::time::Duration),
    node: &PrivateKey,
    account: &PersonalAccount,
    database: DatabaseContext,
    pinned: Option<rocky_core::Commitment>,
) -> Result<Vec<Candidate>, Failure> {
    use futures_util::FutureExt;
    let discovery = transport
        .discovery
        .clone()
        .ok_or(PersonalSignInError::SourceUnavailable)?;
    let own = *node.public_key().as_bytes();
    discovery
        .announce(own, &[*account.discovery().topic()])
        .await?;
    let mut hints = Vec::new();
    if let Ok(Some(first)) = rocky_platform::time::timeout(wait, discovery.found()).await {
        hints.push(first);
        while hints.len() < MAX_CANDIDATES
            && let Some(Some(found)) = discovery.found().now_or_never()
        {
            hints.push(found);
        }
    }
    let _ = discovery.announce(own, &[]).await;
    let mut candidates: Vec<Candidate> = Vec::new();
    for found in hints {
        if found.claimed_node == own
            || candidates
                .iter()
                .any(|known| known.address == found.address)
        {
            continue;
        }
        let hello = PersonalHello::fresh();
        let request = PeerRequest::Identity(IdentityEnrollment::PersonalHello(hello.clone()));
        let Ok(PeerReply::Identity(IdentityReply::PersonalChallenge(challenge))) = exchange(
            &**transport,
            std::slice::from_ref(&found.address),
            database.scope,
            &request,
        )
        .await
        else {
            continue;
        };
        let locator = account.locator();
        let Ok((genesis, _)) =
            challenge.verify(&hello, database, (locator.owner, account.namespace()))
        else {
            continue;
        };
        candidates.push(Candidate {
            address: found.address,
            challenge: *challenge,
            genesis,
        });
    }
    let mut descriptors = candidates
        .iter()
        .map(|candidate| candidate.genesis.descriptor())
        .collect::<Result<Vec<_>, _>>()?;
    descriptors.extend(pinned);
    descriptors.sort_unstable();
    descriptors.dedup();
    if descriptors.len() > 1 {
        return Err(PersonalSignInError::GenesisConflict.into());
    }
    Ok(candidates)
}

fn authorize(
    account: &PersonalAccount,
    node: &PrivateKey,
    database: DatabaseContext,
    candidate: &Candidate,
    act: PersonalAct,
) -> Result<(PersonalAuthorization, rocky_credentials::Exchange), Failure> {
    Ok(PersonalAuthorization::sign(
        (account.root(), &account.identity()),
        node,
        (database, candidate.challenge.responder),
        (candidate.genesis.descriptor()?, candidate.challenge.nonce()),
        act,
    )?)
}

async fn fetch(
    transport: &Connected,
    account: &PersonalAccount,
    node: &PrivateKey,
    database: DatabaseContext,
    candidate: &Candidate,
) -> Result<
    (
        Metadata,
        Vec<(crate::escrow::Slot, rocky_credentials::RootEscrow)>,
    ),
    Failure,
> {
    let mut metadata = None;
    let mut escrow = Vec::new();
    let mut pages = 1;
    let mut page = 0;
    while page < pages {
        let (authorization, secret) = authorize(
            account,
            node,
            database,
            candidate,
            PersonalAct::Bootstrap { page },
        )?;
        let reply = exchange(
            &**transport,
            std::slice::from_ref(&candidate.address),
            database.scope,
            &PeerRequest::Identity(IdentityEnrollment::Personal(Box::new(
                authorization.clone(),
            ))),
        )
        .await?;
        let PeerReply::Identity(IdentityReply::Personal(answer)) = reply else {
            return Err("personal bootstrap reply kind".into());
        };
        let opened: Page = rocky_credentials::decode_bounded(
            &answer.open(&authorization, &secret)?,
            rocky_credentials::MAX_PEER_RPC_MESSAGE,
        )?;
        if page == 0 {
            if opened.pages == 0 || opened.pages > MAX_PAGES {
                return Err("personal bootstrap page count".into());
            }
            pages = opened.pages;
            metadata = Some(
                opened
                    .metadata
                    .ok_or("the first bootstrap page has no metadata")?,
            );
        } else if opened.metadata.is_some() || opened.pages != pages {
            return Err("personal bootstrap pages changed".into());
        }
        escrow.extend(opened.escrow);
        page += 1;
    }
    Ok((*metadata.ok_or("missing bootstrap metadata")?, escrow))
}

async fn discovered(
    (path, directory): (&Path, &Path),
    secrets: &Arc<dyn SecretStore>,
    schema: &crate::Schema,
    account: &PersonalAccount,
    (node, route_secret): (&PrivateKey, [u8; 32]),
    (transport, wait): (&Connected, rocky_platform::time::Duration),
) -> Result<(Settings, RestoreCoverage, Freshness), Failure> {
    let database = database(account, schema);
    let pinned = super::Intent::load(directory)?
        .and_then(|intent| intent.stage.pinned().map(|(descriptor, _)| descriptor));
    let candidates = discover((transport, wait), node, account, database, pinned).await?;
    if candidates.is_empty() {
        return Err(PersonalSignInError::SourceUnavailable.into());
    }
    let mut installing = Installing::sign_in(
        directory,
        secrets,
        (
            account,
            rocky_credentials::DiscoveryCapability::from_secret(*account.discovery().topic()),
        ),
        (node, route_secret),
    )?;
    super::fault::check(directory, super::fault::Stage::Prepared)?;
    let mut last: Option<Failure> = None;
    let mut capacity = false;
    for candidate in &candidates {
        let fetched = fetch(transport, account, node, database, candidate).await;
        let witnessed = fetched.and_then(|(metadata, escrow)| {
            let freshness = witness(&metadata, account, database, candidate)?;
            Ok((metadata, escrow, freshness))
        });
        let (metadata, escrow, freshness) = match witnessed {
            Ok(witnessed) => witnessed,
            Err(error) => {
                last = Some(error);
                continue;
            }
        };
        let held = metadata.held.clone();
        let responder = metadata.responder.clone();
        let source = Source {
            genesis: candidate.genesis.clone(),
            record: metadata.record,
            bootstrap: Some(metadata.bootstrap),
            ttl: metadata.ttl,
            grant: metadata.grant,
            revocations: metadata.revocations,
            groups: metadata.groups,
            epoch_statements: metadata.epoch_statements,
            escrow: (
                escrow
                    .into_iter()
                    .filter(|(slot, _)| slot.is_some())
                    .collect(),
                held,
            ),
            naming: metadata.naming,
            freshness,
        };
        let (mut settings, coverage) = match stage(secrets, account, &mut installing, source) {
            Ok(staged) => staged,
            Err(error) if ends_install(&error) => return Err(error),
            Err(error) => {
                last = Some(error);
                continue;
            }
        };
        let vault = settings.vault(secrets);
        if let Err(error) = renew_custody(
            transport,
            (account, node),
            (database, candidate),
            (&settings, &responder),
        )
        .await
        {
            last = Some(error);
            continue;
        }
        let certificate = settings
            .credentials
            .certificate()
            .ok_or("missing node certificate")?
            .clone();
        let accepted = match publish(
            transport,
            (account, node),
            database,
            candidate,
            (&certificate, installing.publication().bytes()),
        )
        .await
        {
            Ok(accepted) => accepted,
            Err(error) => {
                capacity |= at_capacity(&error);
                last = Some(error);
                continue;
            }
        };
        super::fault::check(directory, super::fault::Stage::Published)?;
        settings.learn_revocations(&accepted)?;
        let owner = account.locator().owner;
        if settings.revocations.tombstones(owner, node.public_key()) {
            return Err(PersonalSignInError::RevokedNode {
                node: rocky_credentials::PeerId::of(node.public_key()),
            }
            .into());
        }
        settings.learn_found(
            &vault,
            candidate.challenge.responder,
            transport.kind(),
            candidate.address.clone(),
            false,
            now()?,
        )?;
        rocky_credentials::write_private(&directory.join("bootstrap.json"), b"true")?;
        installing.installed(&vault, &coverage)?;
        settings.save(path)?;
        return Ok((settings, coverage, freshness));
    }
    if capacity {
        return Err(PersonalSignInError::Capacity {
            limit: rocky_credentials::MAX_PERSONAL_DEVICES,
        }
        .into());
    }
    Err(last.unwrap_or_else(|| PersonalSignInError::SourceUnavailable.into()))
}

async fn publish(
    transport: &Connected,
    (account, node): (&PersonalAccount, &PrivateKey),
    database: DatabaseContext,
    candidate: &Candidate,
    (certificate, publication): (&SignedCapability, [u8; 16]),
) -> Result<RevocationSet, Failure> {
    let (authorization, _) = authorize(
        account,
        node,
        database,
        candidate,
        PersonalAct::Publish {
            certificate: certificate.id()?,
            publication,
        },
    )?;
    let request =
        PeerRequest::LinkEnroll(LinkEnrollment::Personal(Box::new(PersonalPublication {
            authorization,
            certificate: certificate.clone(),
        })));
    let reply = exchange(
        &**transport,
        std::slice::from_ref(&candidate.address),
        database.scope,
        &request,
    )
    .await?;
    let PeerReply::LinkEnroll(accepted) = reply else {
        return Err("personal publication reply kind".into());
    };
    accepted.verify_in(database)?;
    let id = certificate.id()?;
    if !accepted
        .certificates()
        .any(|held| held.id().is_ok_and(|held| held == id))
    {
        return Err("the source did not keep the device certificate".into());
    }
    Ok(accepted)
}

async fn renew_custody(
    transport: &Connected,
    (account, node): (&PersonalAccount, &PrivateKey),
    (database, candidate): (DatabaseContext, &Candidate),
    (settings, responder): (&Settings, &CredentialChain),
) -> Result<(), Failure> {
    let links = responder.capabilities();
    let Some(link) = links.0.last().filter(|_| links.0.len() > 1) else {
        return Ok(());
    };
    let custody = !link.capability.grants.is_empty()
        && link
            .capability
            .grants
            .iter()
            .all(|grant| grant.rights == [rocky_core::Right::Custody].into());
    let time = now()?;
    if !custody || link.capability.expires > time.saturating_add(settings.ttl / 2) {
        return Ok(());
    }
    let authority = rocky_credentials::Authority::issuer(node, &settings.credentials)?;
    let renewed = authority.renew(
        &link.capability,
        &settings.revocations,
        rocky_credentials::Validity {
            issued_at: time,
            lifetime: std::time::Duration::from_secs(settings.ttl),
            clock_skew: std::time::Duration::from_secs(crate::enrollment::SKEW),
        },
    )?;
    let (authorization, _) = authorize(
        account,
        node,
        database,
        candidate,
        PersonalAct::RenewCustody {
            capability: renewed.id()?,
        },
    )?;
    let request = PeerRequest::LinkEnroll(LinkEnrollment::PersonalCustody(Box::new(
        rocky_credentials::PersonalCustodyRenewal {
            authorization,
            chain: authority.chain_of(renewed),
        },
    )));
    let reply = exchange(
        &**transport,
        std::slice::from_ref(&candidate.address),
        database.scope,
        &request,
    )
    .await?;
    let PeerReply::LinkEnroll(known) = reply else {
        return Err("custody renewal reply kind".into());
    };
    known.verify_in(database)?;
    Ok(())
}

fn witness(
    metadata: &Metadata,
    account: &PersonalAccount,
    database: DatabaseContext,
    candidate: &Candidate,
) -> Result<Freshness, Failure> {
    let locator = account.locator();
    if metadata.responder.database_context(locator.publisher)? != database {
        return Err("the responder belongs to another database".into());
    }
    let record = &metadata.record;
    let domain = crate::domain::Domain::at(
        &record.schema,
        *locator.publisher.as_bytes(),
        locator.mesh,
        locator.domain,
        record.generation,
        record.engine,
    )?;
    metadata.responder.historic_provenance_with_families(
        locator.publisher,
        candidate.challenge.responder,
        &domain.families,
    )?;
    let leaf = metadata
        .responder
        .certificate()
        .ok_or("the responder holds no certificate")?;
    if leaf.capability.subject != candidate.challenge.responder {
        return Err("the responder's chain names another node".into());
    }
    metadata.revocations.verify_in(database)?;
    Ok(if anchors(leaf, locator.owner, &metadata.revocations)? {
        Freshness::LocallyAnchored
    } else {
        Freshness::Unwitnessed
    })
}

fn anchors(
    leaf: &SignedCapability,
    owner: PublicKey,
    known: &RevocationSet,
) -> Result<bool, Failure> {
    let node = leaf.capability.subject;
    if known.tombstones(owner, node) || known.unfenced().ends_node_of(owner, node, false) {
        return Ok(false);
    }
    Ok(PersonalLink::of(leaf)?.is_some_and(|link| link.owner == owner))
}

struct Source {
    genesis: SignedManifest,
    record: crate::tip_schemas::Record,
    bootstrap: Option<crate::tip_schemas::Bootstrap>,
    ttl: u64,
    grant: SignedCapability,
    revocations: RevocationSet,
    groups: Vec<GroupRecord>,
    epoch_statements: BTreeMap<String, Vec<rocky_core::Commitment>>,
    escrow: Archived,
    naming: Option<rocky_credentials::MetadataEscrow>,
    freshness: Freshness,
}

fn ends_install(error: &Failure) -> bool {
    matches!(
        error.downcast_ref::<PersonalSignInError>(),
        Some(PersonalSignInError::RevokedNode { .. } | PersonalSignInError::GenesisConflict)
    ) || super::fault::crashed(error)
}

fn stage(
    secrets: &Arc<dyn SecretStore>,
    account: &PersonalAccount,
    installing: &mut Installing,
    source: Source,
) -> Result<(Settings, RestoreCoverage), Failure> {
    let locator = account.locator();
    let (publisher, owner) = (locator.publisher, locator.owner);
    source
        .genesis
        .verify_personal(publisher, owner, account.namespace())?;
    let descriptor = source.genesis.descriptor()?;
    if installing
        .pinned()
        .is_some_and(|pinned| pinned != descriptor)
    {
        return Err(PersonalSignInError::GenesisConflict.into());
    }
    if source.grant.issuer != publisher
        || source.grant.capability.subject != owner
        || source.grant.capability.extended()?.trusted.is_none()
    {
        return Err(crate::enrollment::personal_refusal(
            rocky_credentials::PersonalError::WrongOwner,
        ));
    }
    let vault = Vault::new(secrets.clone(), installing.installation());
    let node = Settings::node_key(&vault)?.public_key();
    let mut settings = Settings::installed(
        (installing.installation().to_owned(), false),
        (publisher, owner),
        (locator.mesh, locator.domain, source.record.schema.clone()),
        HEXLOWER.encode(&source.genesis.to_bytes()?),
        source.ttl,
    );
    source.record.bytes()?;
    settings.generation = source.record.generation;
    settings.engine = source.record.engine;
    settings.bootstrap = source.bootstrap;
    settings.verify_schema()?;
    source.revocations.verify_in(settings.database_context()?)?;
    settings.learn_revocations(&source.revocations)?;
    if settings.revocations.tombstones(owner, node)
        || settings
            .revocations
            .unfenced()
            .ends_node_of(owner, node, false)
    {
        return Err(PersonalSignInError::RevokedNode {
            node: rocky_credentials::PeerId::of(node),
        }
        .into());
    }
    let domain = settings.domain()?;
    let (records, held) = source.escrow;
    let holds = !held.is_empty();
    let (mut recovered, missing) = Recovered::import(&domain, (records, held), account.root())?;
    if let Some(naming) = &source.naming {
        recovered.naming = crate::escrow::open_naming(naming, &settings, &domain, account.root())?;
    }
    if holds && recovered.readers.is_empty() && recovered.secrets.is_empty() {
        return Err(PersonalSignInError::MissingEscrow.into());
    }
    let mut keys: BTreeMap<String, Vec<rocky_core::KeyEpoch>> = BTreeMap::new();
    for (name, epoch) in recovered.readers.keys() {
        keys.entry(name.clone()).or_default().push(*epoch);
    }
    if keys
        .values()
        .any(|epochs| epochs.len() > crate::enrollment::MAX_EPOCHS)
    {
        return Err("key epoch capacity reached".into());
    }
    settings.groups = escrowed_groups(source.groups, std::mem::take(&mut recovered.groups))?;
    settings.epoch_statements = source.epoch_statements;
    let mut authority = crate::enrollment::Issuing::default();
    crate::membership::observe_trusted(
        &settings.revocations,
        &[owner],
        now()?,
        &mut authority.trusted_nodes,
    )?;
    for nodes in authority.trusted_nodes.values_mut() {
        for observation in nodes.values_mut() {
            observation.read = true;
            observation.acknowledged = true;
        }
    }
    settings.authority = Some(authority);
    settings.personal = Some(installing.device(source.freshness));
    if let Some(kept_certificate) = installing.certificate() {
        let mut known = settings.revocations.clone();
        known.link(kept_certificate.clone())?;
        settings.learn_revocations(&known)?;
    }
    let links = CredentialChain(vec![source.grant]);
    let identity: Arc<dyn rocky_credentials::IdentityProvider> = Arc::new(account.identity());
    settings.credentials =
        crate::identities::with_identity(identity, || settings.certify(&vault, links, &domain))?;
    let certificate = settings
        .credentials
        .certificate()
        .ok_or("missing node certificate")?
        .clone();
    let labels: BTreeMap<_, _> = domain
        .scopes()
        .map(|(name, scope)| (scope, name.to_owned()))
        .collect();
    let missing: Vec<(String, rocky_core::KeyEpoch)> = missing
        .into_iter()
        .map(|(scope, epoch)| {
            (
                labels
                    .get(&scope)
                    .cloned()
                    .unwrap_or_else(|| rocky_core::scope_label(&scope)),
                epoch,
            )
        })
        .collect();
    let unkeyed: Vec<String> = domain
        .scopes()
        .filter(|(name, scope)| settings.reads_whole(&domain, scope) && !keys.contains_key(*name))
        .map(|(name, _)| name.to_owned())
        .collect();
    let directory = installing.directory().to_path_buf();
    installing.hold_secrets(recovered.secrets.keys().copied())?;
    installing.import(descriptor, keys)?;
    super::fault::check(&directory, super::fault::Stage::Pinned)?;
    for (index, ((name, epoch), key)) in recovered.readers.iter().enumerate() {
        settings.hold(&vault, name, *epoch, key)?;
        if index == 0 {
            super::fault::check(&directory, super::fault::Stage::Importing)?;
        }
    }
    for ((address, epoch), secret) in &recovered.secrets {
        vault.put(&secret_role(address, *epoch), secret)?;
    }
    if let Some(naming) = recovered.naming {
        vault.put(NAMING_ROLE, &naming)?;
    }
    if installing.certificate().is_none() {
        installing.certify(certificate)?;
    }
    super::fault::check(&directory, super::fault::Stage::Certified)?;
    let mut coverage = RestoreCoverage::held(&settings, &vault)?;
    coverage.missing = missing;
    coverage.unkeyed = unkeyed;
    Ok((settings, coverage))
}

fn escrowed_groups(
    named: Vec<GroupRecord>,
    escrowed: BTreeMap<String, GroupRecord>,
) -> Result<BTreeMap<String, GroupRecord>, Failure> {
    let mut groups: BTreeMap<String, GroupRecord> = named
        .into_iter()
        .map(|record| {
            (
                record_key(&record.address),
                GroupRecord {
                    epochs: Vec::new(),
                    ..record
                },
            )
        })
        .collect();
    for (key, record) in escrowed {
        match groups.get_mut(&key) {
            Some(named) if named.name != record.name => {
                return Err("an escrowed partition differs from the one the source names".into());
            }
            Some(named) => named.epochs = record.epochs,
            None => drop(groups.insert(key, record)),
        }
    }
    Ok(groups)
}

async fn from_backup(
    path: &Path,
    directory: &Path,
    secrets: &Arc<dyn SecretStore>,
    schema: &crate::Schema,
    account: &PersonalAccount,
    backup: &VerifiedBackup,
) -> Result<(Settings, RestoreCoverage), Failure> {
    backup.check(account, &database(account, schema))?;
    let (node, transport) = match Installing::saved_keys(directory, secrets, account)? {
        Some(keys) => keys,
        None => (PrivateKey::new(), rocky_platform::entropy::random()),
    };
    let staging = directory.join(STAGING);
    let opened = crate::backup::open(backup.archive(), &staging)?;
    if opened.id != backup.id() {
        return Err("the backup changed since it was verified".into());
    }
    let store = opened
        .files
        .iter()
        .find(|(name, _)| name == "rocky.sqlite")
        .map(|(_, staged)| staged)
        .ok_or("the backup holds no replica")?;
    let escrow =
        rocky_secure::EncryptedReplica::<rocky_core::TypedRows>::archived_escrow(store).await?;
    let mut installing = Installing::sign_in(
        directory,
        secrets,
        (
            account,
            rocky_credentials::DiscoveryCapability::from_secret(*account.discovery().topic()),
        ),
        (&node, transport),
    )?;
    super::fault::check(directory, super::fault::Stage::Prepared)?;
    let archived = &opened.identity;
    let owner = account.locator().owner;
    let mut revocations = archived.revocations.clone();
    if let Some(archived_sibling) = archived.credentials.certificate() {
        revocations.link(archived_sibling.clone())?;
    }
    let source = Source {
        genesis: archived.manifest()?,
        record: crate::tip_schemas::Record::new(
            &archived.schema,
            archived.generation,
            archived.engine,
        ),
        bootstrap: None,
        ttl: archived.ttl,
        grant: archived.owner_grant(owner)?.clone(),
        revocations,
        groups: archived.groups.values().cloned().collect(),
        epoch_statements: archived.epoch_statements.clone(),
        escrow,
        naming: archived.naming_escrow.clone(),
        freshness: Freshness::Unwitnessed,
    };
    let (mut settings, coverage) = stage(secrets, account, &mut installing, source)?;
    settings.pending = archived.pending.clone();
    settings.pending_follow = archived.pending_follow.clone();
    let context = settings.personal()?.ok_or("a personal backup")?;
    let mut known = settings.revocations.clone();
    known.admit_personal(
        settings
            .credentials
            .certificate()
            .ok_or("missing node certificate")?
            .clone(),
        &context,
    )?;
    settings.learn_revocations(&known)?;
    settings.learn_contacts(archived, &[node.public_key()]);
    let vault = settings.vault(secrets);
    crate::recovery::mark(
        directory,
        &crate::backup::Marker {
            predecessor: opened.inventory.node,
            archive: opened.id.clone(),
            recovery: None,
        },
    )?;
    crate::backup::place(&opened, directory)?;
    super::fault::check(directory, super::fault::Stage::Placed)?;
    installing.installed(&vault, &coverage)?;
    settings.save(path)?;
    rocky_platform::fs::remove_dir_all(&staging)?;
    Ok((settings, coverage))
}

#[cfg(test)]
mod tests;
