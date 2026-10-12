//! Standard DNS-SD on platforms without Apple's system responder.
use super::{Candidate, Failure, Properties, SERVICE, candidate};
use iroh::EndpointAddr;
use mdns_sd::{Receiver, ScopedIp, ServiceDaemon, ServiceEvent, ServiceInfo};
use std::net::{IpAddr, SocketAddr, SocketAddrV6};

pub(super) struct Backend {
    daemon: ServiceDaemon,
    events: Receiver<ServiceEvent>,
    name: String,
    port: u16,
    topics: Vec<[u8; 32]>,
}

impl Backend {
    pub(super) async fn new(
        name: &str,
        port: u16,
        props: &Properties,
        address: &EndpointAddr,
        topics: &[[u8; 32]],
    ) -> Result<Self, Failure> {
        let daemon = ServiceDaemon::new()?;
        let events = daemon.browse(SERVICE)?;
        let backend = Self {
            daemon,
            events,
            name: name.into(),
            port,
            topics: topics.to_vec(),
        };
        backend.publish(props, address)?;
        Ok(backend)
    }

    pub(super) fn publish(
        &self,
        props: &Properties,
        address: &EndpointAddr,
    ) -> Result<(), Failure> {
        let ips: Vec<IpAddr> = address
            .ip_addrs()
            .filter(|addr| addr.port() == self.port)
            .map(|addr| addr.ip())
            .collect();
        let host = format!("{}.local.", self.name);
        let props: std::collections::HashMap<String, String> = props.clone().into_iter().collect();
        let service =
            ServiceInfo::new(SERVICE, &self.name, &host, ips.as_slice(), self.port, props)?
                .enable_addr_auto();
        self.daemon.register(service)?;
        Ok(())
    }

    pub(super) async fn recv(&mut self) -> Result<Option<Candidate>, Failure> {
        let event = self.events.recv_async().await?;
        let ServiceEvent::ServiceResolved(service) = event else {
            return Ok(None);
        };
        let mut props = Properties::new();
        for property in service.get_properties().iter().take(5) {
            let Some(value) = property.val() else {
                return Ok(None);
            };
            if value.len() > 128 || property.key().len() > 2 {
                return Ok(None);
            }
            let Ok(value) = std::str::from_utf8(value) else {
                return Ok(None);
            };
            if props
                .insert(property.key().to_ascii_lowercase(), value.to_owned())
                .is_some()
            {
                return Ok(None);
            }
        }
        let sockets = service.get_addresses().iter().filter_map(|ip| match ip {
            ScopedIp::V4(ip) => Some(SocketAddr::new((*ip.addr()).into(), service.get_port())),
            ScopedIp::V6(ip) => Some(SocketAddr::V6(SocketAddrV6::new(
                *ip.addr(),
                service.get_port(),
                0,
                ip.scope_id().index,
            ))),
            _ => None,
        });
        Ok(candidate(&props, &self.topics, sockets))
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        let _ = self
            .daemon
            .unregister(&format!("{}.{}", self.name, SERVICE));
        let _ = self.daemon.shutdown();
    }
}
