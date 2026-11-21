use serde_json::{Value, json};

pub fn summary(samples: &mut [f64]) -> Value {
    if samples.is_empty() {
        return json!({"n": 0});
    }
    samples.sort_by(f64::total_cmp);
    let at = |quantile: f64| {
        let index = ((samples.len() - 1) as f64 * quantile).round() as usize;
        samples[index]
    };
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    json!({
        "n": samples.len(),
        "mean": mean,
        "min": samples[0],
        "p50": at(0.5),
        "p90": at(0.9),
        "p99": at(0.99),
        "max": samples[samples.len() - 1],
    })
}
