//! A coordinator voter and its setup commands. See
//! `docs/runtime/coordinator-operations.md`.
use p2panda_core::{PrivateKey, PublicKey};
use rocky_coordinator::{
    certified::CertifiedGenesis,
    service::{self, VoterService},
};
use rocky_core::MeshId;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

type Failure = Box<dyn std::error::Error>;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    data: PathBuf,
    genesis: PathBuf,
    trusted_root: PublicKey,
    mesh: MeshId,
    node: u64,
    key: PrivateKey,
    /// The client lane of a public service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client: Option<ClientLane>,
}
/// The voter's iroh endpoint: its secret and how it binds.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientLane {
    /// 64 hexadecimal digits; the endpoint ID certified in genesis.
    secret: String,
    #[serde(default)]
    relay: Option<String>,
    #[serde(default)]
    discovery: bool,
    #[serde(default)]
    bind: Option<String>,
}

const USAGE: &str = "usage:
  rocky-voter CONFIG.json [--provision]
  rocky-voter key OUT_FILE
  rocky-voter keys OUT_DIR [CLIENT_SOCKET_ADDRESS]
  rocky-voter certify PLAN.json ROOT_KEY_FILE OUT_DIR
  rocky-voter local OUT_DIR [BASE_PORT]
  rocky-voter backup CONFIG.json OUT_DIR
  rocky-voter restore BACKUP_DIR CONFIG.json
  rocky-voter admin ADMIN.json initialize
  rocky-voter admin ADMIN.json status VOTER
  rocky-voter admin ADMIN.json snapshot VOTER
  rocky-voter admin ADMIN.json membership EXPECTED_REVISION EXPECTED_VOTERS VOTERS";

#[tokio::main]
async fn main() -> Result<(), Failure> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["key", out] => {
            let key = PrivateKey::new();
            write_private(Path::new(out), &key.to_hex())?;
            println!("{}", key.public_key().to_hex());
            Ok(())
        }
        ["admin", config, command @ ..] => admin(Path::new(config), command).await,
        ["keys", out] => keys(Path::new(out), None),
        ["keys", out, address] => keys(Path::new(out), Some(address)),
        ["certify", plan, root, out] => certify(Path::new(plan), Path::new(root), Path::new(out)),
        ["local", out] => local(Path::new(out), 7300),
        ["local", out, port] => local(Path::new(out), port.parse()?),
        ["backup", config, out] => {
            let (configuration, genesis) = load(Path::new(config))?;
            let backup = service::backup(
                &configuration.data,
                genesis,
                configuration.node,
                Path::new(out),
            )?;
            println!(
                "backed up voter {} through log index {}",
                backup.node, backup.index
            );
            Ok(())
        }
        ["restore", backup, config] => {
            let (configuration, genesis) = load(Path::new(config))?;
            let restored = service::restore(
                Path::new(backup),
                &genesis,
                configuration.node,
                &configuration.data,
            )?;
            println!(
                "restored voter {} through log index {}, fenced until re-added",
                restored.node, restored.index
            );
            Ok(())
        }
        [config] => run(Path::new(config), false).await,
        [config, "--provision"] => run(Path::new(config), true).await,
        _ => Err(USAGE.into()),
    }
}

fn load(path: &Path) -> Result<(Configuration, CertifiedGenesis), Failure> {
    let configuration: Configuration = serde_json::from_slice(&std::fs::read(path)?)?;
    let genesis = CertifiedGenesis::from_bytes(&std::fs::read(&configuration.genesis)?)?;
    genesis.verify(configuration.trusted_root, configuration.mesh)?;
    Ok((configuration, genesis))
}

async fn run(path: &Path, provision: bool) -> Result<(), Failure> {
    let (configuration, genesis) = load(path)?;
    let public = genesis.genesis.public.is_some();
    let service = VoterService::open(
        &configuration.data,
        genesis,
        configuration.trusted_root,
        configuration.mesh,
        configuration.node,
        configuration.key,
        provision,
    )
    .await?;
    match (configuration.client, public) {
        (None, true) => return Err("a public service needs a client lane".into()),
        (Some(_), false) => return Err("only a public service has a client lane".into()),
        (Some(lane), true) => serve_clients(&service, lane).await?,
        (None, false) => {}
    }
    service.serve().await?;
    Ok(())
}

#[cfg(feature = "iroh")]
async fn serve_clients(
    service: &std::sync::Arc<VoterService>,
    lane: ClientLane,
) -> Result<(), Failure> {
    let options = rocky_iroh::Options {
        relay: lane.relay,
        discovery: lane.discovery,
        bind: lane.bind,
    };
    let attachment = options
        .start_serving(
            secret(&lane.secret)?,
            vec![rocky_core::net::Protocol::Coordinate],
        )
        .await
        .map_err(|error| -> Failure { error.to_string().into() })?;
    tokio::spawn(service.clone().serve_clients(attachment.transport()));
    Ok(())
}
#[cfg(not(feature = "iroh"))]
async fn serve_clients(_: &std::sync::Arc<VoterService>, _: ClientLane) -> Result<(), Failure> {
    Err("this voter was built without the iroh client lane".into())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn unhex<const N: usize>(text: &str) -> Result<[u8; N], Failure> {
    let text = text.trim();
    if text.len() != N * 2 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("expected {} hexadecimal digits", N * 2).into());
    }
    let mut bytes = [0; N];
    for (byte, pair) in bytes.iter_mut().zip(text.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair)?, 16)?;
    }
    Ok(bytes)
}
#[cfg(feature = "iroh")]
fn secret(text: &str) -> Result<[u8; 32], Failure> {
    unhex(text)
}
fn write_private(path: &Path, text: &str) -> Result<(), Failure> {
    if path.exists() {
        return Err(format!("{} exists; secrets are never replaced", path.display()).into());
    }
    rocky_credentials::write_private(path, text.as_bytes())
        .map_err(|error| -> Failure { error.to_string().into() })
}

/// New independent voter secrets, written owner-only into `out`; prints
/// the public half a provider certifies.
#[cfg(feature = "iroh")]
fn keys(out: &Path, address: Option<&str>) -> Result<(), Failure> {
    println!(
        "{}",
        serde_json::to_string_pretty(&voter_keys(out, address)?)?
    );
    Ok(())
}
#[cfg(feature = "iroh")]
fn voter_keys(out: &Path, address: Option<&str>) -> Result<serde_json::Value, Failure> {
    use rocky_iroh::iroh::{EndpointAddr, SecretKey};
    std::fs::create_dir_all(out)?;
    let voter = PrivateKey::new();
    let client = PrivateKey::new();
    write_private(&out.join("voter.key"), &voter.to_hex())?;
    write_private(&out.join("client.secret"), &client.to_hex())?;
    let id = SecretKey::from_bytes(client.as_bytes()).public();
    let mut endpoint = EndpointAddr::new(id);
    if let Some(address) = address {
        endpoint = endpoint.with_ip_addr(address.parse()?);
    }
    Ok(serde_json::json!({
        "key": voter.public_key(),
        "client": serde_json::to_string(&endpoint)?,
    }))
}
#[cfg(not(feature = "iroh"))]
fn keys(_: &Path, _: Option<&str>) -> Result<(), Failure> {
    Err("this voter was built without the iroh client lane".into())
}

/// A three-voter service on loopback for development: independent secrets
/// per voter, an admin key, configurations and the certified bundle. The
/// root key exists only in memory while certifying. Relays and discovery
/// are off; voter TCP uses `port + 1..=3`, iroh `port + 11..=13`.
#[cfg(feature = "iroh")]
fn local(out: &Path, port: u16) -> Result<(), Failure> {
    port.checked_add(13)
        .ok_or("base port leaves no room for voter addresses")?;
    if std::fs::read_dir(out).is_ok_and(|mut entries| entries.next().is_some()) {
        return Err(format!("{} is not empty", out.display()).into());
    }
    std::fs::create_dir_all(out)?;
    let out = out.canonicalize()?;
    let root = PrivateKey::new();
    let admin = PrivateKey::new();
    write_private(&out.join("admin.key"), &admin.to_hex())?;
    let mesh = MeshId(
        *rocky_core::commitment("rocky.local-mesh", root.public_key().as_bytes())
            .first_chunk::<16>()
            .ok_or("mesh")?,
    );
    let mut voters = Vec::new();
    for id in 1..=3u16 {
        let dir = out.join(format!("voter-{id}"));
        let udp = format!("127.0.0.1:{}", port + 10 + id);
        let public = voter_keys(&dir, Some(&udp))?;
        voters.push(serde_json::json!({
            "id": id,
            "address": format!("127.0.0.1:{}", port + id),
            "key": public["key"],
            "client": public["client"],
        }));
        let config = serde_json::json!({
            "data": dir.join("store"),
            "genesis": out.join("service/genesis.cbor"),
            "trusted_root": root.public_key(),
            "mesh": mesh,
            "node": id,
            "key": std::fs::read_to_string(dir.join("voter.key"))?,
            "client": {
                "secret": std::fs::read_to_string(dir.join("client.secret"))?,
                "relay": "disabled",
                "discovery": false,
                "bind": udp,
            },
        });
        write_private(&out.join(format!("voter-{id}.json")), &config.to_string())?;
    }
    let plan = serde_json::json!({
        "mesh": hex(&mesh.0),
        "configuration": hex(&rocky_core::commitment("rocky.local-configuration", &mesh.0)[..16]),
        "membership_authority": admin.public_key(),
        "voters": voters,
        "max_control_lifetime": 30 * 86_400,
        "tenants": 6,
        "registrations_per_day": 64,
        "tenant_lanes": 4,
    });
    std::fs::write(out.join("plan.json"), serde_json::to_string_pretty(&plan)?)?;
    certify_plan(&serde_json::from_value(plan)?, &root, &out.join("service"))?;
    let admin_config = serde_json::json!({
        "genesis": out.join("service/genesis.cbor"),
        "trusted_root": root.public_key(),
        "mesh": mesh,
        "key": admin.to_hex(),
        "evidence": out.join("admin.evidence"),
    });
    write_private(&out.join("admin.json"), &admin_config.to_string())?;
    println!("{}", out.join("service/bundle.json").display());
    Ok(())
}
#[cfg(not(feature = "iroh"))]
fn local(_: &Path, _: u16) -> Result<(), Failure> {
    Err("this voter was built without the iroh client lane".into())
}

/// What the membership authority uses to administer voters over provider TCP.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Admin {
    genesis: PathBuf,
    trusted_root: PublicKey,
    mesh: MeshId,
    key: PrivateKey,
    evidence: PathBuf,
}
fn voters(list: &str) -> Result<std::collections::BTreeSet<u64>, Failure> {
    list.split(',').map(|id| Ok(id.trim().parse()?)).collect()
}
async fn admin(path: &Path, command: &[&str]) -> Result<(), Failure> {
    use rocky_coordinator::service::{NetworkCoordinator, Operation};
    let admin: Admin = serde_json::from_slice(&std::fs::read(path)?)?;
    let genesis = CertifiedGenesis::from_bytes(&std::fs::read(&admin.genesis)?)?;
    let client = NetworkCoordinator::open(
        genesis,
        admin.trusted_root,
        admin.mesh,
        admin.key,
        admin.evidence,
    )?;
    match command {
        ["initialize"] => client.initialize(1).await?,
        ["status", voter] => {
            let status = client.status(voter.parse()?).await?;
            println!("{}", serde_json::to_string_pretty(&status)?);
        }
        ["snapshot", voter] => client.snapshot(voter.parse()?).await?,
        ["membership", revision, expected, target] => {
            // A stable ID: retrying the same plan never applies it twice.
            let id = rocky_core::commitment(
                "rocky.voter-cli.membership.v1",
                format!("{revision}/{expected}/{target}").as_bytes(),
            );
            let outcome = client
                .call(
                    id,
                    Operation::Membership {
                        voters: voters(target)?,
                        expected: voters(expected)?,
                        expected_revision: revision.parse()?,
                    },
                )
                .await?;
            println!("{outcome:?}");
        }
        _ => return Err(USAGE.into()),
    }
    Ok(())
}

/// The provider's description of a service, certified offline by its root.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    /// 32 hexadecimal digits each.
    mesh: String,
    configuration: String,
    membership_authority: PublicKey,
    #[serde(default)]
    admission: Option<PublicKey>,
    voters: Vec<PlannedVoter>,
    max_control_lifetime: u64,
    tenants: u32,
    registrations_per_day: u32,
    tenant_lanes: u32,
    #[serde(default)]
    limits: Option<rocky_coordinator::Limits>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlannedVoter {
    id: u64,
    /// Voter-to-voter TCP inside the provider network.
    address: String,
    key: PublicKey,
    /// The iroh endpoint address `keys` printed.
    client: String,
}
fn certify(plan: &Path, root: &Path, out: &Path) -> Result<(), Failure> {
    let plan: Plan = serde_json::from_slice(&std::fs::read(plan)?)?;
    let root: PrivateKey = serde_json::from_value(serde_json::Value::String(
        std::fs::read_to_string(root)?.trim().into(),
    ))?;
    certify_plan(&plan, &root, out)
}
fn certify_plan(plan: &Plan, root: &PrivateKey, out: &Path) -> Result<(), Failure> {
    use rocky_coordinator::certified::*;
    let mesh = MeshId(unhex(&plan.mesh)?);
    let genesis = CertifiedGenesis::sign(
        Genesis {
            mesh,
            configuration: unhex(&plan.configuration)?,
            voters: plan
                .voters
                .iter()
                .map(|v| Voter {
                    id: v.id,
                    address: v.address.clone(),
                    key: v.key,
                })
                .collect(),
            initial_voters: plan.voters.iter().map(|v| v.id).collect(),
            membership_authority: plan.membership_authority,
            directory_definition: [0; 32],
            directory: None,
            authorities: Vec::new(),
            max_control_lifetime: plan.max_control_lifetime,
            public: Some(PublicService {
                endpoints: plan
                    .voters
                    .iter()
                    .map(|v| ClientEndpoint {
                        voter: v.id,
                        kind: "iroh".into(),
                        address: v.client.clone(),
                    })
                    .collect(),
                admission: plan.admission,
                limits: plan.limits.unwrap_or_default(),
                tenants: plan.tenants,
                registrations_per_day: plan.registrations_per_day,
                tenant_lanes: plan.tenant_lanes,
            }),
        },
        root,
    )?;
    std::fs::create_dir_all(out)?;
    std::fs::write(out.join("genesis.cbor"), genesis.to_bytes()?)?;
    std::fs::write(
        out.join("bundle.json"),
        TrustBundle::new(&genesis)?.to_json()?,
    )?;
    println!("genesis {}", hex(&genesis.id()?));
    Ok(())
}
