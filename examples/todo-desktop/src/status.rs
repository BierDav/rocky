use crate::{Shared, short};
use dioxus::prelude::*;
use rocky_sdk::{DatabaseStatus, SyncMode, Timestamp};
use std::time::{Duration, SystemTime};
use todo_app::Member;

const REFRESH: Duration = Duration::from_secs(1);

#[component]
pub(crate) fn Sync(todos: Shared, members: Vec<Member>) -> Element {
    let todos = todos.0;
    let mut status = use_signal(|| None::<DatabaseStatus>);
    let mut error = use_signal(|| None::<String>);
    let mut syncing = use_signal(|| false);
    let polled = todos.clone();
    use_future(move || {
        let todos = polled.clone();
        async move {
            let database = todos.database().clone();
            loop {
                match database.status().await {
                    Ok(current) => {
                        status.set(Some(current));
                        error.set(None);
                    }
                    Err(failure) => error.set(Some(failure.to_string())),
                }
                tokio::time::sleep(REFRESH).await;
            }
        }
    });
    let Some(current) = status() else {
        return rsx! { div { class: "box muted", "Reading sync status…" } };
    };
    let online = current.sync_mode() != SyncMode::Off;
    let peers = current.peers();
    let connected = peers.iter().filter(|peer| peer.connected).count();
    let name = |node: &str| {
        members
            .iter()
            .find(|member| member.node == node)
            .map_or_else(
                || short(node),
                |member| format!("{} ({})", member.name, member.role),
            )
    };
    let last_pass = current
        .last_sync()
        .and_then(|report| report.ended_at())
        .map(ago);
    let blockers = current.blockers().len();
    let state = if !online {
        ("offline", "Offline")
    } else if current.syncing() || syncing() {
        ("busy", "Syncing")
    } else if connected > 0 {
        ("ok", "Online")
    } else {
        ("idle", "No peer connected")
    };
    let toggling = todos.clone();
    let syncer = todos.clone();
    rsx! {
        div { class: "box sync",
            div { class: "sync-title",
                span { class: "dot {state.0}" }
                strong { "{state.1}" }
            }
            p { class: "muted small sync-line",
                "{connected} of {peers.len()} peers connected"
                if let Some(last) = last_pass { " · last pass {last}" }
            }
            div { class: "row",
                button { class: "secondary", disabled: !online || syncing(),
                    onclick: move |_| { let todos = syncer.clone(); syncing.set(true); spawn(async move {
                        if let Err(failure) = todos.database().sync_now().await { error.set(Some(failure.to_string())) }
                        syncing.set(false);
                    }); },
                    "Sync now"
                }
                button { class: "secondary",
                    onclick: move |_| { let todos = toggling.clone(); spawn(async move {
                        if let Err(failure) = todos.database().set_sync(if online { SyncMode::Off } else { SyncMode::Live }).await { error.set(Some(failure.to_string())) }
                    }); },
                    if online { "Go offline" } else { "Go online" }
                }
            }
            if blockers > 0 || current.bootstrapping() {
                p { class: "muted small",
                    if current.bootstrapping() { "Bootstrapping from a peer. " }
                    if blockers > 0 { "{blockers} scopes still wait for keys or data." }
                }
            }
            if !peers.is_empty() {
                table { class: "peers small",
                    thead { tr { th { "Peer" } th { "Link" } th { "Last exchange" } } }
                    tbody {
                        for peer in peers.iter() {
                            tr {
                                td { "{name(&peer.device.to_string())}" }
                                td { if peer.connected { "open" } else { "closed" } }
                                td { {peer.last_exchange.map_or("never".into(), ago)} }
                            }
                        }
                    }
                }
            }
            if let Some(message) = error() { p { role: "alert", "{message}" } }
        }
    }
}

fn ago(at: Timestamp) -> String {
    let elapsed = at
        .system_time()
        .and_then(|at| SystemTime::now().duration_since(at).ok())
        .map_or(0, |elapsed| elapsed.as_secs());
    match elapsed {
        0..=2 => "just now".into(),
        elapsed @ 3..=119 => format!("{elapsed} s ago"),
        elapsed @ 120..=7199 => format!("{} min ago", elapsed / 60),
        elapsed => format!("{} h ago", elapsed / 3600),
    }
}
