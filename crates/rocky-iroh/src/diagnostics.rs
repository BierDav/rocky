use std::time::Duration;
/// One network path to a peer and its QUIC statistics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkPath {
    /// Whether iroh currently sends on this path.
    pub selected: bool,
    /// `"direct"`, `"relay"` or `"other"`.
    pub kind: &'static str,
    /// The remote address or relay URL.
    pub remote: String,
    /// The local address, when iroh knows it.
    pub local: Option<String>,
    /// The current round-trip time.
    pub rtt: Duration,
    /// The current maximum transmission unit in bytes.
    pub mtu: u16,
    /// The congestion window in bytes.
    pub window: u64,
    /// Bytes sent on this path.
    pub sent_bytes: u64,
    /// UDP datagrams sent on this path.
    pub sent_datagrams: u64,
    /// Bytes received on this path.
    pub received_bytes: u64,
    /// UDP datagrams received on this path.
    pub received_datagrams: u64,
    /// Packets lost on this path.
    pub lost_packets: u64,
    /// Bytes lost on this path.
    pub lost_bytes: u64,
    /// Congestion events on this path.
    pub congestion_events: u64,
}

/// One home relay connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayStatus {
    /// The relay URL.
    pub url: String,
    /// Whether the endpoint is connected to it now.
    pub connected: bool,
    /// The last connection error, if any.
    pub last_error: Option<String>,
    /// Why the relay refused the endpoint, if it did.
    pub auth_denied_reason: Option<String>,
}
