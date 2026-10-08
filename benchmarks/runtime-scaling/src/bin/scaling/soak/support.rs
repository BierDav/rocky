use super::{AtomicU64, Durability, Duration, Failure, PathBuf};

pub struct Config {
    pub(super) directory: PathBuf,
    pub(super) seconds: u64,
    pub(super) operations: u64,
    pub(super) rows: usize,
    pub(super) tables: u16,
    pub(super) observers: usize,
    pub(super) interval: u64,
    pub(super) durability: Durability,
    pub(super) instrument: bool,
}
impl Config {
    pub fn parse(args: impl Iterator<Item = String>) -> Result<Self, Failure> {
        let mut config = Self {
            directory: "local/scale-soak".into(),
            seconds: 10_800,
            operations: 5_000_000,
            rows: 500_000,
            tables: 2000,
            observers: 100,
            interval: 60,
            durability: Durability::Full,
            instrument: false,
        };
        let mut args = args;
        while let Some(flag) = args.next() {
            if flag == "--instrument" {
                config.instrument = true;
                continue;
            }
            let value = args
                .next()
                .ok_or_else(|| format!("missing value for {flag}"))?;
            match flag.as_str() {
                "--directory" => config.directory = value.into(),
                "--seconds" => config.seconds = value.parse()?,
                "--operations" => config.operations = value.parse()?,
                "--rows" => config.rows = value.parse()?,
                "--tables" => config.tables = value.parse()?,
                "--observers" => config.observers = value.parse()?,
                "--interval" => config.interval = value.parse()?,
                "--durability" => {
                    config.durability = match value.as_str() {
                        "full" => Durability::Full,
                        "buffered" => Durability::Buffered,
                        _ => return Err("durability must be full or buffered".into()),
                    }
                }
                _ => return Err(format!("unknown soak option {flag}").into()),
            }
        }
        if config.rows < 16
            || !(1..=2000).contains(&config.tables)
            || config.operations < config.rows as u64
            || config.interval == 0
            || config.observers > 120
        {
            return Err("require rows >= 16, operations >= rows, tables 1..=2000, interval > 0, observers <= 120".into());
        }
        Ok(config)
    }
}
// Logarithmic buckets keep memory independent of the duration. The final
// bucket has no finite upper bound; percentiles there report the exact max.
pub(super) struct Latency {
    buckets: [u64; 64],
    pub(super) count: u64,
    pub(super) max_us: u64,
}
impl Latency {
    pub(super) fn new() -> Self {
        Self {
            buckets: [0; 64],
            count: 0,
            max_us: 0,
        }
    }
    pub(super) fn record(&mut self, elapsed: Duration) {
        let us = (elapsed.as_micros() as u64).max(1);
        let bucket = (64 - us.leading_zeros()) as usize;
        self.buckets[bucket.min(63)] += 1;
        self.count += 1;
        self.max_us = self.max_us.max(us);
    }
    pub(super) fn percentile(&self, percent: u64) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let target = (self.count * percent).div_ceil(100);
        let mut seen = 0;
        for (bucket, count) in self.buckets.iter().enumerate() {
            seen += count;
            if seen >= target {
                return if bucket == 63 {
                    self.max_us
                } else {
                    self.max_us.min((1u64 << bucket).saturating_sub(1))
                };
            }
        }
        self.max_us
    }
}
#[derive(Default)]
pub(super) struct Counters {
    pub(super) writes: AtomicU64,
    pub(super) creates: AtomicU64,
    pub(super) deletes: AtomicU64,
    pub(super) observations: AtomicU64,
}
