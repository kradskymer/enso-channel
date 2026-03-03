//! Latency benchmark for MPMC work queues with Criterion.
//!
//! This benchmark is designed to highlight the tunability of `enso_channel::mpmc`
//! under high contention.
//!
//! Metric semantics:
//! - Each Criterion inner iteration executes exactly one burst.
//! - A burst means each producer publishes exactly `burst_size` messages.
//! - Total messages per burst = `burst_size * producers`.
//! - The burst is complete when all those messages have been consumed (work-queue semantics).
//! - We record a raw per-burst sample (`elapsed_ns`) and report it as burst latency (ns/burst).
//!
//! Env vars:
//! - `ENSO_MPMC_BUFFER_SIZE` (default: 4096, must be power of two)
//! - `ENSO_MPMC_BURST_SIZES` (default: 1,16,32,64)
//! - `ENSO_MPMC_OUTPUT` (default: both) values: csv | table | both
//! - `ENSO_MPMC_TIMEOUT_SECS` (default: 300; 0 disables watchdog)
//! - `ENSO_CHANNEL_PINNING` (default: on)
//! - `ENSO_MPMC_OUTPUT_DIR` (optional, default: benches/results)
//! - `ENSO_MPMC_WARMUP_BURSTS` (default: 10_000)
//! - `ENSO_MPMC_MEASURE_BURSTS` (default: 100_000)
//!
//! Run with:
//! - `cargo bench --bench mpmc`

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use criterion::{BenchmarkId, Criterion, Throughput};
use crossbeam_utils::Backoff;

#[path = "bench_support/mod.rs"]
mod bench_support;

use bench_support::{
    parse_u64_env, parse_usize_env, parse_usize_list_env, resolve_output_dir,
    spawn_timeout_watchdog, write_reports, BurstRecorder, BurstStats, CorePinning, OutputMode,
    ReportRow, DEFAULT_RESULTS_DIR,
};

const DEFAULT_BUFFER_SIZE: usize = 4096;
const DEFAULT_BURST_SIZES: &[usize] = &[1, 16, 32, 64];
const DEFAULT_TIMEOUT_SECS: u64 = 600;
const DEFAULT_WARMUP_BURSTS: u64 = 10_000;
const DEFAULT_MEASURE_BURSTS: u64 = 100_000;
const BENCH_NAME: &str = "mpmc";

const TOPOLOGIES: &[(usize, usize)] = &[(2, 2), (4, 4)];

// Include (1,1) as a baseline, then a set of tunable batch pairs.
const ENSO_BATCH_PAIRS: &[(usize, usize)] = &[(1, 1), (4, 16), (4, 32), (8, 16), (8, 32), (8, 64)];

#[derive(Clone)]
struct RunnerConfig {
    buffer_size: usize,
    producers: usize,
    consumers: usize,
    burst_size: usize,
    pinning: Arc<CorePinning>,
    warmup_bursts: u64,
    measure_bursts: u64,
}

#[derive(Debug)]
struct CrossbeamRunner {
    producers: usize,
    consumers: usize,
    burst_size: usize,
    total_messages: usize,
    iter_to_run: Arc<AtomicU64>,
    iter_completed: Arc<AtomicU64>,
    remaining: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    recorder: BurstRecorder,
    producer_handles: Vec<thread::JoinHandle<()>>,
    consumer_handles: Vec<thread::JoinHandle<()>>,
}

impl CrossbeamRunner {
    fn new(config: RunnerConfig) -> Self {
        let RunnerConfig {
            buffer_size,
            producers,
            consumers,
            burst_size,
            pinning,
            warmup_bursts,
            measure_bursts,
        } = config;

        let (tx, rx) = crossbeam_channel::bounded::<u64>(buffer_size);

        let iter_to_run = Arc::new(AtomicU64::new(0));
        let iter_completed = Arc::new(AtomicU64::new(0));
        let remaining = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        let mut producer_handles = Vec::with_capacity(producers);
        for producer_idx in 0..producers {
            let tx_for_thread = tx.clone();
            let iter_to_run_for_thread = iter_to_run.clone();
            let stop_for_thread = stop.clone();
            let pinning_for_thread = pinning.clone();

            producer_handles.push(thread::spawn(move || {
                // Keep core 0 for main, then consumers, then producers.
                pinning_for_thread.pin_current(1 + consumers + producer_idx);
                run_crossbeam_producer(
                    tx_for_thread,
                    iter_to_run_for_thread,
                    stop_for_thread,
                    burst_size,
                );
            }));
        }

        let mut consumer_handles = Vec::with_capacity(consumers);
        for consumer_idx in 0..consumers {
            let rx_for_thread = rx.clone();
            let iter_to_run_for_thread = iter_to_run.clone();
            let iter_completed_for_thread = iter_completed.clone();
            let remaining_for_thread = remaining.clone();
            let stop_for_thread = stop.clone();
            let pinning_for_thread = pinning.clone();

            consumer_handles.push(thread::spawn(move || {
                pinning_for_thread.pin_current(1 + consumer_idx);
                run_crossbeam_consumer(
                    rx_for_thread,
                    iter_to_run_for_thread,
                    iter_completed_for_thread,
                    remaining_for_thread,
                    stop_for_thread,
                );
            }));
        }

        Self {
            producers,
            consumers,
            burst_size,
            total_messages: producers * burst_size,
            iter_to_run,
            iter_completed,
            remaining,
            stop,
            recorder: BurstRecorder::new(warmup_bursts, measure_bursts),
            producer_handles,
            consumer_handles,
        }
    }

    fn run_one_burst(&mut self) {
        let backoff = Backoff::new();
        let start = Instant::now();

        self.remaining.store(self.total_messages, Ordering::Release);
        let iteration = self.iter_to_run.fetch_add(1, Ordering::AcqRel) + 1;

        backoff.reset();
        loop {
            let completed = self.iter_completed.load(Ordering::Acquire);
            std::hint::black_box(completed);
            if completed >= iteration {
                break;
            }
            if self.stop.load(Ordering::Acquire) {
                break;
            }
            backoff.spin();
        }

        let elapsed_ns = start.elapsed().as_nanos() as u64;
        self.recorder.record_burst(elapsed_ns);
    }

    fn stats(&self) -> BurstStats {
        self.recorder.stats()
    }

    fn fill_recorder(&mut self) {
        if self.recorder.is_complete() {
            return;
        }

        let max_extra_bursts = self.recorder.warmup_bursts() + self.recorder.target_samples();
        let mut extra_bursts = 0u64;

        while !self.recorder.is_complete() && extra_bursts < max_extra_bursts {
            self.run_one_burst();
            extra_bursts += 1;
        }

        if !self.recorder.is_complete() {
            eprintln!(
                "warning: recorder incomplete for crossbeam/mpmc (p={}, c={}, burst={}): measured={} target={} remaining={} extra_bursts={} max_extra_bursts={}",
                self.producers,
                self.consumers,
                self.burst_size,
                self.recorder.measured_samples(),
                self.recorder.target_samples(),
                self.recorder.remaining(),
                extra_bursts,
                max_extra_bursts,
            );
        }
    }
}

impl Drop for CrossbeamRunner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);

        for handle in self.consumer_handles.drain(..) {
            let _ = handle.join();
        }
        for handle in self.producer_handles.drain(..) {
            let _ = handle.join();
        }
    }
}

fn run_crossbeam_producer(
    tx: crossbeam_channel::Sender<u64>,
    iter_to_run: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    burst_size: usize,
) {
    let backoff = Backoff::new();
    let mut observed_iter = iter_to_run.load(Ordering::Acquire);

    while !stop.load(Ordering::Acquire) {
        let target_iter = iter_to_run.load(Ordering::Acquire);
        if target_iter == observed_iter {
            backoff.spin();
            continue;
        }

        // `iter_to_run` is a generation number. If this producer ever falls behind,
        // it must only act on the latest generation (the main thread cannot advance
        // generations until the current one completes).
        observed_iter = target_iter;

        for _ in 0..burst_size {
            backoff.reset();
            loop {
                if stop.load(Ordering::Acquire) {
                    return;
                }

                match tx.try_send(std::hint::black_box(0u64)) {
                    Ok(()) => break,
                    Err(crossbeam_channel::TrySendError::Full(_)) => backoff.spin(),
                    Err(crossbeam_channel::TrySendError::Disconnected(_)) => return,
                }
            }
        }
    }
}

fn run_crossbeam_consumer(
    rx: crossbeam_channel::Receiver<u64>,
    iter_to_run: Arc<AtomicU64>,
    iter_completed: Arc<AtomicU64>,
    remaining: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
) {
    let backoff = Backoff::new();
    let mut observed_iter = iter_to_run.load(Ordering::Acquire);

    while !stop.load(Ordering::Acquire) {
        let target_iter = iter_to_run.load(Ordering::Acquire);
        if target_iter == observed_iter {
            backoff.spin();
            continue;
        }

        // Treat `iter_to_run` as a generation number (not a per-consumer counter).
        // If this consumer thread was descheduled, it must skip directly to the
        // latest generation; older generations are already complete.
        observed_iter = target_iter;

        while remaining.load(Ordering::Acquire) > 0 {
            let mut received = 0usize;

            // Crossbeam has no batched recv; grab a small chunk to amortize atomics.
            for _ in 0..64 {
                if remaining.load(Ordering::Acquire) == 0 {
                    break;
                }

                match rx.try_recv() {
                    Ok(value) => {
                        std::hint::black_box(value);
                        received += 1;
                    }
                    Err(crossbeam_channel::TryRecvError::Empty) => break,
                    Err(crossbeam_channel::TryRecvError::Disconnected) => return,
                }
            }

            if received > 0 {
                let prev = remaining.fetch_sub(received, Ordering::AcqRel);
                if prev <= received {
                    remaining.store(0, Ordering::Release);
                    iter_completed.store(observed_iter, Ordering::Release);
                    break;
                }
            } else {
                backoff.spin();
            }

            if stop.load(Ordering::Acquire) {
                return;
            }
        }

        if remaining.load(Ordering::Acquire) == 0 {
            iter_completed.store(observed_iter, Ordering::Release);
        }
    }
}

#[derive(Debug)]
struct EnsoRunner {
    producers: usize,
    consumers: usize,
    burst_size: usize,
    total_messages: usize,
    send_limit: usize,
    recv_limit: usize,
    iter_to_run: Arc<AtomicU64>,
    iter_completed: Arc<AtomicU64>,
    remaining: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    recorder: BurstRecorder,
    producer_handles: Vec<thread::JoinHandle<()>>,
    consumer_handles: Vec<thread::JoinHandle<()>>,
}

impl EnsoRunner {
    fn new(config: RunnerConfig, send_limit: usize, recv_limit: usize) -> Self {
        let RunnerConfig {
            buffer_size,
            producers,
            consumers,
            burst_size,
            pinning,
            warmup_bursts,
            measure_bursts,
        } = config;

        let (tx, rx) = enso_channel::mpmc::channel::<u64>(buffer_size);

        let iter_to_run = Arc::new(AtomicU64::new(0));
        let iter_completed = Arc::new(AtomicU64::new(0));
        let remaining = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        let mut producer_handles = Vec::with_capacity(producers);
        for producer_idx in 0..producers {
            let mut tx_for_thread = tx.clone();
            let iter_to_run_for_thread = iter_to_run.clone();
            let stop_for_thread = stop.clone();
            let pinning_for_thread = pinning.clone();

            producer_handles.push(thread::spawn(move || {
                pinning_for_thread.pin_current(1 + consumers + producer_idx);
                run_enso_producer(
                    &mut tx_for_thread,
                    iter_to_run_for_thread,
                    stop_for_thread,
                    burst_size,
                    send_limit,
                );
            }));
        }

        let mut consumer_handles = Vec::with_capacity(consumers);
        for consumer_idx in 0..consumers {
            let mut rx_for_thread = rx.clone();
            let iter_to_run_for_thread = iter_to_run.clone();
            let iter_completed_for_thread = iter_completed.clone();
            let remaining_for_thread = remaining.clone();
            let stop_for_thread = stop.clone();
            let pinning_for_thread = pinning.clone();

            consumer_handles.push(thread::spawn(move || {
                pinning_for_thread.pin_current(1 + consumer_idx);
                run_enso_consumer(
                    &mut rx_for_thread,
                    iter_to_run_for_thread,
                    iter_completed_for_thread,
                    remaining_for_thread,
                    stop_for_thread,
                    recv_limit,
                );
            }));
        }

        Self {
            producers,
            consumers,
            burst_size,
            total_messages: producers * burst_size,
            send_limit: send_limit.max(1),
            recv_limit: recv_limit.max(1),
            iter_to_run,
            iter_completed,
            remaining,
            stop,
            recorder: BurstRecorder::new(warmup_bursts, measure_bursts),
            producer_handles,
            consumer_handles,
        }
    }

    fn run_one_burst(&mut self) {
        let backoff = Backoff::new();
        let start = Instant::now();

        self.remaining.store(self.total_messages, Ordering::Release);
        let iteration = self.iter_to_run.fetch_add(1, Ordering::AcqRel) + 1;

        backoff.reset();
        loop {
            let completed = self.iter_completed.load(Ordering::Acquire);
            std::hint::black_box(completed);
            if completed >= iteration {
                break;
            }
            if self.stop.load(Ordering::Acquire) {
                break;
            }
            backoff.spin();
        }

        let elapsed_ns = start.elapsed().as_nanos() as u64;
        self.recorder.record_burst(elapsed_ns);
    }

    fn stats(&self) -> BurstStats {
        self.recorder.stats()
    }

    fn fill_recorder(&mut self) {
        if self.recorder.is_complete() {
            return;
        }

        let max_extra_bursts = self.recorder.warmup_bursts() + self.recorder.target_samples();
        let mut extra_bursts = 0u64;

        while !self.recorder.is_complete() && extra_bursts < max_extra_bursts {
            self.run_one_burst();
            extra_bursts += 1;
        }

        if !self.recorder.is_complete() {
            eprintln!(
                "warning: recorder incomplete for enso/mpmc (p={}, c={}, burst={}, send_limit={}, recv_limit={}): measured={} target={} remaining={} extra_bursts={} max_extra_bursts={}",
                self.producers,
                self.consumers,
                self.burst_size,
                self.send_limit,
                self.recv_limit,
                self.recorder.measured_samples(),
                self.recorder.target_samples(),
                self.recorder.remaining(),
                extra_bursts,
                max_extra_bursts,
            );
        }
    }
}

impl Drop for EnsoRunner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);

        for handle in self.consumer_handles.drain(..) {
            let _ = handle.join();
        }
        for handle in self.producer_handles.drain(..) {
            let _ = handle.join();
        }
    }
}

fn run_enso_producer(
    tx: &mut enso_channel::mpmc::Sender<u64>,
    iter_to_run: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    burst_size: usize,
    send_limit: usize,
) {
    let backoff = Backoff::new();
    let mut observed_iter = iter_to_run.load(Ordering::Acquire);

    while !stop.load(Ordering::Acquire) {
        let target_iter = iter_to_run.load(Ordering::Acquire);
        if target_iter == observed_iter {
            backoff.spin();
            continue;
        }

        observed_iter = target_iter;

        let mut sent = 0usize;
        while sent < burst_size {
            let remaining = burst_size - sent;
            let limit = remaining.min(send_limit.max(1));

            backoff.reset();
            loop {
                if stop.load(Ordering::Acquire) {
                    return;
                }

                match tx.try_send_at_most(limit, || std::hint::black_box(0u64)) {
                    Ok(batch) => {
                        sent += batch.capacity();
                        break;
                    }
                    Err(enso_channel::errors::TrySendAtMostError::Full) => backoff.spin(),
                    Err(enso_channel::errors::TrySendAtMostError::Disconnected) => return,
                }
            }
        }
    }
}

fn run_enso_consumer(
    rx: &mut enso_channel::mpmc::Receiver<u64>,
    iter_to_run: Arc<AtomicU64>,
    iter_completed: Arc<AtomicU64>,
    remaining: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    recv_limit: usize,
) {
    let backoff = Backoff::new();
    let mut observed_iter = iter_to_run.load(Ordering::Acquire);

    while !stop.load(Ordering::Acquire) {
        let target_iter = iter_to_run.load(Ordering::Acquire);
        if target_iter == observed_iter {
            backoff.spin();
            continue;
        }

        observed_iter = target_iter;

        while remaining.load(Ordering::Acquire) > 0 {
            let rem = remaining.load(Ordering::Acquire);
            if rem == 0 {
                break;
            }

            let limit = rem.min(recv_limit.max(1));
            let mut received = 0usize;

            match rx.try_recv_at_most(limit) {
                Ok(iter) => {
                    for guard in iter.iter() {
                        std::hint::black_box(*guard);
                        received += 1;
                    }
                }
                Err(enso_channel::errors::TryRecvAtMostError::Empty) => {}
                Err(enso_channel::errors::TryRecvAtMostError::Disconnected) => return,
            }

            if received > 0 {
                let prev = remaining.fetch_sub(received, Ordering::AcqRel);
                if prev <= received {
                    remaining.store(0, Ordering::Release);
                    iter_completed.store(observed_iter, Ordering::Release);
                    break;
                }
            } else {
                backoff.spin();
            }

            if stop.load(Ordering::Acquire) {
                return;
            }
        }

        if remaining.load(Ordering::Acquire) == 0 {
            iter_completed.store(observed_iter, Ordering::Release);
        }
    }
}

#[derive(Debug, Clone)]
struct BenchSettings {
    buffer_size: usize,
    burst_sizes: Vec<usize>,
    output_mode: OutputMode,
    timeout_secs: u64,
    output_dir: std::path::PathBuf,
    warmup_bursts: u64,
    measure_bursts: u64,
}

impl BenchSettings {
    fn from_env() -> Self {
        let buffer_size = parse_usize_env("ENSO_MPMC_BUFFER_SIZE", DEFAULT_BUFFER_SIZE);
        let burst_sizes = parse_usize_list_env("ENSO_MPMC_BURST_SIZES", DEFAULT_BURST_SIZES);
        let output_mode = OutputMode::parse_env("ENSO_MPMC_OUTPUT", "both");
        let timeout_secs = parse_u64_env("ENSO_MPMC_TIMEOUT_SECS", DEFAULT_TIMEOUT_SECS);
        let output_dir = resolve_output_dir("ENSO_MPMC_OUTPUT_DIR", DEFAULT_RESULTS_DIR);
        let warmup_bursts = parse_u64_env("ENSO_MPMC_WARMUP_BURSTS", DEFAULT_WARMUP_BURSTS);
        let measure_bursts = parse_u64_env("ENSO_MPMC_MEASURE_BURSTS", DEFAULT_MEASURE_BURSTS);

        Self {
            buffer_size,
            burst_sizes,
            output_mode,
            timeout_secs,
            output_dir,
            warmup_bursts,
            measure_bursts,
        }
    }
}

fn bench_latency_mpmc(
    c: &mut Criterion,
    settings: &BenchSettings,
    pinning: Arc<CorePinning>,
) -> Vec<ReportRow> {
    let mut rows = Vec::<ReportRow>::new();
    let mut group = c.benchmark_group(BENCH_NAME);

    for &(producers, consumers) in TOPOLOGIES {
        for &burst_size in &settings.burst_sizes {
            group.throughput(Throughput::Elements(1));

            let mut scenario = CrossbeamRunner::new(RunnerConfig {
                buffer_size: settings.buffer_size,
                producers,
                consumers,
                burst_size,
                pinning: pinning.clone(),
                warmup_bursts: settings.warmup_bursts,
                measure_bursts: settings.measure_bursts,
            });

            group.bench_function(
                BenchmarkId::new(
                    format!("crossbeam/mpmc/p{producers}c{consumers}"),
                    burst_size,
                ),
                |b| {
                    b.iter(|| {
                        scenario.run_one_burst();
                    })
                },
            );

            scenario.fill_recorder();

            rows.push(ReportRow {
                scenario: "crossbeam-channel".to_string(),
                producers,
                consumers,
                buffer_size: settings.buffer_size,
                burst_size,
                stats: scenario.stats(),
            });
        }

        for &(send_limit, recv_limit) in ENSO_BATCH_PAIRS {
            for &burst_size in &settings.burst_sizes {
                group.throughput(Throughput::Elements(1));

                let mut scenario = EnsoRunner::new(
                    RunnerConfig {
                        buffer_size: settings.buffer_size,
                        producers,
                        consumers,
                        burst_size,
                        pinning: pinning.clone(),
                        warmup_bursts: settings.warmup_bursts,
                        measure_bursts: settings.measure_bursts,
                    },
                    send_limit,
                    recv_limit,
                );

                group.bench_function(
                    BenchmarkId::new(
                        format!(
                            "enso_channel::mpmc/p{producers}c{consumers}/sl{send_limit}_rl{recv_limit}"
                        ),
                        burst_size,
                    ),
                    |b| {
                        b.iter(|| {
                            scenario.run_one_burst();
                        })
                    },
                );

                scenario.fill_recorder();

                rows.push(ReportRow {
                    scenario: format!(
                        "enso_channel::mpmc send_limit={send_limit} recv_limit={recv_limit}"
                    ),
                    producers,
                    consumers,
                    buffer_size: settings.buffer_size,
                    burst_size,
                    stats: scenario.stats(),
                });
            }
        }
    }

    group.finish();
    rows
}

fn main() {
    let settings = BenchSettings::from_env();

    if !settings.buffer_size.is_power_of_two() {
        eprintln!(
            "ENSO_MPMC_BUFFER_SIZE must be a power of two (got {}).",
            settings.buffer_size
        );
        std::process::exit(2);
    }

    spawn_timeout_watchdog(settings.timeout_secs, BENCH_NAME);

    let pinning = Arc::new(CorePinning::from_env("ENSO_CHANNEL_PINNING"));
    pinning.pin_current(0);

    let mut criterion = Criterion::default().configure_from_args();
    let rows = bench_latency_mpmc(&mut criterion, &settings, pinning);

    criterion.final_summary();
    write_reports(
        &rows,
        settings.output_mode,
        &settings.output_dir,
        BENCH_NAME,
    );
}
