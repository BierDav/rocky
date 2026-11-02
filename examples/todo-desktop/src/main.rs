use dioxus::desktop::muda::{Menu, PredefinedMenuItem, Submenu};
use dioxus::desktop::{
    Config, WindowCloseBehaviour,
    tao::{dpi::LogicalSize, window::WindowBuilder},
};
use rocky_sdk::transport::TransportOptions;
use std::path::PathBuf;
use todo_desktop::{App, Launch};

const USAGE: &str = "usage: todo-desktop [--device] [--data-dir PATH] [--lan [ADDRESS]]

Without --device, several installations share one window over an
in-process link. --device keeps one installation that syncs over iroh;
--lan disables the relay and lookup service and finds peers on the local
network only, optionally binding ADDRESS. Start, join and reset
installations in the window.";

fn launch() -> Result<Launch, String> {
    let mut root = None;
    let mut demo = true;
    let mut transport = TransportOptions::default();
    let mut args = std::env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => root = Some(PathBuf::from(args.next().ok_or(USAGE)?)),
            "--device" => demo = false,
            "--lan" => {
                transport = TransportOptions::Iroh {
                    relay: Some("disabled".into()),
                    lookup: Some("disabled".into()),
                    topic: false,
                    local: true,
                    bind: args.next_if(|next| !next.starts_with("--")),
                };
            }
            _ => return Err(USAGE.into()),
        }
    }
    let root = root.unwrap_or_else(|| {
        PathBuf::from(if demo {
            "local/todo-demo"
        } else {
            "local/todo"
        })
    });
    Ok(Launch {
        root,
        demo,
        transport,
    })
}

/// Window and Edit menus: on macOS the Edit items carry the cut, copy,
/// paste, undo and select-all shortcuts.
fn menu() -> Result<Menu, dioxus::desktop::muda::Error> {
    let window = Submenu::with_items(
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(None),
            &PredefinedMenuItem::fullscreen(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::close_window(None),
            &PredefinedMenuItem::quit(None),
        ],
    )?;
    let edit = Submenu::with_items(
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(None),
            &PredefinedMenuItem::redo(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::cut(None),
            &PredefinedMenuItem::copy(None),
            &PredefinedMenuItem::paste(None),
            &PredefinedMenuItem::select_all(None),
        ],
    )?;
    Menu::with_items(&[&window, &edit])
}

fn main() {
    let _ = dioxus::logger::init(dioxus::logger::tracing::Level::WARN);
    let result = (|| {
        let launch = launch()?;
        std::fs::create_dir_all(&launch.root)
            .map_err(|e| format!("Cannot create {}: {e}", launch.root.display()))?;
        let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
        let config = Config::new()
            .with_window(
                WindowBuilder::new()
                    .with_title("Rocky Todo")
                    .with_inner_size(LogicalSize::new(1360.0, 860.0)),
            )
            .with_menu(menu().map_err(|e| e.to_string())?)
            .with_close_behaviour(WindowCloseBehaviour::WindowHides);
        // Dioxus drives its own executor; the SDK's calls need this runtime.
        let _guard = runtime.enter();
        dioxus::LaunchBuilder::desktop()
            .with_context(launch)
            .with_cfg(config)
            .launch(App);
        Ok::<(), String>(())
    })();
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
