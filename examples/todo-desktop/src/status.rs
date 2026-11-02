//! A device's sync status: networking, each peer's link and last exchange,
//! and what still blocks local data. It also drives periodic sync passes.
use crate::{Shared, short};
use dioxus::prelude::*;
use rocky_sdk::DatabaseStatus;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use todo_app::Member;

/// How often the status refreshes and an online device syncs.
const REFRESH: Duration = Duration::from_secs(1);
const SYNC_EVERY: u32 = 3;

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
            let session = todos.database().session();
            let mut tick: u32 = 0;
            loop {
                tick = tick.wrapping_add(1);
                match session.status().await {
                    Ok(current) => {
                        let online = current.network.networking_enabled;
                        status.set(Some(current));
                        error.set(None);
                        // Peers on the in-process link are only dialed by a pass.
                        if online && tick.is_multiple_of(SYNC_EVERY) && !syncing() {
                            let _ = session.sync_once().await;
                        }
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
    let network = &current.network;
    let online = network.networking_enabled;
    let name = |node: &str| {
        members
            .iter()
            .find(|member| member.node == node)
            .map_or_else(
                || short(node),
                |member| format!("{} ({})", member.name, member.role),
            )
    };
    let last_pass = network
        .last_reconciliation
        .as_ref()
        .and_then(|report| report.ended_at_epoch_millis)
        .map(|millis| ago(millis / 1000));
    let blockers = current.data.blockers.len();
    let state = if !online {
        ("offline", "Offline")
    } else if network.reconciling || syncing() {
        ("busy", "Syncing")
    } else if network.connected_peer_count > 0 {
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
                "{network.connected_peer_count} of {network.peers.len()} peers connected"
                if let Some(last) = last_pass { " · last pass {last}" }
            }
            div { class: "row",
                button { class: "secondary", disabled: !online || syncing(),
                    onclick: move |_| { let todos = syncer.clone(); syncing.set(true); spawn(async move {
                        if let Err(failure) = todos.database().session().sync_once().await { error.set(Some(failure.to_string())) }
                        syncing.set(false);
                    }); },
                    "Sync now"
                }
                button { class: "secondary",
                    onclick: move |_| { let todos = toggling.clone(); spawn(async move {
                        if let Err(failure) = todos.database().session().set_networking_enabled(!online).await { error.set(Some(failure.to_string())) }
                    }); },
                    if online { "Go offline" } else { "Go online" }
                }
            }
            if blockers > 0 || current.data.bootstrap_pending {
                p { class: "muted small",
                    if current.data.bootstrap_pending { "Bootstrapping from a peer. " }
                    if blockers > 0 { "{blockers} scopes still wait for keys or data." }
                }
            }
            if !network.peers.is_empty() {
                table { class: "peers small",
                    thead { tr { th { "Peer" } th { "Link" } th { "Round trip" } th { "Last exchange" } } }
                    tbody {
                        for peer in network.peers.iter() {
                            tr {
                                td { "{name(&peer.node)}" }
                                td { {format!("{:?}", peer.link.state).to_lowercase()} }
                                td { {peer.link.round_trip_millis.map_or("—".into(), |millis| format!("{millis} ms"))} }
                                td { {peer.last_authenticated_exchange_at_epoch_seconds.map_or("never".into(), ago)} }
                            }
                        }
                    }
                }
            }
            if let Some(message) = error() { p { role: "alert", "{message}" } }
        }
    }
}

fn ago(seconds: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(seconds, |now| now.as_secs());
    match now.saturating_sub(seconds) {
        0..=2 => "just now".into(),
        elapsed @ 3..=119 => format!("{elapsed} s ago"),
        elapsed @ 120..=7199 => format!("{} min ago", elapsed / 60),
        elapsed => format!("{} h ago", elapsed / 3600),
    }
}
