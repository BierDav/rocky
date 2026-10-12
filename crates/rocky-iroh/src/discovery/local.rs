//! One fixed DNS-SD type; topic claims are private, bounded and short lived.
use super::{Failure, Finder, LOCAL_PATH, cipher, now, open, seal};
use data_encoding::BASE64URL_NOPAD;
use iroh::{Endpoint, EndpointAddr, EndpointId, TransportAddr, Watcher};
use rand::RngCore;
use std::{collections::BTreeMap, net::SocketAddr, time::Duration};

#[cfg(target_vendor = "apple")]
mod apple;
#[cfg(target_vendor = "apple")]
use apple::Backend;
#[cfg(not(target_vendor = "apple"))]
mod multicast;
#[cfg(not(target_vendor = "apple"))]
use multicast::Backend;

/// The fixed Bonjour type applications declare in their Info.plist.
pub const LOCAL_SERVICE: &str = "_rocky._udp";
const SERVICE: &str = "_rocky._udp.local.";
const VERSION: &str = "2";
const TOPICS: usize = 2;
const TXT_BYTES: usize = 384;
const CLAIM_BYTES: usize = 91;
const AGE: u64 = 120;
const SKEW: u64 = 30;
const REFRESH: Duration = Duration::from_secs(60);
type Properties = BTreeMap<String, String>;
type Candidate = ([u8; 32], EndpointAddr);

fn properties(
    node: &[u8; 32],
    endpoint: EndpointId,
    topics: &[[u8; 32]],
    at: u64,
    port: u16,
) -> Result<Properties, Failure> {
    if topics.is_empty() || topics.len() > TOPICS {
        return Err("local discovery needs one or two topics".into());
    }
    let mut props = Properties::from([
        ("v".into(), VERSION.into()),
        ("ep".into(), endpoint.to_string()),
    ]);
    let plain = [node.as_slice(), &at.to_le_bytes()].concat();
    for (index, topic) in topics.iter().enumerate() {
        let sealed = seal(
            &cipher("rocky.discovery-local-key.v2", topic),
            &plain,
            &[endpoint.as_bytes().as_slice(), &port.to_le_bytes()].concat(),
        )?;
        props.insert(format!("c{index}"), BASE64URL_NOPAD.encode(&sealed));
    }
    Ok(props)
}

fn identify(
    props: &Properties,
    topics: &[[u8; 32]],
    at: u64,
    port: u16,
) -> Option<([u8; 32], EndpointId)> {
    if props.len() < 3
        || props.len() > 4
        || props
            .iter()
            .map(|(key, value)| key.len() + value.len() + 2)
            .sum::<usize>()
            > TXT_BYTES
        || props.get("v")? != VERSION
        || props
            .keys()
            .any(|key| !matches!(key.as_str(), "v" | "ep" | "c0" | "c1"))
    {
        return None;
    }
    let encoded = props.get("ep")?;
    if encoded.len() != 64 {
        return None;
    }
    let endpoint: EndpointId = encoded.parse().ok()?;
    props.get("c0")?;
    for key in ["c0", "c1"] {
        let Some(claim) = props.get(key) else {
            continue;
        };
        if claim.len() != CLAIM_BYTES {
            continue;
        }
        let sealed = BASE64URL_NOPAD.decode(claim.as_bytes()).ok()?;
        for topic in topics.iter().take(TOPICS) {
            let Some(plain) = open(
                &cipher("rocky.discovery-local-key.v2", topic),
                &sealed,
                &[endpoint.as_bytes().as_slice(), &port.to_le_bytes()].concat(),
            ) else {
                continue;
            };
            if plain.len() != 40 {
                continue;
            }
            let issued = u64::from_le_bytes(plain[32..].try_into().ok()?);
            if issued > at.saturating_add(SKEW) || at.saturating_sub(issued) > AGE {
                continue;
            }
            return Some((plain[..32].try_into().ok()?, endpoint));
        }
    }
    None
}

fn candidate(
    props: &Properties,
    topics: &[[u8; 32]],
    sockets: impl IntoIterator<Item = SocketAddr>,
) -> Option<Candidate> {
    let sockets: Vec<_> = sockets.into_iter().take(8).collect();
    let port = sockets.first()?.port();
    let (node, endpoint) = identify(props, topics, now(), port)?;
    let mut address = EndpointAddr::new(endpoint);
    for socket in sockets {
        if socket.port() == port
            && port != 0
            && !socket.ip().is_unspecified()
            && !socket.ip().is_multicast()
        {
            address.addrs.insert(TransportAddr::Ip(socket));
        }
    }
    (!address.addrs.is_empty()).then_some((node, address))
}

/// Bounds candidate resolution/reporting, including unique-name churn and replay.
struct Budget {
    since: tokio::time::Instant,
    seen: Vec<String>,
}
impl Budget {
    fn new() -> Self {
        Self {
            since: tokio::time::Instant::now(),
            seen: Vec::new(),
        }
    }
    fn admit(&mut self, key: &str, at: tokio::time::Instant) -> bool {
        if at.duration_since(self.since) >= Duration::from_secs(30) {
            self.since = at;
            self.seen.clear();
        }
        if key.len() > 256 || self.seen.len() >= 64 || self.seen.iter().any(|seen| seen == key) {
            return false;
        }
        self.seen.push(key.into());
        true
    }
}

pub(super) async fn run(
    finder: &Finder,
    endpoint: &Endpoint,
    node: [u8; 32],
    topics: &[[u8; 32]],
) -> Result<(), Failure> {
    let mut random = [0; 16];
    rand::thread_rng().fill_bytes(&mut random);
    let name = format!("rk{}", data_encoding::HEXLOWER.encode(&random));
    let port = endpoint
        .bound_sockets()
        .first()
        .ok_or("iroh has no bound socket")?
        .port();
    let mut addresses = endpoint.watch_addr();
    let props = properties(&node, endpoint.id(), topics, now(), port)?;
    let mut backend = Backend::new(&name, port, &props, &addresses.get(), topics).await?;
    finder.fail(0, None);
    let mut refresh = tokio::time::interval(REFRESH);
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    refresh.tick().await;
    let mut budget = Budget::new();
    loop {
        tokio::select! {
            _ = refresh.tick() => backend.publish(&properties(&node, endpoint.id(), topics, now(), port)?, &addresses.get())?,
            changed = addresses.updated() => {
                if changed.is_err() { return Ok(()); }
                backend.publish(&properties(&node, endpoint.id(), topics, now(), port)?, &addresses.get())?;
            }
            found = backend.recv() => {
                if let Some((peer, address)) = found?
                    && address.id != endpoint.id() && budget.admit(&address.id.to_string(), tokio::time::Instant::now()) {
                    finder.report(peer, &address, LOCAL_PATH, true);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    #[test]
    fn claims_require_the_topic_endpoint_and_current_version() -> Result<(), Failure> {
        let endpoint = SecretKey::from_bytes(&[1; 32]).public();
        let topic = [3; 32];
        let mut props = properties(&[9; 32], endpoint, &[topic], 1000, 4333)?;
        assert_eq!(
            identify(&props, &[topic], 1000, 4333),
            Some(([9; 32], endpoint))
        );
        assert_eq!(identify(&props, &[[4; 32]], 1000, 4333), None);
        assert_eq!(identify(&props, &[topic], 1000, 4334), None);
        props.insert(
            "ep".into(),
            SecretKey::from_bytes(&[2; 32]).public().to_string(),
        );
        assert_eq!(identify(&props, &[topic], 1000, 4333), None);
        props.insert("ep".into(), endpoint.to_string());
        props.insert("v".into(), "1".into());
        assert_eq!(identify(&props, &[topic], 1000, 4333), None);
        Ok(())
    }

    #[test]
    fn old_future_and_malformed_claims_are_refused() -> Result<(), Failure> {
        let endpoint = SecretKey::from_bytes(&[1; 32]).public();
        let topics = [[3; 32]];
        let mut props = properties(&[9; 32], endpoint, &topics, 1000, 4333)?;
        assert!(identify(&props, &topics, 1120, 4333).is_some());
        assert!(identify(&props, &topics, 1121, 4333).is_none());
        assert!(identify(&props, &topics, 970, 4333).is_some());
        assert!(identify(&props, &topics, 969, 4333).is_none());
        props.insert("c0".into(), "x".repeat(1024));
        assert!(identify(&props, &topics, 1000, 4333).is_none());
        props.insert("c0".into(), "!".repeat(CLAIM_BYTES));
        assert!(identify(&props, &topics, 1000, 4333).is_none());
        Ok(())
    }

    #[test]
    fn previous_topic_works_without_a_public_topic_tag() -> Result<(), Failure> {
        let endpoint = SecretKey::from_bytes(&[1; 32]).public();
        let props = properties(&[9; 32], endpoint, &[[3; 32], [4; 32]], 1000, 4333)?;
        assert_eq!(
            identify(&props, &[[4; 32]], 1000, 4333),
            Some(([9; 32], endpoint))
        );
        assert!(
            props
                .values()
                .all(|value| !value.contains(&"09".repeat(32)))
        );
        assert!(properties(&[9; 32], endpoint, &[[3; 32]; 3], 1000, 4333).is_err());
        Ok(())
    }

    #[test]
    fn duplicate_candidates_and_unique_name_floods_are_bounded() {
        let mut budget = Budget::new();
        let start = budget.since;
        assert!(budget.admit("peer", start));
        assert!(!budget.admit("peer", start));
        for index in 0..63 {
            assert!(budget.admit(&index.to_string(), start));
        }
        assert!(!budget.admit("overflow", start));
        assert!(budget.admit("peer", start + Duration::from_secs(30)));
        assert!(!budget.admit(&"x".repeat(257), start + Duration::from_secs(30)));
    }
}
