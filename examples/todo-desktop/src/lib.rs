//! The todo desktop app: one window, a panel per local installation.
//!
//! The demo keeps several installations in one window, linked in-process;
//! device mode keeps one, linked over iroh. Each panel starts, joins or
//! resets its installation, shows its sync status, and edits lists.
pub mod secure_demo;
mod status;

use dioxus::prelude::*;
use rocky_sdk::{
    Configuration, Failure, Network, TransactionToken,
    transport::{TransportOptions, memory::Hub},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, Mutex},
};
use todo_app::{List, Member, Role, Todo, Todos, configuration, roles};

/// How installations reach each other.
#[derive(Clone)]
pub enum Link {
    /// One in-process hub for every panel.
    Demo(Arc<Hub>),
    /// iroh with these options.
    Iroh(TransportOptions),
}

/// The window's installations: each lives in `root/<name>`.
#[derive(Clone)]
pub struct Workspace {
    root: PathBuf,
    link: Link,
    names: Signal<Vec<String>>,
    /// What closes with the window.
    open: Arc<Mutex<BTreeMap<String, Todos>>>,
}
impl Workspace {
    /// Must be called inside the Dioxus runtime: it owns signals.
    fn new(root: PathBuf, link: Link, names: Vec<String>) -> Self {
        Self {
            root,
            link,
            names: Signal::new(names),
            open: Arc::default(),
        }
    }
    fn configuration(&self, name: &str) -> Result<Configuration, Failure> {
        let network = match &self.link {
            Link::Demo(hub) => Network::Owned(hub.connector(name)),
            Link::Iroh(options) => Network::Owned(Arc::new(options.clone())),
        };
        Ok(configuration(self.root.join(name))?.network(network))
    }
    fn exists(&self, name: &str) -> bool {
        self.root.join(name).exists()
    }
    fn opened(&self, name: &str, todos: &Todos) {
        if let Ok(mut open) = self.open.lock() {
            open.insert(name.into(), todos.clone());
        }
    }
    fn closed(&self, name: &str) -> Option<Todos> {
        self.open.lock().ok()?.remove(name)
    }
    /// The other open installations, for the demo's in-process peering.
    fn others(&self, name: &str) -> Vec<Todos> {
        self.open
            .lock()
            .map(|open| {
                open.iter()
                    .filter(|(other, _)| *other != name)
                    .map(|(_, todos)| todos.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
    async fn close(&self) -> Result<(), Failure> {
        let open: Vec<Todos> = self
            .open
            .lock()
            .map(|mut open| std::mem::take(&mut *open).into_values().collect())
            .unwrap_or_default();
        let mut result = Ok(());
        for todos in open {
            if let Err(error) = todos.database().close().await {
                result = Err(error);
            }
        }
        result
    }
}

/// What `main` chose; the window builds the [`Workspace`] from it.
#[derive(Clone)]
pub struct Launch {
    pub root: PathBuf,
    pub demo: bool,
    pub transport: TransportOptions,
}
impl Launch {
    fn workspace(&self) -> Workspace {
        if !self.demo {
            return Workspace::new(
                self.root.clone(),
                Link::Iroh(self.transport.clone()),
                vec![DEVICE.into()],
            );
        }
        // Panels for the installations already here, at least the defaults.
        let mut names: Vec<String> = DEMO.iter().map(|name| (*name).into()).collect();
        if let Ok(entries) = std::fs::read_dir(&self.root) {
            let mut found: Vec<String> = entries
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.path().is_dir())
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter(|name| !names.contains(name))
                .collect();
            found.sort();
            names.extend(found);
        }
        Workspace::new(
            self.root.clone(),
            Link::Demo(Arc::new(Hub::default())),
            names,
        )
    }
}
const DEMO: [&str; 3] = ["laptop", "phone", "watch"];
const DEVICE: &str = "device";

#[component]
pub fn App() -> Element {
    let launch = use_context::<Launch>();
    let workspace = use_context_provider(|| launch.workspace());
    let mut names = workspace.names;
    let mut closing = use_signal(|| false);
    let desktop = dioxus::desktop::use_window();
    let closer = workspace.clone();
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
            let workspace = closer.clone();
            let desktop = desktop.clone();
            spawn(async move {
                if let Err(error) = workspace.close().await {
                    eprintln!("Failed to close: {error}");
                }
                desktop.set_close_behavior(dioxus::desktop::WindowCloseBehaviour::WindowCloses);
                desktop.close();
            });
        }
    });
    let demo = matches!(workspace.link, Link::Demo(_));
    rsx! {
        style { {include_str!("style.css")} }
        header {
            div {
                h1 { "Rocky Todo" }
                p { class: "muted",
                    if demo { "Demo · every panel is a separate installation, linked in-process" }
                    else { "Device · one installation, linked over iroh" }
                }
            }
            if demo {
                button { class: "secondary", disabled: closing(),
                    onclick: move |_| {
                        let next = (1..).map(|n| format!("device-{n}")).find(|name| !names.read().contains(name));
                        names.write().extend(next);
                    },
                    "Add device"
                }
            }
        }
        main { class: if demo { "panels" } else { "panels single" },
            for name in names() {
                Installation { key: "{name}", name: name.clone(), closing: closing() }
            }
        }
    }
}

#[derive(Clone)]
enum Phase {
    Opening,
    Start,
    Open(Todos),
    Failed(String),
}

/// One installation: its start screen, or the device once open.
#[component]
fn Installation(name: String, closing: bool) -> Element {
    let workspace = use_context::<Workspace>();
    let mut phase = use_signal(|| Phase::Opening);
    let opener = workspace.clone();
    let opening = name.clone();
    use_future(move || {
        let workspace = opener.clone();
        let name = opening.clone();
        async move {
            if !workspace.exists(&name) {
                return phase.set(Phase::Start);
            }
            let opened = async {
                Ok::<_, Failure>(Todos::new(
                    workspace.configuration(&name)?.open().await?.database,
                ))
            }
            .await;
            match opened {
                Ok(todos) => {
                    workspace.opened(&name, &todos);
                    phase.set(Phase::Open(todos));
                }
                Err(failure) => phase.set(Phase::Failed(failure.to_string())),
            }
        }
    });
    let current = phase();
    let resetting = workspace.clone();
    let reset_name = name.clone();
    let reset = move |_| {
        let workspace = resetting.clone();
        let name = reset_name.clone();
        phase.set(Phase::Opening);
        spawn(async move {
            let reset = async {
                if let Some(todos) = workspace.closed(&name) {
                    todos.database().close().await?;
                }
                let directory = workspace.root.join(&name);
                if directory.exists() {
                    tokio::task::spawn_blocking(move || std::fs::remove_dir_all(directory))
                        .await??;
                }
                Ok::<_, Failure>(())
            }
            .await;
            phase.set(match reset {
                Ok(()) => Phase::Start,
                Err(failure) => Phase::Failed(failure.to_string()),
            });
        });
    };
    rsx! {
        section { class: "card",
            match current {
                Phase::Opening => rsx! { h2 { "{name}" } p { class: "muted", "Opening…" } },
                Phase::Start => rsx! { Start { name: name.clone(), phase } },
                Phase::Failed(message) => rsx! {
                    h2 { "{name}" }
                    p { role: "alert", "{message}" }
                    Reset { onreset: reset, closing }
                },
                Phase::Open(todos) => rsx! {
                    Device { name: name.clone(), todos: Shared(todos), closing }
                    Reset { onreset: reset, closing }
                },
            }
        }
    }
}

/// Todos compared by identity, as Dioxus props must compare.
#[derive(Clone)]
struct Shared(Todos);
impl PartialEq for Shared {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(self.0.database(), other.0.database())
    }
}

/// Creates the first, trusted device or joins with an invitation code.
#[component]
fn Start(name: String, phase: Signal<Phase>) -> Element {
    let workspace = use_context::<Workspace>();
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut label = use_signal(|| title_case(&name));
    let mut code = use_signal(String::new);
    let start = move |invitation: Option<String>| {
        let workspace = workspace.clone();
        let name = name.clone();
        busy.set(true);
        spawn(async move {
            let started = async {
                let configuration = workspace.configuration(&name)?;
                let todos = match &invitation {
                    None => roles::create(&configuration, &label()).await?,
                    Some(code) => roles::join(&configuration, code, &label()).await?,
                };
                if invitation.is_some() && matches!(workspace.link, Link::Demo(_)) {
                    // In-process peers find each other only through
                    // contacts, and only within one database.
                    let contact = todos.database().session().contact().await?;
                    let scope = todos.database().scope();
                    let members = workspace.others(&name).into_iter();
                    for other in members.filter(|other| other.database().scope() == scope) {
                        other.database().session().add_peer(&contact).await?;
                        let theirs = other.database().session().contact().await?;
                        todos.database().session().add_peer(&theirs).await?;
                    }
                }
                Ok::<_, Failure>(todos)
            }
            .await;
            busy.set(false);
            match started {
                Ok(todos) => {
                    workspace.opened(&name, &todos);
                    phase.set(Phase::Open(todos));
                }
                Err(failure) => error.set(Some(failure.to_string())),
            }
        });
    };
    let mut create = start.clone();
    let mut join = start;
    rsx! {
        h2 { "{label}" }
        p { class: "muted", "No installation yet." }
        label { class: "field", "Device name"
            input { r#type: "text", value: label(), disabled: busy(), oninput: move |event| label.set(event.value()) }
        }
        div { class: "choice",
            div {
                h3 { "Start a new database" }
                p { class: "muted", "A separate database; this device becomes trusted: it invites others and seals. To share another device's lists, join instead." }
                button { disabled: busy() || label().trim().is_empty(), onclick: move |_| create(None), "Start" }
            }
            div {
                h3 { "Join a database" }
                p { class: "muted", "Paste an invitation from a trusted device. It sets the role." }
                textarea { rows: 3, placeholder: "full:rocky://…", value: code(), disabled: busy(), oninput: move |event| code.set(event.value()) }
                button { disabled: busy() || code().trim().is_empty() || label().trim().is_empty(), onclick: move |_| join(Some(code().trim().to_owned())), "Join" }
            }
        }
        if busy() { p { class: "muted", "Working…" } }
        if let Some(message) = error() { p { role: "alert", "{message}" } }
    }
}

#[component]
fn Reset(onreset: EventHandler<()>, closing: bool) -> Element {
    let mut armed = use_signal(|| false);
    rsx! {
        div { class: "reset",
            if armed() {
                span { class: "muted", "Delete this installation's data? Other devices keep theirs." }
                button { class: "danger", disabled: closing, onclick: move |_| { armed.set(false); onreset.call(()) }, "Delete" }
                button { class: "secondary", onclick: move |_| armed.set(false), "Cancel" }
            } else {
                button { class: "secondary", disabled: closing, onclick: move |_| armed.set(true), "Reset installation" }
            }
        }
    }
}

/// What an open device shows, refreshed on every committed change.
#[derive(Clone, Default, PartialEq)]
struct View {
    node: String,
    role: Option<Role>,
    members: Vec<Member>,
    lists: Vec<List>,
    list: Option<i64>,
    todos: Vec<Todo>,
    shared: BTreeSet<String>,
}
async fn load(todos: &Todos, chosen: Option<i64>) -> Result<View, Failure> {
    let node = todos.node().await?;
    let members = todos.members().await?;
    let role = members
        .iter()
        .find(|member| member.node == node)
        .map(|member| member.role);
    let lists = todos.lists().await?;
    let list = chosen
        .filter(|id| lists.iter().any(|list| list.id == *id))
        .or(lists.first().map(|list| list.id));
    let (rows, shared) = match list {
        Some(list) => (
            todos.todos(list).await?,
            todos.shared_with(list).await.unwrap_or_default(),
        ),
        None => (Vec::new(), BTreeSet::new()),
    };
    Ok(View {
        node,
        role,
        members,
        lists,
        list,
        todos: rows,
        shared,
    })
}

/// Runs one write; on success remembers its token so a later drop of it
/// is reported as this device's.
fn submit(
    mut busy: Signal<bool>,
    mut error: Signal<Option<String>>,
    mut own: Signal<BTreeSet<TransactionToken>>,
    write: impl Future<Output = Result<Option<TransactionToken>, Failure>> + 'static,
) {
    if busy() {
        return;
    }
    busy.set(true);
    spawn(async move {
        match write.await {
            Ok(token) => {
                own.write().extend(token);
                error.set(None);
            }
            Err(failure) => error.set(Some(failure.to_string())),
        }
        busy.set(false);
    });
}

fn form_text(event: &FormEvent, field: &str) -> String {
    event
        .values()
        .into_iter()
        .find_map(|(name, value)| match (name == field, value) {
            (true, FormValue::Text(value)) => Some(value),
            _ => None,
        })
        .unwrap_or_default()
}

fn title_case(name: &str) -> String {
    let mut chars = name.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

#[component]
fn Device(name: String, todos: Shared, closing: bool) -> Element {
    let todos = todos.0;
    let busy = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let own = use_signal(BTreeSet::<TransactionToken>::new);
    let mut notices = use_signal(Vec::<String>::new);
    let mut revision = use_signal(|| 0_u64);
    let mut selected = use_signal(|| None::<i64>);
    let mut share_to = use_signal(|| None::<String>);

    let observed = todos.clone();
    use_future(move || {
        let todos = observed.clone();
        async move {
            let events = match todos.database().observe_tables(None).await {
                Ok(events) => events,
                Err(failure) => return error.set(Some(failure.to_string())),
            };
            // A device that joined while this one was away gets the lists.
            let _ = todos.share_lists_with_members().await;
            loop {
                revision += 1;
                let change = match events.next().await {
                    Ok(change) => change,
                    Err(failure) => return error.set(Some(failure.to_string())),
                };
                if change.tables.iter().any(|table| table == todo_app::DEVICES)
                    && let Err(failure) = todos.share_lists_with_members().await
                {
                    error.set(Some(format!("Sharing lists: {failure}")));
                }
                for token in change.dropped {
                    let reason = match todos.dropped(token).await {
                        Ok(Some((reason, _))) => format!("{reason:?}").to_lowercase(),
                        Ok(None) => "unknown".into(),
                        Err(failure) => failure.to_string(),
                    };
                    let whose = if own.read().contains(&token) {
                        "A change made here"
                    } else {
                        "A change from another device"
                    };
                    notices
                        .write()
                        .push(format!("{whose} was dropped on replay ({reason})."));
                }
            }
        }
    });
    let reader = todos.clone();
    let view = use_resource(move || {
        let todos = reader.clone();
        let _ = revision();
        let chosen = selected();
        async move { load(&todos, chosen).await }
    });
    let (view, unreadable) = match &*view.read() {
        Some(Ok(view)) => (view.clone(), None),
        Some(Err(failure)) => (View::default(), Some(failure.to_string())),
        None => (View::default(), None),
    };
    let disabled = busy() || closing;
    let member_name = |node: &str| {
        view.members
            .iter()
            .find(|member| member.node == node)
            .map_or_else(|| short(node), |member| member.name.clone())
    };
    let label = view
        .members
        .iter()
        .find(|member| member.node == view.node)
        .map_or(name.clone(), |member| member.name.clone());
    let partials: Vec<Member> = view
        .members
        .iter()
        .filter(|member| member.role == Role::Partial && !view.shared.contains(&member.node))
        .cloned()
        .collect();
    let target = share_to()
        .filter(|node| partials.iter().any(|member| member.node == *node))
        .or(partials.first().map(|member| member.node.clone()));
    let shared_names: Vec<String> = view.shared.iter().map(|node| member_name(node)).collect();
    let can_share = view.role.is_some_and(Role::holds_all);
    let list = view.list;
    let rows = view.todos.clone();
    let creating = todos.clone();
    let adding = todos.clone();
    let removing = todos.clone();
    let sharing = todos.clone();
    let input = format!("new-title-{name}");
    let clearing = input.clone();
    rsx! {
        div { class: "device-head",
            h2 { "{label}" }
            if let Some(role) = view.role { span { class: "badge {role}", "{role}" } }
        }
        status::Sync { todos: Shared(todos.clone()), members: view.members.clone() }
        if view.role == Some(Role::Trusted) { Invite { todos: Shared(todos.clone()) } }
        Members { todos: Shared(todos.clone()), members: view.members.clone(), node: view.node.clone(), trusted: view.role == Some(Role::Trusted), closing }

        h3 { "Lists" }
        form { class: "row",
            onsubmit: move |event| {
                let todos = creating.clone();
                let title = form_text(&event, "list");
                submit(busy, error, own, async move {
                    let created = todos.create_list(&title).await?;
                    selected.set(Some(created.id));
                    Ok(created.token)
                });
            },
            select { aria_label: "List", disabled: disabled || view.lists.is_empty(),
                onchange: move |event| selected.set(event.value().parse().ok()),
                if view.lists.is_empty() { option { "No lists held here" } }
                for entry in view.lists.iter() {
                    option { value: "{entry.id}", selected: Some(entry.id) == list, "{entry.title}" }
                }
            }
            input { name: "list", r#type: "text", aria_label: "New list", placeholder: "New list", disabled }
            button { r#type: "submit", disabled, "Create" }
        }
        if view.role == Some(Role::Partial) && view.lists.is_empty() {
            p { class: "muted", "A partial device holds only the lists a trusted or full device shares with it." }
        }
        if let Some(list) = list {
            div { class: "row small",
                if can_share {
                    span { class: "muted",
                        if shared_names.is_empty() { "Not shared from here yet" } else { "Shared from here with {shared_names.join(\", \")}" }
                    }
                    if let Some(target) = target.clone() {
                        select { aria_label: "Partial device",
                            onchange: move |event| share_to.set(Some(event.value())),
                            for member in partials.iter() { option { value: "{member.node}", selected: member.node == target, "{member.name}" } }
                        }
                        button { class: "secondary", disabled,
                            onclick: move |_| {
                                let todos = sharing.clone();
                                let node = target.clone();
                                submit(busy, error, own, async move {
                                    todos.share(list, &[node]).await?;
                                    revision += 1;
                                    Ok(None)
                                });
                            },
                            "Share with partial device"
                        }
                    }
                }
                button { class: "secondary", disabled, onclick: move |_| { let todos = removing.clone(); submit(busy, error, own, async move { todos.delete_list(list).await }) }, "Delete list" }
            }
            form { class: "row",
                onsubmit: move |event| {
                    let todos = adding.clone();
                    let title = form_text(&event, "title");
                    submit(busy, error, own, async move { Ok(todos.add(list, &title).await?.token) });
                    let _ = document::eval(&format!("document.getElementById('{clearing}').value = '';"));
                },
                input { id: "{input}", name: "title", r#type: "text", aria_label: "New todo title", placeholder: "What needs doing?", disabled }
                button { r#type: "submit", disabled, "Add" }
            }
        }
        if let Some(message) = error() { p { role: "alert", "{message}" } }
        if let Some(message) = unreadable { p { role: "alert", "{message}" } }
        for notice in notices() { p { class: "notice", role: "status", "{notice}" } }
        if !notices().is_empty() { button { class: "secondary", onclick: move |_| notices.set(Vec::new()), "Dismiss notices" } }
        ul { class: "todos",
            for (position, row) in rows.iter().enumerate() {
                TodoRow { key: "{row.id}", todos: Shared(todos.clone()), row: row.clone(), position, last: position + 1 == rows.len(), disabled, busy, error, own }
            }
        }
    }
}

/// Invitation codes for new devices, one per role.
#[component]
fn Invite(todos: Shared) -> Element {
    let todos = todos.0;
    let mut code = use_signal(|| None::<String>);
    let mut error = use_signal(|| None::<String>);
    rsx! {
        details { class: "box",
            summary { "Invite a device" }
            div { class: "row",
                for role in Role::ALL {
                    button { class: "secondary",
                        onclick: { let todos = todos.clone(); move |_| { let todos = todos.clone(); spawn(async move {
                            match todos.invite(role).await { Ok(uri) => code.set(Some(uri)), Err(failure) => error.set(Some(failure.to_string())) }
                        }); } },
                        "As {role}"
                    }
                }
            }
            p { class: "muted small", "Trusted devices invite and seal. Full devices receive every list. Partial devices receive only lists shared with them. Each code admits one device; paste it into that device's Join box." }
            if let Some(uri) = code() { textarea { class: "code", readonly: true, rows: 3, value: uri } }
            if let Some(message) = error() { p { role: "alert", "{message}" } }
        }
    }
}

#[component]
fn Members(
    todos: Shared,
    members: Vec<Member>,
    node: String,
    trusted: bool,
    closing: bool,
) -> Element {
    let todos = todos.0;
    let mut confirming = use_signal(|| None::<String>);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    rsx! {
        details { class: "box",
            summary { "Devices ({members.len()})" }
            ul { class: "members",
                for member in members {
                    li {
                        span { "{member.name}" if member.node == node { " (this device)" } }
                        span { class: "badge {member.role}", "{member.role}" }
                        code { class: "muted", "{short(&member.node)}" }
                        if trusted && member.node != node {
                            span { class: "spacer" }
                            if confirming().as_deref() == Some(member.node.as_str()) {
                                button { class: "danger", disabled: busy() || closing,
                                    onclick: { let todos = todos.clone(); let target = member.node.clone(); move |_| {
                                        let todos = todos.clone();
                                        let target = target.clone();
                                        busy.set(true);
                                        spawn(async move {
                                            match todos.remove_device(&target).await {
                                                Ok(()) => error.set(None),
                                                Err(failure) => error.set(Some(failure.to_string())),
                                            }
                                            confirming.set(None);
                                            busy.set(false);
                                        });
                                    }},
                                    "Revoke"
                                }
                                button { class: "secondary", onclick: move |_| confirming.set(None), "Cancel" }
                            } else {
                                button { class: "secondary", disabled: busy() || closing,
                                    onclick: { let target = member.node.clone(); move |_| confirming.set(Some(target.clone())) },
                                    "Remove"
                                }
                            }
                        }
                    }
                }
            }
            if trusted {
                p { class: "muted small", "Removing revokes the device: it can no longer sync, and lists are no longer shared with it. Its local copy stays on that device." }
            }
            if let Some(message) = error() { p { role: "alert", "{message}" } }
        }
    }
}

pub(crate) fn short(node: &str) -> String {
    node.chars().take(8).collect()
}

#[component]
fn TodoRow(
    todos: Shared,
    row: Todo,
    position: usize,
    last: bool,
    disabled: bool,
    busy: Signal<bool>,
    error: Signal<Option<String>>,
    own: Signal<BTreeSet<TransactionToken>>,
) -> Element {
    let todos = todos.0;
    let (list, id) = (row.list, row.id);
    let write = move |action: Action| {
        let todos = todos.clone();
        submit(busy, error, own, async move {
            match action {
                Action::Complete(done) => todos.set_completed(list, id, done).await,
                Action::Rename(title) => todos.rename(list, id, &title).await,
                Action::Raise => todos.raise_priority(list, id).await,
                Action::Move(to) => todos.move_to(list, id, to).await,
                Action::Delete => todos.delete(list, id).await,
            }
        });
    };
    let write_rename = write.clone();
    let write_up = write.clone();
    let write_down = write.clone();
    let write_raise = write.clone();
    let write_delete = write.clone();
    let write_complete = write;
    rsx! {
        li { class: if row.completed { "todo done" } else { "todo" }, "data-todo": "{row.id}",
            input { r#type: "checkbox", aria_label: "Complete {row.title}", checked: row.completed, disabled,
                onchange: move |event| write_complete(Action::Complete(event.checked())) }
            form { onsubmit: move |event| write_rename(Action::Rename(form_text(&event, "title"))),
                input { key: "{row.title}", name: "title", r#type: "text", aria_label: "Rename {row.title}", initial_value: row.title.clone(), disabled }
            }
            button { class: "icon", title: "Raise priority", aria_label: "Raise priority of {row.title}", disabled, onclick: move |_| write_raise(Action::Raise), "▲ {row.priority}" }
            button { class: "icon", title: "Move up", aria_label: "Move {row.title} up", disabled: disabled || position == 0, onclick: move |_| write_up(Action::Move(position.saturating_sub(1))), "↑" }
            button { class: "icon", title: "Move down", aria_label: "Move {row.title} down", disabled: disabled || last, onclick: move |_| write_down(Action::Move(position + 1)), "↓" }
            button { class: "icon", title: "Delete", aria_label: "Delete {row.title}", disabled, onclick: move |_| write_delete(Action::Delete), "✕" }
        }
    }
}

#[derive(Clone)]
enum Action {
    Complete(bool),
    Rename(String),
    Raise,
    /// The index among the list's other todos.
    Move(usize),
    Delete,
}
