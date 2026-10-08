# Interrupted local storage soak

The original local process and its parent disappeared without a recorded exit
status. The terminal tool session no longer exists. The cause is unconfirmed;
no benchmark task error is present in the log. Last interval: 2,400 seconds and
2,945,263 main operations. This does not pass the three-hour soak.

An independent recovery audit passed for all three domains. Its applied frontier
includes writes after the last interval; the interruption prevented the runner's
final workload/acknowledgement checks. Recovery does not qualify this run.

The original profile label said Rust 1.95.0, but captured `rustc` reports the
Homebrew 1.99.0 toolchain. Treat that label as incorrect, and do not assume the
local binary uses the remote builds' compiler. The replacement run preserves the
same binary hash, labels the compiler discrepancy, and runs as a launchd job.
