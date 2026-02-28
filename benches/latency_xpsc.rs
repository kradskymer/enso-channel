//! Latency benchmark for XPSC (SPSC, MPSC) channels.
//!
//! Design goals:
//! - Close to market-data stream: continuous publish/consume under bursts.
//! - Template-grade: small, independent, easy to copy/extend.
//! - Tail-friendly: reports p50/p90/p99/p99.9.
//! - Fair: all implementations use `try_*` + busy-spin.
//!
//! Metric semantics
//! - One sample per iteration (a "burst").
//! - We measure the time to *receive* `burst_size` messages.
//! - We record an amortized per-message latency: `elapsed_ns / burst_size`.
//!
//! Config (env vars)
//! - `ENSO_XPSC_BUFFER_SIZE` (default: 4096)
//!   - Must be a power of two (same constraint as `enso_channel` ring buffer).
//! - `ENSO_XPSC_BURST_SIZES` (default: 16,64,128)
//! - `ENSO_XPSC_OUTPUT` (default: both) values: csv | table | both
//! - `ENSO_XPSC_TIMEOUT_SECS` (default: 0 = disabled)
//!   - If non-zero, aborts the run instead of potentially spinning forever.
//! - `ENSO_CHANNEL_PINNING` (default: on)
//!   - Set to `0|off|false|no` to disable CPU pinning.
//!
//! Run with:
//! - `cargo bench --bench latency_xpsc`

use std::sync::{Arc, Barrier, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use core_affinity::CoreId;
use crossbeam_utils::Backoff;
use hdrhistogram::Histogram;

// =============================================================================
// Defaults
// =============================================================================

const DEFAULT_BUFFER_SIZE: usize = 4096;
const DEFAULT_BURST_SIZES: &[usize] = &[16, 64, 128];

// These are intentionally not configurable (per request) to keep the bench stable.
// Burst sizes already define total traffic volume.
const WARMUP_BURSTS: usize = 10_000;
const MEASURE_BURSTS: usize = 100_000;

// =============================================================================
// CPU Pinning (optional)
// =============================================================================

static AVAILABLE_CORES: OnceLock<Vec<CoreId>> = OnceLock::new();

fn pinning_enabled() -> bool {
    match std::env::var("ENSO_CHANNEL_PINNING") {
        Ok(v) => !matches!(v.as_str(), "0" | "off" | "false" | "no"),
        Err(_) => true,
    }
}

fn available_cores() -> &'static [CoreId] {
    AVAILABLE_CORES
        .get_or_init(|| core_affinity::get_core_ids().unwrap_or_default())
        .as_slice()
}

fn pin_thread(thread_index: usize) {
    if !pinning_enabled() {
        return;
    }
    let cores = available_cores();
    if cores.is_empty() {
        return;
    }
    let core = cores[thread_index % cores.len()];
    let _ = core_affinity::set_for_current(core);
}

// =============================================================================
// Output
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    Csv,
    Table,
    Both,
}

impl OutputMode {
    fn parse_env() -> Self {
        let v = std::env::var("ENSO_XPSC_OUTPUT").unwrap_or_else(|_| "both".to_string());
        match v.as_str() {
            "csv" => Self::Csv,
            "table" => Self::Table,
            "both" => Self::Both,
            _ => Self::Both,
        }
    }

    fn wants_csv(self) -> bool {
        matches!(self, Self::Csv | Self::Both)
    }

    fn wants_table(self) -> bool {
        matches!(self, Self::Table | Self::Both)
    }
}

fn print_kv(key: &str, value: impl std::fmt::Display) {
    println!("# {}={}", key, value);
}

// =============================================================================
// Stats
// =============================================================================

#[derive(Debug, Clone)]
struct Stats {
    samples: u64,
    mean_ns: f64,
    min_ns: u64,
    p50_ns: u64,
    p90_ns: u64,
    p99_ns: u64,
    p99_9_ns: u64,
    max_ns: u64,
}

fn stats_from_hist(hist: &Histogram<u64>) -> Stats {
    let samples = hist.len();
    debug_assert!(samples > 0, "no samples recorded");

    Stats {
        samples,
        mean_ns: hist.mean(),
        min_ns: hist.min(),
        p50_ns: hist.value_at_quantile(0.50),
        p90_ns: hist.value_at_quantile(0.90),
        p99_ns: hist.value_at_quantile(0.99),
        p99_9_ns: hist.value_at_quantile(0.999),
        max_ns: hist.max(),
    }
}

#[derive(Debug, Clone)]
struct Row {
    imp: &'static str,
    kind: &'static str,
    producers: usize,
    consumers: usize,
    batch_size: usize,
    burst_size: usize,
    buffer_size: usize,
    flags: String,
    stats: Stats,
}

fn print_csv(rows: &[Row]) {
    println!(
        "impl,kind,producers,consumers,batch_size,burst_size,buffer_size,flags,samples,mean_ns,min_ns,p50_ns,p90_ns,p99_ns,p99_9_ns,max_ns"
    );
    for r in rows {
        println!(
            "{},{},{},{},{},{},{},{},{},{:.3},{},{},{},{},{},{}",
            r.imp,
            r.kind,
            r.producers,
            r.consumers,
            r.batch_size,
            r.burst_size,
            r.buffer_size,
            r.flags,
            r.stats.samples,
            r.stats.mean_ns,
            r.stats.min_ns,
            r.stats.p50_ns,
            r.stats.p90_ns,
            r.stats.p99_ns,
            r.stats.p99_9_ns,
            r.stats.max_ns,
        );
    }
}

fn print_table(rows: &[Row]) {
    // Comment-prefixed table so scripts can parse CSV by ignoring '#' lines.
    println!("#");
    println!("# Table: burst-amortized per-message receive+read latency (ns)");
    println!(
        "# {:<20} {:<6} {:>3} {:>3} {:>5} {:>5} {:>9} {:>7} {:>7} {:>7} {:>7} {:>7}",
        "impl", "kind", "P", "C", "batch", "burst", "samples", "mean", "p50", "p99", "p99.9", "max"
    );
    for r in rows {
        println!(
            "# {:<20} {:<6} {:>3} {:>3} {:>5} {:>5} {:>9} {:>7.1} {:>7} {:>7} {:>7} {:>7}",
            r.imp,
            r.kind,
            r.producers,
            r.consumers,
            r.batch_size,
            r.burst_size,
            r.stats.samples,
            r.stats.mean_ns,
            r.stats.p50_ns,
            r.stats.p99_ns,
            r.stats.p99_9_ns,
            r.stats.max_ns,
        );
    }
}

// =============================================================================
// Config parsing
// =============================================================================

fn parse_usize_env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

fn parse_usize_list_env(name: &str, default: &[usize]) -> Vec<usize> {
    let Some(v) = std::env::var(name).ok() else {
        return default.to_vec();
    };
    let mut out = Vec::new();
    for part in v.split(',') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        if let Ok(n) = p.parse::<usize>() {
            if n > 0 {
                out.push(n);
            }
        }
    }
    if out.is_empty() {
        default.to_vec()
    } else {
        out
    }
}

fn parse_u64_env(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

// =============================================================================
// Bench runners
// =============================================================================

fn new_histogram() -> Histogram<u64> {
    // `new(3)` creates an auto-resizing histogram with 3 significant digits.
    // Auto-resize avoids hard-coding a max latency bound.
    Histogram::<u64>::new(3).expect("failed to create histogram")
}

fn record_sample(hist: &mut Histogram<u64>, per_msg_ns: u64) {
    if let Err(_e) = hist.record(per_msg_ns) {
        // Auto-resize should generally prevent errors, but keep this bench robust.
        hist.saturating_record(per_msg_ns);
    }
}

fn run_crossbeam_spsc(buffer_size: usize, burst_size: usize) -> Stats {
    let total_bursts = WARMUP_BURSTS + MEASURE_BURSTS;
    let total_messages = total_bursts * burst_size;

    let (tx, rx) = crossbeam_channel::bounded::<u64>(buffer_size);
    let barrier = Arc::new(Barrier::new(2));

    let barrier_p = barrier.clone();
    let producer = thread::spawn(move || {
        pin_thread(1);
        barrier_p.wait();

        let backoff = Backoff::new();
        for _ in 0..total_messages {
            backoff.reset();
            loop {
                match tx.try_send(std::hint::black_box(0u64)) {
                    Ok(()) => break,
                    Err(_) => backoff.spin(),
                }
            }
        }
    });

    pin_thread(0);
    barrier.wait();

    let mut hist = new_histogram();
    let backoff = Backoff::new();
    for burst_i in 0..total_bursts {
        let start = Instant::now();
        for _ in 0..burst_size {
            backoff.reset();
            loop {
                match rx.try_recv() {
                    Ok(v) => {
                        std::hint::black_box(v);
                        break;
                    }
                    Err(_) => backoff.spin(),
                }
            }
        }
        let elapsed_ns = start.elapsed().as_nanos() as u64;
        if burst_i >= WARMUP_BURSTS {
            record_sample(&mut hist, elapsed_ns / burst_size as u64);
        }
    }

    producer.join().expect("producer panicked");
    stats_from_hist(&hist)
}

fn run_enso_spsc(buffer_size: usize, burst_size: usize, batch_size: usize) -> Stats {
    let total_bursts = WARMUP_BURSTS + MEASURE_BURSTS;
    let total_messages = total_bursts * burst_size;

    let (mut tx, mut rx) = enso_channel::mpsc::channel::<u64>(buffer_size);
    let barrier = Arc::new(Barrier::new(2));

    let barrier_p = barrier.clone();
    let producer = thread::spawn(move || {
        pin_thread(1);
        barrier_p.wait();

        let backoff = Backoff::new();
        if batch_size == 1 {
            for _ in 0..total_messages {
                backoff.reset();
                loop {
                    match tx.try_send(std::hint::black_box(0u64)) {
                        Ok(()) => break,
                        Err(_) => backoff.spin(),
                    }
                }
            }
        } else {
            let mut sent = 0usize;
            while sent < total_messages {
                let remaining = total_messages - sent;
                let to_send = remaining.min(batch_size);
                backoff.reset();
                loop {
                    if let Ok(mut batch) = tx.try_send_many(to_send, || std::hint::black_box(0u64))
                    {
                        let n = batch.capacity();
                        for _ in 0..n {
                            batch.write_next(std::hint::black_box(0u64));
                        }
                        batch.finish();
                        sent += n;
                        break;
                    } else {
                        backoff.spin();
                    }
                }
            }
        }
    });

    pin_thread(0);
    barrier.wait();

    let mut hist = new_histogram();
    let backoff = Backoff::new();
    for burst_i in 0..total_bursts {
        let start = Instant::now();
        if batch_size == 1 {
            for _ in 0..burst_size {
                backoff.reset();
                loop {
                    match rx.try_recv() {
                        Ok(v) => {
                            // Force an actual payload read for fairness vs value-returning impls.
                            std::hint::black_box(*v);
                            break;
                        }
                        Err(_) => backoff.spin(),
                    }
                }
            }
        } else {
            let mut received = 0usize;
            while received < burst_size {
                let remaining = burst_size - received;
                let to_recv = remaining.min(batch_size);
                backoff.reset();
                loop {
                    if let Ok(iter) = rx.try_recv_many(to_recv) {
                        for g in iter {
                            std::hint::black_box(*g);
                            received += 1;
                        }
                        break;
                    } else {
                        backoff.spin();
                    }
                }
            }
        }
        let elapsed_ns = start.elapsed().as_nanos() as u64;
        if burst_i >= WARMUP_BURSTS {
            record_sample(&mut hist, elapsed_ns / burst_size as u64);
        }
    }

    producer.join().expect("producer panicked");
    stats_from_hist(&hist)
}

fn run_enso_mpsc_p1(buffer_size: usize, burst_size: usize, batch_size: usize) -> Stats {
    let total_bursts = WARMUP_BURSTS + MEASURE_BURSTS;
    let total_messages = total_bursts * burst_size;

    let (tx, mut rx) = enso_channel::mpsc::channel::<u64>(buffer_size);
    let mut tx = tx;
    let barrier = Arc::new(Barrier::new(2));

    let barrier_p = barrier.clone();
    let producer = thread::spawn(move || {
        pin_thread(1);
        barrier_p.wait();

        let backoff = Backoff::new();
        if batch_size == 1 {
            for _ in 0..total_messages {
                backoff.reset();
                loop {
                    match tx.try_send(std::hint::black_box(0u64)) {
                        Ok(()) => break,
                        Err(_) => backoff.spin(),
                    }
                }
            }
        } else {
            let mut sent = 0usize;
            while sent < total_messages {
                let remaining = total_messages - sent;
                let to_send = remaining.min(batch_size);
                backoff.reset();
                loop {
                    if let Ok(mut batch) = tx.try_send_many(to_send, || std::hint::black_box(0u64))
                    {
                        let n = batch.capacity();
                        for _ in 0..n {
                            batch.write_next(std::hint::black_box(0u64));
                        }
                        batch.finish();
                        sent += n;
                        break;
                    } else {
                        backoff.spin();
                    }
                }
            }
        }
    });

    pin_thread(0);
    barrier.wait();

    let mut hist = new_histogram();
    let backoff = Backoff::new();
    for burst_i in 0..total_bursts {
        let start = Instant::now();
        if batch_size == 1 {
            for _ in 0..burst_size {
                backoff.reset();
                loop {
                    match rx.try_recv() {
                        Ok(v) => {
                            std::hint::black_box(*v);
                            break;
                        }
                        Err(_) => backoff.spin(),
                    }
                }
            }
        } else {
            let mut received = 0usize;
            while received < burst_size {
                let remaining = burst_size - received;
                let to_recv = remaining.min(batch_size);
                backoff.reset();
                loop {
                    if let Ok(iter) = rx.try_recv_many(to_recv) {
                        for g in iter {
                            std::hint::black_box(*g);
                            received += 1;
                        }
                        break;
                    } else {
                        backoff.spin();
                    }
                }
            }
        }
        let elapsed_ns = start.elapsed().as_nanos() as u64;
        if burst_i >= WARMUP_BURSTS {
            record_sample(&mut hist, elapsed_ns / burst_size as u64);
        }
    }

    producer.join().expect("producer panicked");
    stats_from_hist(&hist)
}

// =============================================================================
// Main
// =============================================================================

fn main() {
    let buffer_size = parse_usize_env("ENSO_XPSC_BUFFER_SIZE", DEFAULT_BUFFER_SIZE);
    let burst_sizes = parse_usize_list_env("ENSO_XPSC_BURST_SIZES", DEFAULT_BURST_SIZES);
    let output = OutputMode::parse_env();
    let timeout_secs = parse_u64_env("ENSO_XPSC_TIMEOUT_SECS", 0);

    if !buffer_size.is_power_of_two() {
        eprintln!(
            "ENSO_XPSC_BUFFER_SIZE must be a power of two (got {}).",
            buffer_size
        );
        std::process::exit(2);
    }

    // Timeout is a run-control feature. Keep it at the top-level (main) so runner
    // loops stay focused on the benchmark logic.
    if timeout_secs != 0 {
        let started = Instant::now();
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(timeout_secs));
            eprintln!(
                "latency_xpsc: timeout after {}s (elapsed={}ms) — aborting",
                timeout_secs,
                started.elapsed().as_millis()
            );
            std::process::abort();
        });
    }

    print_kv("bench", "latency_xpsc");
    print_kv("version", "v1");
    print_kv("metric", "burst_amortized_per_message_latency_ns");
    print_kv("buffer_size", buffer_size);
    print_kv(
        "burst_sizes",
        burst_sizes
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(","),
    );
    print_kv("warmup_bursts", WARMUP_BURSTS);
    print_kv("measure_bursts", MEASURE_BURSTS);
    print_kv(
        "pinning_enabled",
        if pinning_enabled() { "true" } else { "false" },
    );
    print_kv("cores_detected", available_cores().len());
    print_kv(
        "output",
        match output {
            OutputMode::Csv => "csv",
            OutputMode::Table => "table",
            OutputMode::Both => "both",
        },
    );
    print_kv("timeout_secs", timeout_secs);

    let mut rows = Vec::<Row>::new();

    // crossbeam-channel: SPSC only
    for &burst in &burst_sizes {
        rows.push(Row {
            imp: "crossbeam-channel",
            kind: "spsc",
            producers: 1,
            consumers: 1,
            batch_size: 1,
            burst_size: burst,
            buffer_size,
            flags: String::new(),
            stats: run_crossbeam_spsc(buffer_size, burst),
        });
    }

    // enso_channel: exclusive SPSC
    for &batch in &[1usize, 4, 16] {
        for &burst in &burst_sizes {
            let flags = if burst < batch {
                "burst_lt_batch".to_string()
            } else {
                String::new()
            };
            rows.push(Row {
                imp: "enso_channel",
                kind: "spsc",
                producers: 1,
                consumers: 1,
                batch_size: batch,
                burst_size: burst,
                buffer_size,
                flags,
                stats: run_enso_spsc(buffer_size, burst, batch),
            });
        }
    }

    // enso_channel: exclusive MPSC, producers=1
    for &batch in &[1usize, 4, 16] {
        for &burst in &burst_sizes {
            let flags = if burst < batch {
                "burst_lt_batch".to_string()
            } else {
                String::new()
            };
            rows.push(Row {
                imp: "enso_channel",
                kind: "mpsc",
                producers: 1,
                consumers: 1,
                batch_size: batch,
                burst_size: burst,
                buffer_size,
                flags,
                stats: run_enso_mpsc_p1(buffer_size, burst, batch),
            });
        }
    }

    if output.wants_csv() {
        print_csv(&rows);
    }
    if output.wants_table() {
        print_table(&rows);
    }
}
