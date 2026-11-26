use data_encoding::BASE64URL_NOPAD;
use iroh::{EndpointAddr, EndpointId, RelayUrl};
use rocky_core::net::Failure;
use std::net::{IpAddr, SocketAddr};

pub(crate) const SCHEME: &str = "iroh";
pub(crate) const MAX_DIRECT: usize = 2;
const ROUTE_FORMAT: u8 = 1;

/// The `iroh:` route that names an endpoint, its first relay and the two direct addresses most
/// likely reachable from another network; a relay URL is at most 255 bytes.
pub fn route(address: &EndpointAddr) -> Result<String, Failure> {
    let mut bytes = vec![ROUTE_FORMAT];
    bytes.extend(address.id.as_bytes());
    let relay = address
        .relay_urls()
        .next()
        .map(ToString::to_string)
        .unwrap_or_default();
    bytes.push(u8::try_from(relay.len()).map_err(|_| "a relay URL of at most 255 bytes")?);
    bytes.extend(relay.as_bytes());
    for socket in reachable_first(address).take(MAX_DIRECT) {
        put_socket(&mut bytes, socket);
    }
    Ok(format!("{SCHEME}:{}", BASE64URL_NOPAD.encode(&bytes)))
}

/// The endpoint, relay and direct addresses an `iroh:` route names.
pub fn endpoint_addr(route: &str) -> Result<EndpointAddr, Failure> {
    let encoded = route
        .strip_prefix(SCHEME)
        .and_then(|rest| rest.strip_prefix(':'))
        .ok_or("not an iroh: route")?;
    let bytes = BASE64URL_NOPAD.decode(encoded.as_bytes())?;
    let mut rest = bytes.as_slice();
    if take(&mut rest, 1) != Some(&[ROUTE_FORMAT][..]) {
        return Err("unsupported iroh route format".into());
    }
    let malformed = "malformed iroh route";
    let id = EndpointId::from_bytes(take(&mut rest, 32).ok_or(malformed)?.try_into()?)?;
    let mut address = EndpointAddr::new(id);
    let Some(&[relay]) = take(&mut rest, 1) else {
        return Err(malformed.into());
    };
    let relay = std::str::from_utf8(take(&mut rest, usize::from(relay)).ok_or(malformed)?)?;
    if !relay.is_empty() {
        address = address.with_relay_url(relay.parse::<RelayUrl>()?);
    }
    for _ in 0..MAX_DIRECT {
        if rest.is_empty() {
            break;
        }
        address = address.with_ip_addr(take_socket(&mut rest).ok_or(malformed)?);
    }
    if !rest.is_empty() {
        return Err(malformed.into());
    }
    Ok(address)
}

pub(crate) fn reachable_first(address: &EndpointAddr) -> impl Iterator<Item = &SocketAddr> {
    let mut sockets: Vec<_> = address.ip_addrs().collect();
    sockets.sort_by_key(|socket| (reach(socket.ip()), socket.is_ipv6()));
    sockets.into_iter()
}

fn reach(ip: IpAddr) -> Reach {
    match ip {
        IpAddr::V4(ip) if ip.is_loopback() || ip.is_unspecified() => Reach::ThisHost,
        IpAddr::V4(ip) if ip.is_link_local() => Reach::ThisLink,
        IpAddr::V4(ip) if ip.octets()[0] == 100 && ip.octets()[1] & 0xc0 == 64 => Reach::SharedNat,
        IpAddr::V4(ip) if ip.is_private() => Reach::PrivateNetwork,
        IpAddr::V6(ip) if ip.is_loopback() || ip.is_unspecified() => Reach::ThisHost,
        IpAddr::V6(ip) if ip.is_unicast_link_local() => Reach::ThisLink,
        IpAddr::V6(ip) if ip.is_unique_local() => Reach::PrivateNetwork,
        _ => Reach::Global,
    }
}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Reach {
    Global,
    PrivateNetwork,
    SharedNat,
    ThisLink,
    ThisHost,
}

pub(crate) fn put_socket(out: &mut Vec<u8>, socket: &SocketAddr) {
    match socket.ip() {
        IpAddr::V4(ip) => {
            out.push(4);
            out.extend(ip.octets());
        }
        IpAddr::V6(ip) => {
            out.push(6);
            out.extend(ip.octets());
        }
    }
    out.extend(socket.port().to_le_bytes());
}

pub(crate) fn take_socket(bytes: &mut &[u8]) -> Option<SocketAddr> {
    let ip = match take(bytes, 1)? {
        [4] => IpAddr::from(<[u8; 4]>::try_from(take(bytes, 4)?).ok()?),
        [6] => IpAddr::from(<[u8; 16]>::try_from(take(bytes, 16)?).ok()?),
        _ => return None,
    };
    let port = u16::from_le_bytes(take(bytes, 2)?.try_into().ok()?);
    Some(SocketAddr::new(ip, port))
}

pub(crate) fn take<'a>(bytes: &mut &'a [u8], count: usize) -> Option<&'a [u8]> {
    let (head, rest) = bytes.split_at_checked(count)?;
    *bytes = rest;
    Some(head)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    fn socket(text: &str) -> SocketAddr {
        text.parse().unwrap_or_else(|_| unreachable!())
    }

    fn many_interfaces() -> Result<EndpointAddr, Failure> {
        Ok(EndpointAddr::new(SecretKey::from_bytes(&[3; 32]).public())
            .with_relay_url("https://euc1-1.relay.n0.iroh-canary.iroh.link./".parse()?)
            .with_addrs(
                [
                    "127.0.0.1:50123",
                    "[::1]:50123",
                    "[fe80::1c2b:3aff:fe4d:5e6f]:50123",
                    "169.254.10.20:50123",
                    "100.101.102.103:50123",
                    "[fd7a:115c:a1e0::1234:5678]:50123",
                    "172.17.0.1:50123",
                    "192.168.178.20:50123",
                    "10.0.0.5:50123",
                ]
                .map(|text| iroh::TransportAddr::Ip(socket(text))),
            ))
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_route_names_the_relay_and_only_the_most_reachable_direct_addresses() -> Result<(), Failure>
    {
        let address = many_interfaces()?;
        let route = route(&address)?;
        assert!(route.len() <= 140, "{} bytes: {route}", route.len());
        let named = endpoint_addr(&route)?;
        assert_eq!(named.id, address.id);
        assert_eq!(
            named.relay_urls().collect::<Vec<_>>(),
            address.relay_urls().collect::<Vec<_>>()
        );
        let mut direct: Vec<_> = named.ip_addrs().copied().collect();
        direct.sort();
        assert_eq!(
            direct,
            [socket("10.0.0.5:50123"), socket("172.17.0.1:50123")],
            "private IPv4 before private IPv6, CGNAT, link-local and loopback"
        );
        Ok(())
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_global_address_comes_before_a_private_one_and_ipv4_before_ipv6() {
        let address = EndpointAddr::new(SecretKey::from_bytes(&[4; 32]).public()).with_addrs(
            [
                "[2001:db8::42]:1",
                "192.168.1.2:1",
                "[fd00::2]:1",
                "8.8.4.4:1",
            ]
            .map(|text| iroh::TransportAddr::Ip(socket(text))),
        );
        assert_eq!(
            reachable_first(&address).copied().collect::<Vec<_>>(),
            [
                "8.8.4.4:1",
                "[2001:db8::42]:1",
                "192.168.1.2:1",
                "[fd00::2]:1"
            ]
            .map(socket)
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_route_without_relay_or_direct_address_round_trips() -> Result<(), Failure> {
        let bare = EndpointAddr::new(SecretKey::from_bytes(&[5; 32]).public());
        assert_eq!(endpoint_addr(&route(&bare)?)?, bare);
        let loopback = bare.clone().with_ip_addr(socket("127.0.0.1:4433"));
        assert_eq!(endpoint_addr(&route(&loopback)?)?, loopback);
        Ok(())
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_route_of_another_format_or_with_trailing_bytes_is_refused() -> Result<(), Failure> {
        let valid = route(&many_interfaces()?)?;
        let bytes = BASE64URL_NOPAD.decode(valid.trim_start_matches("iroh:").as_bytes())?;
        let mut other = bytes.clone();
        other[0] = ROUTE_FORMAT + 1;
        let mut trailing = bytes.clone();
        put_socket(&mut trailing, &socket("1.2.3.4:5"));
        let json = format!(
            "iroh:{}",
            BASE64URL_NOPAD.encode(&serde_json::to_vec(&many_interfaces()?)?)
        );
        for refused in [
            format!("iroh:{}", BASE64URL_NOPAD.encode(&other)),
            format!("iroh:{}", BASE64URL_NOPAD.encode(&trailing)),
            format!("iroh:{}", BASE64URL_NOPAD.encode(&bytes[..bytes.len() - 1])),
            json,
            "tcp://127.0.0.1:1".to_owned(),
        ] {
            assert!(endpoint_addr(&refused).is_err(), "{refused}");
        }
        Ok(())
    }
}
