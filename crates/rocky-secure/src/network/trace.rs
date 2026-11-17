use crate::{Error, PublicKey};
use tracing::{Span, field::Empty};
use web_time::Instant;

pub(crate) fn peer(key: &PublicKey) -> String {
    let mut hex = key.to_hex();
    hex.truncate(12);
    hex
}
pub(crate) fn session(channel: u16) -> Span {
    tracing::debug_span!("session", channel)
}
pub(crate) fn serve(session: &Span, channel: u16, family: &'static str) -> Span {
    tracing::debug_span!(parent: session, "serve", channel, family, outcome = Empty)
}
pub(crate) fn outcome<T>(span: &Span, result: &Result<T, Error>) {
    span.record(
        "outcome",
        match result {
            Ok(_) => "ok",
            Err(error) => error.category(),
        },
    );
}
pub(crate) struct Request {
    span: Span,
    started: Instant,
    bytes: usize,
    deadline: rocky_platform::time::Instant,
}
impl Request {
    pub(crate) fn start(session: &Span, channel: u16, family: &'static str) -> Self {
        Self {
            span: tracing::debug_span!(
                parent: session,
                "request",
                channel,
                family,
                bytes = Empty,
                duration_ms = Empty,
                outcome = Empty
            ),
            started: Instant::now(),
            bytes: 0,
            deadline: rocky_platform::time::Instant::now() + super::REQUEST_LIMIT,
        }
    }
    pub(crate) fn deadline(&self) -> rocky_platform::time::Instant {
        self.deadline
    }
    pub(crate) fn sent(&mut self, bytes: usize) {
        self.bytes = self.bytes.saturating_add(bytes);
    }
    pub(crate) fn received(&mut self, bytes: usize) {
        self.bytes = self.bytes.saturating_add(bytes);
    }
    pub(crate) fn finish(self, outcome: &'static str) {
        self.span.record("bytes", self.bytes as u64);
        self.span.record(
            "duration_ms",
            self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        );
        self.span.record("outcome", outcome);
    }
}
