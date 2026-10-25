//! One sync pass between two nodes as a tree of spans whose fields name no
//! row value, key or invitation.
mod common;
use common::{Device, Failure};
use rocky_sdk::{Network, transport::memory::Hub};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
};
use tracing::{
    Subscriber,
    field::{Field, Visit},
    span::{Attributes, Id, Record},
};
use tracing_subscriber::{Layer, layer::Context, prelude::*, registry::LookupSpan};

#[derive(Clone, Debug, Default)]
struct Span {
    name: String,
    parent: Option<u64>,
    fields: Vec<(String, String)>,
}
#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<BTreeMap<u64, Span>>>);
struct Fields<'a>(&'a mut Vec<(String, String)>);
impl Visit for Fields<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.push((field.name().into(), format!("{value:?}")));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.push((field.name().into(), value.into()));
    }
}
impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Recorder {
    fn on_new_span(&self, attributes: &Attributes<'_>, id: &Id, context: Context<'_, S>) {
        let mut span = Span {
            name: attributes.metadata().name().into(),
            parent: context
                .span(id)
                .and_then(|span| span.parent())
                .map(|parent| parent.id().into_u64()),
            ..Span::default()
        };
        attributes.record(&mut Fields(&mut span.fields));
        if let Ok(mut spans) = self.0.lock() {
            spans.insert(id.into_u64(), span);
        }
    }
    fn on_record(&self, id: &Id, values: &Record<'_>, _: Context<'_, S>) {
        if let Ok(mut spans) = self.0.lock()
            && let Some(span) = spans.get_mut(&id.into_u64())
        {
            values.record(&mut Fields(&mut span.fields));
        }
    }
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        let mut fields = Vec::new();
        event.record(&mut Fields(&mut fields));
        if let Ok(mut spans) = self.0.lock() {
            let next = u64::MAX - spans.len() as u64;
            spans.insert(
                next,
                Span {
                    name: format!("event {}", event.metadata().name()),
                    parent: None,
                    fields,
                },
            );
        }
    }
}
/// Whether `id` lies under a span named `ancestor`.
fn under(spans: &BTreeMap<u64, Span>, mut id: u64, ancestor: &str) -> bool {
    while let Some(parent) = spans.get(&id).and_then(|span| span.parent) {
        if spans.get(&parent).is_some_and(|span| span.name == ancestor) {
            return true;
        }
        id = parent;
    }
    false
}
/// Every small file the devices keep: their secrets among them.
fn secrets(directory: &Path, found: &mut Vec<Vec<u8>>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            secrets(&path, found);
        } else if let Ok(bytes) = std::fs::read(&path)
            && (16..=4096).contains(&bytes.len())
        {
            found.push(bytes);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sync_pass_is_a_span_tree_without_application_data() -> Result<(), Failure> {
    let recorder = Recorder::default();
    tracing_subscriber::registry()
        .with(recorder.clone())
        .try_init()?;
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = Device::create(
        &root.path().join("a"),
        Some(Network::Owned(hub.connector("a"))),
    )?;
    a.follow_main().await;
    let title = "Quietly kept row value 7f3a";
    a.add(1, title).await;
    let invitation = a.invite().await;
    let b = Device::join(
        &root.path().join("b"),
        &invitation,
        Network::Owned(hub.connector("b")),
    )?;
    b.sync().await.expect("sync");
    assert_eq!(b.titles().await, [title]);
    let spans = recorder.0.lock().map_err(|_| "recorder")?.clone();
    let named = |name: &str| {
        spans
            .iter()
            .filter(|(_, span)| span.name == name)
            .map(|(id, _)| *id)
            .collect::<Vec<_>>()
    };
    let field = |id: u64, name: &str| {
        spans[&id]
            .fields
            .iter()
            .rev()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value.clone())
    };
    let pass = named("sync_pass");
    assert!(!pass.is_empty(), "{spans:?}");
    assert!(
        pass.iter()
            .any(|id| field(*id, "outcome").as_deref() == Some("ok"))
    );
    for (name, ancestor) in [
        ("exchange", "sync_pass"),
        ("dial", "exchange"),
        ("admission", "exchange"),
        ("ready", "exchange"),
        ("session", "exchange"),
        ("request", "session"),
        ("page_apply", "sync_pass"),
    ] {
        assert!(
            named(name).iter().any(|id| under(&spans, *id, ancestor)),
            "{name} under {ancestor}: {spans:#?}"
        );
    }
    let rows = named("request")
        .into_iter()
        .find(|id| field(*id, "family").as_deref() == Some("rows"))
        .ok_or("a rows request")?;
    for name in ["bytes", "duration_ms", "outcome", "channel"] {
        assert!(field(rows, name).is_some(), "{name}: {:?}", spans[&rows]);
    }
    // No field carries a row value, a key or the invitation.
    let mut forbidden = vec![title.to_owned(), invitation.clone()];
    forbidden.extend(
        invitation
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|part| part.len() >= 12)
            .map(str::to_owned),
    );
    let mut stored = Vec::new();
    secrets(&root.path().join("a").join("secrets"), &mut stored);
    secrets(&root.path().join("b").join("secrets"), &mut stored);
    assert!(
        !stored.is_empty(),
        "the devices keep their secrets in files"
    );
    for bytes in stored {
        forbidden.push(bytes.iter().map(|byte| format!("{byte:02x}")).collect());
        forbidden.push(format!("{bytes:?}"));
        forbidden.extend(
            String::from_utf8(bytes)
                .ok()
                .filter(|text| text.len() >= 16),
        );
    }
    for span in spans.values() {
        for (name, value) in &span.fields {
            for secret in &forbidden {
                assert!(
                    !value.contains(secret.as_str()),
                    "{}.{name} carries protected data",
                    span.name
                );
            }
        }
    }
    a.close().await;
    b.close().await;
    Ok(())
}
