//! Drives the production two-panel WebView: `offline` (default), `reopen` after it, or `publication`.
use dioxus::desktop::{Config, WindowCloseBehaviour};
use dioxus::prelude::*;
use std::path::PathBuf;
use todo_desktop::{App, Demo};

fn main() {
    let _ = dioxus::logger::init(dioxus::logger::tracing::Level::WARN);
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .expect("data directory argument");
    let scenario = std::env::args().nth(2).unwrap_or_else(|| "offline".into());
    assert!(
        ["offline", "reopen", "publication"].contains(&scenario.as_str()),
        "unknown scenario {scenario}"
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let demo = runtime.block_on(Demo::open(&path)).unwrap();
    dioxus::LaunchBuilder::desktop()
        .with_context(demo)
        .with_context(Scenario(scenario))
        .with_cfg(
            Config::new()
                .with_close_behaviour(WindowCloseBehaviour::WindowHides)
                .with_menu(None),
        )
        .launch(Smoke);
}
#[derive(Clone)]
struct Scenario(String);
#[component]
fn Smoke() -> Element {
    let demo = use_context::<Demo>();
    let scenario = use_context::<Scenario>().0;
    let desktop = dioxus::desktop::use_window();
    use_future(move || {
        let demo = demo.clone();
        let scenario = scenario.clone();
        let desktop = desktop.clone();
        async move {
            let mut eval = document::eval(include_str!("native_smoke.js"));
            eval.send(scenario.as_str()).unwrap();
            let result: String = eval.recv().await.unwrap();
            assert_eq!(result, "passed", "native UI scenario: {result}");
            let laptop = demo.replicas[0].query().await.unwrap();
            let phone = demo.replicas[1].query().await.unwrap();
            let message = match scenario.as_str() {
                "reopen" => {
                    assert_eq!(laptop.rows, phone.rows);
                    assert_eq!(laptop.rows.len(), 2);
                    assert_eq!(laptop.rows[0].title, "Phone competing title");
                    assert!(laptop.rows[0].completed);
                    assert_eq!(laptop.rows[1].title, "Phone survivor");
                    assert_eq!(
                        demo.replicas[0].sync_state().await.unwrap(),
                        demo.replicas[1].sync_state().await.unwrap()
                    );
                    "Native two-replica UI reopen and convergence passed"
                }
                "publication" => {
                    assert_eq!(laptop.rows, phone.rows);
                    let publications = demo.replicas[0].confirmed_publications().await.unwrap();
                    assert_eq!(
                        publications,
                        demo.replicas[1].confirmed_publications().await.unwrap()
                    );
                    assert_eq!(publications.len(), 3);
                    assert_eq!(
                        todo_app::decode_snapshot(&publications[1].package.rows)
                            .unwrap()
                            .len(),
                        1
                    );
                    "Native strong publication: immutable rows, outage recovery, peer application and Phone takeover passed"
                }
                _ => {
                    assert_eq!(laptop.rows.len(), 1);
                    assert_eq!(laptop.rows[0].title, "Laptop competing title");
                    assert_eq!(phone.rows.len(), 2);
                    "Native two-replica UI offline actions passed"
                }
            };
            demo.close().await.unwrap();
            eprintln!("{message}");
            desktop.set_close_behavior(WindowCloseBehaviour::WindowCloses);
            desktop.close();
        }
    });
    rsx! { App {} }
}
