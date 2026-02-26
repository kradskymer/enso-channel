//! Shared benchmark configuration helpers used by enso_channel benches.
#![allow(dead_code)]

use std::env;
use std::time::Duration;

/// Parse a comma/space/semicolon-separated list of positive usize values from an
/// environment variable. Returns `default` if the env var is missing or parsing
/// yields no values.
pub fn parse_usize_list(env_key: &str, default: &[usize]) -> Vec<usize> {
    let Ok(value) = env::var(env_key) else {
        return default.to_vec();
    };

    let parsed: Vec<usize> = value
        .split([',', ' ', ';'])
        .filter_map(|s| s.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .collect();

    if parsed.is_empty() {
        default.to_vec()
    } else {
        parsed
    }
}

/// Parse a single positive usize env var with a default fallback.
pub fn parse_usize(env_key: &str, default: usize) -> usize {
    env::var(env_key)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

/// Default capacity list used by several benches.
pub fn get_capacities() -> Vec<usize> {
    parse_usize_list("ENSO_CHANNEL_BENCH_CAPACITY", &[512, 1024, 4096])
}

/// Whether the long-mode is enabled.
pub fn is_long_mode() -> bool {
    match env::var("ENSO_CHANNEL_BENCH_LONG") {
        Ok(v) => matches!(v.as_str(), "1" | "true" | "TRUE" | "True"),
        Err(_) => false,
    }
}

/// Pick message count based on short/long defaults and optional env overrides.
pub fn get_message_count(short_default: usize, long_default: usize) -> usize {
    if is_long_mode() {
        env::var("ENSO_CHANNEL_BENCH_MESSAGES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(long_default)
    } else {
        env::var("ENSO_CHANNEL_BENCH_SHORT_MESSAGES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(short_default)
    }
}

/// Warmup duration using millisecond defaults.
pub fn get_warmup_time(short_default_ms: u64, long_default_ms: u64) -> Duration {
    let default_ms = if is_long_mode() {
        long_default_ms
    } else {
        short_default_ms
    };
    env::var("ENSO_CHANNEL_BENCH_WARMUP_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_millis(default_ms))
}

/// Measurement duration using seconds defaults.
pub fn get_measurement_time(short_default_s: u64, long_default_s: u64) -> Duration {
    let default_secs = if is_long_mode() {
        long_default_s
    } else {
        short_default_s
    };
    env::var("ENSO_CHANNEL_BENCH_MEASURE_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(default_secs))
}

/// Sample size selection with optional env override.
pub fn get_sample_size(short_default: usize, long_default: usize) -> usize {
    env::var("ENSO_CHANNEL_BENCH_SAMPLES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or_else(|| {
            if is_long_mode() {
                long_default
            } else {
                short_default
            }
        })
}

/// Producer counts used by multi-producer benches.
pub fn get_producer_counts() -> Vec<usize> {
    parse_usize_list("ENSO_CHANNEL_BENCH_PRODUCERS", &[2, 4, 8])
}
