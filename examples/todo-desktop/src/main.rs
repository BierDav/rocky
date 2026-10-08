use dioxus::desktop::{
    Config, WindowCloseBehaviour,
    tao::{dpi::LogicalSize, window::WindowBuilder},
};
use std::path::PathBuf;
use todo_desktop::{App, Demo};

fn data_dir() -> Result<PathBuf, String> {
    let mut args = std::env::args_os().skip(1);
    match args.next() {
        None => Ok(PathBuf::from("local/todo-demo")),
        Some(flag) if flag == "--data-dir" => {
            let path = args.next().ok_or("--data-dir requires a path")?;
            if args.next().is_some() {
                return Err("usage: todo-desktop [--data-dir PATH]".into());
            }
            Ok(PathBuf::from(path))
        }
        _ => Err("usage: todo-desktop [--data-dir PATH]".into()),
    }
}
fn main() {
    let _ = dioxus::logger::init(dioxus::logger::tracing::Level::WARN);
    let result = (|| {
        let dir = data_dir()?;
        let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
        let demo = runtime
            .block_on(Demo::open(&dir))
            .map_err(|e| format!("Cannot open {}: {e}", dir.display()))?;
        let config = Config::new()
            .with_window(
                WindowBuilder::new()
                    .with_title("Rocky Todo — Laptop and Phone")
                    .with_inner_size(LogicalSize::new(1200.0, 600.0)),
            )
            .with_menu(None)
            .with_close_behaviour(WindowCloseBehaviour::WindowHides);
        dioxus::LaunchBuilder::desktop()
            .with_context(demo)
            .with_cfg(config)
            .launch(App);
        Ok::<(), String>(())
    })();
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
