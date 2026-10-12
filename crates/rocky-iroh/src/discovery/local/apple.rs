//! The OS owns multicast; registration never binds or closes iroh's UDP socket.
use super::{Budget, Candidate, Failure, Properties, SERVICE, TXT_BYTES, candidate, identify};
use async_dnssd::{
    Browse, BrowseData, BrowseResult, BrowsedFlags, RegisterData, Registration, ResolvedHostFlags,
    TxtRecord,
};
use futures_util::{StreamExt, stream::FuturesUnordered};
use iroh::EndpointAddr;
use rocky_core::net::BoxFuture;
use std::{net::SocketAddr, time::Duration};

pub(super) struct Backend {
    registration: Registration,
    browser: Browse,
    pending: FuturesUnordered<BoxFuture<'static, Option<Candidate>>>,
    budget: Budget,
    topics: Vec<[u8; 32]>,
}

fn txt(props: &Properties) -> Result<TxtRecord, Failure> {
    let mut txt = TxtRecord::new();
    for (key, value) in props {
        txt.set_value(key.as_bytes(), value.as_bytes())
            .map_err(|error| format!("invalid DNS-SD TXT: {error:?}"))?;
    }
    Ok(txt)
}

impl Backend {
    pub(super) async fn new(
        name: &str,
        port: u16,
        props: &Properties,
        _: &EndpointAddr,
        topics: &[[u8; 32]],
    ) -> Result<Self, Failure> {
        let txt = txt(props)?;
        let data = RegisterData {
            name: Some(name),
            domain: Some("local."),
            txt: txt.rdata(),
            ..Default::default()
        };
        let (registration, _) = tokio::time::timeout(
            Duration::from_secs(15),
            async_dnssd::register_extended("_rocky._udp", port, data)?,
        )
        .await??;
        let browser = async_dnssd::browse_extended(
            "_rocky._udp",
            BrowseData {
                domain: Some("local."),
                ..Default::default()
            },
        );
        Ok(Self {
            registration,
            browser,
            pending: FuturesUnordered::new(),
            budget: Budget::new(),
            topics: topics.to_vec(),
        })
    }

    pub(super) fn publish(&self, props: &Properties, _: &EndpointAddr) -> Result<(), Failure> {
        self.registration
            .get_default_txt_record()
            .update_record(txt(props)?.rdata(), 120)?;
        Ok(())
    }

    pub(super) async fn recv(&mut self) -> Result<Option<Candidate>, Failure> {
        loop {
            tokio::select! {
                result = self.pending.next(), if !self.pending.is_empty() => return Ok(result.flatten()),
                event = self.browser.next() => {
                    let event = event.ok_or("Bonjour browsing stopped")??;
                    if event.flags.contains(BrowsedFlags::ADD) && event.domain == "local."
                        && event.reg_type.trim_end_matches('.') == SERVICE.trim_end_matches(".local.")
                        && self.pending.len() < 16
                        && self.budget.admit(&format!("{}:{:?}", event.service_name, event.interface), tokio::time::Instant::now()) {
                        let topics = self.topics.clone();
                        self.pending.push(Box::pin(async move {
                            tokio::time::timeout(Duration::from_secs(3), resolve(event, &topics)).await.ok().flatten()
                        }));
                    }
                }
            }
        }
    }
}

async fn resolve(event: BrowseResult, topics: &[[u8; 32]]) -> Option<Candidate> {
    let service = event.resolve().next().await?.ok()?;
    if service.txt.len() > TXT_BYTES || !service.host_target.ends_with(".local.") {
        return None;
    }
    let txt = TxtRecord::parse(&service.txt)?;
    let mut props = Properties::new();
    for (key, value) in txt.iter() {
        let key = std::str::from_utf8(key).ok()?.to_ascii_lowercase();
        let value = std::str::from_utf8(value?).ok()?.to_owned();
        if props.insert(key, value).is_some() {
            return None;
        }
    }
    identify(&props, topics, super::super::now(), service.port)?;
    let mut hosts = service.resolve_socket_address();
    let first = hosts.next().await?.ok()?;
    let mut sockets = Vec::with_capacity(8);
    if first.flags.contains(ResolvedHostFlags::ADD) {
        sockets.push(SocketAddr::from(first.address));
    }
    // A and AAAA can arrive in separate callbacks; retain both route families.
    let until = tokio::time::Instant::now() + Duration::from_millis(250);
    while sockets.len() < 8 {
        match tokio::time::timeout_at(until, hosts.next()).await {
            Ok(Some(Ok(host))) if host.flags.contains(ResolvedHostFlags::ADD) => {
                sockets.push(SocketAddr::from(host.address));
            }
            Ok(Some(Ok(_))) => {}
            _ => break,
        }
    }
    candidate(&props, topics, sockets)
}
