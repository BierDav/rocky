//! Non-destructive personal sign-in: a device holding only the passkey
//! finds the database through its discovery topic, or takes a verified
//! backup, and installs itself as one more owner device. It never revokes,
//! rotates or grants anything: the root opens the escrow the source holds,
//! the owner certifies the new node, and a gap stays a reported gap.
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

/// Hints one attempt asks, and bootstrap pages it accepts from one source.
const MAX_CANDIDATES: usize = 8;
const MAX_PAGES: u32 = 4096;

/// Where a personal sign-in finds the database.
pub enum SignInSource {
    /// Any owner device or custodian announcing under the passkey's
    /// discovery topic on the configured network.
    Discover,
    /// An offline backup archive: the same non-destructive install.
    Backup(VerifiedBackup),
}

/// A sign-in as the database thread receives it.
pub(crate) struct SignIn {
    /// Taken once.
    pub(crate) account: Mutex<Option<PersonalAccount>>,
    pub(crate) source: Mutex<Option<SignInSource>>,
    /// How long discovery waits for a first source.
    pub(crate) wait: rocky_platform::time::Duration,
    /// What the device restored and how far it is witnessed, for the caller.
    pub(crate) outcome: Arc<Mutex<Option<(RestoreCoverage, Freshness)>>>,
}

/// Readies `directory` for a sign-in of `account`, refusing other state
/// untouched: only this account's unfinished sign-in continues, and what
/// it left besides its intent and keys goes. An installed database, which
/// may have been handed out, is never removed: it opens instead.
pub(crate) fn prepare_sign_in(
    directory: &Path,
    account: &PersonalAccount,
) -> Result<(), PersonalSignInError> {
    let Some(saved) = super::Intent::load(directory)? else {
        return super::check_empty(directory, account);
    };
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
        if rocky_platform::fs::exists(directory.join("restore")) {
            rocky_platform::fs::remove_dir_all(directory.join("restore"))?;
        }
        Ok(())
    })();
    removed.map_err(PersonalSignInError::Failed)
}

/// What a sign-in that left no install intent wrote goes: entries of
/// `directory` absent from `before`, and the directory if it was new.
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

/// The database `account` locates for `schema`'s application.
fn database(account: &PersonalAccount, schema: &crate::Schema) -> DatabaseContext {
    let locator = account.locator();
    DatabaseContext {
        publisher: locator.publisher,
        scope: locator.scope(&schema.application),
    }
}

/// Whether a source refused for the owner's device limit. Its code is an
/// unverified claim: it ends only that source's attempt.
fn at_capacity(error: &Failure) -> bool {
    let limit = rocky_credentials::PersonalError::DeviceLimit {
        limit: rocky_credentials::MAX_PERSONAL_DEVICES,
    };
    error
        .downcast_ref::<PeerError>()
        .is_some_and(|error| error.code == limit.code())
}

/// A source that answered a hello with this account's genesis.
struct Candidate {
    address: String,
    challenge: PersonalChallenge,
    genesis: SignedManifest,
}

/// Installs a sign-in: from the discovered sources over `network`, or from
/// a verified backup. Returns the installed settings and the started link.
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
                            let key = Settings::transport_key(&settings.vault(secrets))?;
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
            let (node, transport_key) = match Installing::saved_keys(directory, secrets, &account)?
            {
                Some(keys) => keys,
                None => (PrivateKey::new(), rocky_platform::entropy::random()),
            };
            let (transport, owned) = network.start(true, transport_key).await?;
            let installed = discovered(
                (path, directory),
                secrets,
                schema,
                &account,
                (&node, transport_key),
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
            // A revoked node never signs in again: the next attempt is a
            // fresh node, as any passkey holder's may be. Only a signed
            // statement this device verified reports it.
            if matches!(
                error.downcast_ref::<PersonalSignInError>(),
                Some(PersonalSignInError::RevokedNode { .. })
            ) {
                super::discard(directory, secrets)?;
            }
            return Err(error);
        }
    };
    *sign_in.outcome.lock().map_err(|_| "outcome poisoned")? = Some(outcome);
    Ok((settings, link))
}

/// Finds this account's sources under its discovery topic: each answered
/// a hello with the genesis the passkey's root signed for this namespace.
/// Sources whose authenticated genesis differs are a conflict.
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
        // Hints already found are asked too, so a conflict is never decided
        // by which answered first.
        while hints.len() < MAX_CANDIDATES
            && let Some(Some(found)) = discovery.found().now_or_never()
        {
            hints.push(found);
        }
    }
    let _ = discovery.announce(own, &[]).await;
    let mut candidates: Vec<Candidate> = Vec::new();
    for found in hints {
        if found.node == own
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
        // A responder proving another account's or database's genesis is
        // no source of this one.
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

/// Signs one act for `candidate` with the account's root and owner and the
/// node key.
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

/// Every bootstrap page `candidate` holds.
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

/// The discovered install: find sources, fetch and verify one's bootstrap,
/// install from it and publish the node's certificate to it.
async fn discovered(
    (path, directory): (&Path, &Path),
    secrets: &Arc<dyn SecretStore>,
    schema: &crate::Schema,
    account: &PersonalAccount,
    (node, transport_key): (&PrivateKey, [u8; 32]),
    (transport, wait): (&Connected, rocky_platform::time::Duration),
) -> Result<(Settings, RestoreCoverage, Freshness), Failure> {
    let database = database(account, schema);
    let pinned = super::Intent::load(directory)?
        .and_then(|intent| intent.stage.pinned().map(|(descriptor, _)| descriptor));
    let candidates = discover((transport, wait), node, account, database, pinned).await?;
    if candidates.is_empty() {
        return Err(PersonalSignInError::SourceUnavailable.into());
    }
    // Only a source that answered makes this an install: its keys are kept.
    let mut installing = Installing::sign_in(
        directory,
        secrets,
        (
            account,
            rocky_credentials::DiscoveryCapability::from_secret(*account.discovery().topic()),
        ),
        (node, transport_key),
    )?;
    // A source's refusal or forged answer ends only its own attempt: only
    // signed statements this device verifies end the install.
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
        let source = Source {
            genesis: candidate.genesis.clone(),
            record: metadata.record,
            bootstrap: Some(metadata.bootstrap),
            ttl: metadata.ttl,
            grant: metadata.grant,
            revocations: metadata.revocations,
            groups: metadata.groups,
            epoch_statements: metadata.epoch_statements,
            escrow: (escrow, held),
        };
        let (mut settings, coverage) = stage(secrets, account, &mut installing, source)?;
        let vault = settings.vault(secrets);
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
        // The first sync brings the ciphertext; checkpoints first.
        rocky_credentials::write_private(&directory.join("bootstrap.json"), b"true")?;
        settings.save(path)?;
        installing.keep_discovery(&vault)?;
        return Ok((settings, coverage, freshness));
    }
    // A source answered: the intent stays, and a retry resumes it.
    if capacity {
        return Err(PersonalSignInError::Capacity {
            limit: rocky_credentials::MAX_PERSONAL_DEVICES,
        }
        .into());
    }
    Err(last.unwrap_or_else(|| PersonalSignInError::SourceUnavailable.into()))
}

/// Publishes the node's certificate to `candidate`, which keeps and relays
/// it; returns the revocations it holds since.
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

/// How far a bootstrap from `candidate` is witnessed: another owner device
/// no statement it carries ended anchors it; a custodian alone does not.
/// The responder must belong to the database the passkey's root signed.
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
    // The responder holds a chain of this database, checked before anything
    // is kept: a forged one is no source.
    let record = &metadata.record;
    let domain = crate::domain::Domain::at(
        &record.schema,
        *locator.publisher.as_bytes(),
        locator.mesh,
        locator.domain,
        record.generation,
        record.engine,
    )?;
    metadata.responder.provenance_with_families(
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

/// Whether `leaf` certifies an owner device no statement `known` holds
/// ended: only such a device's durable store anchors what it serves.
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

/// What one source hands a sign-in.
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
}

/// Verifies `source` against the account, opens its escrow with the root,
/// keeps the keys and pins them, and certifies the node: the settings of
/// one more owner device, not yet saved. Nothing is revoked or rotated;
/// an epoch no record opens is reported, never replaced.
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
    // The genesis and followed graph authenticate the schema record.
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
    let (recovered, missing) = Recovered::import(&domain, (records, held), account.root())?;
    if holds && recovered.readers.is_empty() && recovered.secrets.is_empty() {
        return Err(PersonalSignInError::MissingEscrow.into());
    }
    for ((name, epoch), key) in &recovered.readers {
        settings.hold(&vault, name, *epoch, key)?;
    }
    installing.hold_secrets(recovered.secrets.keys().copied())?;
    for ((address, epoch), secret) in &recovered.secrets {
        vault.put(&secret_role(address, *epoch), secret)?;
    }
    if let Some(naming) = recovered.naming {
        vault.put(NAMING_ROLE, &naming)?;
    }
    // Partitions the source names without a key stay known, keyless.
    let mut groups: BTreeMap<String, GroupRecord> = source
        .groups
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
    for (key, record) in recovered.groups {
        match groups.get_mut(&key) {
            Some(named) if named.name != record.name => {
                return Err("an escrowed partition differs from the one the source names".into());
            }
            Some(named) => named.epochs = record.epochs,
            None => drop(groups.insert(key, record)),
        }
    }
    settings.groups = groups;
    settings.epoch_statements = source.epoch_statements;
    settings.authority = Some(crate::enrollment::Issuing::default());
    settings.personal = Some(installing.device());
    installing.import(descriptor, settings.keys.clone())?;
    // A retry publishes the certificate it kept, never another.
    if let Some(certificate) = installing.certificate() {
        let mut known = settings.revocations.clone();
        known.link(certificate.clone())?;
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
    if installing.certificate().is_none() {
        installing.certify(certificate)?;
    }
    let mut coverage = RestoreCoverage::held(&settings, &vault)?;
    let labels: BTreeMap<_, _> = domain
        .scopes()
        .map(|(name, scope)| (scope, name.to_owned()))
        .collect();
    coverage.missing = missing
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
    coverage.unkeyed = domain
        .scopes()
        .filter(|(name, scope)| {
            settings.reads_whole(&domain, scope) && !settings.keys.contains_key(*name)
        })
        .map(|(name, _)| name.to_owned())
        .collect();
    Ok((settings, coverage))
}

/// Installs from a verified backup without any network: the archived
/// store, rebound to this node at open, under the archived owner grant.
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
    let staging = directory.join("restore");
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
    let archived = &opened.identity;
    let owner = account.locator().owner;
    let mut revocations = archived.revocations.clone();
    // The archived device stays a trusted sibling.
    if let Some(certificate) = archived.credentials.certificate() {
        revocations.link(certificate.clone())?;
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
    };
    let (mut settings, coverage) = stage(secrets, account, &mut installing, source)?;
    settings.pending = archived.pending.clone();
    settings.pending_follow = archived.pending_follow.clone();
    // The device admits itself as the sources it meets later will.
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
    // The marker first: an identity without it would open as if installed.
    // The identity last: until it exists nothing opens the placed store,
    // and a retry removes it.
    crate::recovery::mark(
        directory,
        &crate::backup::Marker {
            predecessor: opened.inventory.node,
            archive: opened.id.clone(),
            recovery: None,
        },
    )?;
    crate::backup::place(&opened, directory)?;
    settings.save(path)?;
    rocky_platform::fs::remove_dir_all(&staging)?;
    installing.keep_discovery(&vault)?;
    Ok((settings, coverage))
}

#[cfg(test)]
mod tests;
