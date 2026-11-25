use p2panda_core::{PrivateKey, PublicKey};
use rocky_coordinator::certified::CertifiedGenesis;
use rocky_core::MeshId;
use rocky_voter::{self as service, provision::*};
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub(super) use rocky_voter::Failure;

const USAGE: &str = "usage:
  rocky-voter CONFIG.json [--provision]
  rocky-voter key OUT_FILE
  rocky-voter keys OUT_DIR [CLIENT_SOCKET_ADDRESS]
  rocky-voter certify PLAN.json ROOT_KEY_FILE OUT_DIR
  rocky-voter local OUT_DIR [BASE_PORT]
  rocky-voter relay HTTP_BIND_ADDRESS
  rocky-voter check BUNDLE.json RELAY_URL TENANT_KEY_FILE
  rocky-voter backup CONFIG.json OUT_DIR
  rocky-voter restore BACKUP_DIR CONFIG.json
  rocky-voter admin ADMIN.json initialize
  rocky-voter admin ADMIN.json status VOTER
  rocky-voter admin ADMIN.json snapshot VOTER
  rocky-voter admin ADMIN.json membership EXPECTED_REVISION EXPECTED_VOTERS VOTERS";

#[tokio::main]
pub(super) async fn main() -> Result<(), Failure> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["relay", address] => relay(address).await,
        ["check", bundle, relay, key] => check(Path::new(bundle), relay, Path::new(key)).await,
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

async fn check(bundle: &Path, relay: &str, key: &Path) -> Result<(), Failure> {
    super::public_check::run(bundle, relay, key).await
}

async fn relay(address: &str) -> Result<(), Failure> {
    super::relay::serve(address.parse()?).await
}

fn load(path: &Path) -> Result<(Configuration, CertifiedGenesis), Failure> {
    let configuration: Configuration = serde_json::from_slice(&std::fs::read(path)?)?;
    let genesis = service::provision::load(&configuration)?;
    Ok((configuration, genesis))
}

async fn run(path: &Path, provision: bool) -> Result<(), Failure> {
    let (configuration, genesis) = load(path)?;
    check_client_lane(&configuration, &genesis)?;
    let service = open(&configuration, genesis, provision).await?;
    let mut clients = None;
    if let Some(lane) = &configuration.client {
        let attachment = lane.start().await?;
        tokio::spawn(service.clone().serve_clients(attachment.transport()));
        clients = Some(attachment);
    }
    let Err(stopped) = service.serve().await;
    drop(clients);
    Err(stopped.into())
}

fn write_private(path: &Path, text: &str) -> Result<(), Failure> {
    if path.exists() {
        return Err(format!("{} exists; secrets are never replaced", path.display()).into());
    }
    rocky_credentials::write_private(path, text.as_bytes())
        .map_err(|error| -> Failure { error.to_string().into() })
}

fn keys(out: &Path, address: Option<&str>) -> Result<(), Failure> {
    println!(
        "{}",
        serde_json::to_string_pretty(&voter_keys(out, address)?)?
    );
    Ok(())
}
fn voter_keys(out: &Path, address: Option<&str>) -> Result<serde_json::Value, Failure> {
    std::fs::create_dir_all(out)?;
    let voter = PrivateKey::new();
    let client = PrivateKey::new();
    write_private(&out.join("voter.key"), &voter.to_hex())?;
    write_private(&out.join("client.secret"), &client.to_hex())?;
    let address = address.map(str::parse).transpose()?;
    Ok(serde_json::json!({
        "key": voter.public_key(),
        "client": client_endpoint(client.as_bytes(), address)?,
    }))
}

const LOCAL_VOTERS: u16 = 3;
const LOCAL_IROH_PORT_OFFSET: u16 = 10;

fn local(out: &Path, port: u16) -> Result<(), Failure> {
    port.checked_add(LOCAL_IROH_PORT_OFFSET + LOCAL_VOTERS)
        .ok_or("base port leaves no room for voter addresses")?;
    if std::fs::read_dir(out).is_ok_and(|mut entries| entries.next().is_some()) {
        return Err(format!("{} is not empty", out.display()).into());
    }
    std::fs::create_dir_all(out)?;
    let out = out.canonicalize()?;
    let root = PrivateKey::new();
    let admin = PrivateKey::new();
    write_private(&out.join("admin.key"), &admin.to_hex())?;
    let mesh = local_mesh(&root);
    let genesis = out.join("service/genesis.cbor");
    let mut voters = Vec::new();
    for id in 1..=LOCAL_VOTERS {
        let key = PrivateKey::new();
        let secret = PrivateKey::new();
        let client_bind =
            std::net::SocketAddr::from(([127, 0, 0, 1], port + LOCAL_IROH_PORT_OFFSET + id));
        voters.push(PlannedVoter {
            id: id.into(),
            address: format!("127.0.0.1:{}", port + id),
            key: key.public_key(),
            client: client_endpoint(secret.as_bytes(), Some(client_bind))?,
        });
        let configuration = Configuration {
            data: out.join(format!("voter-{id}/store")),
            genesis: genesis.clone(),
            trusted_root: root.public_key(),
            mesh,
            node: id.into(),
            key,
            client: Some(ClientLane::loopback(secret.as_bytes(), client_bind)),
        };
        write_private(
            &out.join(format!("voter-{id}.json")),
            &serde_json::to_string(&configuration)?,
        )?;
    }
    let plan = Plan::local(&root, admin.public_key(), voters);
    std::fs::write(out.join("plan.json"), serde_json::to_string_pretty(&plan)?)?;
    certify_plan(&plan, &root, &out.join("service"))?;
    let admin_config = serde_json::json!({
        "genesis": genesis,
        "trusted_root": root.public_key(),
        "mesh": mesh,
        "key": admin.to_hex(),
    });
    write_private(&out.join("admin.json"), &admin_config.to_string())?;
    println!("{}", out.join("service/bundle.json").display());
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Admin {
    genesis: PathBuf,
    trusted_root: PublicKey,
    mesh: MeshId,
    key: PrivateKey,
}
fn voters(list: &str) -> Result<std::collections::BTreeSet<u64>, Failure> {
    list.split(',').map(|id| Ok(id.trim().parse()?)).collect()
}
fn membership_plan_id(revision: &str, expected: &str, target: &str) -> rocky_core::Commitment {
    rocky_core::commitment(
        "rocky.voter-cli.membership.v1",
        format!("{revision}/{expected}/{target}").as_bytes(),
    )
}

async fn admin(path: &Path, command: &[&str]) -> Result<(), Failure> {
    use rocky_coordinator::service::{NetworkCoordinator, Operation};
    let admin: Admin = serde_json::from_slice(&std::fs::read(path)?)?;
    let genesis = CertifiedGenesis::from_bytes(&std::fs::read(&admin.genesis)?)?;
    let client = NetworkCoordinator::open(genesis, admin.trusted_root, admin.mesh, admin.key)?;
    match command {
        ["initialize"] => client.initialize(1).await?,
        ["status", voter] => {
            let status = client.status(voter.parse()?).await?;
            println!("{}", serde_json::to_string_pretty(&status)?);
        }
        ["snapshot", voter] => client.snapshot(voter.parse()?).await?,
        ["membership", revision, expected, target] => {
            let id = membership_plan_id(revision, expected, target);
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

fn certify(plan: &Path, root: &Path, out: &Path) -> Result<(), Failure> {
    let plan: Plan = serde_json::from_slice(&std::fs::read(plan)?)?;
    let root: PrivateKey = serde_json::from_value(serde_json::Value::String(
        std::fs::read_to_string(root)?.trim().into(),
    ))?;
    certify_plan(&plan, &root, out)
}
fn certify_plan(plan: &Plan, root: &PrivateKey, out: &Path) -> Result<(), Failure> {
    use rocky_coordinator::certified::TrustBundle;
    let genesis = plan.certify(root)?;
    std::fs::create_dir_all(out)?;
    std::fs::write(out.join("genesis.cbor"), genesis.to_bytes()?)?;
    std::fs::write(
        out.join("bundle.json"),
        TrustBundle::new(&genesis)?.to_json()?,
    )?;
    println!("genesis {}", hex(&genesis.id()?));
    Ok(())
}
