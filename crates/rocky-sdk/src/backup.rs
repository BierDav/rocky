use rocky_credentials::PublicKey;
use serde::{Deserialize, Serialize};

pub(crate) const MARKER: &str = "restore.json";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub name: String,
    pub length: u64,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    pub format: u32,
    pub created: u64,
    pub publisher: PublicKey,
    pub scope: rocky_core::Scope,
    pub manifest: String,
    pub node: PublicKey,
    pub revision: u64,
    pub files: Vec<Entry>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Marker {
    pub predecessor: PublicKey,
    pub archive: String,
    pub recovery: Option<Recovery>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Recovery {
    Placing,
    Placed,
    Keyed,
}

pub(crate) use archive::{Unsupported, inspect, open, place, write};
mod archive {
    use super::*;
    use crate::{Failure, enrollment::Settings};
    use data_encoding::HEXLOWER;
    use rocky_credentials::{PrivateKey, Signature};
    use sha2::{Digest, Sha256};
    use std::{
        io::{Read, Write},
        path::{Path, PathBuf},
    };

    fn reader(path: &Path) -> Result<impl Read, Failure> {
        #[cfg(not(target_family = "wasm"))]
        return Ok(std::io::BufReader::new(std::fs::File::open(path)?));
        #[cfg(target_family = "wasm")]
        return Ok(std::io::Cursor::new(rocky_platform::fs::read(path)?));
    }
    struct Sink {
        #[cfg(not(target_family = "wasm"))]
        out: std::io::BufWriter<std::fs::File>,
        #[cfg(target_family = "wasm")]
        out: (PathBuf, Vec<u8>),
    }
    impl Sink {
        fn create(path: &Path) -> Result<Self, Failure> {
            #[cfg(not(target_family = "wasm"))]
            return Ok(Self {
                out: std::io::BufWriter::new(std::fs::File::create(path)?),
            });
            #[cfg(target_family = "wasm")]
            return Ok(Self {
                out: (path.to_path_buf(), Vec::new()),
            });
        }
        fn finish(self, database: bool) -> Result<(), Failure> {
            #[cfg(not(target_family = "wasm"))]
            {
                let _ = database;
                self.out
                    .into_inner()
                    .map_err(|error| error.into_error())?
                    .sync_all()?;
            }
            #[cfg(target_family = "wasm")]
            {
                let (path, bytes) = self.out;
                if database {
                    rocky_platform::fs::write_database(path, &bytes)?;
                } else {
                    rocky_platform::fs::write(path, bytes)?;
                }
            }
            Ok(())
        }
    }
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            #[cfg(not(target_family = "wasm"))]
            return self.out.write(bytes);
            #[cfg(target_family = "wasm")]
            return self.out.1.write(bytes);
        }
        fn flush(&mut self) -> std::io::Result<()> {
            #[cfg(not(target_family = "wasm"))]
            return self.out.flush();
            #[cfg(target_family = "wasm")]
            return Ok(());
        }
    }

    const MAGIC: &[u8] = b"rocky.backup.v4\n";
    const FORMAT: u32 = 4;
    const MAX_HEADER: usize = 256 * 1024;
    const DATABASE: &str = "rocky.sqlite";
    const IDENTITY: &str = "identity.json";

    fn label(inventory: &[u8]) -> Vec<u8> {
        [b"rocky.backup.v4".as_slice(), inventory].concat()
    }
    fn digest(path: &Path) -> Result<(u64, String), Failure> {
        let mut reader = reader(path)?;
        let (mut hash, mut length, mut buffer) = (Sha256::new(), 0u64, vec![0; 64 * 1024]);
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                return Ok((length, HEXLOWER.encode(&hash.finalize())));
            }
            hash.update(&buffer[..read]);
            length += read as u64;
        }
    }

    pub(crate) fn write(
        target: &Path,
        staged: &[(&str, PathBuf)],
        settings: &Settings,
        scope: rocky_core::Scope,
        revision: u64,
        node: &PrivateKey,
        created: u64,
    ) -> Result<Inventory, Failure> {
        let mut files = Vec::new();
        for (name, path) in staged {
            let (length, sha256) = digest(path)?;
            files.push(Entry {
                name: (*name).to_string(),
                length,
                sha256,
            });
        }
        let inventory = Inventory {
            format: FORMAT,
            created,
            publisher: settings.publisher,
            scope,
            manifest: HEXLOWER.encode(&settings.manifest()?.verify(settings.publisher)?.hash()),
            node: node.public_key(),
            revision,
            files,
        };
        let header = serde_json::to_vec(&inventory)?;
        let signature = node.sign(&label(&header));
        let partial = target.with_extension("partial");
        let mut out = Sink::create(&partial)?;
        out.write_all(MAGIC)?;
        out.write_all(&(header.len() as u32).to_le_bytes())?;
        out.write_all(&header)?;
        out.write_all(&signature.to_bytes())?;
        for (_, path) in staged {
            std::io::copy(&mut reader(path)?, &mut out)?;
        }
        out.finish(false)?;
        rocky_platform::fs::replace_database_not_atomic_in_browser(&partial, target)?;
        Ok(inventory)
    }

    pub(crate) struct Opened {
        pub inventory: Inventory,
        pub identity: Settings,
        pub files: Vec<(String, PathBuf)>,
        pub id: String,
    }
    #[derive(Debug)]
    pub(crate) struct Unsupported(&'static str);
    impl std::fmt::Display for Unsupported {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "unsupported {}", self.0)
        }
    }
    impl std::error::Error for Unsupported {}
    fn header(input: &mut impl Read) -> Result<(Inventory, Vec<u8>), Failure> {
        let mut magic = vec![0; MAGIC.len()];
        input.read_exact(&mut magic)?;
        if magic != MAGIC {
            return Err("not a Rocky backup archive".into());
        }
        let mut length = [0; 4];
        input.read_exact(&mut length)?;
        let length = u32::from_le_bytes(length) as usize;
        if length > MAX_HEADER {
            return Err("backup inventory quota".into());
        }
        let mut header = vec![0; length];
        input.read_exact(&mut header)?;
        let mut signature = [0; 64];
        input.read_exact(&mut signature)?;
        let inventory: Inventory = serde_json::from_slice(&header)?;
        if inventory.format != FORMAT {
            return Err(Unsupported("backup archive format").into());
        }
        if !inventory
            .node
            .verify(&label(&header), &Signature::from_bytes(&signature))
        {
            return Err("the backup archive's signature does not verify".into());
        }
        let names: Vec<&str> = inventory
            .files
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        if names.first() != Some(&IDENTITY) || names.get(1) != Some(&DATABASE) || names.len() != 2 {
            return Err("the backup archive holds unexpected files".into());
        }
        Ok((inventory, header))
    }
    fn identity(inventory: &Inventory, bytes: &[u8]) -> Result<Settings, Failure> {
        let record: serde_json::Value = serde_json::from_slice(bytes)?;
        if record["version"].as_u64() != Some(u64::from(crate::enrollment::VERSION)) {
            return Err(Unsupported("backup identity version").into());
        }
        let identity: Settings = serde_json::from_value(record)?;
        let manifest = identity.manifest()?.verify(inventory.publisher)?;
        if identity.publisher != inventory.publisher
            || HEXLOWER.encode(&manifest.hash()) != inventory.manifest
        {
            return Err("the backup identity differs from its inventory".into());
        }
        identity.credentials.provenance_with_families(
            inventory.publisher,
            inventory.node,
            &identity.domain()?.families,
        )?;
        Ok(identity)
    }
    pub(crate) fn inspect(archive: &Path) -> Result<(Inventory, Settings, String), Failure> {
        let mut input = reader(archive)?;
        let (inventory, header) = header(&mut input)?;
        let mut identity = Vec::new();
        for entry in &inventory.files {
            let mut hash = Sha256::new();
            let mut taken = (&mut input).take(entry.length);
            let mut buffer = vec![0; 64 * 1024];
            let mut length = 0u64;
            loop {
                let read = taken.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                hash.update(&buffer[..read]);
                if entry.name == IDENTITY {
                    if identity.len() + read > MAX_HEADER * 64 {
                        return Err("backup identity quota".into());
                    }
                    identity.extend_from_slice(&buffer[..read]);
                }
                length += read as u64;
            }
            if (length, HEXLOWER.encode(&hash.finalize())) != (entry.length, entry.sha256.clone()) {
                return Err(format!("{} differs from the backup inventory", entry.name).into());
            }
        }
        if input.read(&mut [0])? != 0 {
            return Err("the backup archive has trailing bytes".into());
        }
        let identity = self::identity(&inventory, &identity)?;
        let id = HEXLOWER.encode(&rocky_core::commitment("rocky.backup.id.v1", &header));
        Ok((inventory, identity, id))
    }

    pub(crate) fn open(archive: &Path, staging: &Path) -> Result<Opened, Failure> {
        let mut input = reader(archive)?;
        let (inventory, header) = header(&mut input)?;
        rocky_platform::fs::create_dir_all(staging)?;
        let mut files = Vec::new();
        for entry in &inventory.files {
            let path = staging.join(format!("{}.restore", entry.name));
            let mut out = Sink::create(&path)?;
            std::io::copy(&mut (&mut input).take(entry.length), &mut out)?;
            out.finish(entry.name == DATABASE)?;
            if digest(&path)? != (entry.length, entry.sha256.clone()) {
                return Err(format!("{} differs from the backup inventory", entry.name).into());
            }
            files.push((entry.name.clone(), path));
        }
        if input.read(&mut [0])? != 0 {
            return Err("the backup archive has trailing bytes".into());
        }
        let identity = identity(&inventory, &rocky_platform::fs::read(&files[0].1)?)?;
        Ok(Opened {
            id: HEXLOWER.encode(&rocky_core::commitment("rocky.backup.id.v1", &header)),
            inventory,
            identity,
            files,
        })
    }

    pub(crate) fn place(opened: &Opened, directory: &Path) -> Result<(), Failure> {
        for (name, staged) in &opened.files {
            if name == IDENTITY {
                continue;
            }
            let target = directory.join(name);
            if name == DATABASE {
                for suffix in ["", "-wal", "-shm"] {
                    let mut stale = target.clone().into_os_string();
                    stale.push(suffix);
                    match rocky_platform::fs::remove_file(PathBuf::from(stale)) {
                        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                            return Err(error.into());
                        }
                        _ => {}
                    }
                }
                rocky_platform::fs::sync_dir(directory)?;
            }
            rocky_platform::fs::replace_database_not_atomic_in_browser(staged, target)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
