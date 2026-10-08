//! Always-on custody launcher; configuration and runbook are in docs.
use rocky_sdk::{CustodyPeer, Failure};

fn main() -> Result<(), Failure> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let [config] = args.as_slice() else {
        return Err("usage: rocky-peer <configuration.json>".into());
    };
    let peer = CustodyPeer::open(&std::fs::read_to_string(config)?)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        println!("{}", peer.contact().await?);
        #[cfg(unix)]
        {
            let mut terminate =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            tokio::select! {
                result = tokio::signal::ctrl_c() => result?,
                _ = terminate.recv() => {},
            }
        }
        #[cfg(not(unix))]
        tokio::signal::ctrl_c().await?;
        peer.close().await
    })
}
