//! Endpoint observations, independent of database status.
use std::time::Duration;
/// One network path to the peer of a stream, as its transport measures it
/// when asked. Counters cover the path's lifetime so far.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkPath {
    /// Whether the transport sends application data on this path now.
    pub selected: bool,
    /// `direct` for an IP path, `relay` through a relay server.
    pub kind: &'static str,
    /// The peer's address on this path: a socket address or relay URL.
    pub remote: String,
    /// This node's address on it, where the transport knows one.
    pub local: Option<String>,
    pub rtt: Duration,
    /// The largest payload the path currently carries, in bytes.
    pub mtu: u16,
    /// The congestion window, in bytes.
    pub window: u64,
    pub sent_bytes: u64,
    pub sent_datagrams: u64,
    pub received_bytes: u64,
    pub received_datagrams: u64,
    pub lost_packets: u64,
    pub lost_bytes: u64,
    pub congestion_events: u64,
}

/// One home relay's actual connection observation, supplied by the endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayStatus {
    pub url: String,
    pub connected: bool,
    pub last_error: Option<String>,
    pub auth_denied_reason: Option<String>,
}
