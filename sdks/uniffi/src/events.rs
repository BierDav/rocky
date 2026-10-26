//! An application's opt-in sink for Rocky's spans and events. Rendered
//! lines leave the emitting thread through a bounded queue and reach the
//! listener on a thread of their own; a full queue drops and counts.
use std::sync::{
    Arc, Mutex, OnceLock, PoisonError,
    atomic::{AtomicU8, AtomicU64, Ordering},
    mpsc,
};
use std::time::Instant;
use tracing::{
    Level, Metadata, Subscriber,
    field::{Field, Visit},
    span::{Attributes, Id, Record},
};
use tracing_subscriber::{Layer, layer::Context, prelude::*, registry::LookupSpan};

const QUEUE: usize = 1024;

/// The most detailed level a listener receives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum NativeLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}
impl NativeLevel {
    fn rank(self) -> u8 {
        match self {
            Self::Error => 1,
            Self::Warn => 2,
            Self::Info => 3,
            Self::Debug => 4,
            Self::Trace => 5,
        }
    }
}
fn rank(level: &Level) -> u8 {
    match *level {
        Level::ERROR => 1,
        Level::WARN => 2,
        Level::INFO => 3,
        Level::DEBUG => 4,
        Level::TRACE => 5,
    }
}

/// One rendered span or event: a closed span names its fields and how long
/// it lasted, inside the spans that contain it.
#[derive(Clone, Debug, uniffi::Record)]
pub struct NativeEvent {
    pub level: NativeLevel,
    pub at_epoch_millis: u64,
    /// The containing spans, outermost first, separated by `/`.
    pub path: String,
    pub name: String,
    pub fields: String,
    /// Set for a closed span.
    pub duration_millis: Option<u64>,
}

/// Receives Rocky's events, never on a database thread.
#[uniffi::export(with_foreign)]
pub trait NativeEventListener: Send + Sync {
    fn event(&self, event: NativeEvent);
}

struct Sink {
    level: AtomicU8,
    dropped: AtomicU64,
    queue: mpsc::SyncSender<NativeEvent>,
    listener: Arc<Mutex<Option<Arc<dyn NativeEventListener>>>>,
}
static SINK: OnceLock<Option<Sink>> = OnceLock::new();

/// Installs the process's subscriber on first use; `None` when the
/// application installed one of its own.
fn sink() -> Option<&'static Sink> {
    SINK.get_or_init(|| {
        let (queue, receive) = mpsc::sync_channel::<NativeEvent>(QUEUE);
        let listener: Arc<Mutex<Option<Arc<dyn NativeEventListener>>>> = Arc::default();
        let delivered = listener.clone();
        std::thread::Builder::new()
            .name("rocky-events".into())
            .spawn(move || {
                for event in receive {
                    let current = delivered
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone();
                    if let Some(listener) = current {
                        listener.event(event);
                    }
                }
            })
            .ok()?;
        tracing_subscriber::registry()
            .with(Render)
            .try_init()
            .ok()?;
        Some(Sink {
            level: AtomicU8::new(0),
            dropped: AtomicU64::new(0),
            queue,
            listener,
        })
    })
    .as_ref()
}

/// Delivers events up to `level` to `listener`, replacing any listener
/// before; `None` stops delivery. Fails when the application installed a
/// `tracing` subscriber of its own.
#[uniffi::export]
pub fn set_event_listener(
    listener: Option<Arc<dyn NativeEventListener>>,
    level: NativeLevel,
) -> Result<(), crate::NativeError> {
    let sink = sink().ok_or_else(|| crate::NativeError::Failure {
        detail: "another tracing subscriber is installed".into(),
    })?;
    let rank = if listener.is_some() { level.rank() } else { 0 };
    *sink.listener.lock().unwrap_or_else(PoisonError::into_inner) = listener;
    sink.level.store(rank, Ordering::SeqCst);
    tracing::callsite::rebuild_interest_cache();
    Ok(())
}
/// Events dropped because the queue was full, since the process started.
#[uniffi::export]
pub fn dropped_events() -> u64 {
    SINK.get()
        .and_then(Option::as_ref)
        .map_or(0, |sink| sink.dropped.load(Ordering::Relaxed))
}

fn enabled(metadata: &Metadata<'_>) -> bool {
    SINK.get()
        .and_then(Option::as_ref)
        .is_some_and(|sink| rank(metadata.level()) <= sink.level.load(Ordering::Relaxed))
}
fn deliver(event: NativeEvent) {
    if let Some(sink) = SINK.get().and_then(Option::as_ref)
        && sink.queue.try_send(event).is_err()
    {
        sink.dropped.fetch_add(1, Ordering::Relaxed);
    }
}
fn level_of(level: &Level) -> NativeLevel {
    match *level {
        Level::ERROR => NativeLevel::Error,
        Level::WARN => NativeLevel::Warn,
        Level::INFO => NativeLevel::Info,
        Level::DEBUG => NativeLevel::Debug,
        Level::TRACE => NativeLevel::Trace,
    }
}
fn epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// A span's recorded fields and when it opened.
struct Open {
    fields: String,
    opened: Instant,
}
struct Fields<'a>(&'a mut String);
impl Visit for Fields<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        self.0.push_str(&format!("{}={value:?}", field.name()));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        self.0.push_str(&format!("{}={value}", field.name()));
    }
}
struct Render;
impl Render {
    fn path<S: Subscriber + for<'a> LookupSpan<'a>>(
        context: &Context<'_, S>,
        span: Option<&Id>,
    ) -> String {
        let Some(span) = span.and_then(|id| context.span(id)) else {
            return String::new();
        };
        let names: Vec<&str> = span.scope().from_root().map(|span| span.name()).collect();
        names.join("/")
    }
}
impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Render {
    fn register_callsite(
        &self,
        metadata: &'static Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        if enabled(metadata) {
            tracing::subscriber::Interest::always()
        } else {
            tracing::subscriber::Interest::never()
        }
    }
    fn enabled(&self, metadata: &Metadata<'_>, _: Context<'_, S>) -> bool {
        enabled(metadata)
    }
    fn on_new_span(&self, attributes: &Attributes<'_>, id: &Id, context: Context<'_, S>) {
        let mut fields = String::new();
        attributes.record(&mut Fields(&mut fields));
        if let Some(span) = context.span(id) {
            span.extensions_mut().insert(Open {
                fields,
                opened: Instant::now(),
            });
        }
    }
    fn on_record(&self, id: &Id, values: &Record<'_>, context: Context<'_, S>) {
        if let Some(span) = context.span(id)
            && let Some(open) = span.extensions_mut().get_mut::<Open>()
        {
            values.record(&mut Fields(&mut open.fields));
        }
    }
    fn on_event(&self, event: &tracing::Event<'_>, context: Context<'_, S>) {
        let mut fields = String::new();
        event.record(&mut Fields(&mut fields));
        let parent = event
            .parent()
            .cloned()
            .or_else(|| context.current_span().id().cloned());
        deliver(NativeEvent {
            level: level_of(event.metadata().level()),
            at_epoch_millis: epoch_millis(),
            path: Self::path(&context, parent.as_ref()),
            name: event.metadata().name().into(),
            fields,
            duration_millis: None,
        });
    }
    fn on_close(&self, id: Id, context: Context<'_, S>) {
        let Some(span) = context.span(&id) else {
            return;
        };
        let path = span
            .parent()
            .map(|parent| Self::path(&context, Some(&parent.id())))
            .unwrap_or_default();
        let level = level_of(span.metadata().level());
        let name = span.name().into();
        let Some(open) = span.extensions_mut().remove::<Open>() else {
            return;
        };
        deliver(NativeEvent {
            level,
            at_epoch_millis: epoch_millis(),
            path,
            name,
            fields: open.fields,
            duration_millis: Some(open.opened.elapsed().as_millis() as u64),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[derive(Default)]
    struct Collect {
        events: Mutex<Vec<(NativeEvent, Option<String>)>>,
        hold: Mutex<()>,
    }
    impl NativeEventListener for Collect {
        fn event(&self, event: NativeEvent) {
            drop(self.hold.lock());
            let thread = std::thread::current().name().map(str::to_owned);
            if let Ok(mut events) = self.events.lock() {
                events.push((event, thread));
            }
        }
    }
    fn wait(collect: &Collect, count: usize) {
        for _ in 0..2000 {
            if collect.events.lock().map_or(0, |events| events.len()) >= count {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("events were not delivered");
    }

    #[test]
    fn closed_spans_reach_the_listener_off_thread_and_overflow_is_counted() {
        let collect = Arc::new(Collect::default());
        set_event_listener(Some(collect.clone()), NativeLevel::Debug).unwrap();
        tracing::trace_span!("filtered").in_scope(|| {});
        tracing::info_span!("outer").in_scope(|| {
            let span = tracing::debug_span!("probe", count = 3u64, outcome = tracing::field::Empty);
            span.record("outcome", "ok");
        });
        wait(&collect, 2);
        let events = collect.events.lock().unwrap().clone();
        let (probe, thread) = events
            .iter()
            .find(|(event, _)| event.name == "probe")
            .unwrap();
        assert_eq!(probe.path, "outer");
        assert_eq!(probe.fields, "count=3 outcome=ok");
        assert!(probe.duration_millis.is_some());
        assert_eq!(thread.as_deref(), Some("rocky-events"));
        assert!(events.iter().all(|(event, _)| event.name != "filtered"));
        // A listener that stops taking events fills the queue; the rest drop.
        let held = collect.hold.lock().unwrap();
        let before = dropped_events();
        for _ in 0..QUEUE * 2 {
            tracing::debug_span!("burst").in_scope(|| {});
        }
        assert!(dropped_events() > before);
        drop(held);
        set_event_listener(None, NativeLevel::Error).unwrap();
        tracing::error_span!("after").in_scope(|| {});
        std::thread::sleep(Duration::from_millis(20));
        assert!(
            collect
                .events
                .lock()
                .unwrap()
                .iter()
                .all(|(event, _)| event.name != "after")
        );
    }
}
