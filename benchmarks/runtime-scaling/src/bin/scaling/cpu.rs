//! Safe process- and thread-CPU sampling for probes. No unsafe code: Linux
//! reads `/proc`, macOS shells out to `ps`. CPU time is independent of wall
//! time, so the difference is what the run actually burned on processors.
use std::time::Duration;

/// Total CPU time (user + system) consumed by this process so far.
pub(super) fn process() -> Option<Duration> {
    #[cfg(target_os = "linux")]
    {
        linux::process()
    }
    #[cfg(target_os = "macos")]
    {
        macos::process()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// CPU time of every thread whose name starts with `prefix`, summed. Names are
/// readable only on Linux (`/proc/self/task/*/comm`); elsewhere this returns
/// `None` rather than guess from unordered per-thread rows.
pub(super) fn threads(prefix: &str) -> Option<Duration> {
    #[cfg(target_os = "linux")]
    {
        linux::threads(prefix)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = prefix;
        None
    }
}

/// Parses the macOS `ps -o time=` format: `[D-]HH:MM:SS[.ff]` or `MM:SS.ff`.
#[cfg(any(target_os = "macos", test))]
fn parse_ps_time(text: &str) -> Option<Duration> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let (days, clock) = match text.split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, text),
    };
    let mut fields = clock.split(':');
    let (hours, minutes, seconds) = match (fields.next(), fields.next(), fields.next()) {
        (Some(h), Some(m), Some(s)) => (h.parse::<u64>().ok()?, m.parse::<u64>().ok()?, s),
        (Some(m), Some(s), None) => (0, m.parse::<u64>().ok()?, s),
        _ => return None,
    };
    let seconds = seconds.parse::<f64>().ok()?;
    Some(Duration::from_secs_f64(
        days as f64 * 86_400.0 + hours as f64 * 3_600.0 + minutes as f64 * 60.0 + seconds,
    ))
}

/// Parses the CPU fields of `/proc/<pid>/stat` or `/proc/self/task/<tid>/stat`
/// (fields 14 and 15, user and system ticks) at the standard 100 Hz USER_HZ.
#[cfg(any(target_os = "linux", test))]
fn parse_stat_cpu(stat: &str) -> Option<Duration> {
    // The comm field is parenthesized and may hold spaces; fields resume after
    // the last ')'.
    let rest = stat.rsplit_once(')')?.1;
    let mut fields = rest.split_whitespace();
    // rest starts after the comm field's ')', so field 3 ('state') is first.
    let _state = fields.next()?;
    for _ in 4..14 {
        fields.next()?;
    }
    let user: u64 = fields.next()?.parse().ok()?;
    let system: u64 = fields.next()?.parse().ok()?;
    Some(Duration::from_millis((user + system) * 10))
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{Duration, parse_ps_time};

    pub(super) fn process() -> Option<Duration> {
        let output = std::process::Command::new("ps")
            .args(["-o", "time=", "-p", &std::process::id().to_string()])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        parse_ps_time(&String::from_utf8(output.stdout).ok()?)
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{Duration, parse_stat_cpu};

    fn stat(path: &str) -> Option<Duration> {
        parse_stat_cpu(&std::fs::read_to_string(path).ok()?)
    }

    pub(super) fn process() -> Option<Duration> {
        stat("/proc/self/stat")
    }

    pub(super) fn threads(prefix: &str) -> Option<Duration> {
        let mut total = Duration::ZERO;
        let mut any = false;
        for entry in std::fs::read_dir("/proc/self/task").ok()? {
            let entry = entry.ok()?;
            let comm = std::fs::read_to_string(entry.path().join("comm")).ok()?;
            if !comm.trim_start().starts_with(prefix) {
                continue;
            }
            let path = entry.path().join("stat");
            if let Some(cpu) = path.to_str().and_then(stat) {
                total += cpu;
                any = true;
            }
        }
        any.then_some(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ps_time_forms() {
        assert_eq!(parse_ps_time("0:00.01"), Some(Duration::from_millis(10)));
        assert_eq!(
            parse_ps_time("1:02.50"),
            Some(Duration::from_secs(62) + Duration::from_millis(500))
        );
        assert_eq!(parse_ps_time("2:03:04"), Some(Duration::from_secs(7_384)));
        assert_eq!(
            parse_ps_time("1-00:00:00"),
            Some(Duration::from_secs(86_400))
        );
        assert_eq!(parse_ps_time(""), None);
    }

    #[test]
    fn parses_stat_cpu_ticks() {
        // pid (comm) state ppid ... utime stime: 150 ticks each at 100 Hz.
        let stat = "42 (a b) R 1 42 42 0 -1 4194560 0 0 0 0 150 150 0 0 20 0 1 0 5 0 0";
        assert_eq!(parse_stat_cpu(stat), Some(Duration::from_secs(3)));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reads_this_process_cpu() {
        assert!(process().is_some());
    }
}
