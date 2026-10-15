//! Two independently durable replicas with a controllable development connection.
pub mod secure_demo;
pub mod transport;
use dioxus::prelude::*;
use rocky_core::OperationId;
use rocky_runtime::{Mutation, Replica, Snapshot};
use todo_app::{Action, TodoApp, VisibleRow};

pub type Laptop = Replica<TodoApp>;

#[derive(Clone)]
pub struct Demo {
    pub replicas: [Laptop; 2],
}
impl Demo {
    pub async fn open(dir: &std::path::Path) -> Result<Self, rocky_runtime::Error> {
        let laptop = Laptop::open_with_config(
            dir.join("laptop.db"),
            TodoApp,
            todo_app::StaticPolicy::default(),
            demo_identity(),
            runtime_config(),
        )
        .await?;
        let phone = Laptop::open_with_config(
            dir.join("phone.db"),
            TodoApp,
            todo_app::StaticPolicy::default(),
            rocky_runtime::Identity {
                node: rocky_core::fixtures::PHONE,
                ..demo_identity()
            },
            runtime_config(),
        )
        .await;
        match phone {
            Ok(phone) => Ok(Self {
                replicas: [laptop, phone],
            }),
            Err(e) => {
                laptop.close().await?;
                Err(e)
            }
        }
    }
    pub async fn close(&self) -> Result<(), rocky_runtime::Error> {
        let laptop = self.replicas[0].close().await;
        let phone = self.replicas[1].close().await;
        laptop.and(phone)
    }
}

fn submit(
    replica: &Laptop,
    entity: Option<OperationId>,
    action: Action,
    mut pending: Signal<bool>,
    mut error: Signal<Option<String>>,
    success: impl FnOnce() + 'static,
) {
    let result = action
        .encode()
        .map_err(|e| e.to_string())
        .and_then(|payload| {
            replica
                .mutate(Mutation { entity, payload })
                .map_err(|e| e.to_string())
        });
    match result {
        Ok(receipt) => {
            pending.set(true);
            error.set(None);
            spawn(async move {
                match receipt.committed().await {
                    Ok(_) => success(),
                    Err(e) => error.set(Some(e.to_string())),
                }
                pending.set(false);
            });
        }
        Err(e) => error.set(Some(e)),
    }
}

fn submitted_title(event: &FormEvent) -> String {
    event
        .values()
        .into_iter()
        .find_map(|(name, value)| match (name.as_str(), value) {
            ("title", FormValue::Text(value)) => Some(value),
            _ => None,
        })
        .unwrap_or_default()
}

#[component]
pub fn App() -> Element {
    let demo = use_context::<Demo>();
    let mut connected = use_signal(|| true);
    let mut progress = use_signal(|| {
        [
            rocky_runtime::PeerProgress::default(),
            rocky_runtime::PeerProgress::default(),
        ]
    });
    let mut transport_error = use_signal(|| None::<String>);
    let syncing = demo.clone();
    use_future(move || {
        let demo = syncing.clone();
        async move {
            let mut transport = transport::SimulatedTransport::default();
            loop {
                transport.set_paused(!connected());
                match transport.pump(&demo.replicas).await {
                    Ok(()) => transport_error.set(None),
                    Err(e) => transport_error.set(Some(e.to_string())),
                }
                if let (Ok(laptop), Ok(phone)) = (
                    demo.replicas[0]
                        .peer_progress(rocky_core::fixtures::PHONE)
                        .await,
                    demo.replicas[1]
                        .peer_progress(rocky_core::fixtures::LAPTOP)
                        .await,
                ) {
                    progress.set([laptop, phone]);
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    });
    let desktop = dioxus::desktop::use_window();
    let mut closing = use_signal(|| false);
    dioxus::desktop::use_wry_event_handler(move |event, _| {
        use dioxus::desktop::tao::event::{Event, WindowEvent};
        if matches!(
            event,
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            }
        ) && !closing()
        {
            closing.set(true);
            let demo = demo.clone();
            let desktop = desktop.clone();
            spawn(async move {
                if let Err(e) = demo.close().await {
                    eprintln!("Failed to close replicas: {e}");
                }
                desktop.set_close_behavior(dioxus::desktop::WindowCloseBehaviour::WindowCloses);
                desktop.close();
            });
        }
    });
    rsx! {
        style { {include_str!("style.css")} }
        main {
            h1 { "Rocky Todo" }
            button { id: "connection", disabled: closing(), onclick: move |_| connected.set(!connected()),
                if connected() { "Pause connection" } else { "Resume connection" }
            }
            p { class: "status", if connected() { "Connected · simulated in-process transport" } else { "Paused · edits stay on each device until resumed" } }
            if let Some(error) = transport_error() { p { role: "alert", "{error}" } }
            div { class: "replicas",
                ReplicaPanel { index: 0, name: "Laptop", closing: closing(), progress: progress()[0].clone() }
                ReplicaPanel { index: 1, name: "Phone", closing: closing(), progress: progress()[1].clone() }
            }
        }
    }
}

#[component]
fn ReplicaPanel(
    index: usize,
    name: &'static str,
    closing: bool,
    progress: rocky_runtime::PeerProgress,
) -> Element {
    let replica = use_context::<Demo>().replicas[index].clone();
    let mut snapshot = use_signal(|| None::<Snapshot<VisibleRow>>);
    let mut error = use_signal(|| None::<String>);
    let pending = use_signal(|| false);
    let mut title = use_signal(String::new);
    let observed = replica.clone();
    use_future(move || {
        let observed = observed.clone();
        let observer = observed.observe();
        async move {
            let mut observer = match observer {
                Ok(observer) => observer,
                Err(e) => {
                    error.set(Some(e.to_string()));
                    return;
                }
            };
            loop {
                match observer.next().await {
                    Ok(committed) => {
                        snapshot.set(Some(committed));
                    }
                    Err(e) => {
                        error.set(Some(e.to_string()));
                        break;
                    }
                }
            }
        }
    });
    let adding = replica.clone();
    let editing = replica.clone();
    let rows = snapshot
        .read()
        .as_ref()
        .map(|s| s.rows.clone())
        .unwrap_or_default();
    let revision = snapshot.read().as_ref().map(|s| s.revision.get());
    rsx! {
        section { id: "panel-{index}",
            h2 { "{name}" }
            p { class: "status", "Outgoing pending: {progress.pending} · peer received: {progress.received} · peer applied: {progress.applied}" }
            p { class: "status", if let Some(revision) = revision { "Committed locally · revision {revision}" } else { "Opening local data…" } }
            form {
                onsubmit: move |event| {
                    if pending() || closing { return; }
                    submit(&adding, None, Action::CreateTodo { title: submitted_title(&event).trim().into(), completed: false }, pending, error, move || {
                        title.set(String::new());
                        let _ = document::eval(&format!("document.getElementById('new-title-{index}').value = '';"));
                    });
                },
                input { id: "new-title-{index}", name: "title", r#type: "text", aria_label: "New todo title", placeholder: "What needs doing?", initial_value: "", disabled: pending() || closing, oninput: move |event| title.set(event.value()) }
                button { r#type: "submit", disabled: pending() || closing || title().trim().is_empty(), "Add" }
            }
            if let Some(message) = error() { p { role: "alert", "{message}" } }
            if pending() { p { class: "status", "Saving…" } }
            ul {
                for row in rows {
                    TodoRow {
                        key: "{row.id.node.0:?}:{row.id.sequence.get()}", row,
                        disabled: pending() || closing,
                        onaction: { let replica = editing.clone(); move |(id, action)| submit(&replica, Some(id), action, pending, error, || {}) }
                    }
                }
            }
        }
    }
}

#[component]
fn TodoRow(
    row: VisibleRow,
    disabled: bool,
    onaction: EventHandler<(OperationId, Action)>,
) -> Element {
    let mut draft = use_signal(|| None::<String>);
    let value = row.title.clone();
    rsx! {
        li { "data-entity": "{row.id.sequence.get()}",
            input { r#type: "checkbox", aria_label: "Complete {row.title}", checked: row.completed, disabled,
                onchange: move |event| onaction.call((row.id, Action::SetCompleted(event.checked()))) }
            form {
                onsubmit: move |event| {
                    if disabled { return; }
                    if draft().is_some() { onaction.call((row.id, Action::SetTitle(submitted_title(&event).trim().into()))); draft.set(None); }
                },
                input { key: "{row.title}", name: "title", r#type: "text", aria_label: "Rename {row.title}", initial_value: value, disabled, oninput: move |event| draft.set(Some(event.value())) }
                button { r#type: "submit", disabled: disabled || draft().is_none(), "Rename" }
            }
            button { r#type: "button", disabled, onclick: move |_| onaction.call((row.id, Action::DeleteTodo)), "Delete" }
        }
    }
}

fn runtime_config() -> rocky_runtime::RuntimeConfig {
    use rocky_core::fixtures::*;
    rocky_runtime::RuntimeConfig {
        authors: vec![(LAPTOP, OWNER), (PHONE, OWNER)],
        delivery_targets: vec![LAPTOP, PHONE],
        full_nodes: Vec::new(),
    }
}

fn demo_identity() -> rocky_runtime::Identity {
    use rocky_core::fixtures::*;
    rocky_runtime::Identity {
        scope: SCOPE,
        node: LAPTOP,
        caller: OWNER,
    }
}
