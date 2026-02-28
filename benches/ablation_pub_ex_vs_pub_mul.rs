//! Ablation benchmark: compare exclusive::spsc (pub_ex) vs exclusive::mpsc (pub_mul with P=1)
//!
//! - Script-friendly output: `# key=val` header, CSV rows, comment-prefixed table.
//! - Experiment 1: spin-read hotspot (producer pacing via nanos sleep).
//! - Experiment 2: large-batch publish-range cost (batch sizes up to 1024).
//!
//! Run with:
//! - `cargo bench --bench ablation_pub_ex_vs_pub_mul`

use std::sync::{Arc, Barrier, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use core_affinity::CoreId;
use crossbeam_utils::Backoff;
use hdrhistogram::Histogram;

const DEFAULT_BUFFER_SIZE: usize = 4096;
const DEFAULT_BURST_SIZES: &[usize] = &[64, 256, 1024];
const DEFAULT_BATCH_SIZES: &[usize] = &[1, 16, 256, 1024];

// Experiment A (spin-read hotspot): enough samples for stable percentiles.
const HOTSPOT_WARMUP_BURSTS: usize = 2000;
const HOTSPOT_MEASURE_BURSTS: usize = 10_000;

// Experiment B (large batch publish-range cost): use larger bursts to avoid
// `elapsed_ns / burst_size` rounding down to 0 on very fast paths.
const BATCH_WARMUP_BURSTS: usize = 500;
const BATCH_MEASURE_BURSTS: usize = 3000;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    Csv,
    Table,
    Both,
}

impl OutputMode {
    fn parse_env() -> Self {
        let v = std::env::var("ENSO_ABLATION_OUTPUT").unwrap_or_else(|_| "both".to_string());
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

#[derive(Debug, Clone)]
struct Stats {
    samples: u64,
    mean_ps: f64,
    min_ps: u64,
    p50_ps: u64,
    p90_ps: u64,
    p99_ps: u64,
    p99_9_ps: u64,
    max_ps: u64,
}

fn stats_from_hist(hist: &Histogram<u64>) -> Stats {
    let samples = hist.len();
    debug_assert!(samples > 0, "no samples recorded");

    Stats {
        samples,
        mean_ps: hist.mean(),
        min_ps: hist.min(),
        p50_ps: hist.value_at_quantile(0.50),
        p90_ps: hist.value_at_quantile(0.90),
        p99_ps: hist.value_at_quantile(0.99),
        p99_9_ps: hist.value_at_quantile(0.999),
        max_ps: hist.max(),
    }
}

#[derive(Debug, Clone)]
struct Row {
    exp: &'static str,
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
        "exp,impl,kind,producers,consumers,batch_size,burst_size,buffer_size,flags,samples,mean_ps,min_ps,p50_ps,p90_ps,p99_ps,p99_9_ps,max_ps"
    );
    for r in rows {
        println!(
            "{},{},{},{},{},{},{},{},{},{},{:.3},{},{},{},{},{},{}",
            r.exp,
            r.imp,
            r.kind,
            r.producers,
            r.consumers,
            r.batch_size,
            r.burst_size,
            r.buffer_size,
            r.flags,
            r.stats.samples,
            r.stats.mean_ps,
            r.stats.min_ps,
            r.stats.p50_ps,
            r.stats.p90_ps,
            r.stats.p99_ps,
            r.stats.p99_9_ps,
            r.stats.max_ps,
        );
    }
}

fn print_table(rows: &[Row]) {
    println!("#");
    println!("# Table: burst-amortized per-message receive+read latency (ns)");
    println!(
        "# {:<9} {:<20} {:<14} {:>3} {:>3} {:>5} {:>6} {:>9} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "exp",
        "impl",
        "kind",
        "P",
        "C",
        "batch",
        "burst",
        "samples",
        "mean",
        "p50",
        "p99",
        "p99.9",
        "max"
    );
    for r in rows {
        let mean_ns = r.stats.mean_ps / 1000.0;
        let p50_ns = r.stats.p50_ps as f64 / 1000.0;
        let p99_ns = r.stats.p99_ps as f64 / 1000.0;
        let p99_9_ns = r.stats.p99_9_ps as f64 / 1000.0;
        let max_ns = r.stats.max_ps as f64 / 1000.0;
        println!(
            "# {:<9} {:<20} {:<14} {:>3} {:>3} {:>5} {:>6} {:>9} {:>8.3} {:>8.3} {:>8.3} {:>8.3} {:>8.3}",
            r.exp,
            r.imp,
            r.kind,
            r.producers,
            r.consumers,
            r.batch_size,
            r.burst_size,
            r.stats.samples,
            mean_ns,
            p50_ns,
            p99_ns,
            p99_9_ns,
            max_ns,
        );
    }
}

fn new_histogram() -> Histogram<u64> {
    Histogram::<u64>::new(3).expect("failed to create histogram")
}

fn record_sample(hist: &mut Histogram<u64>, per_msg_ps: u64) {
    if let Err(_e) = hist.record(per_msg_ps) {
        hist.saturating_record(per_msg_ps);
    }
}

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

fn run_enso_spsc(
    buffer_size: usize,
    burst_size: usize,
    batch_size: usize,
    pacing_nanos: u64,
    warmup_bursts: usize,
    measure_bursts: usize,
) -> Stats {
    let total_bursts = warmup_bursts + measure_bursts;
    let total_messages = total_bursts * burst_size;

    let (mut tx, mut rx) = enso_channel::exclusive::spsc::channel::<u64>(buffer_size);
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
                if pacing_nanos != 0 {
                    thread::sleep(Duration::from_nanos(pacing_nanos));
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
                if pacing_nanos != 0 {
                    thread::sleep(Duration::from_nanos(pacing_nanos));
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
        if burst_i >= warmup_bursts {
            let elapsed_ps = elapsed_ns.saturating_mul(1000);
            record_sample(&mut hist, elapsed_ps / burst_size as u64);
        }
    }

    producer.join().expect("producer panicked");
    stats_from_hist(&hist)
}

fn run_enso_mpsc_p1(
    buffer_size: usize,
    burst_size: usize,
    batch_size: usize,
    pacing_nanos: u64,
    warmup_bursts: usize,
    measure_bursts: usize,
) -> Stats {
    let total_bursts = warmup_bursts + measure_bursts;
    let total_messages = total_bursts * burst_size;

    let (tx, mut rx) = enso_channel::exclusive::mpsc::channel::<u64>(buffer_size);
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
                if pacing_nanos != 0 {
                    thread::sleep(Duration::from_nanos(pacing_nanos));
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
                if pacing_nanos != 0 {
                    thread::sleep(Duration::from_nanos(pacing_nanos));
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
        if burst_i >= warmup_bursts {
            let elapsed_ps = elapsed_ns.saturating_mul(1000);
            record_sample(&mut hist, elapsed_ps / burst_size as u64);
        }
    }

    producer.join().expect("producer panicked");
    stats_from_hist(&hist)
}

fn main() {
    let buffer_size = parse_usize_env("ENSO_ABLATION_BUFFER_SIZE", DEFAULT_BUFFER_SIZE);
    let burst_sizes = parse_usize_list_env("ENSO_ABLATION_BURST_SIZES", DEFAULT_BURST_SIZES);
    let batch_sizes = parse_usize_list_env("ENSO_ABLATION_BATCH_SIZES", DEFAULT_BATCH_SIZES);
    let pacing_nanos = parse_u64_env("ENSO_ABLATION_PACING_NANOS", 0);
    let output = OutputMode::parse_env();
    let timeout_secs = parse_u64_env("ENSO_ABLATION_TIMEOUT_SECS", 0);

    if !buffer_size.is_power_of_two() {
        eprintln!(
            "ENSO_ABLATION_BUFFER_SIZE must be a power of two (got {}).",
            buffer_size
        );
        std::process::exit(2);
    }

    if timeout_secs != 0 {
        let started = Instant::now();
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(timeout_secs));
            eprintln!(
                "ablation_pub_ex_vs_pub_mul: timeout after {}s (elapsed={}ms) — aborting",
                timeout_secs,
                started.elapsed().as_millis()
            );
            std::process::abort();
        });
    }

    print_kv("bench", "ablation_pub_ex_vs_pub_mul");
    print_kv("version", "v1");
    print_kv("metric", "burst_amortized_per_message_latency_ps");
    print_kv("buffer_size", buffer_size);
    print_kv(
        "burst_sizes",
        burst_sizes
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(","),
    );
    print_kv(
        "batch_sizes",
        batch_sizes
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(","),
    );
    print_kv("pacing_nanos", pacing_nanos);
    print_kv("hotspot_warmup_bursts", HOTSPOT_WARMUP_BURSTS);
    print_kv("hotspot_measure_bursts", HOTSPOT_MEASURE_BURSTS);
    print_kv("batch_warmup_bursts", BATCH_WARMUP_BURSTS);
    print_kv("batch_measure_bursts", BATCH_MEASURE_BURSTS);
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

    // Experiment A: Spin-read hotspot — producer pacing slows producer so consumer spins.
    for &burst in &burst_sizes {
        // SPSC (pub_ex)
        rows.push(Row {
            exp: "hotspot",
            imp: "enso_channel",
            kind: "spsc_pub_ex",
            producers: 1,
            consumers: 1,
            batch_size: 1,
            burst_size: burst,
            buffer_size,
            flags: format!("pacing_nanos={}", pacing_nanos),
            stats: run_enso_spsc(
                buffer_size,
                burst,
                1,
                pacing_nanos,
                HOTSPOT_WARMUP_BURSTS,
                HOTSPOT_MEASURE_BURSTS,
            ),
        });

        // MPSC with producers=1 (pub_mul)
        rows.push(Row {
            exp: "hotspot",
            imp: "enso_channel",
            kind: "mpsc_pub_mul_p1",
            producers: 1,
            consumers: 1,
            batch_size: 1,
            burst_size: burst,
            buffer_size,
            flags: format!("pacing_nanos={}", pacing_nanos),
            stats: run_enso_mpsc_p1(
                buffer_size,
                burst,
                1,
                pacing_nanos,
                HOTSPOT_WARMUP_BURSTS,
                HOTSPOT_MEASURE_BURSTS,
            ),
        });
    }

    // Experiment B: Large-batch publish-range cost — vary batch sizes and large bursts.
    // Pick a larger burst to keep `elapsed_ns / burst_size` above the timer's effective
    // resolution on very fast paths (avoids 0 ns/message artifacts).
    let large_burst = buffer_size * 4;
    print_kv("batch_burst_size", large_burst);
    for &batch in &batch_sizes {
        rows.push(Row {
            exp: "batch_cost",
            imp: "enso_channel",
            kind: "spsc_pub_ex",
            producers: 1,
            consumers: 1,
            batch_size: batch,
            burst_size: large_burst,
            buffer_size,
            flags: String::new(),
            stats: run_enso_spsc(
                buffer_size,
                large_burst,
                batch,
                0,
                BATCH_WARMUP_BURSTS,
                BATCH_MEASURE_BURSTS,
            ),
        });
        rows.push(Row {
            exp: "batch_cost",
            imp: "enso_channel",
            kind: "mpsc_pub_mul_p1",
            producers: 1,
            consumers: 1,
            batch_size: batch,
            burst_size: large_burst,
            buffer_size,
            flags: String::new(),
            stats: run_enso_mpsc_p1(
                buffer_size,
                large_burst,
                batch,
                0,
                BATCH_WARMUP_BURSTS,
                BATCH_MEASURE_BURSTS,
            ),
        });
    }

    // TODO: Experiment C: Fanout SPMC vs MPMC (1 producer, N consumers) — optional follow-up.

    if output.wants_csv() {
        print_csv(&rows);
    }
    if output.wants_table() {
        print_table(&rows);
    }
}
