mod agent;
mod local;
mod net;
mod retry;
mod schema;
mod stats;

use rocky_sdk::Failure;

const USAGE: &str =
    "usage: rocky-mesh-bench agent | local <dir> <single-ops> <batch-rows> <batches> <body-bytes>";

fn main() -> Result<(), Failure> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["agent"] => runtime.block_on(agent::run()),
        ["local", dir, single, batch_rows, batches, body] => {
            runtime.block_on(local::run(local::Plan {
                directory: dir.into(),
                single: single.parse()?,
                batch_rows: batch_rows.parse()?,
                batches: batches.parse()?,
                body: body.parse()?,
            }))
        }
        _ => Err(USAGE.into()),
    };
    runtime.shutdown_timeout(std::time::Duration::from_secs(10));
    result
}
