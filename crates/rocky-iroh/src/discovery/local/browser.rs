//! A browser has no multicast sockets: asking for local discovery reports
//! the path unavailable, and peers are found through the topic instead.
use super::{Candidate, Failure, Properties};
use iroh::EndpointAddr;

pub(super) struct Backend;

impl Backend {
    pub(super) async fn new(
        _name: &str,
        _port: u16,
        _props: &Properties,
        _address: &EndpointAddr,
        _topics: &[[u8; 32]],
    ) -> Result<Self, Failure> {
        Err("local network discovery is unavailable in a browser".into())
    }
    pub(super) fn publish(&self, _: &Properties, _: &EndpointAddr) -> Result<(), Failure> {
        Ok(())
    }
    pub(super) async fn recv(&mut self) -> Result<Option<Candidate>, Failure> {
        Ok(None)
    }
}
