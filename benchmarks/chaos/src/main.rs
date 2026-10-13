mod abuse;
mod cluster;
mod extra;
mod fuzz;
mod node;
mod probes;
mod scenarios;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    match args.first().map(String::as_str) {
        Some("node") => runtime.block_on(node::run(args[1].clone())),
        Some("scenario") => runtime.block_on(scenarios::run(&args[1..])),
        Some("fuzz") => runtime.block_on(fuzz::run(&args[1..])),
        _ => Err(
            "usage: rocky-chaos node <options> | scenario <name...|all> | fuzz <seed> <steps>"
                .into(),
        ),
    }
}
