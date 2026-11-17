use crate::{
    Failure, Freshness, Opened,
    domain::Domain,
    enrollment::{Settings, key_role},
    secrets::{SecretStore, Vault, write_private},
};
use rocky_core::{Commitment, KeyEpoch};
use rocky_credentials::{
    DatabaseKind, PeerId, PersonalAccount, PersonalError, PersonalNamespace, PrivateKey, PublicKey,
    SignedCapability,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};

mod backup;
pub(crate) mod bootstrap;
pub(crate) mod devices;
pub(crate) mod sign_in;
pub use backup::VerifiedBackup;
pub use sign_in::SignInSource;
pub(crate) use sign_in::{SignIn, prepare_sign_in, tidy};

pub(crate) const DISCOVERY_ROLE: &str = "personal-discovery";
const INTENT_VERSION: u32 = 4;
const INTENT_FILE: &str = "personal.intent";
const MAX_INTENT: usize = 64 * 1024;

pub struct PersonalOpened {
    pub opened: Opened,
    pub restored: RestoreCoverage,
    pub freshness: Freshness,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RestoreCoverage {
    pub readers: BTreeMap<String, Vec<KeyEpoch>>,
    pub partitions: BTreeMap<String, Vec<KeyEpoch>>,
    pub missing: Vec<(String, KeyEpoch)>,
    pub unkeyed: Vec<String>,
    pub naming: bool,
}
impl RestoreCoverage {
    pub(crate) fn held(settings: &Settings, vault: &Vault) -> Result<Self, Failure> {
        Ok(Self {
            readers: settings.keys.clone(),
            partitions: settings
                .groups
                .iter()
                .map(|(address, group)| (address.clone(), group.epochs.clone()))
                .collect(),
            missing: Vec::new(),
            unkeyed: Vec::new(),
            naming: vault.find(crate::groups::NAMING_ROLE)?.is_some(),
        })
    }
    pub fn complete(&self) -> bool {
        self.missing.is_empty() && self.unkeyed.is_empty() && self.naming
    }
}

#[derive(Debug)]
pub enum PersonalSignInError {
    PrfUnsupported,
    WrongLocalAccount,
    ExistingState,
    SourceUnavailable,
    UnsupportedFormat,
    GenesisConflict,
    MissingEscrow,
    Capacity { limit: usize },
    RevokedNode { node: PeerId },
    Failed(Failure),
}
impl PersonalSignInError {
    pub fn code(&self) -> String {
        match self {
            Self::PrfUnsupported => "prf_unsupported".into(),
            Self::WrongLocalAccount => "wrong_local_account".into(),
            Self::ExistingState => "existing_state".into(),
            Self::SourceUnavailable => "source_unavailable".into(),
            Self::UnsupportedFormat => "unsupported_format".into(),
            Self::GenesisConflict => "genesis_conflict".into(),
            Self::MissingEscrow => "missing_escrow".into(),
            Self::Capacity { .. } => "device_limit".into(),
            Self::RevokedNode { .. } => "device_revoked".into(),
            Self::Failed(error) => crate::CallFailure::from_failure(error).code().to_owned(),
        }
    }
    pub(crate) fn from_failure(error: Failure) -> Self {
        let error = match error.downcast::<Self>() {
            Ok(refusal) => return *refusal,
            Err(error) => error,
        };
        match error.downcast_ref::<rocky_credentials::Error>() {
            Some(rocky_credentials::Error::Personal(PersonalError::Tombstoned { node })) => {
                Self::RevokedNode { node: *node }
            }
            Some(rocky_credentials::Error::Personal(PersonalError::DeviceLimit { limit })) => {
                Self::Capacity { limit: *limit }
            }
            _ => Self::Failed(error),
        }
    }
}
impl std::fmt::Display for PersonalSignInError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PrfUnsupported => {
                f.write_str("the passkey's PRF output is unavailable for this namespace")
            }
            Self::WrongLocalAccount => {
                f.write_str("this directory holds another account's personal database")
            }
            Self::ExistingState => {
                f.write_str("this directory already holds local state: open it instead")
            }
            Self::SourceUnavailable => f.write_str("no source of the database is reachable"),
            Self::UnsupportedFormat => f.write_str("unsupported local state format"),
            Self::GenesisConflict => {
                f.write_str("the database's genesis differs from the one pinned here")
            }
            Self::MissingEscrow => f.write_str("a required escrowed key is missing"),
            Self::Capacity { limit } => {
                write!(f, "the owner already has {limit} devices in this database")
            }
            Self::RevokedNode { node } => write!(f, "device {} was revoked", node.to_hex()),
            Self::Failed(error) => write!(f, "{error}"),
        }
    }
}
impl std::error::Error for PersonalSignInError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Failed(error) => Some(&**error),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct PublicationId([u8; 16]);
impl PublicationId {
    pub(crate) fn bytes(self) -> [u8; 16] {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Device {
    pub(crate) publication: PublicationId,
    pub(crate) unwitnessed: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Gaps {
    missing: Vec<(String, KeyEpoch)>,
    unkeyed: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Stage {
    Prepared,
    Pinned {
        descriptor: Commitment,
        epochs: BTreeMap<String, Vec<KeyEpoch>>,
    },
    Certified {
        descriptor: Commitment,
        epochs: BTreeMap<String, Vec<KeyEpoch>>,
        certificate: Box<SignedCapability>,
    },
}
impl Stage {
    fn pinned(&self) -> Option<(Commitment, &BTreeMap<String, Vec<KeyEpoch>>)> {
        match self {
            Self::Prepared => None,
            Self::Pinned { descriptor, epochs }
            | Self::Certified {
                descriptor, epochs, ..
            } => Some((*descriptor, epochs)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Origin {
    Create { creation: [u8; 16] },
    SignIn,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    version: u32,
    origin: Origin,
    installation: String,
    publisher: PublicKey,
    owner: PublicKey,
    namespace: PersonalNamespace,
    node: PublicKey,
    publication: PublicationId,
    stage: Stage,
    secrets: BTreeSet<(Commitment, KeyEpoch)>,
    gaps: Option<Gaps>,
}
impl Intent {
    fn path(directory: &Path) -> PathBuf {
        directory.join(INTENT_FILE)
    }
    fn load(directory: &Path) -> Result<Option<Self>, PersonalSignInError> {
        let path = Self::path(directory);
        if !rocky_platform::fs::exists(&path) {
            return Ok(None);
        }
        let bytes = rocky_platform::fs::read(&path)
            .map_err(|error| PersonalSignInError::Failed(error.into()))?;
        let intent: Self = rocky_credentials::decode_bounded(&bytes, MAX_INTENT)
            .map_err(|_| PersonalSignInError::UnsupportedFormat)?;
        let epochs_valid = intent.stage.pinned().is_none_or(|(_, epochs)| {
            epochs.values().all(|epochs| {
                !epochs.is_empty()
                    && epochs.iter().all(|epoch| epoch.valid())
                    && epochs.windows(2).all(|pair| pair[0] < pair[1])
            })
        });
        if intent.version != INTENT_VERSION || !epochs_valid {
            return Err(PersonalSignInError::UnsupportedFormat);
        }
        Ok(Some(intent))
    }
    fn save(&self, directory: &Path) -> Result<(), Failure> {
        write_private(&Self::path(directory), &rocky_credentials::encode(self)?)
    }
    fn finished(
        &self,
        directory: &Path,
        account: (PublicKey, PublicKey, &PersonalNamespace),
        secrets: &Arc<dyn SecretStore>,
    ) -> Result<Option<Finished>, PersonalSignInError> {
        let identity = directory.join("identity.json");
        if self.origin != Origin::SignIn
            || !self.names(account)
            || !rocky_platform::fs::exists(&identity)
        {
            return Ok(None);
        }
        let settings = load_settings(&identity)?;
        let vault = settings.vault(secrets);
        let node = Settings::node_key(&vault)
            .map(|key| key.public_key())
            .map_err(PersonalSignInError::Failed)?;
        let Some(device) = settings
            .personal
            .filter(|_| settings.installation == self.installation && node == self.node)
        else {
            return Ok(None);
        };
        let gaps = self.gaps.clone().unwrap_or_default();
        let mut restored =
            RestoreCoverage::held(&settings, &vault).map_err(PersonalSignInError::Failed)?;
        restored.missing = gaps.missing;
        restored.unkeyed = gaps.unkeyed;
        Ok(Some(Finished {
            restored,
            freshness: if device.unwitnessed {
                Freshness::Unwitnessed
            } else {
                Freshness::LocallyAnchored
            },
        }))
    }
    fn names(&self, account: (PublicKey, PublicKey, &PersonalNamespace)) -> bool {
        (self.publisher, self.owner, &self.namespace) == account
    }
    fn continues(
        &self,
        account: (PublicKey, PublicKey, &PersonalNamespace),
        create: bool,
    ) -> Result<(), PersonalSignInError> {
        if !self.names(account) {
            return Err(PersonalSignInError::WrongLocalAccount);
        }
        if matches!(self.origin, Origin::Create { .. }) != create {
            return Err(PersonalSignInError::ExistingState);
        }
        Ok(())
    }
}

pub(crate) fn check_installed(
    directory: &Path,
    account: &PersonalAccount,
) -> Result<(), PersonalSignInError> {
    let locator = account.locator();
    let names = (locator.publisher, locator.owner, account.namespace());
    let identity = directory.join("identity.json");
    if !rocky_platform::fs::exists(&identity) {
        return Ok(());
    }
    let settings = load_settings(&identity)?;
    let theirs = matches!(
        settings.kind().map_err(PersonalSignInError::Failed)?,
        DatabaseKind::Personal { owner, namespace, .. }
            if (settings.publisher, owner, &namespace) == names
    );
    Err(if theirs {
        PersonalSignInError::ExistingState
    } else {
        PersonalSignInError::WrongLocalAccount
    })
}

pub(crate) fn check_empty(
    directory: &Path,
    account: &PersonalAccount,
) -> Result<(), PersonalSignInError> {
    let locator = account.locator();
    let names = (locator.publisher, locator.owner, account.namespace());
    check_installed(directory, account)?;
    match Intent::load(directory)? {
        Some(intent) => intent.continues(names, true),
        None => {
            let unfinished = [
                "rocky.sqlite",
                "identity.join.json",
                "identity.install.json",
                crate::backup::MARKER,
            ];
            if unfinished
                .iter()
                .any(|name| rocky_platform::fs::exists(directory.join(name)))
            {
                return Err(PersonalSignInError::ExistingState);
            }
            Ok(())
        }
    }
}

fn load_settings(path: &Path) -> Result<Settings, PersonalSignInError> {
    let bytes = rocky_platform::fs::read(path)
        .map_err(|error| PersonalSignInError::Failed(error.into()))?;
    let version = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|value| value["version"].as_u64());
    if version != Some(u64::from(crate::enrollment::VERSION)) {
        return Err(PersonalSignInError::UnsupportedFormat);
    }
    Settings::load(path).map_err(PersonalSignInError::Failed)
}

pub(crate) struct Create {
    pub(crate) namespace: PersonalNamespace,
    pub(crate) discovery: std::sync::Mutex<Option<rocky_credentials::DiscoveryCapability>>,
    pub(crate) coverage: Arc<std::sync::Mutex<Option<RestoreCoverage>>>,
}

pub(crate) struct Installing {
    directory: PathBuf,
    intent: Intent,
    resumed: bool,
    discovery: rocky_credentials::DiscoveryCapability,
}
impl Installing {
    pub(crate) fn resume(
        directory: &Path,
        store: &Arc<dyn SecretStore>,
        root: &dyn rocky_credentials::IdentityProvider,
        (namespace, discovery): (&PersonalNamespace, rocky_credentials::DiscoveryCapability),
    ) -> Result<Self, Failure> {
        let publisher = root.public_key()?;
        let owner = root
            .owner_identity()
            .ok_or("a personal root derives its owner identity")?
            .public_key()?;
        let saved = Intent::load(directory)?;
        let resumed = saved.is_some();
        let intent = match saved {
            Some(intent) => {
                intent.continues((publisher, owner, namespace), true)?;
                let vault = Vault::new(store.clone(), &intent.installation);
                vault.get("transport")?;
                if PrivateKey::from_bytes(&vault.get("node")?).public_key() != intent.node {
                    return Err("the install intent names another node key".into());
                }
                intent
            }
            None => {
                let installation =
                    data_encoding::HEXLOWER.encode(&crate::enrollment::random::<16>());
                let vault = Vault::new(store.clone(), &installation);
                let node = PrivateKey::from_bytes(&vault.create("node")?).public_key();
                vault.create("transport")?;
                let intent = Intent {
                    version: INTENT_VERSION,
                    origin: Origin::Create {
                        creation: crate::enrollment::random(),
                    },
                    installation,
                    publisher,
                    owner,
                    namespace: namespace.clone(),
                    node,
                    publication: PublicationId(crate::enrollment::random()),
                    stage: Stage::Prepared,
                    secrets: BTreeSet::new(),
                    gaps: None,
                };
                intent.save(directory)?;
                intent
            }
        };
        Ok(Self {
            directory: directory.to_path_buf(),
            intent,
            resumed,
            discovery,
        })
    }
    pub(crate) fn resumed(&self) -> bool {
        self.resumed
    }
    pub(crate) fn creation(&self) -> Result<[u8; 16], Failure> {
        match self.intent.origin {
            Origin::Create { creation } => Ok(creation),
            Origin::SignIn => Err(PersonalSignInError::ExistingState.into()),
        }
    }
    pub(crate) fn installation(&self) -> &str {
        &self.intent.installation
    }
    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }
    pub(crate) fn device(&self, freshness: Freshness) -> Device {
        Device {
            publication: self.intent.publication,
            unwitnessed: freshness == Freshness::Unwitnessed,
        }
    }
    pub(crate) fn naming(&self, vault: &Vault) -> Result<(), Failure> {
        if vault.find(crate::groups::NAMING_ROLE)?.is_none() {
            vault.create(crate::groups::NAMING_ROLE)?;
        }
        Ok(())
    }
    pub(crate) fn pin(
        &mut self,
        vault: &Vault,
        descriptor: Commitment,
        fresh: impl FnOnce() -> Result<BTreeMap<String, Vec<KeyEpoch>>, Failure>,
    ) -> Result<BTreeMap<String, Vec<KeyEpoch>>, Failure> {
        if let Some((pinned, epochs)) = self.intent.stage.pinned() {
            if pinned != descriptor {
                return Err(PersonalSignInError::GenesisConflict.into());
            }
            for (name, epochs) in epochs {
                for epoch in epochs {
                    let key = vault.get(&key_role(name, *epoch))?;
                    if KeyEpoch::new(epoch.generation, &key) != *epoch {
                        return Err(format!("the pinned key of {name} differs").into());
                    }
                }
            }
            return Ok(epochs.clone());
        }
        let epochs = fresh()?;
        self.intent.stage = Stage::Pinned {
            descriptor,
            epochs: epochs.clone(),
        };
        self.intent.save(&self.directory)?;
        Ok(epochs)
    }
    pub(crate) fn sign_in(
        directory: &Path,
        store: &Arc<dyn SecretStore>,
        (account, discovery): (&PersonalAccount, rocky_credentials::DiscoveryCapability),
        (node, transport): (&PrivateKey, [u8; 32]),
    ) -> Result<Self, Failure> {
        let locator = account.locator();
        let names = (locator.publisher, locator.owner, account.namespace());
        let saved = Intent::load(directory)?;
        let resumed = saved.is_some();
        let intent = match saved {
            Some(intent) => {
                intent.continues(names, false)?;
                intent
            }
            None => {
                let installation =
                    data_encoding::HEXLOWER.encode(&crate::enrollment::random::<16>());
                let vault = Vault::new(store.clone(), &installation);
                vault.put("node", node.as_bytes())?;
                vault.put("transport", &transport)?;
                let intent = Intent {
                    version: INTENT_VERSION,
                    origin: Origin::SignIn,
                    installation,
                    publisher: locator.publisher,
                    owner: locator.owner,
                    namespace: account.namespace().clone(),
                    node: node.public_key(),
                    publication: PublicationId(crate::enrollment::random()),
                    stage: Stage::Prepared,
                    secrets: BTreeSet::new(),
                    gaps: None,
                };
                intent.save(directory)?;
                intent
            }
        };
        let vault = Vault::new(store.clone(), &intent.installation);
        if PrivateKey::from_bytes(&vault.get("node")?).public_key() != intent.node {
            return Err("the install intent names another node key".into());
        }
        Ok(Self {
            directory: directory.to_path_buf(),
            intent,
            resumed,
            discovery,
        })
    }
    pub(crate) fn saved_keys(
        directory: &Path,
        store: &Arc<dyn SecretStore>,
        account: &PersonalAccount,
    ) -> Result<Option<(PrivateKey, [u8; 32])>, Failure> {
        let locator = account.locator();
        match Intent::load(directory)? {
            Some(intent) => {
                intent.continues(
                    (locator.publisher, locator.owner, account.namespace()),
                    false,
                )?;
                let vault = Vault::new(store.clone(), &intent.installation);
                Ok(Some((
                    PrivateKey::from_bytes(&vault.get("node")?),
                    vault.get("transport")?,
                )))
            }
            None => Ok(None),
        }
    }
    pub(crate) fn publication(&self) -> PublicationId {
        self.intent.publication
    }
    pub(crate) fn pinned(&self) -> Option<Commitment> {
        self.intent.stage.pinned().map(|(descriptor, _)| descriptor)
    }
    pub(crate) fn certificate(&self) -> Option<&SignedCapability> {
        match &self.intent.stage {
            Stage::Certified { certificate, .. } => Some(certificate),
            _ => None,
        }
    }
    pub(crate) fn import(
        &mut self,
        descriptor: Commitment,
        mut epochs: BTreeMap<String, Vec<KeyEpoch>>,
    ) -> Result<(), Failure> {
        if let Some((pinned, earlier)) = self.intent.stage.pinned() {
            if pinned != descriptor {
                return Err(PersonalSignInError::GenesisConflict.into());
            }
            for (name, held) in earlier {
                let merged = epochs.entry(name.clone()).or_default();
                merged.extend(held);
                merged.sort_unstable();
                merged.dedup();
            }
        }
        self.intent.stage = match std::mem::replace(&mut self.intent.stage, Stage::Prepared) {
            Stage::Certified { certificate, .. } => Stage::Certified {
                descriptor,
                epochs,
                certificate,
            },
            _ => Stage::Pinned { descriptor, epochs },
        };
        self.intent.save(&self.directory)
    }
    pub(crate) fn hold_secrets(
        &mut self,
        secrets: impl IntoIterator<Item = (Commitment, KeyEpoch)>,
    ) -> Result<(), Failure> {
        let before = self.intent.secrets.len();
        self.intent.secrets.extend(secrets);
        if self.intent.secrets.len() == before {
            return Ok(());
        }
        self.intent.save(&self.directory)
    }
    pub(crate) fn certify(&mut self, certificate: SignedCapability) -> Result<(), Failure> {
        let (descriptor, epochs) = self
            .intent
            .stage
            .pinned()
            .map(|(descriptor, epochs)| (descriptor, epochs.clone()))
            .ok_or("a certificate needs a pinned genesis")?;
        self.intent.stage = Stage::Certified {
            descriptor,
            epochs,
            certificate: Box::new(certificate),
        };
        self.intent.save(&self.directory)
    }
    pub(crate) fn installed(
        mut self,
        vault: &Vault,
        coverage: &RestoreCoverage,
    ) -> Result<(), Failure> {
        vault.put(DISCOVERY_ROLE, self.discovery.topic())?;
        self.intent.gaps = Some(Gaps {
            missing: coverage.missing.clone(),
            unkeyed: coverage.unkeyed.clone(),
        });
        self.intent.save(&self.directory)
    }
    pub(crate) fn keep_discovery(self, vault: &Vault) -> Result<(), Failure> {
        vault.put(DISCOVERY_ROLE, self.discovery.topic())?;
        Ok(())
    }
}

pub(crate) fn fresh_readers(
    vault: &Vault,
    domain: &Domain,
) -> Result<BTreeMap<String, Vec<KeyEpoch>>, Failure> {
    let mut keys = BTreeMap::new();
    for (name, _) in domain.scopes() {
        let key = crate::enrollment::random::<32>();
        let epoch = KeyEpoch::new(1, &key);
        vault.put(&key_role(name, epoch), &key)?;
        keys.insert(name.to_owned(), vec![epoch]);
    }
    Ok(keys)
}

pub(crate) fn discard(directory: &Path, store: &Arc<dyn SecretStore>) -> Result<(), Failure> {
    let Some(intent) = Intent::load(directory)? else {
        return Ok(());
    };
    let vault = Vault::new(store.clone(), &intent.installation);
    let mut roles = vec![
        "node".to_owned(),
        "transport".to_owned(),
        DISCOVERY_ROLE.to_owned(),
        crate::groups::NAMING_ROLE.to_owned(),
    ];
    if let Some((_, epochs)) = intent.stage.pinned() {
        for (name, epochs) in epochs {
            roles.extend(epochs.iter().map(|epoch| key_role(name, *epoch)));
        }
    }
    roles.extend(
        intent
            .secrets
            .iter()
            .map(|(address, epoch)| crate::groups::secret_role(address, *epoch)),
    );
    vault.forget(roles)?;
    finish(directory)
}

pub(crate) fn finish(directory: &Path) -> Result<(), Failure> {
    if !rocky_platform::fs::exists(Intent::path(directory)) {
        return Ok(());
    }
    let staging = directory.join(sign_in::STAGING);
    if rocky_platform::fs::exists(&staging) {
        rocky_platform::fs::remove_dir_all(&staging)?;
    }
    match rocky_platform::fs::remove_file(Intent::path(directory)) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

pub(crate) struct Finished {
    pub(crate) restored: RestoreCoverage,
    pub(crate) freshness: Freshness,
}

pub(crate) fn unfinished(directory: &Path) -> bool {
    rocky_platform::fs::exists(Intent::path(directory))
        && !rocky_platform::fs::exists(directory.join("identity.json"))
}

pub(crate) mod fault {
    use crate::Failure;
    use std::path::Path;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Stage {
        Prepared,
        Pinned,
        Importing,
        Certified,
        Published,
        Placed,
        Saved,
    }
    #[derive(Debug)]
    pub(crate) struct Crashed(pub(crate) Stage);
    impl std::fmt::Display for Crashed {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "injected crash at {:?}", self.0)
        }
    }
    impl std::error::Error for Crashed {}

    #[cfg(test)]
    pub(crate) static CRASHES: std::sync::Mutex<Vec<(std::path::PathBuf, Stage)>> =
        std::sync::Mutex::new(Vec::new());
    #[cfg(test)]
    pub(crate) fn check(directory: &Path, stage: Stage) -> Result<(), Failure> {
        let mut crashes = CRASHES.lock().map_err(|_| "poisoned fault")?;
        match crashes
            .iter()
            .position(|(at, crash)| at == directory && *crash == stage)
        {
            Some(index) => {
                crashes.remove(index);
                Err(Box::new(Crashed(stage)))
            }
            None => Ok(()),
        }
    }
    #[cfg(not(test))]
    pub(crate) fn check(_: &Path, _: Stage) -> Result<(), Failure> {
        Ok(())
    }
    pub(crate) fn crashed(error: &Failure) -> bool {
        error.is::<Crashed>()
    }
}

#[cfg(test)]
mod tests;
