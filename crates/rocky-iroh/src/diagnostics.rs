use std::time::Duration;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkPath {
    pub selected: bool,
    pub kind: &'static str,
    pub remote: String,
    pub local: Option<String>,
    pub rtt: Duration,
    pub mtu: u16,
    pub window: u64,
    pub sent_bytes: u64,
    pub sent_datagrams: u64,
    pub received_bytes: u64,
    pub received_datagrams: u64,
    pub lost_packets: u64,
    pub lost_bytes: u64,
    pub congestion_events: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayStatus {
    pub url: String,
    pub connected: bool,
    pub last_error: Option<String>,
    pub auth_denied_reason: Option<String>,
}
