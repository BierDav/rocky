use dioxus::prelude::*;
use todo_desktop::secure_demo::SecureDemo;

#[component]
fn App() -> Element {
    let demo = use_context::<SecureDemo>();
    let mut name = use_signal(|| "Alice".to_string());
    let mut salary = use_signal(|| "75000".to_string());
    let mut public_name = use_signal(|| "Alicia".to_string());
    let mut error = use_signal(|| None::<String>);
    let mut busy = use_signal(|| false);
    let mut refresh = use_signal(|| 0u64);
    let query_demo = demo.clone();
    let view = use_resource(move || {
        let demo = query_demo.clone();
        let _revision = refresh();
        async move {
            let public = demo.editor.snapshot(demo.scopes[0]).await?;
            let confirmed = demo.editor.snapshot(demo.scopes[0]).await?;
            let private = demo.admin.snapshot(demo.scopes[1]).await?;
            let row = demo.employee_id();
            let name = public
                .rows
                .get(&row)
                .or_else(|| confirmed.rows.get(&row))
                .and_then(|fields| fields.get(&demo.app().field("display_name")))
                .map(|bytes| String::from_utf8_lossy(bytes).to_string())
                .unwrap_or_else(|| "Unavailable".into());
            let salary = private
                .rows
                .get(&row)
                .and_then(|fields| fields.get(&demo.app().field("salary")))
                .map(|bytes| String::from_utf8_lossy(bytes).to_string())
                .unwrap_or_else(|| "Unavailable".into());
            Ok::<_, rocky_runtime::Error>((name, salary, confirmed.complete))
        }
    });
    let admin = demo.clone();
    let editor = demo.clone();
    let sync = demo.clone();
    rsx! {
        main { style: "font-family: system-ui; padding: 32px; max-width: 900px; margin: auto;",
            h1 { "Employees · encrypted native sync" }
            p { "Development credentials · two clients and an opaque relay · sync is manual" }
            div { style: "display: flex; gap: 48px;",
                section {
                    h2 { "Admin" }
                    label { "Name " input { value: "{name}", oninput: move |event| name.set(event.value()) } }
                    br {}
                    label { "Salary " input { value: "{salary}", oninput: move |event| salary.set(event.value()) } }
                    br {}
                    button { disabled: busy(), onclick: move |_| {
                        let demo = admin.clone(); let name = name(); let salary = salary(); busy.set(true); error.set(None);
                        spawn(async move { if let Err(problem) = demo.set_employee(&name, &salary).await { error.set(Some(problem.to_string())); } busy.set(false); refresh += 1; });
                    }, "Confirm employee" }
                }
                section {
                    h2 { "Public editor" }
                    input { value: "{public_name}", oninput: move |event| public_name.set(event.value()) }
                    button { disabled: busy(), onclick: move |_| {
                        let demo = editor.clone(); let name = public_name(); busy.set(true); error.set(None);
                        spawn(async move { if let Err(problem) = demo.edit_public_name(&name).await { error.set(Some(problem.to_string())); } busy.set(false); refresh += 1; });
                    }, "Save public name offline" }
                    p { "Payroll read and write access: denied" }
                }
            }
            button { disabled: busy(), onclick: move |_| {
                let demo = sync.clone(); busy.set(true); error.set(None);
                spawn(async move { if let Err(problem) = demo.pump().await { error.set(Some(problem.to_string())); } busy.set(false); refresh += 1; });
            }, "Sync through relay (native TCP)" }
            match &*view.read_unchecked() {
                Some(Ok((name, salary, complete))) => rsx! { p { "Editor name: {name}" } p { "Admin payroll: {salary}" } p { "Local confirmed public scope complete: {complete}" } },
                Some(Err(problem)) => rsx! { p { "{problem}" } },
                None => rsx! { p { "Loading…" } },
            }
            if let Some(problem) = error() { p { style: "color: #b00020", "{problem}" } }
        }
    }
}
fn main() {
    let result = (|| {
        let mut args = std::env::args_os().skip(1);
        let dir = match args.next() {
            None => std::path::PathBuf::from("local/employees-demo"),
            Some(flag) if flag == "--data-dir" => {
                std::path::PathBuf::from(args.next().ok_or("--data-dir requires a path")?)
            }
            _ => return Err("usage: employees-desktop [--data-dir PATH]".into()),
        };
        if args.next().is_some() {
            return Err("usage: employees-desktop [--data-dir PATH]".into());
        }
        let runtime = tokio::runtime::Runtime::new().map_err(|problem| problem.to_string())?;
        let demo = runtime
            .block_on(SecureDemo::open(&dir))
            .map_err(|problem| problem.to_string())?;
        let _guard = runtime.enter();
        dioxus::LaunchBuilder::desktop()
            .with_context(demo)
            .launch(App);
        Ok::<_, String>(())
    })();
    if let Err(problem) = result {
        eprintln!("{problem}");
        std::process::exit(1);
    }
}
