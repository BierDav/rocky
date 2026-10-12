//! A desktop peer for checking the mobile DNS-SD backend before the phone-to-phone gate.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let probe = rocky_discovery_probe::Probe::new()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut previous = String::new();
    loop {
        let status = probe.status();
        if status != previous {
            println!("{status}");
            previous = status.clone();
        }
        let value: serde_json::Value = serde_json::from_str(&status)?;
        if value["stage"] == "passed" {
            probe.shutdown()?;
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            probe.shutdown()?;
            return Err("no authenticated local peer within 30 seconds".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
